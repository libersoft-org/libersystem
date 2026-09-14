//! THE CLIENT SIDE OF A PRESENT QUEUE.
//!
//! WHAT A CLIENT ACTUALLY HAS TO GET RIGHT, gathered in one place so every application does not
//! reimplement it: acknowledge a configuration by serial before presenting into it, acquire without
//! blocking, treat `again` as "wait for the event" rather than as an error, hand an image back when
//! a resize arrives instead of presenting a frame nobody wanted, and rebuild the queue when the
//! generation moves.
//!
//! THE STATE MACHINE IS THE PROFILE'S. `Available -> Acquired -> PendingPresent -> Available`, with
//! `abandon` as the edge back from `Acquired` that is not a present and `Stale` for everything a
//! generation change left behind. An application that tracked this itself would get the abandon edge
//! wrong, which leaks an image per resize.

#![no_std]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use core::cell::RefCell;
use display_proto::codec::Handles;
pub use display_proto::generated::liber::display::v1::{AcquiredImage, SurfaceConfiguration, SurfaceEvent};
use display_proto::generated::liber::display::v1::{DamageRegion, ImageLimits, PresentQueue, SurfaceRequest, display, surface as wire};
use display_proto::generated::liber::graphics::v1::{Extent2d, Offset2d, Rect as WireRect};
use graphics_core::format::{PixelFormat, PixelStorage};
use graphics_core::geom::Extent2D;
use graphics_core::layout::ImageLayout;
use graphics_core::pixel::OutputLuminance;
use input_proto::generated::liber::input::v1::input;
use ipc_client::ChannelTransport;
use rt::{Polled, PolledCaps, RIGHT_MAP, RIGHT_READ, RIGHT_TRANSFER, close, duplicate, map_object, memory_object_create, try_recv, try_recv_caps, try_send, unmap_object};

pub use pix::{BlitResult, Image, Rect, Target};

/// An extent on the wire, so a caller need not reach into the generated module to name one.
pub const fn wire_extent(width: u32, height: u32) -> Extent2d {
	Extent2d { width, height }
}

/// A connection, which owns as many surfaces as the application needs.
pub type Client = Rc<RefCell<display::Client<ChannelTransport>>>;

pub fn connect(channel: u64) -> Client {
	Rc::new(RefCell::new(display::Client::new(ChannelTransport { chan: channel })))
}

/// What the service will negotiate.
pub fn image_limits(client: &Client) -> Option<Result<ImageLimits, Error>> {
	client.borrow_mut().image_limits()
}

/// One mapped presentable image.
///
/// THE DESCRIPTION IS THE SHARED MODEL'S: a client that draws into this gets the same `ImageLayout`
/// every other image in the system carries, checked by the same constructor, rather than a private
/// description whose channel shifts were written out by hand.
pub struct Mapping {
	handle: u64,
	addr: u64,
	layout: ImageLayout,
}

impl Mapping {
	/// CREATE one presentable image in THIS process's Domain, and map it.
	///
	/// THE SUPPLIER ALLOCATES AND THE SUPPLIER PAYS. `SYS_MEMORY_OBJECT_CREATE` charges the Domain
	/// that CREATES an object and the charge stays with it when the capability moves, so a client
	/// that asked the service to allocate would be spending the SERVICE's budget on its own pixels -
	/// and one such client could exhaust it while its own budget still looked healthy.
	///
	/// THE LAYOUT IS THE SHARED MODEL'S, so what is created is exactly as long as a scanout engine
	/// may touch: `pitch * height`, the span whole rows are read from.
	fn create(extent: Extent2D, pitch: u32, format: PixelFormat) -> Option<Mapping> {
		let layout = ImageLayout::scanout(extent, pitch, PixelStorage::Known(format)).ok()?;
		let length = layout.backend_access_span(true)?;
		let handle = memory_object_create(length);
		if handle < 0 {
			return None;
		}
		let handle = handle as u64;
		let addr = match unsafe { map_object(handle) } {
			Some(addr) => addr,
			None => {
				close(handle);
				return None;
			}
		};
		// ZEROED BEFORE ANYTHING CAN SEE IT. A presentable image whose first frame is whatever the
		// frame allocator last held is a window that shows another process's memory for one frame.
		unsafe { core::ptr::write_bytes(addr as *mut u8, 0, length as usize) };
		Some(Mapping { handle, addr, layout })
	}

	pub const fn addr(&self) -> u64 {
		self.addr
	}

	/// What this image's pixels are, in the terms every image in this tree is described in.
	pub const fn layout(&self) -> ImageLayout {
		self.layout
	}
}

impl Drop for Mapping {
	fn drop(&mut self) {
		unmap_object(self.handle);
		close(self.handle);
	}
}

/// One surface: its configuration, its queue, and the images mapped for the current generation.
pub struct Surface {
	client: RefCell<wire::Client<ChannelTransport>>,
	channel: u64,
	configuration: SurfaceConfiguration,
	/// The serial this client has ACKNOWLEDGED. A present names it, and a present that named an
	/// unacknowledged one would be a frame drawn for a configuration nobody agreed to.
	acknowledged: u64,
	images: Vec<Mapping>,
	generation: u64,
	pitch: u32,
	/// The client's end of PRODUCER_READY, carrying SEND alone.
	producer: u64,
	/// The client's end of PRESENT_DONE, carrying RECEIVE and WAIT.
	done: u64,
}

impl Surface {
	/// Create a surface and bring it to the state a first present is legal from: the configuration
	/// acknowledged and the queue built.
	pub fn create(client: &Client, logical: Extent2d, images: u32) -> Option<Result<Surface, Error>> {
		let channel = match client.borrow_mut().create_surface(&SurfaceRequest { logical_extent: logical, images })? {
			Ok(channel) => channel,
			Err(error) => return Some(Err(error)),
		};
		let mut surface = Surface { client: RefCell::new(wire::Client::new(ChannelTransport { chan: channel })), channel, configuration: SurfaceConfiguration { serial: 0, generation: 0, logical_extent: Extent2d { width: 0, height: 0 }, physical_extent: Extent2d { width: 0, height: 0 }, scale: display_proto::generated::liber::display::v1::ScaleRatio { numerator: 1, denominator: 1 }, transform: display_proto::generated::liber::display::v1::OutputTransform::Normal, output: 0, format: display_proto::generated::liber::graphics::v1::PixelFormat::B8g8r8x8Unorm, colour: display_proto::generated::liber::display::v1::OutputColour { space: display_proto::generated::liber::graphics::v1::ColorSpace::Srgb, sdr_white_nits: None, min_nits: None, max_nits: None, max_frame_average_nits: None }, subpixel: display_proto::generated::liber::display::v1::SubpixelLayout::Unknown, visible: false, focused: false }, acknowledged: u64::MAX, images: Vec::new(), generation: u64::MAX, pitch: 0, producer: 0, done: 0 };
		if let Err(error) = surface.rebuild()? {
			return Some(Err(error));
		}
		Some(Ok(surface))
	}

	/// Re-read the configuration, acknowledge it, and rebuild the queue for its generation.
	///
	/// THE ORDER IS THE LIFECYCLE'S: `configure -> rebuild -> ack -> first present Full`. Building
	/// the queue before acknowledging would build it for a configuration the service does not yet
	/// believe the client has seen.
	pub fn rebuild(&mut self) -> Option<Result<(), Error>> {
		let configuration = match self.client.borrow_mut().configuration()? {
			Ok(configuration) => configuration,
			Err(error) => return Some(Err(error)),
		};
		// THE OLD IMAGES GO FIRST. They belong to a generation that no longer exists, and an image
		// of a stale generation is never presented into a new one.
		self.images.clear();
		let queue: PresentQueue = match self.client.borrow_mut().queue()? {
			Ok(queue) => queue,
			Err(error) => return Some(Err(error)),
		};
		self.release_endpoints();
		self.producer = queue.producer;
		self.done = queue.done;
		self.pitch = queue.pitch;
		self.generation = queue.generation;
		let extent = Extent2D::new(configuration.physical_extent.width, configuration.physical_extent.height);
		let format = PixelFormat::from(configuration.format);
		if self.images.try_reserve_exact(queue.images as usize).is_err() {
			return Some(Err(Error::Exhausted));
		}
		for index in 0..queue.images {
			let mapping = Mapping::create(extent, queue.pitch, format)?;
			// THE SERVICE NEEDS `read` AND `map` AND DOES NOT NEED `write`: it composes FROM these
			// pixels and never into them, so the duplicate that travels carries neither `write` nor
			// `duplicate`. It DOES carry `transfer`, because a capability without it cannot be moved
			// at all and this one has to reach the service; that the service never passes an
			// imported image onward is the service's own rule, and the rule that is enforced
			// STRUCTURALLY is the one about the surface channel - which arrives here without
			// `transfer` and therefore cannot leave this process.
			//
			// AND THIS END KEEPS ITS OWN HANDLE, which is what it draws through: the mapping above
			// is the client's, so its Domain keeps the memory and gets it back when its handles go.
			let granted = duplicate(mapping.handle, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
			if granted < 0 {
				return Some(Err(Error::Exhausted));
			}
			let granted = granted as u64;
			match self.client.borrow_mut().provide_image(&index, &granted) {
				Some(Ok(())) => {}
				// THE REQUEST NEVER LEFT, so the duplicate is still this process's to close. Every
				// other ending means the capability moved and the service owns it; closing a handle
				// value that has already died would be this end guessing about the other's.
				Some(Err(Error::Again)) => {
					close(granted);
					return Some(Err(Error::Again));
				}
				Some(Err(error)) => return Some(Err(error)),
				None => return None,
			}
			self.images.push(mapping);
		}
		if let Err(error) = self.client.borrow_mut().ack_configure(&configuration.serial)? {
			return Some(Err(error));
		}
		self.acknowledged = configuration.serial;
		self.configuration = configuration;
		Some(Ok(()))
	}

	fn release_endpoints(&mut self) {
		for handle in [self.producer, self.done] {
			if handle != 0 {
				close(handle);
			}
		}
		self.producer = 0;
		self.done = 0;
	}

	pub fn configuration(&self) -> &SurfaceConfiguration {
		&self.configuration
	}

	pub fn generation(&self) -> u64 {
		self.generation
	}

	pub fn pitch(&self) -> u32 {
		self.pitch
	}

	pub fn image_count(&self) -> usize {
		self.images.len()
	}

	pub fn image(&self, index: u32) -> Option<&Mapping> {
		self.images.get(index as usize)
	}

	/// WHAT THE OUTPUT THIS SURFACE REACHES CAN SHOW, as the service reported it.
	///
	/// A COLOUR SPACE NAME IS NOT ENOUGH TO TONE MAP WITH, and the luminances are `None` while
	/// nothing in this system asks the panel - which a client must read as "use the profile's stated
	/// reference" rather than as zero.
	pub fn output_colour(&self) -> OutputLuminance {
		OutputLuminance { sdr_white_nits: self.configuration.colour.sdr_white_nits, min_nits: self.configuration.colour.min_nits, max_nits: self.configuration.colour.max_nits, max_frame_average_nits: self.configuration.colour.max_frame_average_nits }
	}

	/// Take the next image WITHOUT BLOCKING.
	pub fn acquire(&self) -> Option<Result<AcquiredImage, Error>> {
		self.client.borrow_mut().acquire_next()
	}

	/// Give an acquired image back without presenting it.
	///
	/// THE EDGE AN APPLICATION FORGETS. A client that acquired and then decided not to draw - a
	/// resize arrived, the window closed - must return the image, or a resized window leaks one per
	/// resize.
	pub fn abandon(&self, image: u32) -> Option<Result<(), Error>> {
		self.client.borrow_mut().abandon(&image)
	}

	/// Present an acquired image, naming the configuration and generation it was drawn for.
	pub fn present(&self, image: u32, damage: DamageRegion) -> Option<Result<u64, Error>> {
		self.client.borrow_mut().present(&image, &self.acknowledged, &self.generation, &damage)
	}

	/// Present the WHOLE image, spelled as the variant rather than as a rectangle covering the
	/// extent - a rectangle can be wrong by a pixel and a variant cannot, and the first present of a
	/// generation must be this one.
	pub fn present_whole(&self, image: u32) -> Option<Result<u64, Error>> {
		self.present(image, DamageRegion { whole: true, rects: Vec::new() })
	}

	/// Present a bounded list of damaged rectangles.
	///
	/// AN EMPTY LIST IS "NOTHING CHANGED" and is a legal present: it is still ordered and it still
	/// completes. A list that will not fit is refused HERE rather than trimmed, because exceeding
	/// the bound is the caller's problem - solved by merging or by sending the whole image - and
	/// never the service's, solved by dropping rectangles.
	pub fn present_rects(&self, image: u32, rects: &[Rect]) -> Option<Result<u64, Error>> {
		let mut wire: Vec<WireRect> = Vec::new();
		wire.try_reserve_exact(rects.len()).ok()?;
		for rect in rects {
			// The wire's origin is signed, because an offset in an image space can be negative in
			// the general case; damage cannot, and a coordinate that does not fit is a caller error
			// rather than something to wrap around.
			wire.push(WireRect { origin: Offset2d { x: i32::try_from(rect.x).ok()?, y: i32::try_from(rect.y).ok()? }, size: Extent2d { width: rect.width, height: rect.height } });
		}
		self.present(image, DamageRegion { whole: false, rects: wire })
	}

	/// The live event stream: configuration, image availability, per-present completion, close
	/// requests, visibility and focus.
	pub fn events(&self) -> Option<u64> {
		self.client.borrow_mut().events()
	}

	/// The one-shot proof channel for this surface's input focus.
	pub fn input_focus(&self) -> Option<Result<u64, Error>> {
		self.client.borrow_mut().input_focus()
	}

	/// The client's end of PRESENT_DONE, which a frame loop waits on.
	pub fn done_endpoint(&self) -> u64 {
		self.done
	}

	/// The client's end of PRODUCER_READY.
	pub fn producer_endpoint(&self) -> u64 {
		self.producer
	}
}

impl Drop for Surface {
	fn drop(&mut self) {
		let _ = self.client.borrow_mut().close();
		self.images.clear();
		self.release_endpoints();
		if self.channel != 0 {
			close(self.channel);
		}
	}
}

/// Read one event frame and whatever capabilities it carried.
///
/// `&mut`, and the list is SPENT by a successful read: what remains afterwards is what the decoded
/// value did not adopt, so the caller closes it unconditionally rather than knowing whether the
/// decode worked.
pub fn read_event(message: &[u8], handles: &mut Handles) -> Option<SurfaceEvent> {
	wire::events_read(message, handles)
}

/// Take one event off a live stream WITHOUT blocking, decoded.
///
/// `None` FOR BOTH "nothing yet" AND "the stream ended", because a client's answer to each is the
/// same: stop reading and go back to waiting. A caller that must tell them apart watches the
/// stream's handle, which is where a peer-close shows.
pub fn try_read_event(stream: u64, buffer: &mut [u8]) -> Option<SurfaceEvent> {
	let PolledCaps::Message { len, mut handles } = try_recv_caps(stream, buffer) else { return None };
	let event = read_event(&buffer[..len], &mut handles);
	// WHAT THE DECODE DID NOT ADOPT IS CLOSED, whichever way it went: the list is non-owning
	// metadata, so a capability left in it is one nothing will ever close.
	for handle in handles.as_slice() {
		close(*handle);
	}
	event
}

/// Say on PRODUCER_READY that one image is DRAWN.
///
/// THE OTHER HALF OF THE COMPLETION PAIR, and the direction a present already implies - so what it
/// is for is the client that wants to say "this image is finished" WITHOUT a call: the send is
/// bounded and non-blocking, and the service consumes it with the present or the abandon that
/// follows. A client that stopped presenting and kept signalling would fill a queue nothing drains,
/// which is why nothing here retries.
pub fn signal_ready(endpoint: u64, image: u32) -> bool {
	if endpoint == 0 {
		return false;
	}
	try_send(endpoint, &image.to_le_bytes(), 0)
}

/// One release off PRESENT_DONE: the present serial it settles and the image it gives back.
///
/// THIS IS THE PAIR A FRAME LOOP WAITS ON WITHOUT A DISPATCH, which is why it is a bare frame and
/// not a typed call: a loop that had to run a dispatch to learn an image came back would be paying
/// for a protocol it is not having a conversation over.
pub fn try_read_release(endpoint: u64, buffer: &mut [u8]) -> Option<(u64, u32)> {
	let Polled::Message { len, handle } = try_recv(endpoint, buffer) else { return None };
	if handle != 0 {
		close(handle);
	}
	if len < 12 {
		return None;
	}
	let serial = u64::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3], buffer[4], buffer[5], buffer[6], buffer[7]]);
	let image = u32::from_le_bytes([buffer[8], buffer[9], buffer[10], buffer[11]]);
	Some((serial, image))
}

pub fn subscribe_keys(channel: u64, focus: u64) -> Option<u64> {
	input::Client::new(ChannelTransport { chan: channel }).subscribe_keys(&focus)
}

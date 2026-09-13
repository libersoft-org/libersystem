#![no_std]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use core::cell::RefCell;
use display_proto::codec::Handles;
use display_proto::generated::liber::display::v1::{DamageRegion, DisplayEvent, SurfaceInfo, display};
use display_proto::generated::liber::graphics::v1::{Extent2d, Offset2d, Rect as WireRect};
use graphics_core::format::{PixelFormat, PixelStorage};
use graphics_core::geom::Extent2D;
use graphics_core::layout::ImageLayout;
use graphics_core::pixel::OutputLuminance;
use input_proto::generated::liber::input::v1::input;
use ipc_client::ChannelTransport;
use rt::{close, map_object, unmap_object};

pub use pix::{BlitResult, Image, Rect, Target};

pub type Client = Rc<RefCell<display::Client<ChannelTransport>>>;

pub fn connect(channel: u64) -> Client {
	Rc::new(RefCell::new(display::Client::new(ChannelTransport { chan: channel })))
}

/// A mapped display surface: the handle, where it is mapped, and WHAT ITS PIXELS ARE.
///
/// THE DESCRIPTION IS THE SHARED MODEL'S. This used to rebuild an ABI `Framebuffer` here with the
/// channel shifts written out as 16, 8 and 0 - a fourth private description of a pixel plane in this
/// tree, and the one whose numbers were literal rather than derived from the format the display
/// actually reported. A client that draws into this mapping now gets the same `ImageLayout` every
/// other image in the system carries, checked by the same constructor.
pub struct Mapping {
	handle: u64,
	addr: u64,
	layout: ImageLayout,
	/// What the display said its output can show. Carried beside the layout because it is a property
	/// of the destination rather than of the pixels.
	colour: OutputLuminance,
}

impl Mapping {
	pub fn from_info(info: SurfaceInfo) -> Option<Mapping> {
		let handle = info.pixels.handle;
		// WHAT THE DISPLAY REPORTED, CHECKED ONCE. The format must be the one this mapping draws
		// into; the extent and the pitch are the shared constructor's to accept or refuse; and the
		// object has to be at least as long as that layout says a scanout engine may touch - which is
		// `pitch * height`, the span the display reads whole rows from.
		let described = surface_layout(&info).filter(|layout| layout.backend_access_span(true).is_some_and(|span| info.pixels.len >= span));
		let (Some(layout), true) = (described, handle != 0) else {
			if handle != 0 {
				close(handle);
			}
			return None;
		};
		let addr = match unsafe { map_object(handle) } {
			Some(addr) => addr,
			None => {
				close(handle);
				return None;
			}
		};
		Some(Mapping { handle, addr, layout, colour: output_luminance(&info) })
	}

	pub const fn addr(&self) -> u64 {
		self.addr
	}

	/// What this mapping's pixels are, in the terms every image in this tree is described in.
	pub const fn layout(&self) -> ImageLayout {
		self.layout
	}

	/// WHAT THE OUTPUT THIS SURFACE REACHES CAN SHOW, as the display reported it.
	///
	/// A COLOUR SPACE NAME IS NOT ENOUGH TO TONE MAP WITH, and the luminances are `None` while
	/// nothing in this system asks the panel - which a client must read as "use the profile's stated
	/// reference" rather than as zero.
	pub fn output_colour(&self) -> OutputLuminance {
		self.colour
	}
}

// The display's own report of what its output can show, as the image model's description of the
// same thing. The colour space is not read from here: it is the LAYOUT's, and a surface whose
// reported space disagreed with its format would be two answers to one question.
fn output_luminance(info: &SurfaceInfo) -> OutputLuminance {
	OutputLuminance { sdr_white_nits: info.colour.sdr_white_nits, min_nits: info.colour.min_nits, max_nits: info.colour.max_nits, max_frame_average_nits: info.colour.max_frame_average_nits }
}

// THE ONE PLACE A DISPLAY SURFACE DESCRIPTION BECOMES AN IMAGE LAYOUT.
//
// ONE FORMAT, NAMED RATHER THAN WRITTEN OUT AS SHIFTS. `B8G8R8X8` is what DisplayService hands a
// client, and a consumer that packs pixels into it asks the shared registry what that name means
// rather than keeping its own copy of the masks - which is how a `R8G8B8X8` hand-off would have been
// drawn with red and blue swapped and nothing would have said so.
fn surface_layout(info: &SurfaceInfo) -> Option<ImageLayout> {
	let format = PixelFormat::from(info.format);
	if format != PixelFormat::B8G8R8X8Unorm {
		return None;
	}
	ImageLayout::scanout(Extent2D::new(info.width, info.height), info.pitch, PixelStorage::Known(format)).ok()
}

impl Drop for Mapping {
	fn drop(&mut self) {
		unmap_object(self.handle);
		close(self.handle);
	}
}

pub fn acquire(client: &Client, width: u32, height: u32) -> Option<Result<Mapping, Error>> {
	match client.borrow_mut().acquire(&width, &height)? {
		Ok(info) => Some(Mapping::from_info(info).ok_or(Error::Invalid)),
		Err(error) => Some(Err(error)),
	}
}

/// Present ONE damaged rectangle, which is what a client with a single dirty region has.
pub fn present(client: &Client, rect: Rect) -> Option<Result<(), Error>> {
	present_rects(client, core::slice::from_ref(&rect))
}

/// Present the WHOLE surface, spelled as the variant rather than as a rectangle covering the extent.
///
/// A RECTANGLE CAN BE WRONG BY A PIXEL AND A VARIANT CANNOT, which is the profile's own reason - and
/// it is what the first present of a generation must send.
pub fn present_whole(client: &Client) -> Option<Result<(), Error>> {
	client.borrow_mut().present(&DamageRegion { whole: true, rects: Vec::new() })
}

/// Present a bounded list of damaged rectangles.
///
/// AN EMPTY LIST IS "NOTHING CHANGED" and is a legal present: it is still ordered and it still
/// completes. MORE THAN THE WIRE BOUND IS THE CALLER'S PROBLEM, solved by merging the rectangles or
/// by sending the whole surface - never the service's, solved by dropping some of them - so a list
/// that will not fit is refused here rather than trimmed, and the call does not happen.
pub fn present_rects(client: &Client, rects: &[Rect]) -> Option<Result<(), Error>> {
	let mut wire: Vec<WireRect> = Vec::new();
	wire.try_reserve_exact(rects.len()).ok()?;
	for rect in rects {
		// The wire's origin is signed, because an offset in an image space can be negative in the
		// general case; a surface's damage cannot, and a coordinate that does not fit is a caller
		// error rather than something to wrap around.
		wire.push(WireRect { origin: Offset2d { x: i32::try_from(rect.x).ok()?, y: i32::try_from(rect.y).ok()? }, size: Extent2d { width: rect.width, height: rect.height } });
	}
	client.borrow_mut().present(&DamageRegion { whole: false, rects: wire })
}

pub fn release(client: &Client) -> Option<Result<(), Error>> {
	client.borrow_mut().release()
}

pub fn events(client: &Client) -> Option<u64> {
	client.borrow_mut().events()
}

// A display event frame and whatever capabilities it carried. The frame transport takes a bounded
// list now, like every other message on this wire - it used to take exactly one handle, and a frame
// whose element type declared two would have had the second dropped by the encoder.
//
// `&mut`, and the list is SPENT by a successful read: what remains afterwards is what the decoded
// value did not adopt, so the caller closes it unconditionally rather than knowing whether the
// decode worked. Passing it through unchanged was how one caller in this tree came to close its
// handles before decoding and another after, both correct only by accident of their element types.
pub fn read_event(message: &[u8], handles: &mut Handles) -> Option<DisplayEvent> {
	display::events_read(message, handles)
}

pub fn input_focus(client: &Client) -> Option<Result<u64, Error>> {
	client.borrow_mut().input_focus()
}

pub fn subscribe_keys(channel: u64, focus: u64) -> Option<u64> {
	input::Client::new(ChannelTransport { chan: channel }).subscribe_keys(&focus)
}

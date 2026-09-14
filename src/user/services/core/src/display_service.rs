// DisplayService - capability-scoped surfaces with their own present queues.
//
// The service is the only userspace process that maps the physical display backing. A connection
// creates as many SURFACES as the application needs, each its own capability with its own present
// queue, its own event stream and its own configuration snapshot; the visible one is copied or
// nearest-neighbour scaled into the scanout.
//
// BEFORE A COMPOSITOR EXISTS AT MOST ONE SURFACE IS VISIBLE. The others keep independent resources,
// generations and queues while hidden, acquiring from them answers `not-visible`, and frames
// accepted before a visibility change still settle IN ORDER with the appropriate discarded outcome.
// That is a contract rather than a limitation: a compositor arrives later and implements the same
// object model without breaking it.
//
// EVERY ANSWER HERE IS `WSI Profile 1`'s - the twelve configuration fields, the six events, the four
// image states and eight transitions, the four acquire answers, the four present outcomes, the nine
// damage rules and the two completion pairs.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use pix::{Image, Rect, Target};
use proto::codec::Handles;
use proto::system::display::{self, Service};
use proto::system::display_admin::{self, Service as AdminService};
use proto::system::display_device::{self};
use proto::system::display_stats::{self, Service as StatsService};
use proto::system::surface::{self, Service as SurfaceService};
use proto::system::{AcquiredImage, ColorSpace, DamageRegion, DeviceEvent, DisplayResources, Error, Extent2d, FrameTiming, ImageLimits, Offset2d, OutputColour, OutputTransform, PixelFormat, PresentComplete, PresentOutcome, PresentQueue, PresentationStats, ProviderInfo, ProviderKind, Rect as WireRect, ScaleRatio, Scanout as DeviceScanout, SubpixelLayout, SurfaceConfiguration, SurfaceEvent, SurfaceRequest, TimestampEvidence, provider_catalogue};
use rt::*;

const MAX_DIM: u32 = 8192;
const REQUEST_MAX: usize = 128;
const REPLY_MAX: usize = 128;

struct Scanout {
	gpu: u64,
	handle: u64,
	addr: u64,
	fb: Framebuffer,
	width: u32,
	height: u32,
	/// WHICH GENERATION THESE PIXELS ARE. The driver moves it when the BACKING moves, and every
	/// present names it - so a frame drawn against a backing the driver has given back is refused
	/// rather than transferred into memory that is no longer ours.
	generation: u32,
	/// The device's event stream, or zero when this scanout came from the boot framebuffer rather
	/// than from a driver.
	events: u64,
	/// THE DRIVER WENT AWAY UNDER THIS MAPPING.
	///
	/// DIFFERENT FROM "THERE IS NO DRIVER", which is the boot framebuffer and is a backend that
	/// works. Here the pixels still have somewhere to go and nothing is looking at them any more, so
	/// a present that copied into them and reported `displayed` would be reporting a frame nobody
	/// saw - and `driver-lost` is the outcome the profile grew for exactly this.
	lost: bool,
}

impl Scanout {
	fn available(&self) -> bool {
		self.addr != 0 && self.width != 0 && self.height != 0
	}
}

/// THE FOUR STATES AN IMAGE OF A PRESENT QUEUE CAN BE IN.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ImageState {
	/// The queue owns it and it can be acquired.
	Available,
	/// The client owns it and may draw into it.
	Acquired,
	/// The service owns it: accepted and not yet completed.
	PendingPresent,
	/// It belongs to a generation that no longer exists, and is never presented into a new one.
	Stale,
}

/// One slot of a present queue.
///
/// A SLOT EXISTS BEFORE ITS MEMORY DOES. The count is negotiated by `queue` and the images are
/// SUPPLIED by the client one at a time, so a queue spends a moment with slots and no pixels - and
/// `handle == 0` is what says so. A partially supplied queue is a queue that cannot be presented
/// from, which is why `acquire_next` answers `again` until every slot is filled.
struct QueueImage {
	/// The imported MemoryObject, or zero while the slot is still empty.
	handle: u64,
	addr: u64,
	state: ImageState,
}

impl QueueImage {
	const fn empty() -> QueueImage {
		QueueImage { handle: 0, addr: 0, state: ImageState::Available }
	}

	const fn supplied(&self) -> bool {
		self.handle != 0
	}
}

/// One accepted present, waiting its turn.
///
/// FIFO IS THE ORDER OF ACCEPTED PRESENTS, by present-call order and not by acquire order - so a
/// client that acquires two images and presents them the other way round gets them in the order it
/// PRESENTED them.
struct Pending {
	serial: u64,
	image: u32,
}

struct Surface {
	/// This service's end of the surface's own channel.
	chan: u64,
	/// The connection that created it, so closing a connection closes its surfaces.
	owner: u64,
	images: Vec<QueueImage>,
	width: u32,
	height: u32,
	pitch: u32,
	generation: u64,
	serial: u64,
	/// The serial the client has acknowledged, or `None` before its first acknowledgement. A present
	/// naming an unacknowledged serial is refused.
	acknowledged: Option<u64>,
	next_present: u64,
	pending: Vec<Pending>,
	focus_proof: u64,
	events: Option<EventStream>,
	/// This service's end of PRODUCER_READY: RECEIVE and WAIT.
	producer: u64,
	/// This service's end of PRESENT_DONE: SEND.
	done: u64,
	visible: bool,
	initialized: bool,
	/// Whether this surface is the console's, which is what a restore falls back to.
	console: bool,
	/// The client's ends, minted and attenuated, waiting for the `queue` call that hands them over.
	/// STAGED RATHER THAN SENT IMMEDIATELY: a capability travels in a reply, and the reply that
	/// carries these is `queue`'s.
	pending_client_producer: u64,
	pending_client_done: u64,
}

impl Surface {
	fn image_index(&self, index: u32) -> Option<usize> {
		let slot = index as usize;
		(slot < self.images.len()).then_some(slot)
	}

	fn first_available(&self) -> Option<u32> {
		self.images.iter().position(|image| image.state == ImageState::Available && image.supplied()).map(|index| index as u32)
	}

	/// Whether every slot of the current generation has been supplied. A queue missing one cannot be
	/// presented from, and the client is told to wait rather than handed an index with no pixels.
	fn complete(&self) -> bool {
		!self.images.is_empty() && self.images.iter().all(QueueImage::supplied)
	}
}

struct EventStream {
	producer: u64,
	seq: u32,
}

struct Client {
	chan: u64,
	task: u64,
}

#[derive(Default)]
struct PerfStats {
	presents: u64,
	direct_presents: u64,
	scaled_presents: u64,
	source_pixels: u64,
	output_pixels: u64,
	blit_ns: u64,
	flush_ns: u64,
	max_present_ns: u64,
	/// ONE COUNTER PER OUTCOME, so a client's own report and this service's can be COMPARED rather
	/// than argued about: a client that says it presented a hundred frames and a service that says
	/// it displayed sixty disagree about something a single `presents` counter cannot name.
	displayed: u64,
	discarded: u64,
	replaced: u64,
	lost: u64,
}

impl PerfStats {
	fn snapshot(&self) -> PresentationStats {
		PresentationStats { presents: self.presents, direct_presents: self.direct_presents, scaled_presents: self.scaled_presents, source_pixels: self.source_pixels, output_pixels: self.output_pixels, blit_ns: self.blit_ns, flush_ns: self.flush_ns, max_present_ns: self.max_present_ns, displayed: self.displayed, discarded: self.discarded, replaced: self.replaced, lost: self.lost }
	}

	fn record(&mut self, outcome: PresentOutcome) {
		match outcome {
			PresentOutcome::Displayed => self.displayed = self.displayed.saturating_add(1),
			PresentOutcome::DiscardedOccluded => self.discarded = self.discarded.saturating_add(1),
			PresentOutcome::ReplacedByResize => self.replaced = self.replaced.saturating_add(1),
			PresentOutcome::DriverLost => self.lost = self.lost.saturating_add(1),
		}
	}
}

/// How many images a surface may have. THE RANGE IS THE PROFILE'S: two is the fewest that can
/// double-buffer and three is the most this service will hold, and neither number is in the
/// interface - the service advertises the range, the client asks, and the service answers with what
/// it gave.
const MIN_IMAGES: u32 = 2;
const MAX_IMAGES: u32 = 3;

/// WHAT ONE CONNECTION MAY MULTIPLY, AND THE BOUND ON EACH.
///
/// THE KERNEL CHARGES THE MEMORY AND NOT THE BOOKKEEPING. A presentable image is a MemoryObject and
/// is charged to the Domain that created it, which is the client's; every other structure here - the
/// surface record, its queue slots, its pending-present list, its event stream and its place in this
/// loop's wait vector - is this service's heap and this service's wait set, charged to nobody. Those
/// are exactly what an adversarial client can multiply, so each has a STATED bound and exhausting one
/// is a typed refusal that releases everything the attempt had taken.
///
/// A CONNECTION IS A WINDOW BUDGET. Sixteen surfaces is more than any application in this tree opens
/// and far fewer than it takes to matter; the service-wide ceiling is what stops a client that can
/// open connections from multiplying the per-connection bound.
const MAX_SURFACES_PER_CONNECTION: usize = 16;
const MAX_SURFACES: usize = 64;
/// How many accepted presents ONE SURFACE may have outstanding. A present holds an image until it
/// completes, so the queue's own count is the honest ceiling: more in flight than there are images
/// is a list that could only grow.
const MAX_PENDING_PRESENTS: usize = MAX_IMAGES as usize;
/// The most damage rectangles ONE PRESENT may carry. The wire list is bounded and the decoder
/// refuses a seventeenth, so more than this never reaches a handler at all - which is why it is a
/// number this service REPORTS rather than one it checks.
const MAX_DAMAGE_RECTS: u64 = 16;

struct DisplayState {
	scanout: Scanout,
	surfaces: Vec<Surface>,
	focus_control: u64,
	kill_control: u64,
	console: u64,
	active: u64,
	stats: PerfStats,
	/// How many times the display path has been RESET: a scanout adopted after the previous one went
	/// away. A machine whose GPU driver keeps restarting shows it here rather than in a log nobody
	/// reads.
	resets: u64,
	// REPORT THE NEXT PRESENT, because a scanout was just adopted.
	//
	// A frame reaching the display AFTER a driver restart is what the enforcing profile has to show,
	// and nothing in the guest could say so: ConsoleService latches one line per outcome for the
	// whole boot, so a
	// present that lands after a rebind repeats an outcome already reported and prints nothing - and
	// once the console is taken, a service's `print` goes to its VT rather than to the serial log a
	// harness reads. The truth lives here, in the process that owns the scanout and performs the
	// copy, and it is per ADOPTION rather than per boot: one line for the first present through each
	// provider this service adopts, written to the debug port so it reaches the serial log whatever
	// owns the console.
	report_present: bool,
	/// A surface that asked to be closed, torn down AFTER its answer has gone out.
	///
	/// `close` ANSWERS `result<unit, error>`, AND THE ANSWER TRAVELS ON THE CHANNEL THE TEARDOWN
	/// CLOSES. Doing both inside the handler made the reply unsendable the moment it was produced,
	/// so a client that called `close` waited for an answer this service had already destroyed the
	/// route for. One step of deferral is the whole fix: reply, then unwind.
	closing: Option<u64>,
}

impl DisplayState {
	fn new(scanout: Scanout, focus_control: u64, kill_control: u64) -> DisplayState {
		DisplayState { scanout, surfaces: Vec::new(), focus_control, kill_control, console: 0, active: 0, stats: PerfStats::default(), report_present: true, closing: None, resets: 0 }
	}

	fn surface_index(&self, chan: u64) -> Option<usize> {
		self.surfaces.iter().position(|surface: &Surface| surface.chan == chan)
	}

	/// Create a surface: its own channel, its own queue, its own completion pairs.
	///
	/// EVERYTHING IT OWNS IS BUILT HERE OR NOTHING IS. A half-built surface - images allocated and
	/// no completion channel, or the reverse - would be one whose first acquire answers a refusal
	/// nobody can act on, so every failure below unwinds what it had taken.
	fn create_surface(&mut self, owner: u64, request: &SurfaceRequest) -> Result<u64, Error> {
		if (request.logical_extent.width == 0) != (request.logical_extent.height == 0) {
			return Err(Error::Invalid);
		}
		if !self.scanout.available() {
			return Err(Error::NotFound);
		}
		let native: bool = request.logical_extent.width == 0;
		let width: u32 = if native { self.scanout.width } else { request.logical_extent.width };
		let height: u32 = if native { self.scanout.height } else { request.logical_extent.height };
		if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
			return Err(Error::Invalid);
		}
		// THE COUNT IS NEGOTIATED AND CLAMPED RATHER THAN REFUSED. A client that asks for one image
		// cannot double-buffer and a client that asks for ten is asking for memory this service will
		// not hold; answering with what was given is the contract, and `queue` reports it.
		// THE BOUNDS ARE CHECKED BEFORE ANYTHING IS TAKEN, so a refusal costs nothing to unwind.
		if self.surfaces.len() >= MAX_SURFACES || self.surfaces.iter().filter(|surface| surface.owner == owner).count() >= MAX_SURFACES_PER_CONNECTION {
			return Err(Error::Exhausted);
		}
		if self.surfaces.try_reserve(1).is_err() {
			return Err(Error::Exhausted);
		}
		let images: u32 = request.images.clamp(MIN_IMAGES, MAX_IMAGES);
		let pitch: u32 = width.checked_mul(4).ok_or(Error::Invalid)?;
		// The layout a supplied image must satisfy. Checked here so a geometry that cannot be
		// described is refused before any channel is minted.
		let _: u64 = (pitch as u64).checked_mul(height as u64).ok_or(Error::Invalid)?;

		let Some((service_end, client_end)) = channel() else { return Err(Error::Again) };
		// TWO ORDINARY CHANNEL PAIRS AND NO NEW KERNEL OBJECT. `Event` is not this: it lacks the
		// authority split and the peer-close lifecycle a completion needs.
		let Some((producer_service, producer_client)) = channel() else {
			close(service_end);
			close(client_end);
			return Err(Error::Again);
		};
		let Some((done_client, done_service)) = channel() else {
			close(service_end);
			close(client_end);
			close(producer_service);
			close(producer_client);
			return Err(Error::Again);
		};

		let mut surface = Surface { chan: service_end, owner, images: Vec::new(), width, height, pitch, generation: 1, serial: 1, acknowledged: None, next_present: 1, pending: Vec::new(), focus_proof: 0, events: None, producer: producer_service, done: done_service, visible: false, initialized: false, console: false, pending_client_producer: 0, pending_client_done: 0 };
		if !reserve_slots(&mut surface, images) {
			self.drop_surface_resources(&mut surface);
			close(client_end);
			close(producer_client);
			close(done_client);
			return Err(Error::Exhausted);
		}

		// THE CLIENT'S ENDS ARE HELD AT FULL AUTHORITY HERE AND ATTENUATED BY THE SEND THAT HANDS
		// THEM OVER - see `surface_grant`. The producer end may only SEND and the completion end may
		// only RECEIVE and WAIT, so neither can be used as the other, and NEITHER CARRIES `transfer`
		// or `duplicate`: a client cannot keep a copy while giving one away, and cannot move its
		// completion endpoint to another process.
		//
		// This used to `duplicate` them down to the intended rights BEFORE the reply, and had to add
		// `transfer` back to every mask - because a capability without it cannot be MOVED AT ALL, so
		// the reply carrying it could not be sent and the client waited for an answer that never
		// came. Attenuating at the SEND is what makes the intended rights reachable.
		surface.pending_client_producer = producer_client;
		surface.pending_client_done = done_client;

		if self.console == 0 && native {
			self.console = service_end;
			surface.console = true;
		}
		self.surfaces.push(surface);
		// THE NEWEST SURFACE BECOMES THE VISIBLE ONE unless it is the console arriving first, which
		// is what makes an application that starts take the screen and a console that starts not.
		if self.active == 0 || !self.surfaces.last().is_some_and(|surface| surface.console) {
			self.set_active(service_end);
		}
		Ok(client_end)
	}

	fn drop_surface_resources(&mut self, surface: &mut Surface) {
		for image in surface.images.drain(..) {
			// AN EMPTY SLOT HAS NOTHING TO RELEASE, and unmapping handle zero would be this service
			// asking the kernel about a capability it never held.
			if image.supplied() {
				unmap_object(image.handle);
				close(image.handle);
			}
		}
		for handle in [surface.focus_proof, surface.producer, surface.done, surface.pending_client_producer, surface.pending_client_done] {
			if handle != 0 {
				close(handle);
			}
		}
		surface.focus_proof = 0;
		surface.producer = 0;
		surface.done = 0;
		surface.pending_client_producer = 0;
		surface.pending_client_done = 0;
		if let Some(stream) = surface.events.take() {
			close(stream.producer);
		}
	}

	/// The current configuration snapshot, which is every field that decides how a client draws.
	fn configuration(&self, chan: u64) -> Result<SurfaceConfiguration, Error> {
		let index: usize = self.surface_index(chan).ok_or(Error::Invalid)?;
		Ok(self.snapshot(index))
	}

	fn snapshot(&self, index: usize) -> SurfaceConfiguration {
		let surface: &Surface = &self.surfaces[index];
		SurfaceConfiguration {
			serial: surface.serial,
			generation: surface.generation,
			// THE PHYSICAL EXTENT IS AUTHORITATIVE AND THE LOGICAL ONE IS DERIVED. At a scale of one
			// they are the same number, and they are still two fields - so the day a scale arrives,
			// nothing above here changes shape.
			logical_extent: Extent2d { width: surface.width, height: surface.height },
			physical_extent: Extent2d { width: surface.width, height: surface.height },
			scale: ScaleRatio { numerator: 1, denominator: 1 },
			transform: OutputTransform::Normal,
			output: 0,
			format: PixelFormat::B8g8r8x8Unorm,
			colour: self.output_colour(),
			// NOTHING HERE ASKS THE PANEL, so the layout is UNKNOWN rather than `NoneLayout`: the
			// first says nothing was reported and the second says the panel has no subpixel geometry,
			// and a text rasteriser treats them differently.
			subpixel: SubpixelLayout::Unknown,
			visible: surface.visible,
			focused: surface.chan == self.active,
		}
	}

	/// Acknowledge a configuration by serial.
	///
	/// A STALE OR UNKNOWN SERIAL IS REFUSED RATHER THAN APPLIED. An acknowledgement is the client
	/// saying it has rebuilt for a particular configuration, and accepting one for a configuration
	/// that has already been replaced would let it present frames drawn for a size nothing has.
	fn ack_configure(&mut self, chan: u64, serial: u64) -> Result<(), Error> {
		let index: usize = self.surface_index(chan).ok_or(Error::Invalid)?;
		if self.surfaces[index].serial != serial {
			return Err(Error::Invalid);
		}
		self.surfaces[index].acknowledged = Some(serial);
		Ok(())
	}

	/// The present queue for the current generation, with the client's completion endpoints.
	///
	/// THE ENDPOINTS ARE HANDED OVER ONCE PER GENERATION. Calling this again after a generation
	/// change is how a client rebuilds, and each call mints a fresh pair rather than re-sending a
	/// handle this service no longer owns.
	fn queue(&mut self, chan: u64) -> Result<PresentQueue, Error> {
		let index: usize = self.surface_index(chan).ok_or(Error::Invalid)?;
		if self.surfaces[index].pending_client_producer == 0 || self.surfaces[index].pending_client_done == 0 {
			let Some((producer_service, producer_client)) = channel() else { return Err(Error::Again) };
			let Some((done_client, done_service)) = channel() else {
				close(producer_service);
				close(producer_client);
				return Err(Error::Again);
			};
			let surface: &mut Surface = &mut self.surfaces[index];
			for handle in [surface.producer, surface.done] {
				if handle != 0 {
					close(handle);
				}
			}
			surface.producer = producer_service;
			surface.done = done_service;
			// Held at full authority and attenuated by the send that hands them over - see
			// `surface_grant`.
			surface.pending_client_producer = producer_client;
			surface.pending_client_done = done_client;
		}
		let surface: &mut Surface = &mut self.surfaces[index];
		Ok(PresentQueue { images: surface.images.len() as u32, pitch: surface.pitch, generation: surface.generation, producer: core::mem::take(&mut surface.pending_client_producer), done: core::mem::take(&mut surface.pending_client_done) })
	}

	/// SUPPLY one image of the current generation's queue: a MemoryObject the CLIENT created.
	///
	/// EVERY REFUSAL RELEASES WHAT IT WAS HANDED. A capability this service refuses is a capability
	/// nothing else will ever close, so an import that does not become a slot is closed here - which
	/// is the half a validation written as a series of early returns forgets.
	fn provide_image(&mut self, chan: u64, index: u32, handle: u64) -> Result<(), Error> {
		let Some(surface_index) = self.surface_index(chan) else {
			close(handle);
			return Err(Error::Invalid);
		};
		let needed: u64 = (self.surfaces[surface_index].pitch as u64).saturating_mul(self.surfaces[surface_index].height as u64);
		let Some(slot) = self.surfaces[surface_index].image_index(index) else {
			close(handle);
			return Err(Error::Invalid);
		};
		// A SLOT ALREADY FILLED IS NOT REFILLED WITHIN A GENERATION. Swapping the memory under a
		// frame this service may already be composing from is exactly the hostile case, and a
		// generation change is the only thing that empties a queue.
		if self.surfaces[surface_index].images[slot].supplied() {
			close(handle);
			return Err(Error::Invalid);
		}
		// THE OBJECT'S OWN SIZE AND NOT A LENGTH THE CLIENT DECLARED. A client that said `len` and
		// handed over something smaller would be describing a buffer this service then reads past,
		// so what is checked is what the kernel says the object is.
		let Some(info) = object_info(handle) else {
			close(handle);
			return Err(Error::Invalid);
		};
		if info.size < needed {
			close(handle);
			return Err(Error::Invalid);
		}
		let Some(addr) = (unsafe { map_object(handle) }) else {
			close(handle);
			return Err(Error::Exhausted);
		};
		let surface: &mut Surface = &mut self.surfaces[surface_index];
		surface.images[slot] = QueueImage { handle, addr, state: ImageState::Available };
		// A QUEUE THAT HAS JUST BECOME COMPLETE HAS AN IMAGE TO GIVE, and a client waiting on the
		// event rather than polling has to be told so.
		if surface.complete() {
			self.notify_image_available(surface_index);
		}
		Ok(())
	}

	/// Take the next image WITHOUT BLOCKING.
	///
	/// IT DOES NOT BLOCK, and that is a property of this service rather than a preference: one
	/// dispatch loop serves the GPU, the admin channel, the kill control and every client, so
	/// blocking inside an acquire handler would stop the only loop that could deliver the release
	/// that would unblock it.
	fn acquire_next(&mut self, chan: u64) -> Result<AcquiredImage, Error> {
		let index: usize = self.surface_index(chan).ok_or(Error::Invalid)?;
		if self.surfaces[index].acknowledged != Some(self.surfaces[index].serial) {
			return Ok(AcquiredImage::OutOfDate);
		}
		if !self.surfaces[index].visible {
			return Ok(AcquiredImage::NotVisible);
		}
		// A PARTIALLY SUPPLIED QUEUE IS A QUEUE THAT CANNOT BE PRESENTED FROM. `again` is the answer
		// rather than a refusal because the client's own next `provide-image` is what fixes it, and
		// `again` is exactly "nothing to give yet; wait for `image-available`".
		if !self.surfaces[index].complete() {
			return Ok(AcquiredImage::Again);
		}
		let Some(image) = self.surfaces[index].first_available() else {
			return Ok(AcquiredImage::Again);
		};
		let slot: usize = image as usize;
		self.surfaces[index].images[slot].state = ImageState::Acquired;
		Ok(AcquiredImage::Image(image))
	}

	/// DRAIN WHATEVER THE CLIENT SAID ON PRODUCER_READY.
	///
	/// THE SIGNAL IS CONSUMED BY THE CALL IT PRECEDES. There are two completion pairs, one per
	/// direction; PRODUCER_READY is the client's, and the present or the abandon that follows says
	/// the same thing and carries what this service acts on. So what is owed here is that the
	/// endpoint is READ - a queue nothing drains fills after a few dozen frames, and then a client
	/// doing exactly what the contract asks starts failing to signal.
	///
	/// BOUNDED, because it is reached from a handler: a client that filled the queue and kept
	/// filling it must not be able to hold the only progress loop this service has.
	fn drain_ready(&mut self, index: usize) {
		let endpoint: u64 = self.surfaces[index].producer;
		if endpoint == 0 {
			return;
		}
		let mut frame: [u8; 16] = [0; 16];
		for _ in 0..MAX_IMAGES {
			let Polled::Message { handle, .. } = try_recv(endpoint, &mut frame) else { return };
			if handle != 0 {
				close(handle);
			}
		}
	}

	/// Give an acquired image back WITHOUT presenting it.
	///
	/// THE EDGE AN IMPLEMENTATION FORGETS. A client that acquired and then decided not to draw must
	/// have a way back that is not a present, or a resized window leaks an image per resize.
	fn abandon(&mut self, chan: u64, image: u32) -> Result<(), Error> {
		let index: usize = self.surface_index(chan).ok_or(Error::Invalid)?;
		self.drain_ready(index);
		let slot: usize = self.surfaces[index].image_index(image).ok_or(Error::Invalid)?;
		if self.surfaces[index].images[slot].state != ImageState::Acquired {
			return Err(Error::Invalid);
		}
		self.surfaces[index].images[slot].state = ImageState::Available;
		self.notify_image_available(index);
		Ok(())
	}

	// WHAT THIS SERVICE'S OUTPUT CAN SHOW, reported rather than assumed.
	//
	// THE COLOUR SPACE IS KNOWN AND THE LUMINANCE IS NOT, and the difference is the whole reason the
	// fields are optional. The space follows from the format every scanout in this system uses; the
	// luminance would come from the panel, over DDC, which needs an I2C or AUX transport no driver
	// here can reach yet - so this service says `none` rather than inventing a number, and a consumer
	// handed `none` uses the profile's own stated reference white point. When the display-discovery
	// item lands, the numbers come from the monitor and nothing above here changes shape.
	/// WHAT THIS SERVICE HOLDS THAT THE KERNEL CHARGES NOBODY FOR, with the bound each is held to.
	///
	/// `waiters` is passed in because the wait vector is built by the loop and not by this state:
	/// reporting a number this function recomputed would be reporting a second answer to the
	/// question, which is how an observability field comes to disagree with the thing it observes.
	fn resources(&self, waiters: u64) -> DisplayResources {
		DisplayResources {
			surfaces: self.surfaces.len() as u64,
			surface_bound: MAX_SURFACES as u64,
			present_images: self.surfaces.iter().map(|surface| surface.images.len() as u64).sum(),
			image_bound: MAX_IMAGES as u64,
			queued_presents: self.surfaces.iter().map(|surface| surface.pending.len() as u64).sum(),
			present_bound: MAX_PENDING_PRESENTS as u64,
			// ZERO, AND THAT IS THE POINT: damage lives for the duration of the present call that
			// carried it and is never stored, so there is nothing here for a client to multiply.
			damage_entries: 0,
			damage_bound: MAX_DAMAGE_RECTS,
			waiters,
			waiter_bound: MAX_WAIT_HANDLES as u64,
			resets: self.resets,
			faulted: self.scanout.lost || !self.scanout.available(),
		}
	}

	fn output_colour(&self) -> OutputColour {
		OutputColour { space: ColorSpace::Srgb, sdr_white_nits: None, min_nits: None, max_nits: None, max_frame_average_nits: None }
	}

	// PRESENT ONE ACQUIRED IMAGE, which CONSUMES it: there is no safe path back to those pixels.
	//
	// THE PRESENT NAMES ITS CONFIGURATION SERIAL AND ITS IMAGE GENERATION, and both must match the
	// acknowledged current configuration at acceptance - which is what stops a frame drawn for one
	// size from being shown at another.
	//
	// THE NINE DAMAGE ANSWERS ARE `WSI Profile 1`'s AND ARE IMPLEMENTED HERE RATHER THAN RESTATED.
	// An empty list is "nothing changed": the present is ordered, it completes, and nothing is
	// copied. A rectangle outside the extent is a typed refusal and never a clamp, because clamping
	// presents pixels nobody asked to present. More than the bound never reaches here at all - the
	// wire list is bounded and the decoder refuses a seventeenth rectangle.
	//
	// AND EVERY PRESENTED IMAGE CONTAINS A COMPLETE VALID FRAME. The damage is a HINT about which
	// pixels changed against the previously presented frame and is never permission to leave the
	// rest undefined, which is what makes presenting into a DIFFERENT image than the last one safe.
	fn present(&mut self, chan: u64, image: u32, serial: u64, generation: u64, damage: &DamageRegion) -> Result<u64, Error> {
		let index: usize = self.surface_index(chan).ok_or(Error::Invalid)?;
		self.drain_ready(index);
		let slot: usize = self.surfaces[index].image_index(image).ok_or(Error::Invalid)?;
		if self.surfaces[index].images[slot].state != ImageState::Acquired {
			return Err(Error::Invalid);
		}
		if self.surfaces[index].acknowledged != Some(serial) || self.surfaces[index].serial != serial {
			return Err(Error::Invalid);
		}
		if self.surfaces[index].generation != generation {
			return Err(Error::Invalid);
		}
		let (surface_width, surface_height): (u32, u32) = (self.surfaces[index].width, self.surfaces[index].height);
		// EVERY RECTANGLE IS CHECKED BEFORE ANY OF THEM IS DRAWN. A frame whose fourth rectangle is
		// out of bounds must present none of it: half a frame is a frame nobody asked for, and the
		// typed refusal has to mean the whole call was refused.
		let mut rects: Vec<Rect> = Vec::new();
		if damage.whole {
			rects.push(Rect { x: 0, y: 0, width: surface_width, height: surface_height });
		} else {
			for rect in &damage.rects {
				let x: u32 = u32::try_from(rect.origin.x).map_err(|_| Error::Invalid)?;
				let y: u32 = u32::try_from(rect.origin.y).map_err(|_| Error::Invalid)?;
				let x1: u32 = x.checked_add(rect.size.width).ok_or(Error::Invalid)?;
				let y1: u32 = y.checked_add(rect.size.height).ok_or(Error::Invalid)?;
				if rect.size.width == 0 || rect.size.height == 0 || x1 > surface_width || y1 > surface_height {
					return Err(Error::Invalid);
				}
				rects.push(Rect { x, y, width: rect.size.width, height: rect.size.height });
			}
		}
		// ACCEPTED: it has a place in the order, whatever happens to it afterwards.
		let present_serial: u64 = self.surfaces[index].next_present;
		self.surfaces[index].next_present = present_serial.saturating_add(1);
		self.surfaces[index].images[slot].state = ImageState::PendingPresent;
		// THE BOUND AND THE ALLOCATION, and the refusal RELEASES THE PARTIAL TRANSACTION: the image
		// goes back to `Acquired`, because a present that was not accepted did not consume anything.
		if self.surfaces[index].pending.len() >= MAX_PENDING_PRESENTS || self.surfaces[index].pending.try_reserve(1).is_err() {
			self.surfaces[index].images[slot].state = ImageState::Acquired;
			self.surfaces[index].next_present = present_serial;
			return Err(Error::Exhausted);
		}
		self.surfaces[index].pending.push(Pending { serial: present_serial, image });
		self.stats.presents = self.stats.presents.saturating_add(1);

		// A FRAME ACCEPTED WHILE HIDDEN COMPLETES IN ORDER AS DISCARDED. It kept its place and never
		// reached a screen, which is a different fact from "it was refused" and is why the outcome
		// is per present rather than a single error.
		if !self.surfaces[index].visible {
			self.complete(index, present_serial, PresentOutcome::DiscardedOccluded);
			return Ok(present_serial);
		}
		// AN EMPTY LIST COMPLETES WITHOUT COPYING. The frame is pixel-identical to the last one, so
		// there is nothing to transfer - and answering an error here would make "nothing changed" a
		// failure a client has to work around.
		if rects.is_empty() {
			self.complete(index, present_serial, PresentOutcome::Displayed);
			return Ok(present_serial);
		}
		let outcome: PresentOutcome = self.copy_and_flush(index, slot, &rects);
		self.complete(index, present_serial, outcome);
		Ok(present_serial)
	}

	/// Copy one image's damaged rectangles into the scanout and wait for the device.
	fn copy_and_flush(&mut self, index: usize, slot: usize, rects: &[Rect]) -> PresentOutcome {
		let source_pixels: u64 = rects.iter().map(|rect| rect.width as u64 * rect.height as u64).sum();
		let start_ns: u64 = clock_ns();
		let mut output_pixels: u64 = 0;
		let mut direct: bool = true;
		// EVERY RECTANGLE IS COPIED, THEN ALL OF THEM ARE PRESENTED IN ONE CALL. The blit's own
		// rectangle is what the device is told about, because a scaled surface's damage lands
		// somewhere else on the scanout than where the client drew it.
		let mut transferred: Vec<Rect> = Vec::new();
		for rect in rects {
			let blit: pix::BlitResult = self.blit(index, slot, *rect);
			output_pixels = output_pixels.saturating_add(blit.pixels);
			direct &= blit.direct;
			if transferred.try_reserve(1).is_err() {
				break;
			}
			transferred.push(blit.rect);
		}
		let blit_done_ns: u64 = clock_ns();
		let result: Result<(), Error> = self.flush_damage(&transferred);
		let done_ns: u64 = clock_ns();
		self.stats.blit_ns = self.stats.blit_ns.saturating_add(blit_done_ns.saturating_sub(start_ns));
		self.stats.flush_ns = self.stats.flush_ns.saturating_add(done_ns.saturating_sub(blit_done_ns));
		self.stats.max_present_ns = self.stats.max_present_ns.max(done_ns.saturating_sub(start_ns));
		if direct {
			self.stats.direct_presents = self.stats.direct_presents.saturating_add(1);
		} else {
			self.stats.scaled_presents = self.stats.scaled_presents.saturating_add(1);
		}
		self.stats.source_pixels = self.stats.source_pixels.saturating_add(source_pixels);
		self.stats.output_pixels = self.stats.output_pixels.saturating_add(output_pixels);
		// ONE LINE PER ADOPTED PROVIDER, AND TO THE DEBUG PORT. Presenting is a hot path, so this is
		// latched; it goes through `debug_write` rather than `print` because `print` follows stdout
		// to a VT once the console is taken, and a line the serial log never carries cannot be
		// evidence for anything. AND ONLY FOR A PRESENT THAT REACHED A PROVIDER: `flush` answers
		// `Ok(())` without sending anything when there is no scanout to send to.
		if self.report_present && self.scanout.gpu != 0 {
			self.report_present = false;
			debug_write(if result.is_ok() { b"DisplayService: a frame reached the display through the provider it adopted\n".as_slice() } else { b"DisplayService: a frame did NOT reach the display through the provider it adopted\n".as_slice() });
		}
		// A BACKEND THAT CANNOT OBSERVE SCANOUT REPORTS `Displayed` FOR DRIVER-COMPLETED, and the
		// EVIDENCE field is what says which - which is why the outcome and the evidence are two
		// fields rather than one.
		if result.is_ok() { PresentOutcome::Displayed } else { PresentOutcome::DriverLost }
	}

	/// Settle one accepted present: release its image and tell the client what became of it.
	fn complete(&mut self, index: usize, serial: u64, outcome: PresentOutcome) {
		let Some(position) = self.surfaces[index].pending.iter().position(|pending| pending.serial == serial) else { return };
		let pending: Pending = self.surfaces[index].pending.remove(position);
		if let Some(slot) = self.surfaces[index].image_index(pending.image) {
			// A STALE IMAGE STAYS STALE: it completed or was discarded first - a frame in flight is
			// not un-submitted - and is then still of a generation that no longer exists.
			if self.surfaces[index].images[slot].state == ImageState::PendingPresent {
				self.surfaces[index].images[slot].state = ImageState::Available;
			}
		}
		self.stats.record(outcome);
		// THE COMPLETION GOES BOTH WAYS: the event carries the outcome and the evidence, and the
		// PRESENT_DONE endpoint carries the release a frame loop waits on without a dispatch.
		let evidence: TimestampEvidence = TimestampEvidence::Unavailable;
		let complete = PresentComplete { serial, outcome, evidence, timing: FrameTiming { preferred_deadline: None, refresh_interval: None } };
		self.emit(index, &SurfaceEvent::PresentComplete(complete));
		let done: u64 = self.surfaces[index].done;
		if done != 0 {
			let mut frame: [u8; 16] = [0; 16];
			frame[..8].copy_from_slice(&serial.to_le_bytes());
			frame[8..12].copy_from_slice(&pending.image.to_le_bytes());
			let _ = try_send(done, &frame, 0);
		}
		self.notify_image_available(index);
	}

	/// Tell a client an image is available, which is what it waits on instead of blocking.
	fn notify_image_available(&mut self, index: usize) {
		if self.surfaces[index].first_available().is_some() {
			self.emit(index, &SurfaceEvent::ImageAvailable);
		}
	}

	fn close_surface(&mut self, chan: u64) -> Result<(), Error> {
		if self.surface_index(chan).is_none() {
			return Err(Error::Invalid);
		}
		// DEFERRED BY ONE STEP ON PURPOSE - see `closing`. The teardown closes this surface's own
		// channel, which is where the answer to this very call has to travel.
		self.closing = Some(chan);
		Ok(())
	}

	fn input_focus(&mut self, chan: u64) -> Result<u64, Error> {
		if chan != self.active {
			return Err(Error::Denied);
		}
		let index: usize = self.surface_index(chan).ok_or(Error::Invalid)?;
		let proof: u64 = core::mem::take(&mut self.surfaces[index].focus_proof);
		if proof == 0 { Err(Error::Again) } else { Ok(proof) }
	}

	fn revoke_focus(&mut self) {
		for surface in &mut self.surfaces {
			if surface.focus_proof != 0 {
				close(surface.focus_proof);
				surface.focus_proof = 0;
			}
		}
	}

	fn focus_command(&self, command: &[u8], handle: u64) -> bool {
		if self.focus_control == 0 || !send_blocking(self.focus_control, command, handle) {
			return false;
		}
		let mut reply: [u8; 8] = [0; 8];
		match recv_blocking(self.focus_control, &mut reply) {
			Received::Message { len, handle } => {
				if handle != 0 {
					close(handle);
				}
				len >= 2 && &reply[..2] == b"OK"
			}
			Received::Closed => false,
		}
	}

	/// Make one surface the visible, scanout-bound one.
	///
	/// AT MOST ONE IS VISIBLE BEFORE A COMPOSITOR EXISTS, and the others keep their resources,
	/// generations and queues - which is what makes "hidden" different from "closed" and what a
	/// compositor later relaxes without breaking anything above it.
	fn set_active(&mut self, chan: u64) {
		self.revoke_focus();
		let previous: u64 = self.active;
		self.active = chan;
		for index in 0..self.surfaces.len() {
			let surface_chan: u64 = self.surfaces[index].chan;
			let visible: bool = surface_chan == chan;
			if self.surfaces[index].visible != visible {
				self.surfaces[index].visible = visible;
				self.emit(index, &SurfaceEvent::VisibilityChanged(visible));
			}
			// FOCUS BELONGS TO THE SURFACE, so the event does too.
			if surface_chan == chan || surface_chan == previous {
				self.emit(index, &SurfaceEvent::FocusChanged(visible));
			}
		}
		if self.focus_control == 0 {
			return;
		}
		if chan == 0 {
			self.focus_command(b"CLEAR", 0);
			return;
		}
		if chan == self.console {
			self.focus_command(b"CONSOLE", 0);
			return;
		}
		let Some(index) = self.surface_index(chan) else { return };
		let (proof, registered): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return,
		};
		if self.focus_command(b"SET", registered) {
			self.surfaces[index].focus_proof = proof;
		} else {
			close(proof);
			close(registered);
		}
	}

	fn remove_surface(&mut self, chan: u64, restore: bool) {
		if let Some(index) = self.surface_index(chan) {
			let mut surface: Surface = self.surfaces.swap_remove(index);
			// EVERY FRAME STILL IN FLIGHT SETTLES BEFORE THE SURFACE GOES. A present that never
			// completes is a client blocked on a completion that will not arrive, which outlives the
			// surface that owed it.
			for pending in core::mem::take(&mut surface.pending) {
				self.stats.record(PresentOutcome::DriverLost);
				let _ = pending;
			}
			self.drop_surface_resources(&mut surface);
			if surface.chan != 0 {
				close(surface.chan);
			}
		}
		if !restore {
			return;
		}
		if self.console == chan {
			self.console = 0;
		}
		if self.active == chan {
			let next: u64 = if self.console != 0 && self.surface_index(self.console).is_some() { self.console } else { 0 };
			self.set_active(next);
			if self.active != 0 {
				self.present_active_full();
			}
		}
	}

	/// Close every surface a connection owns. A CONNECTION GOING AWAY TAKES ITS SURFACES WITH IT,
	/// which is what makes a crashed application's windows disappear rather than freeze.
	fn drop_client(&mut self, owner: u64) {
		loop {
			let Some(index) = self.surfaces.iter().position(|surface| surface.owner == owner) else { break };
			let chan: u64 = self.surfaces[index].chan;
			self.remove_surface(chan, true);
		}
	}

	fn set_event_stream(&mut self, chan: u64, producer: u64) {
		let Some(index) = self.surface_index(chan) else {
			close(producer);
			return;
		};
		if let Some(old) = self.surfaces[index].events.replace(EventStream { producer, seq: 0 }) {
			close(old.producer);
		}
		// THE FIRST THING A NEW STREAM CARRIES IS THE CONFIGURATION, because a client that opened
		// its stream after the surface was made would otherwise wait for a change that may never
		// come.
		let snapshot = self.snapshot(index);
		self.emit(index, &SurfaceEvent::Configure(snapshot));
	}

	/// Send one event to one surface's stream.
	///
	/// NON-BLOCKING, AND THAT IS THE WHOLE POINT. A completion is emitted from inside the `present`
	/// handler, while the client is waiting for that present's reply - so a BLOCKING send on a queue
	/// the client cannot drain deadlocks the only loop this service has: the client waits for the
	/// reply, the service waits for room, and neither moves. The profile says it in as many words:
	/// ordinary completion sends are bounded and nonblocking in the service loop, and neither a
	/// destructor nor a handler may block the only progress loop.
	///
	/// A FULL QUEUE DROPS THE EVENT AND KEEPS THE STREAM. Every event here is a snapshot or a
	/// completion the client can also learn by asking - `configuration` re-reads the snapshot and
	/// `acquire-next` re-reports availability - so a dropped one costs a frame rather than
	/// correctness. A stream whose PEER HAS GONE is a different thing, and that one is closed.
	fn emit(&mut self, index: usize, event: &SurfaceEvent) {
		let Some(stream) = self.surfaces[index].events.as_mut() else { return };
		let mut frame: [u8; 256] = [0; 256];
		let mut frame_handles = Handles::new();
		let outcome: SendOutcome = match surface::events_frame(stream.seq, event, &mut frame, &mut frame_handles) {
			Some(n) => try_send_caps_outcome(stream.producer, &frame[..n], frame_handles.as_slice()),
			None => SendOutcome::Failed,
		};
		match outcome {
			SendOutcome::Delivered => {
				stream.seq = stream.seq.wrapping_add(1);
				return;
			}
			// The client is behind. Drop this event and keep the stream.
			SendOutcome::Stalled => {
				for handle in frame_handles.as_slice() {
					close(*handle);
				}
				return;
			}
			SendOutcome::Failed => {}
		}
		for handle in frame_handles.as_slice() {
			close(*handle);
		}
		if let Some(dead) = self.surfaces[index].events.take() {
			close(dead.producer);
		}
	}

	/// A NEW CONFIGURATION IS A NEW SERIAL, and a change to the extent is a new GENERATION with it -
	/// every image of the old one becomes stale rather than being presented into a size it was not
	/// drawn for.
	fn reconfigure(&mut self, index: usize, width: u32, height: u32) {
		let changed: bool = self.surfaces[index].width != width || self.surfaces[index].height != height;
		self.surfaces[index].serial = self.surfaces[index].serial.saturating_add(1);
		self.surfaces[index].acknowledged = None;
		if changed {
			self.surfaces[index].width = width;
			self.surfaces[index].height = height;
			self.surfaces[index].pitch = width.saturating_mul(4);
			self.surfaces[index].generation = self.surfaces[index].generation.saturating_add(1);
			for image in &mut self.surfaces[index].images {
				image.state = ImageState::Stale;
			}
			// A FRAME IN FLIGHT IS NOT UN-SUBMITTED: it settles first, as replaced.
			for pending in core::mem::take(&mut self.surfaces[index].pending) {
				self.stats.record(PresentOutcome::ReplacedByResize);
				let complete = PresentComplete { serial: pending.serial, outcome: PresentOutcome::ReplacedByResize, evidence: TimestampEvidence::Unavailable, timing: FrameTiming { preferred_deadline: None, refresh_interval: None } };
				self.emit(index, &SurfaceEvent::PresentComplete(complete));
			}
			self.rebuild_images(index);
		}
		let snapshot = self.snapshot(index);
		self.emit(index, &SurfaceEvent::Configure(snapshot));
	}

	/// Rebuild a surface's image SLOTS for its current generation.
	///
	/// A GENERATION CHANGE INVALIDATES THE WHOLE SET and this service releases every imported
	/// handle: no image ever crosses a generation, which is the rule the state machine already
	/// states for `Stale`. The client's Domain gets its memory back when its own handles go, and
	/// what is left here is empty slots the client fills for the new extent.
	fn rebuild_images(&mut self, index: usize) {
		let count: u32 = self.surfaces[index].images.len() as u32;
		let surface: &mut Surface = &mut self.surfaces[index];
		for image in surface.images.drain(..) {
			if image.supplied() {
				unmap_object(image.handle);
				close(image.handle);
			}
		}
		surface.initialized = false;
		if !reserve_slots(surface, count) {
			// A REBUILD THAT CANNOT EVEN BOOK ITS SLOTS LEAVES THE SURFACE WITH NONE, and every
			// acquire answers `again` - which is a client that waits rather than one handed an index
			// naming nothing.
			surface.images.clear();
		}
	}

	fn notify_resize(&mut self) {
		let (width, height): (u32, u32) = (self.scanout.width, self.scanout.height);
		for index in 0..self.surfaces.len() {
			// ONLY A NATIVE-SIZED SURFACE FOLLOWS THE SCANOUT. A fixed-size client stays its own
			// size and is scaled, which is what it asked for by naming one.
			if self.surfaces[index].console {
				self.reconfigure(index, width, height);
			} else {
				let snapshot = self.snapshot(index);
				self.emit(index, &SurfaceEvent::Configure(snapshot));
			}
		}
	}

	fn present_active_full(&mut self) {
		let Some(index) = self.surface_index(self.active) else { return };
		let Some(slot) = self.surfaces[index].images.iter().position(|image| image.state != ImageState::Stale && image.supplied()) else { return };
		let width: u32 = self.surfaces[index].width;
		let height: u32 = self.surfaces[index].height;
		let blit: pix::BlitResult = self.blit(index, slot, Rect { x: 0, y: 0, width, height });
		let _ = self.flush_damage(&[blit.rect]);
	}

	fn blit(&mut self, index: usize, slot: usize, damage: Rect) -> pix::BlitResult {
		let first: bool = !self.surfaces[index].initialized;
		self.surfaces[index].initialized = true;
		let surface: &Surface = &self.surfaces[index];
		let source_len: usize = surface.pitch as usize * surface.height as usize;
		let target_len: usize = self.scanout.fb.pitch as usize * self.scanout.height as usize;
		let source: &[u8] = unsafe { core::slice::from_raw_parts(surface.images[slot].addr as *const u8, source_len) };
		let target: &mut [u8] = unsafe { core::slice::from_raw_parts_mut(self.scanout.addr as *mut u8, target_len) };
		// THE DESCRIPTORS ARE CHECKED WHERE THEY ARE BUILT NOW, not assumed by the blitter. Both
		// constructors refuse a buffer too small for the geometry it claims and a scanout whose
		// channel masks overlap - the firmware hand-off is the one place masks come from, and two
		// channels sharing a bit is a picture whose colours shift as its content does.
		let source = Image::rgba(source, surface.width, surface.height, surface.pitch).expect("DisplayService sized the surface it is presenting");
		let fb = &self.scanout.fb;
		let target = Target::packed(target, self.scanout.width, self.scanout.height, fb.pitch, fb.bytes_per_pixel, (fb.red_shift, fb.red_size), (fb.green_shift, fb.green_size), (fb.blue_shift, fb.blue_size)).expect("DisplayService validates the scanout it was handed before presenting into it");
		pix::blit(source, target, damage, first).expect("DisplayService validates surface and scanout bounds before blitting")
	}

	// TRANSFER THE DAMAGED RECTANGLES AND WAIT FOR THE DEVICE TO ACKNOWLEDGE THEM.
	//
	// ONE CALL FOR THE WHOLE FRAME. This used to be one `PRESENT` message per rectangle, each with
	// its own reply, and the driver coalesced whatever it found queued; the typed call carries the
	// LIST, so the driver keeps the rectangles apart without a second protocol for saying so.
	// AND THE INTERLEAVING IS GONE WITH IT. A resize used to arrive on the same channel as the
	// present's answer, so this loop had to recognise and handle a framebuffer replacement while
	// waiting for an acknowledgement. Events have their own stream now: what comes back from a
	// present is the present's answer and nothing else.
	fn flush_damage(&mut self, rects: &[Rect]) -> Result<(), Error> {
		// THE BACKEND WENT AWAY UNDER THIS FRAME, which is not the same as there being no backend:
		// a boot framebuffer has no driver channel and works.
		if self.scanout.lost {
			return Err(Error::Closed);
		}
		if self.scanout.gpu == 0 || rects.is_empty() {
			return Ok(());
		}
		let mut wire: Vec<WireRect> = Vec::new();
		if wire.try_reserve_exact(rects.len()).is_err() {
			return Err(Error::Exhausted);
		}
		for rect in rects {
			let (Ok(x), Ok(y)) = (i32::try_from(rect.x), i32::try_from(rect.y)) else {
				return Err(Error::Invalid);
			};
			wire.push(WireRect { origin: Offset2d { x, y }, size: Extent2d { width: rect.width, height: rect.height } });
		}
		let damage = DamageRegion { whole: false, rects: wire };
		let mut client = display_device::Client::new(ChannelTransport { chan: self.scanout.gpu });
		match client.present(&self.scanout.generation, &damage) {
			Some(Ok(())) => Ok(()),
			Some(Err(error)) => Err(error),
			None => Err(Error::Closed),
		}
	}

	// THE DRIVER CHANNEL THIS SCANOUT CAME IN ON IS GIVEN BACK.
	//
	// The peer-close arm cleared the channel NUMBER and left the handle open, which is a handle this
	// process holds for the rest of its life against a peer that has gone. The framebuffer mapping
	// is deliberately left alone: the console keeps drawing into memory it already mapped, which is
	// what it did before this path existed, and `adopt_scanout` is what releases it - when there is
	// something to replace it with.
	fn release_scanout(&mut self) {
		if self.scanout.gpu != 0 {
			close(self.scanout.gpu);
			self.scanout.gpu = 0;
			// AND EVERY PRESENT FROM HERE IS `driver-lost` UNTIL SOMETHING IS ADOPTED. The mapping is
			// deliberately left in place - the console keeps drawing into memory it already mapped -
			// but nothing is reading it, so a present that reported `displayed` would be reporting a
			// frame nobody saw.
			self.scanout.lost = true;
		}
		// AND THE PENDING REPORT GOES WITH IT (corrected 2026-09-03). The latch is armed by an
		// adoption and answered by the first present through the adopted provider; a provider that
		// went away before that present has nothing to report, and leaving the latch armed let the
		// NEXT present - which reaches no driver at all, because `flush` returns early with no
		// scanout - emit the line that says a frame reached the display. A gate reading that line
		// would pass on a driver that did the framebuffer handshake and then died.
		self.report_present = false;
	}

	// PRESENT ON A REPLACEMENT PROVIDER, or answer false and change nothing.
	//
	// The whole of the post-rebind restore, in one method: the same `FB` handshake `init_scanout` performs at
	// bootstrap, run against a connection the catalogue minted after a rebind, with the old mapping
	// released only once the new one is known good. Every surface is marked uninitialised because
	// each is copied into the scanout on its next present and the scanout it was last drawn against
	// is gone.
	fn adopt_scanout(&mut self, gpu: u64, _buf: &mut [u8]) -> bool {
		unsafe {
			let Some(described) = ask_scanout(gpu) else { return false };
			let handle: u64 = described.backing.handle;
			let Some((fb, width, height)) = describe_framebuffer(&described) else {
				close(handle);
				return false;
			};
			let addr: i64 = dma_buffer_map(handle);
			if sys_is_err(addr as u64) || !valid_scanout(&fb, width, height) {
				if !sys_is_err(addr as u64) {
					dma_buffer_unmap(handle);
				}
				close(handle);
				return false;
			}
			let old_events: u64 = self.scanout.events;
			let old: u64 = self.scanout.handle;
			self.scanout = Scanout { gpu, handle, addr: addr as u64, fb, width, height, generation: described.generation, events: open_device_events(gpu), lost: false };
			if old_events != 0 {
				close(old_events);
			}
			// THE NEXT PRESENT IS THE EVIDENCE THIS ADOPTION WORKED, so it is reported - see
			// `DisplayState::report_present`.
			self.report_present = true;
			self.resets = self.resets.saturating_add(1);
			for surface in &mut self.surfaces {
				surface.initialized = false;
			}
			if old != 0 {
				dma_buffer_unmap(old);
				close(old);
			}
			true
		}
	}

	// WHAT THE DEVICE REPORTED, ACTED ON.
	//
	// TWO EVENTS AND THEY ARE NOT THE SAME EVENT, which is the whole reason the wire distinguishes
	// them: a RESIZE is the same pixels differently framed and costs nothing but a reflow, and a
	// REPLACEMENT is a new backing at a new generation, where everything drawn against the old one is
	// gone. Answering both by remapping would remap on every window drag; answering both by reflowing
	// would draw into memory the driver has given back.
	fn handle_device_event(&mut self, event: DeviceEvent) -> bool {
		let replaced = match event {
			DeviceEvent::Resized(extent) => {
				if extent.width == 0 || extent.height == 0 || extent.width > self.scanout.fb.width || extent.height > self.scanout.fb.height {
					return false;
				}
				self.scanout.width = extent.width;
				self.scanout.height = extent.height;
				for surface in &mut self.surfaces {
					surface.initialized = false;
				}
				return true;
			}
			DeviceEvent::Replaced(scanout) => scanout,
		};
		let handle: u64 = replaced.backing.handle;
		let Some((fb, width, height)) = describe_framebuffer(&replaced) else {
			close(handle);
			return false;
		};
		let addr: i64 = unsafe { dma_buffer_map(handle) };
		if sys_is_err(addr as u64) || !valid_scanout(&fb, width, height) {
			if !sys_is_err(addr as u64) {
				dma_buffer_unmap(handle);
			}
			close(handle);
			return false;
		}
		let old: u64 = self.scanout.handle;
		self.scanout.handle = handle;
		self.scanout.addr = addr as u64;
		self.scanout.fb = fb;
		self.scanout.width = width;
		self.scanout.height = height;
		// THE GENERATION MOVES WITH THE BACKING, and the next present names the new one. A present
		// still in flight for the old generation is refused by the driver rather than drawn.
		self.scanout.generation = replaced.generation;
		for surface in &mut self.surfaces {
			surface.initialized = false;
		}
		if old != 0 {
			dma_buffer_unmap(old);
			close(old);
		}
		true
	}
}

// HAND A REPLY OVER, GRANTING EACH CAPABILITY EXACTLY THE AUTHORITY THE CONTRACT SAYS IT CARRIES.
//
// A capability moves with the rights the SENDER's handle has unless the send is told otherwise, so
// the ordinary send would give every client the full authority of an endpoint this service minted -
// `transfer` and `duplicate` included. `grant` is one mask per capability in encoding order; an
// empty grant is a reply whose capabilities are meant to travel with what they hold.
fn send_reply(chan: u64, bytes: &[u8], handles: &[u64], grant: &[u32]) -> bool {
	if handles.is_empty() {
		return send_blocking(chan, bytes, 0);
	}
	if grant.len() == handles.len() {
		return send_caps_blocking_attenuated(chan, bytes, handles, grant);
	}
	send_caps_blocking(chan, bytes, handles)
}

// THE AUTHORITY A CONNECTION'S REPLY GRANTS, per capability, in encoding order.
fn display_grant(op: u16) -> &'static [u32] {
	match op {
		// THE SURFACE STAYS WHERE IT WAS CREATED. Without `transfer` the endpoint cannot be moved to
		// another process AT THE SYSCALL, which is the only place that rule can be enforced: a
		// message names the endpoint it arrived on, and neither it nor `ObjectInfo` reports the
		// holder, so a service cannot detect a live transfer however it is written. One process
		// identity owns a surface's images, its resize authority and its cleanup - and a client that
		// wants another process to draw gives it the pixels through its own protocol.
		display::OP_CREATE_SURFACE => &[RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT],
		_ => &[],
	}
}

// THE AUTHORITY A SURFACE'S REPLY GRANTS, per capability, in encoding order.
fn surface_grant(op: u16) -> &'static [u32] {
	match op {
		// The producer end may only SEND and the completion end may only RECEIVE and WAIT, so
		// neither can be used as the other, and neither carries `transfer` or `duplicate`.
		surface::OP_QUEUE => &[RIGHT_SEND, RIGHT_RECEIVE | RIGHT_WAIT],
		// THE FOCUS PROOF IS MEANT TO TRAVEL, and is the one capability here that is: its whole
		// purpose is to be handed to `input.subscribe-keys`. So it keeps `transfer` - and nothing
		// beyond what a one-shot proof needs to be sent once.
		surface::OP_INPUT_FOCUS => &[RIGHT_SEND | RIGHT_TRANSFER],
		_ => &[],
	}
}

/// Reserve a generation's image SLOTS. Answers false and leaves nothing behind on failure.
///
/// THE SLOTS ARE THIS SERVICE'S AND THE PIXELS ARE THE CLIENT'S. Nothing is allocated here beyond
/// the bookkeeping, because `SYS_MEMORY_OBJECT_CREATE` charges the Domain that CREATES an object -
/// so a service that allocated its clients' images would pay for every one of them, and one client
/// could exhaust this service's quota while its own budget still looked healthy.
///
/// A FREE FUNCTION AND NOT A METHOD, because a rebuild needs it while holding a `&mut` to one
/// surface - and taking the surface out of the vector to satisfy that borrow is what MOVED it to
/// the end and left every index the caller held pointing at a different surface.
fn reserve_slots(surface: &mut Surface, images: u32) -> bool {
	if surface.images.try_reserve_exact(images as usize).is_err() {
		return false;
	}
	for _ in 0..images {
		surface.images.push(QueueImage::empty());
	}
	true
}

struct DisplayCall<'a> {
	state: &'a mut DisplayState,
	chan: u64,
}

impl Service for DisplayCall<'_> {
	fn create_surface(&mut self, request: SurfaceRequest) -> Result<u64, Error> {
		self.state.create_surface(self.chan, &request)
	}

	fn image_limits(&mut self) -> Result<ImageLimits, Error> {
		Ok(ImageLimits { minimum: MIN_IMAGES, maximum: MAX_IMAGES })
	}
}

/// One surface's own calls, on its own channel.
///
/// A SURFACE IS A CAPABILITY AND NOT AN INDEX. A client that named its surface by number on a shared
/// connection could name another client's, and every handler would have to check - which is a check
/// a capability makes unnecessary.
struct SurfaceCall<'a> {
	state: &'a mut DisplayState,
	chan: u64,
}

impl SurfaceService for SurfaceCall<'_> {
	fn configuration(&mut self) -> Result<SurfaceConfiguration, Error> {
		self.state.configuration(self.chan)
	}

	fn ack_configure(&mut self, serial: u64) -> Result<(), Error> {
		self.state.ack_configure(self.chan, serial)
	}

	fn queue(&mut self) -> Result<PresentQueue, Error> {
		self.state.queue(self.chan)
	}

	fn acquire_next(&mut self) -> Result<AcquiredImage, Error> {
		self.state.acquire_next(self.chan)
	}

	fn abandon(&mut self, image: u32) -> Result<(), Error> {
		self.state.abandon(self.chan, image)
	}

	fn present(&mut self, image: u32, serial: u64, generation: u64, damage: DamageRegion) -> Result<u64, Error> {
		self.state.present(self.chan, image, serial, generation, &damage)
	}

	fn events(&mut self) -> Vec<SurfaceEvent> {
		Vec::new()
	}

	fn input_focus(&mut self) -> Result<u64, Error> {
		self.state.input_focus(self.chan)
	}

	fn close(&mut self) -> Result<(), Error> {
		self.state.close_surface(self.chan)
	}

	fn provide_image(&mut self, index: u32, image: u64) -> Result<(), Error> {
		self.state.provide_image(self.chan, index, image)
	}
}

/// THE OBSERVATION ENDPOINT, AND IT IS NOT THE ADMIN ONE.
///
/// A SEPARATE INTERFACE ON A SEPARATE ROOT, because authority to READ what this service holds must
/// not be authority to bind a process to a display or to take the screen. The System Graph is handed
/// this and nothing else, which makes "never as enforcement" a property of the capability rather
/// than a promise about the caller.
struct StatsCall<'a> {
	state: &'a DisplayState,
	waiters: u64,
}

impl StatsService for StatsCall<'_> {
	fn resources(&mut self) -> DisplayResources {
		self.state.resources(self.waiters)
	}
}

struct AdminCall<'a> {
	clients: &'a mut Vec<Client>,
	stats: &'a PerfStats,
}

impl AdminService for AdminCall<'_> {
	fn bind(&mut self, task: u64) -> Result<u64, Error> {
		if task == 0 {
			return Err(Error::Invalid);
		}
		let (server, client): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => {
				close(task);
				return Err(Error::Again);
			}
		};
		self.clients.push(Client { chan: server, task });
		Ok(client)
	}

	fn stats(&mut self) -> PresentationStats {
		self.stats.snapshot()
	}

	/// THE VISIBILITY RULE IS THE SERVICE'S AND NOT A CLIENT'S. A client that could make itself
	/// visible is a client that can take the screen, which is exactly what a compositor exists to
	/// arbitrate - so until one does, the decision lives behind the privileged boundary.
	fn set_visible(&mut self, task: u64, surface: u64) -> Result<(), Error> {
		if task != 0 {
			close(task);
		}
		let _ = surface;
		Err(Error::Unsupported)
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 128] = [0; 128];
	unsafe {
		let focus_control: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if len >= 5 && &buf[..5] == b"FOCUS" => handle,
			_ => fail_bootstrap(bootstrap, b"focus", b"input focus channel not delivered"),
		};
		let kill_control: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if len >= 4 && &buf[..4] == b"KILL" => handle,
			_ => fail_bootstrap(bootstrap, b"kill", b"emergency input channel not delivered"),
		};
		let admin: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if len >= 5 && &buf[..5] == b"ADMIN" => handle,
			_ => fail_bootstrap(bootstrap, b"admin", b"display admin channel not delivered"),
		};
		let service: u64 = recv_tagged(bootstrap, &mut buf, b"SERVE").unwrap_or_else(|| fail_bootstrap(bootstrap, b"serve", b"missing serve channel"));
		// The DisplayController capability, last in the sequence. `SYS_FRAMEBUFFER_MAP` requires
		// it; without one this service falls back to whatever the GPU driver offers and takes no
		// boot framebuffer, which is the same degradation as a machine with no framebuffer at
		// all rather than a failure to start.
		let display_ctl: u64 = recv_tagged(bootstrap, &mut buf, b"DISPLAYCTL").unwrap_or(0);
		// THE GPU IS DISCOVERED, NOT HANDED OVER (2026-09-02).
		//
		// This service used to be given the display driver's channel under `GPU`, taken by
		// DeviceManager into a slot of its own and routed down the boot chain - the per-kind
		// injection the provider catalogue exists to replace, and the reason a rebound GPU could not
		// restore a picture: a
		// slot is filled once, so a GPU driver that crashed and rebound published a replacement
		// provider that had nowhere to go and no way to reach the service already running.
		//
		// What arrives now is a connection to the provider CATALOGUE, and this service asks it for
		// the display kind. The subscription answers with what is published NOW and continues as a
		// stream, so the device this service starts on and the device it recovers onto arrive down
		// the same path.
		//
		// LAST IN THE ROLE LIST, because the bootstrap is read POSITIONALLY at every hop.
		let catalogue: u64 = recv_tagged(bootstrap, &mut buf, b"CATALOGUE").unwrap_or(0);
		// THE OBSERVATION ROOT, and it is OPTIONAL: a boot that granted none is a boot whose System
		// Graph reports no display resources, which is a smaller answer and not a broken display.
		// LAST, like every addition to a positional bootstrap.
		let stats_root: u64 = recv_tagged(bootstrap, &mut buf, b"STATS").unwrap_or(0);
		let providers: u64 = subscribe_to_displays(catalogue);
		// THE SNAPSHOT IS ALREADY IN THE CHANNEL, which is what makes a subscription usable at
		// bootstrap rather than only afterwards: the catalogue registers a subscriber and sends it
		// everything published, in one step, before it answers. So the provider this machine has is
		// readable here, without blocking, and a machine that has none falls through to the boot
		// framebuffer exactly as one with no display driver always did.
		let gpu: u64 = take_published_display(catalogue, providers, &mut buf);
		let scanout: Scanout = init_scanout(gpu, display_ctl, &mut buf);
		if !scanout.available() {
			fail_bootstrap(bootstrap, b"display", b"no framebuffer available");
		}
		send_blocking(bootstrap, b"DisplayService: online", 0);
		serve_display(service, admin, stats_root, catalogue, providers, DisplayState::new(scanout, focus_control, kill_control));
	}
}

// SUBSCRIBE TO THE DISPLAY KIND, or answer zero.
//
// Zero is not a failure. A boot that granted no catalogue connection, or a catalogue that refuses
// the subscription, is a system whose display cannot be replaced - which this service reports and
// goes on serving whatever scanout it has, the same way it serves a machine with no GPU at all.
fn subscribe_to_displays(catalogue: u64) -> u64 {
	if catalogue == 0 {
		print(b"DisplayService: no provider catalogue - this instance cannot follow a display that rebinds\n");
		return 0;
	}
	let subscription: u64 = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Display).unwrap_or(0);
	if subscription == 0 {
		print(b"DisplayService: the catalogue refused a display subscription\n");
	}
	subscription
}

// OPEN A CONNECTION TO ONE PUBLISHED PROVIDER, or answer zero.
//
// The catalogue mints the pair and hands the driver the server end; what comes back is the client
// end this service talks the driver's byte protocol over - the same channel `GPU` used to carry,
// reached by asking instead of by being given.
fn open_provider(catalogue: u64, info: &ProviderInfo) -> u64 {
	if catalogue == 0 {
		return 0;
	}
	match provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(info) {
		Some(Ok(handle)) => handle,
		// A REFUSAL IS SAID. A kind that admits one consumer refuses the second ask, and a
		// service that cannot tell that from "no device" cannot report either.
		Some(Err(_)) => {
			print(b"DisplayService: the catalogue refused a connection to the display provider it published\n");
			0
		}
		None => {
			print(b"DisplayService: the catalogue did not answer the connection it published\n");
			0
		}
	}
}

// THE FIRST LIVE DISPLAY PROVIDER THE SUBSCRIPTION HAS ALREADY QUEUED, connected to.
//
// POLLED, NEVER BLOCKED. The snapshot is written into the subscription before `subscribe` answers,
// so what is here is here; waiting for more would hang the boot of every machine whose display is
// the loader's framebuffer and whose catalogue therefore has nothing to publish.
//
// It stops at the first provider it CONNECTS to rather than draining the channel: a frame this
// function reads and drops is a publication the standing loop will never see, and a second display
// is exactly the thing this milestone exists to stop dropping.
fn take_published_display(catalogue: u64, providers: u64, buf: &mut [u8]) -> u64 {
	if catalogue == 0 || providers == 0 {
		return 0;
	}
	loop {
		let PolledCaps::Message { len, handles } = try_recv_caps(providers, buf) else { return 0 };
		for &handle in handles.as_slice() {
			close(handle);
		}
		let mut frame_handles = wire::Handles::new();
		let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else {
			print(b"DisplayService: a provider frame did not decode\n");
			continue;
		};
		if !info.live {
			continue;
		}
		let opened: u64 = open_provider(catalogue, &info);
		if opened != 0 {
			return opened;
		}
	}
}

// Ask a display device for its scanout, over the typed wire.
fn ask_scanout(gpu: u64) -> Option<DeviceScanout> {
	let mut client = display_device::Client::new(ChannelTransport { chan: gpu });
	match client.scanout() {
		Some(Ok(scanout)) if scanout.backing.handle != 0 => Some(scanout),
		Some(Ok(scanout)) => {
			// A DESCRIPTION WITH NO BACKING IS NOT A SCANOUT, and the handle is zero rather than
			// absent because the wire carries a buffer either way.
			if scanout.backing.handle != 0 {
				close(scanout.backing.handle);
			}
			None
		}
		_ => None,
	}
}

// Open the device's event stream, or zero when it has none to give.
fn open_device_events(gpu: u64) -> u64 {
	let mut client = display_device::Client::new(ChannelTransport { chan: gpu });
	client.events().unwrap_or(0)
}

// The ABI framebuffer a scanout description implies, plus the visible extent.
//
// THE DESCRIPTION IS CHECKED HERE AND THE MASKS COME FROM THE REGISTRY. A driver names a format; what
// that name means as channel shifts is the shared model's answer, not this service's - which is what
// stops a `R8G8B8X8` device from being drawn into as though it were `B8G8R8X8`.
fn describe_framebuffer(scanout: &DeviceScanout) -> Option<(Framebuffer, u32, u32)> {
	let layout = graphics_core::layout::ImageLayout::try_from(&scanout.layout).ok()?;
	let masks = layout.storage.packed_masks()?;
	let fb = Framebuffer { width: layout.extent.width, height: layout.extent.height, pitch: layout.pitch, bytes_per_pixel: masks.bytes_per_pixel as u32, red_shift: masks.red.shift, red_size: masks.red.bits, green_shift: masks.green.shift, green_size: masks.green.bits, blue_shift: masks.blue.shift, blue_size: masks.blue.bits, _pad: [0; 2] };
	// THE BACKING MUST HOLD WHAT THE LAYOUT DESCRIBES. A driver that hands over a shorter object
	// than its own description is a mapping this service would read past.
	if scanout.backing.len < layout.backend_access_span(true)? {
		return None;
	}
	Some((fb, scanout.visible.width, scanout.visible.height))
}

unsafe fn init_scanout(gpu: u64, display_ctl: u64, _buf: &mut [u8]) -> Scanout {
	unsafe {
		if gpu != 0
			&& let Some(described) = ask_scanout(gpu)
		{
			let handle: u64 = described.backing.handle;
			match describe_framebuffer(&described) {
				Some((fb, width, height)) => {
					let addr: i64 = dma_buffer_map(handle);
					if !sys_is_err(addr as u64) && valid_scanout(&fb, width, height) {
						return Scanout { gpu, handle, addr: addr as u64, fb, width, height, generation: described.generation, events: open_device_events(gpu), lost: false };
					}
					if !sys_is_err(addr as u64) {
						dma_buffer_unmap(handle);
					}
					close(handle);
				}
				None => close(handle),
			}
		}
		// THE BOOT FRAMEBUFFER, which is not a device and has no generation to move: it is one
		// mapping for the life of the process, so a present names generation zero and nothing ever
		// makes that stale.
		let mut fb: Framebuffer = Framebuffer::default();
		let addr: i64 = framebuffer_map(display_ctl, &mut fb);
		if !sys_is_err(addr as u64) && valid_scanout(&fb, fb.width, fb.height) { Scanout { gpu: 0, handle: 0, addr: addr as u64, width: fb.width, height: fb.height, fb, generation: 0, events: 0, lost: false } } else { Scanout { gpu: 0, handle: 0, addr: 0, fb: Framebuffer::default(), width: 0, height: 0, generation: 0, events: 0, lost: false } }
	}
}

fn serve_display(root: u64, admin: u64, stats_root: u64, catalogue: u64, mut providers: u64, mut state: DisplayState) -> ! {
	let mut clients: Vec<Client> = alloc::vec![Client { chan: root, task: 0 }];
	// THE OBSERVATION ROOT IS A FACTORY LIKE EVERY OTHER ROOT IN THIS SYSTEM, and it was not: it
	// answered `resources()` and NOTHING else, so a supervisor minting an independent connection
	// from it - which is what `service_connect` does, and what the whole broker pattern is - sent
	// the reserved connect opcode, got no reply at all, and blocked FOREVER inside its own
	// bootstrap. The boot chain stopped there: SystemGraphService never received its serve root and
	// the shell after it was never started, on a system whose display was working perfectly.
	let mut stats: Vec<u64> = if stats_root != 0 { alloc::vec![stats_root] } else { Vec::new() };
	let mut request: [u8; REQUEST_MAX] = [0; REQUEST_MAX];
	let mut reply: [u8; REPLY_MAX] = [0; REPLY_MAX];
	loop {
		// A SURFACE THAT ASKED TO BE CLOSED GOES NOW, ONE STEP AFTER ITS ANSWER WENT OUT. The
		// teardown closes the surface's own channel and can present the restore on the way, and
		// neither is something the handler that owes a reply on that channel may do.
		if let Some(closing) = state.closing.take() {
			state.remove_surface(closing, true);
		}
		let mut waits: Vec<u64> = Vec::with_capacity(clients.len() + 4);
		// THE DEVICE'S EVENT STREAM AND THE DEVICE'S CHANNEL ARE TWO DIFFERENT WAITS NOW. A present
		// is a call on the channel and answers on it; a resize or a replacement arrives on the
		// stream. Waiting on the channel is still what notices the driver going away.
		if state.scanout.events != 0 {
			waits.push(state.scanout.events);
		}
		if state.scanout.gpu != 0 {
			waits.push(state.scanout.gpu);
		}
		if providers != 0 {
			waits.push(providers);
		}
		if state.kill_control != 0 {
			waits.push(state.kill_control);
		}
		waits.push(admin);
		// THE OBSERVATION ROOT, WHICH IS WAITED ON LIKE ANY OTHER and answers like no other: nothing
		// reachable from it changes a thing.
		// EVERY OBSERVATION CHANNEL IS WAITED ON: the root, and each connection minted from it.
		let stats_count: usize = stats.len();
		for &observation in &stats {
			waits.push(observation);
		}
		waits.extend(clients.iter().map(|client| client.chan));
		// EVERY SURFACE IS ITS OWN CHANNEL AND ITS OWN WAIT. A surface whose channel was not waited
		// on would be a capability a client holds and cannot use, which is worse than not having it.
		let connections: usize = clients.len();
		waits.extend(state.surfaces.iter().map(|surface| surface.chan));
		// AND EVERY BOUND CLIENT'S PROCESS, so a client's death is noticed INDEPENDENTLY of channel
		// peer lifetime and of whether the driver is making progress.
		//
		// A CHANNEL CANNOT ANSWER THIS QUESTION. It stays open while ANYONE holds its peer, so a
		// surface's imported images - and the mappings this service made of them - would outlive the
		// process the contract says owns them, which is precisely the reclamation this contract
		// promises. A process handle becomes ready when the process terminates, which is the kernel
		// telling this loop exactly that.
		let surfaces_watched: usize = state.surfaces.len();
		let mut watched: Vec<usize> = Vec::new();
		if watched.try_reserve(clients.len()).is_ok() {
			for (index, client) in clients.iter().enumerate() {
				if client.task != 0 {
					waits.push(client.task);
					watched.push(index);
				}
			}
		}
		let ready: i64 = wait_any(&waits, 0);
		if ready < 0 {
			continue;
		}
		let events_first: bool = state.scanout.events != 0;
		if events_first && ready == 0 {
			match recv_caps_blocking(state.scanout.events, &mut request) {
				ReceivedCaps::Message { len, handles: mut frame_handles } => {
					// THE FRAME'S CAPABILITIES ARE SPENT BY A SUCCESSFUL READ and what is left is
					// closed unconditionally, which is the rule this transport already states: the
					// alternative is a reader that closes before decoding on one path and after on
					// the other, correct only by accident of the element type.
					let event = display_device::events_read(&request[..len], &mut frame_handles);
					for handle in frame_handles.as_slice() {
						close(*handle);
					}
					if let Some(event) = event
						&& state.handle_device_event(event)
					{
						state.notify_resize();
						state.present_active_full();
					}
				}
				// THE STREAM ENDED. The device is still there - a stream can be lost on its own -
				// so the channel is given back and the next adoption opens a new one.
				ReceivedCaps::Closed => {
					close(state.scanout.events);
					state.scanout.events = 0;
				}
			}
			continue;
		}
		let gpu_first: bool = state.scanout.gpu != 0;
		if gpu_first && ready == if events_first { 1 } else { 0 } {
			// NOTHING ARRIVES ON THIS CHANNEL UNASKED any more: a present's answer is read by the
			// call that made it, and events have their own stream. What this wait notices is the
			// driver GOING AWAY, which is the one thing a channel says without being asked.
			match recv_blocking(state.scanout.gpu, &mut request) {
				Received::Message { handle, .. } => {
					if handle != 0 {
						close(handle);
					}
				}
				// THE DRIVER WENT AWAY, AND WHAT IT LEFT GOES WITH IT. This only cleared the
				// channel number, so the dead handle stayed open in this process and the
				// scanout kept pointing into a mapping of a buffer whose owner had gone. A
				// replacement cannot be adopted on top of that, which is half of why M4's
				// restore was unreachable.
				Received::Closed => state.release_scanout(),
			}
			continue;
		}
		// A PUBLICATION OR A WITHDRAWAL, AND THE ONLY ONE THAT MATTERS IS A DISPLAY ARRIVING
		// WHILE THIS SERVICE HAS NONE.
		//
		// That is M4: a GPU driver crashes, DeviceManager rebinds it, the new binding publishes
		// its provider, and the display path comes back without restarting the service or
		// anything above it. A withdrawal needs nothing here - the driver channel closing is the
		// authoritative signal and the arm above already handles it.
		let providers_index: usize = events_first as usize + gpu_first as usize;
		let providers_present: bool = providers != 0;
		if providers_present && ready as usize == providers_index {
			match recv_blocking(providers, &mut request) {
				Received::Message { len, handle } => {
					if handle != 0 {
						close(handle);
					}
					let mut frame_handles = wire::Handles::new();
					match provider_catalogue::subscribe_read(&request[..len], &mut frame_handles) {
						Some(info) if info.live && state.scanout.gpu == 0 => {
							let opened: u64 = open_provider(catalogue, &info);
							if opened == 0 {
								print(b"DisplayService: a display provider is published and this service could not connect to it\n");
							} else if state.adopt_scanout(opened, &mut request) {
								print(b"DisplayService: a display provider was published and this service presents on it\n");
								state.notify_resize();
								state.present_active_full();
							} else {
								// A CONNECTION THAT CANNOT ANSWER THE FRAMEBUFFER HANDSHAKE IS
								// GIVEN BACK, not held: a provider this service keeps and does
								// not use is a channel the driver waits on forever.
								close(opened);
								print(b"DisplayService: a published display provider did not hand over a usable framebuffer\n");
							}
						}
						// A WITHDRAWAL, WHICH THIS SERVICE ACTS ON BY SAYING SO AND NOTHING
						// MORE - and now it says so (2026-09-03).
						//
						// The arm was silent, and silence is the same as the announcement never
						// arriving: `Catalogue::announce_gone` could be emptied and nothing in
						// any suite would notice, so the second of M7's two production effects
						// had no oracle at all. It is a LINE and not a state change on purpose:
						// the scanout is released when the driver's channel closes, which is the
						// authoritative signal for a driver that has actually gone, and a
						// withdrawal frame is a manager saying the publication is over.
						Some(info) if !info.live => {
							print(b"DisplayService: a display provider was withdrawn and this service was told\n");
						}
						Some(_) => {}
						None => print(b"DisplayService: a provider frame did not decode\n"),
					}
				}
				// THE SUBSCRIPTION ENDED. DeviceManager is gone or dropped it; this service keeps
				// whatever scanout it has and stops expecting new ones.
				Received::Closed => {
					close(providers);
					providers = 0;
				}
			}
			continue;
		}
		let kill_index: usize = events_first as usize + gpu_first as usize + providers_present as usize;
		let kill_present: bool = state.kill_control != 0;
		if kill_present && ready as usize == kill_index {
			match recv_blocking(state.kill_control, &mut request) {
				Received::Message { len, handle } => {
					if handle != 0 {
						close(handle);
					}
					// THE VICTIM IS THE CONNECTION THAT OWNS THE ACTIVE SURFACE, and never a
					// connection whose channel happens to equal it. `active` names a SURFACE now,
					// and a surface channel is never a connection channel - so this search matched
					// nothing and the emergency command revoked nothing at all. The root connection
					// is excluded because closing it is this service's own shutdown, which is not
					// what an emergency revoke is for.
					let owner: u64 = state.surface_index(state.active).map(|index| state.surfaces[index].owner).unwrap_or(0);
					if len >= 4
						&& &request[..4] == b"KILL"
						&& state.active != 0
						&& state.active != state.console
						&& owner != 0 && let Some(victim) = clients.iter().position(|client| client.chan == owner)
						&& victim != 0
					{
						let chan: u64 = clients[victim].chan;
						if clients[victim].task != 0 {
							let _ = signal(clients[victim].task, SIG_KILL);
						}
						state.drop_client(chan);
						close(chan);
						let victim: Client = clients.swap_remove(victim);
						if victim.task != 0 {
							close(victim.task);
						}
					}
				}
				Received::Closed => state.kill_control = 0,
			}
			continue;
		}
		let admin_index: usize = events_first as usize + gpu_first as usize + providers_present as usize + kill_present as usize;
		if ready as usize == admin_index {
			match recv_caps_blocking(admin, &mut request) {
				ReceivedCaps::Message { len, handles: caps } => {
					let mut reply_handle = proto::codec::Handles::new();
					// EVERY CAPABILITY THE MESSAGE CARRIED. This was `Handles::from_slice(&[handle])`
					// over the single-handle receive, which keeps the first and drops the rest - so a
					// client sending stdin, stdout and stderr had two destroyed before dispatch.
					let mut handle = caps;
					let mut call = AdminCall { clients: &mut clients, stats: &state.stats };
					if let Some(n) = display_admin::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle) {
						if !send_caps_blocking(admin, &reply[..n], reply_handle.as_slice()) {
							for &leftover in reply_handle.as_slice() {
								close(leftover);
							}
						}
					} else {
						for &leftover in reply_handle.as_slice() {
							close(leftover);
						}
					}
					for &unclaimed in handle.as_slice() {
						close(unclaimed);
					}
				}
				ReceivedCaps::Closed => exit(),
			}
			continue;
		}
		if stats_count > 0 && ready as usize > admin_index && ready as usize <= admin_index + stats_count {
			let which: usize = ready as usize - admin_index - 1;
			let observation: u64 = stats[which];
			match recv_blocking(observation, &mut request) {
				Received::Message { len, handle } => {
					if handle != 0 {
						close(handle);
					}
					let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
					if op == HEARTBEAT_OP {
						send_blocking(observation, b"PONG", 0);
					} else if op == CONNECT_OP {
						// AN INDEPENDENT CONNECTION, because two observers sharing one channel take
						// each other's replies - the same reason every other root in this system
						// mints rather than shares.
						match channel() {
							Some((mine, theirs)) => {
								stats.push(mine);
								send_blocking(observation, &[], theirs);
							}
							None => {
								send_blocking(observation, &[], 0);
							}
						}
					} else {
						// THE NUMBER THE LOOP ITSELF WAITS ON, passed in rather than recomputed: a
						// second answer to the same question is how an observability field comes to
						// disagree with the thing it observes.
						let mut reply_handle = proto::codec::Handles::new();
						let mut request_handle = proto::codec::Handles::new();
						let mut call = StatsCall { state: &state, waiters: waits.len() as u64 };
						if let Some(n) = display_stats::dispatch(&mut call, &request[..len], &mut request_handle, &mut reply, &mut reply_handle) {
							send_blocking(observation, &reply[..n], 0);
						}
					}
				}
				// AN OBSERVER WENT AWAY, which changes nothing about serving a display. The ROOT
				// going away is the same: what is left is the connections already minted from it.
				Received::Closed => {
					close(observation);
					stats.remove(which);
				}
			}
			continue;
		}
		// PAST THE ADMIN ROOT AND PAST EVERY OBSERVATION CHANNEL: the admin root is one slot and the
		// observation channels are `stats_count` of them, so a client's own index starts after both.
		let client_index: usize = ready as usize - admin_index - 1 - stats_count;
		// PAST THE CONNECTIONS IS A SURFACE, and a surface channel is dispatched to the surface
		// interface rather than to the connection's. Past the surfaces is a WATCHED CLIENT PROCESS.
		if client_index >= connections + surfaces_watched {
			// THE PROCESS THAT OWNS THESE SURFACES ENDED, and everything it owned goes with it. The
			// connection is closed here rather than waited for: its peer may be held by whatever
			// launched the client, and this service is not entitled to keep a surface's images
			// mapped for as long as somebody else keeps a channel open.
			let Some(&victim) = watched.get(client_index - connections - surfaces_watched) else { continue };
			let Some(client) = clients.get(victim) else { continue };
			let chan: u64 = client.chan;
			state.drop_client(chan);
			close(chan);
			let gone: Client = clients.swap_remove(victim);
			if gone.task != 0 {
				close(gone.task);
			}
			continue;
		}
		if client_index >= connections {
			let surface_index: usize = client_index - connections;
			let Some(chan) = state.surfaces.get(surface_index).map(|surface| surface.chan) else { continue };
			match recv_caps_blocking(chan, &mut request) {
				ReceivedCaps::Message { len, handles: caps } if len == 0 => {
					for &leftover in caps.as_slice() {
						close(leftover);
					}
				}
				ReceivedCaps::Message { len, handles: caps } => {
					let mut handle = caps;
					let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
					if op == surface::OP_EVENTS {
						open_events(chan, &request[..len], &mut handle, &mut state);
					} else {
						let mut reply_handle = proto::codec::Handles::new();
						let mut call = SurfaceCall { state: &mut state, chan };
						if let Some(n) = surface::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle) {
							if !send_reply(chan, &reply[..n], reply_handle.as_slice(), surface_grant(op)) {
								for &leftover in reply_handle.as_slice() {
									close(leftover);
								}
							}
						} else {
							for &leftover in reply_handle.as_slice() {
								close(leftover);
							}
						}
					}
					for &unclaimed in handle.as_slice() {
						close(unclaimed);
					}
				}
				// THE CLIENT LET GO OF ITS SURFACE. Everything it owned goes with it, including any
				// frame still in flight.
				ReceivedCaps::Closed => state.remove_surface(chan, true),
			}
			continue;
		}
		let chan: u64 = clients[client_index].chan;
		match recv_caps_blocking(chan, &mut request) {
			ReceivedCaps::Message { len, handles: caps } if len == 0 => {
				for &leftover in caps.as_slice() {
					close(leftover);
				}
				if client_index == 0 {
					exit();
				}
				state.drop_client(chan);
				close(chan);
				clients.swap_remove(client_index);
			}
			ReceivedCaps::Message { len, handles: caps } => {
				// EVERY CAPABILITY THE MESSAGE CARRIED. This was `Handles::from_slice(&[handle])`
				// over the single-handle receive, which keeps the first and drops the rest - so a
				// client sending stdin, stdout and stderr had two destroyed before dispatch.
				let mut handle = caps;
				let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
				if op == HEARTBEAT_OP {
					send_blocking(chan, b"PONG", 0);
				} else if op == CONNECT_OP && clients[client_index].task == 0 {
					match channel() {
						Some((mine, theirs)) => {
							clients.push(Client { chan: mine, task: 0 });
							send_blocking(chan, &[], theirs);
						}
						None => {
							send_blocking(chan, &[], 0);
						}
					}
				} else if op == surface::OP_EVENTS {
					open_events(chan, &request[..len], &mut handle, &mut state);
				} else if state.surface_index(chan).is_some() {
					// A SURFACE'S OWN CHANNEL, dispatched to the surface interface.
					let mut reply_handle = proto::codec::Handles::new();
					let mut call = SurfaceCall { state: &mut state, chan };
					if let Some(n) = surface::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle) {
						if !send_reply(chan, &reply[..n], reply_handle.as_slice(), surface_grant(op)) {
							for &leftover in reply_handle.as_slice() {
								close(leftover);
							}
						}
					} else {
						for &leftover in reply_handle.as_slice() {
							close(leftover);
						}
					}
				} else {
					let mut reply_handle = proto::codec::Handles::new();
					let mut call = DisplayCall { state: &mut state, chan };
					if let Some(n) = display::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle) {
						if !send_reply(chan, &reply[..n], reply_handle.as_slice(), display_grant(op)) {
							for &leftover in reply_handle.as_slice() {
								close(leftover);
							}
						}
					} else {
						for &leftover in reply_handle.as_slice() {
							close(leftover);
						}
					}
				}
				for &unclaimed in handle.as_slice() {
					close(unclaimed);
				}
			}
			ReceivedCaps::Closed => {
				if client_index == 0 {
					exit();
				}
				state.drop_client(chan);
				close(chan);
				let client: Client = clients.swap_remove(client_index);
				if client.task != 0 {
					close(client.task);
				}
			}
		}
	}
}

fn open_events(chan: u64, request: &[u8], request_handle: &mut proto::codec::Handles, state: &mut DisplayState) {
	if request.len() != 6 || !request_handle.is_empty() {
		return;
	}
	let corr: u32 = read_u32(request, 2);
	request_handle.clear();
	let (producer, consumer): (u64, u64) = match channel() {
		Some(pair) => pair,
		None => return,
	};
	// A CLIENT ONLY RECEIVES ON ITS EVENT STREAM, so that is all the endpoint carries: no `send`,
	// which would let a client forge its own configuration events, and no `transfer`, which would
	// let the stream outlive the process the surface belongs to.
	if send_blocking_attenuated(chan, &corr.to_le_bytes(), consumer, RIGHT_RECEIVE | RIGHT_WAIT) {
		state.set_event_stream(chan, producer);
	} else {
		close(producer);
		close(consumer);
	}
}

fn valid_scanout(fb: &Framebuffer, width: u32, height: u32) -> bool {
	width != 0 && height != 0 && width <= fb.width && height <= fb.height && fb.bytes_per_pixel != 0 && fb.bytes_per_pixel <= 4 && fb.pitch >= fb.width.saturating_mul(fb.bytes_per_pixel)
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

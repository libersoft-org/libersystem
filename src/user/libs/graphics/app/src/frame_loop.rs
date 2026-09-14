//! THE THIN HALF: the policy above, wired to a real surface.
//!
//! WHAT IS HERE IS ONLY WHAT NEEDS A KERNEL - acquiring, mapping, presenting, waiting on
//! PRESENT_DONE, re-reading a configuration. Every decision it takes comes from `Pacing`, which is a
//! value with no syscalls in it, so the rules are checked as fixtures rather than by booting.

use crate::pacing::{Pacing, Step};
use base_proto::generated::liber::base::v1::Error;
use graphics_core::geom::Extent2D;
use graphics_core::layout::ImageLayout;
use rt::{NANOS_PER_TICK, clock, clock_ns, wait_any};
use surface::{AcquiredImage, Client, Surface, wire_extent};

/// One acquired image, and what it is.
pub struct Frame {
	pub index: u32,
	pub layout: ImageLayout,
	pub addr: u64,
}

/// What a rebuild produced, for a caller that recreates what IT owns.
pub struct Rebuilt {
	pub extent: Extent2D,
	pub generation: u64,
}

/// A surface, a queue and the policy that paces them.
pub struct FrameLoop {
	surface: Surface,
	pacing: Pacing,
	events: u64,
	/// How many events this loop has read, and how many of them were each of the kinds a frame loop
	/// acts on. Reported rather than inferred, because "the stream said nothing", "there is no
	/// stream" and "the event was dropped by a full queue" look identical from the outside and are
	/// three different faults.
	seen: u32,
	configures: u32,
	visibility: u32,
}

impl FrameLoop {
	/// Open a surface and bring it to the state a first present is legal from.
	pub fn open(client: &Client, width: u32, height: u32, images: u32) -> Option<Result<FrameLoop, Error>> {
		let surface = match Surface::create(client, wire_extent(width, height), images)? {
			Ok(surface) => surface,
			Err(error) => return Some(Err(error)),
		};
		let mut pacing = Pacing::new(images);
		pacing.adopt(surface.configuration(), surface.image_count() as u32);
		// THE EVENT STREAM IS OPENED WITH THE SURFACE and not on the first change, because a client
		// that opened it later would have missed the changes in between - and a loop that polls
		// instead is the busy spin the stream exists to replace.
		let events = surface.events().unwrap_or(0);
		Some(Ok(FrameLoop { surface, pacing, events, seen: 0, configures: 0, visibility: 0 }))
	}

	pub fn pacing(&self) -> &Pacing {
		&self.pacing
	}

	pub fn surface(&self) -> &Surface {
		&self.surface
	}

	/// Drain whatever the service has said, WITHOUT blocking.
	pub fn poll_events(&mut self) {
		if self.events == 0 {
			return;
		}
		let mut frame: [u8; 256] = [0; 256];
		while let Some(event) = surface::try_read_event(self.events, &mut frame) {
			self.seen = self.seen.saturating_add(1);
			match event {
				display_proto::generated::liber::display::v1::SurfaceEvent::Configure(_) => self.configures = self.configures.saturating_add(1),
				display_proto::generated::liber::display::v1::SurfaceEvent::VisibilityChanged(_) => self.visibility = self.visibility.saturating_add(1),
				_ => {}
			}
			self.pacing.on_event(&event);
		}
	}

	/// What to do now.
	pub fn step(&mut self) -> Step {
		self.poll_events();
		self.pacing.step(clock_ns())
	}

	/// Take the next image, or say why there is none.
	///
	/// THE ANSWERS ARE THE PROFILE'S FOUR and each one changes the policy rather than being retried:
	/// `again` means wait for `image-available`, `not-visible` means throttle, `out-of-date` means
	/// rebuild. A loop that treated any of them as "ask again" would spin against a service that is
	/// deliberately not blocking.
	pub fn acquire(&mut self) -> Option<Frame> {
		match self.surface.acquire() {
			Some(Ok(AcquiredImage::Image(index))) => {
				let mapping = self.surface.image(index)?;
				Some(Frame { index, layout: mapping.layout(), addr: mapping.addr() })
			}
			Some(Ok(AcquiredImage::Again)) => {
				self.pacing.on_again();
				None
			}
			Some(Ok(AcquiredImage::NotVisible)) => {
				self.pacing.on_not_visible();
				None
			}
			Some(Ok(AcquiredImage::OutOfDate)) => {
				self.pacing.on_out_of_date();
				None
			}
			_ => None,
		}
	}

	/// Give an acquired image back without presenting it.
	///
	/// THE EDGE AN APPLICATION FORGETS, and the loop's own reason for it: a resize that arrives
	/// between the acquire and the draw makes the frame one nobody wanted, and returning the image
	/// is what stops a resized window leaking one per resize.
	pub fn abandon(&mut self, frame: Frame) {
		let _ = self.surface.abandon(frame.index);
	}

	/// Whether the loop has been asked to close. A REQUEST AND NEVER A TEARDOWN: the application
	/// decides, which is what makes an unsaved-changes prompt possible.
	pub fn close_requested(&self) -> bool {
		self.pacing.close_requested()
	}

	/// Present a drawn image WHOLE, which is what the first frame of a generation must be.
	pub fn present_whole(&mut self, frame: Frame) -> bool {
		// THE RENDER IS COMPLETE, SAID ON PRODUCER_READY. There are two completion pairs, one per
		// direction, and this is the one the client owns: the present that follows says the same
		// thing and carries damage with it, so the signal is what a client sends when it wants the
		// fact on the wire without waiting for a call to return.
		surface::signal_ready(self.surface.producer_endpoint(), frame.index);
		match self.surface.present_whole(frame.index) {
			Some(Ok(serial)) => {
				self.pacing.on_present(serial, clock_ns());
				true
			}
			_ => false,
		}
	}

	/// Present a drawn image's damaged rectangles.
	pub fn present_rects(&mut self, frame: Frame, rects: &[surface::Rect]) -> bool {
		surface::signal_ready(self.surface.producer_endpoint(), frame.index);
		match self.surface.present_rects(frame.index, rects) {
			Some(Ok(serial)) => {
				self.pacing.on_present(serial, clock_ns());
				true
			}
			_ => false,
		}
	}

	/// Re-read the configuration, rebuild the queue for it, and say what the caller must recreate.
	///
	/// THE LOOP REACQUIRES THE SURFACE'S IMAGES AND THE CALLER RECREATES WHAT IT OWNS, which is the
	/// split this whole helper exists for: a renderer that had to know about a present queue to
	/// survive a resize is a renderer with a window system inside it.
	pub fn rebuild(&mut self) -> Option<Result<Rebuilt, Error>> {
		if let Err(error) = self.surface.rebuild()? {
			return Some(Err(error));
		}
		let configuration = self.surface.configuration().clone();
		self.pacing.adopt(&configuration, self.surface.image_count() as u32);
		Some(Ok(Rebuilt { extent: Extent2D::new(configuration.physical_extent.width, configuration.physical_extent.height), generation: configuration.generation }))
	}

	/// Wait until something this loop is waiting on can move.
	///
	/// NEVER A SPIN AND NEVER AN UNBOUNDED BLOCK. What it waits on is the completion endpoint and
	/// the event stream, with the deadline the policy chose - so a loop with nothing to do costs
	/// nothing, and one whose backend has gone still wakes.
	/// THE KERNEL'S WAIT IS IN TICKS AND THE TIMING CONTRACT IS IN NANOSECONDS.
	///
	/// Mixing the two is what a loop that never wakes up looks like: a nanosecond value passed where
	/// an absolute tick deadline belongs is a wait of about four months. The conversion is here,
	/// once, at the one place the two units meet - and it ROUNDS UP, because a wait one tick short
	/// wakes early, finds the deadline has not passed, and waits again, which is a spin with extra
	/// steps.
	fn tick_deadline(until: Option<u64>) -> u64 {
		// ZERO MEANS "NO DEADLINE" TO THE KERNEL, which is exactly right for a wait with none.
		let Some(until) = until else { return 0 };
		let now = clock_ns();
		if until <= now {
			// ALREADY DUE, and zero would mean the opposite - so ask for the next tick.
			return clock().saturating_add(1);
		}
		clock().saturating_add(((until - now) / NANOS_PER_TICK).saturating_add(1))
	}

	pub fn park(&mut self, until: Option<u64>) {
		let mut handles: [u64; 2] = [0; 2];
		let mut count = 0usize;
		for handle in [self.surface.done_endpoint(), self.events] {
			if handle != 0 {
				handles[count] = handle;
				count += 1;
			}
		}
		if count == 0 {
			return;
		}
		let _ = wait_any(&handles[..count], Self::tick_deadline(until));
		self.poll_events();
		// EVERY RELEASE THE COMPLETION ENDPOINT CARRIED, drained without blocking: it is the pair a
		// frame loop waits on WITHOUT a dispatch, and a loop that read one per wake would fall
		// behind a service that settled two.
		let mut frame: [u8; 32] = [0; 32];
		while let Some((serial, _image)) = surface::try_read_release(self.surface.done_endpoint(), &mut frame) {
			self.pacing.on_release(serial);
		}
	}

	/// The live event stream's handle, for a caller that waits on more than this surface.
	pub fn event_stream(&self) -> u64 {
		self.events
	}

	/// How many events this loop has read.
	pub fn events_seen(&self) -> u32 {
		self.seen
	}

	pub fn configures_seen(&self) -> u32 {
		self.configures
	}

	pub fn visibility_seen(&self) -> u32 {
		self.visibility
	}
}

impl Drop for FrameLoop {
	fn drop(&mut self) {
		if self.events != 0 {
			rt::close(self.events);
		}
	}
}

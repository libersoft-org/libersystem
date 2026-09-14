//! WHAT A FRAME LOOP DECIDES, with nothing in it that needs a kernel to decide.
//!
//! Every rule the integration gate names is here as a function of state and a clock reading, so each
//! one is a fixture rather than something a reader has to boot a machine to believe.

use alloc::vec::Vec;
use display_proto::generated::liber::display::v1::{PresentComplete, PresentOutcome, SurfaceConfiguration, SurfaceEvent};

/// WHAT THE BACKEND CAN SAY ABOUT WHEN THE NEXT FRAME IS DUE.
///
/// EVERY FIELD IS OPTIONAL BECAUSE THE BACKEND MAY NOT KNOW. An application that slept on a
/// hard-coded sixteen milliseconds is an application that is wrong on every display that is not
/// sixty hertz, and one that BUSY-SPINS because the backend said nothing is worse: it is wrong on
/// every display and it costs a core.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timing {
	/// When the next frame ought to be ready, in monotonic nanoseconds.
	pub preferred_deadline: Option<u64>,
	/// The output's refresh interval in nanoseconds.
	pub refresh_interval: Option<u64>,
}

/// WHAT THE LOOP SHOULD DO NEXT.
///
/// THERE IS NO "SPIN" ANSWER, which is the point: every state that is not `Draw` names something to
/// WAIT ON, and a loop that cannot draw waits on an event or a deadline rather than asking again
/// immediately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
	/// Acquire an image and render into it.
	Draw,
	/// Every image this loop is allowed is with the service. Wait for a completion on PRESENT_DONE.
	AwaitCompletion,
	/// The configuration moved on. Re-read it, rebuild what depends on the extent, acknowledge, and
	/// supply a new set of images before drawing again.
	Rebuild,
	/// Nothing to draw yet. `until` is a monotonic deadline to wait until, or `None` for "wait for
	/// an event with no deadline of your own" - which is what a backend that reports no timing
	/// leaves a client with, and is still not a spin.
	Idle { until: Option<u64> },
}

/// The policy half of a frame loop.
#[derive(Clone, Debug)]
pub struct Pacing {
	/// The most frames that may be with the service at once - the NEGOTIATED image count, because a
	/// loop that kept more in flight than it has images would be waiting for an image it already
	/// holds.
	limit: usize,
	/// The serials of presents this loop has had ACCEPTED and not yet seen settle.
	///
	/// A LIST OF SERIALS AND NOT A COUNT, because the same present settles by TWO routes: the
	/// completion event a dispatching loop reads, and the release on PRESENT_DONE a loop that does
	/// not dispatch waits on. A counter would be decremented twice by a loop that reads both, and a
	/// loop that believed it had more images than it does presents into one the service is reading.
	/// Settling BY SERIAL is idempotent, which is what makes reading both safe.
	pending: Vec<u64>,
	/// The generation the images belong to. A configuration whose generation moved makes every one
	/// of them stale, and no image ever crosses a generation.
	generation: u64,
	/// The serial this loop has acknowledged, and the serial the service last announced. A present
	/// names the acknowledged one, so while they differ there is nothing legal to draw.
	acknowledged: u64,
	announced: u64,
	visible: bool,
	focused: bool,
	timing: Timing,
	/// When the loop may next draw, in monotonic nanoseconds. Zero means "now".
	next_frame: u64,
	/// What the last completed present became, for a caller that reports or reacts.
	last_outcome: Option<PresentOutcome>,
	/// Whether the service has said an image is available since the last time this loop was told
	/// there was none. The event exists precisely so a client waits for it rather than polling.
	image_available: bool,
	close_requested: bool,
}

/// HOW LONG TO THROTTLE A BACKGROUND LOOP.
///
/// A HIDDEN CLIENT MUST NOT BE BUSY AND MUST NOT BE DEAD. Frames it accepts while hidden are
/// discarded in order and never reach a screen, so drawing them is work thrown away - but a loop
/// that stopped entirely would not notice becoming visible again on a backend whose event it missed.
/// A quarter of a second is slow enough to cost nothing and fast enough that a person does not see
/// it.
pub const BACKGROUND_INTERVAL_NS: u64 = 250_000_000;

/// WHAT TO WAIT WHEN THE BACKEND SAYS NOTHING AT ALL.
///
/// Not a refresh rate and not pretending to be one: it is the interval at which a loop with no
/// timing contract wakes to see whether anything changed. A backend that reports a real deadline
/// overrides it, which is the whole reason the timing fields are optional rather than defaulted.
pub const UNPACED_INTERVAL_NS: u64 = 16_000_000;

impl Pacing {
	/// A loop that has a queue of `images` and has not yet seen a configuration.
	pub fn new(images: u32) -> Pacing {
		Pacing {
			// A QUEUE OF NONE IS NOT A QUEUE. Clamped at one so `AwaitCompletion` cannot be the
			// answer forever on a service that negotiated nothing.
			limit: (images as usize).max(1),
			pending: Vec::new(),
			generation: 0,
			acknowledged: u64::MAX,
			announced: u64::MAX,
			visible: false,
			focused: false,
			timing: Timing::default(),
			next_frame: 0,
			last_outcome: None,
			image_available: true,
			close_requested: false,
		}
	}

	/// Adopt a configuration this loop has rebuilt for and acknowledged.
	///
	/// THE ACKNOWLEDGEMENT IS WHAT MAKES A FRAME LEGAL, so it is recorded here rather than inferred:
	/// a loop that presented against a serial it had not acknowledged would have every frame refused.
	pub fn adopt(&mut self, configuration: &SurfaceConfiguration, images: u32) {
		self.generation = configuration.generation;
		self.acknowledged = configuration.serial;
		self.announced = configuration.serial;
		self.visible = configuration.visible;
		self.focused = configuration.focused;
		self.limit = (images as usize).max(1);
		// A REBUILD REPLACES EVERY IMAGE, so nothing this loop believed was in flight is any more:
		// the frames that were went with the generation they belonged to.
		self.pending.clear();
		self.image_available = true;
		// AND THE FIRST FRAME OF A NEW GENERATION IS DUE NOW. The window has nothing on it at the
		// new size, so pacing it behind the deadline the OLD generation set would leave a resized
		// window blank for a frame nobody is waiting for.
		self.next_frame = 0;
	}

	/// What the service said, folded in.
	pub fn on_event(&mut self, event: &SurfaceEvent) {
		match event {
			// THE SNAPSHOT IS NOT ADOPTED HERE. A configuration is adopted when the loop has
			// REBUILT for it and acknowledged it, which is the caller's work; what this records is
			// that one arrived, so `step` answers `Rebuild` until it has been.
			SurfaceEvent::Configure(configuration) => {
				self.announced = configuration.serial;
				self.visible = configuration.visible;
				self.focused = configuration.focused;
			}
			SurfaceEvent::ImageAvailable => self.image_available = true,
			SurfaceEvent::PresentComplete(complete) => self.on_completion(complete),
			SurfaceEvent::CloseRequested => self.close_requested = true,
			SurfaceEvent::VisibilityChanged(visible) => self.visible = *visible,
			SurfaceEvent::FocusChanged(focused) => self.focused = *focused,
		}
	}

	/// A present this loop made was accepted, under the serial the service answered with.
	pub fn on_present(&mut self, serial: u64, now: u64) {
		// A RESERVATION THAT FAILS IS A FRAME THIS LOOP FORGETS IT SENT, which would let it run
		// ahead of the queue - so the slot is booked and a short heap paces the loop to nothing
		// rather than past its limit.
		if self.pending.try_reserve(1).is_ok() {
			self.pending.push(serial);
		}
		// AND THE NEXT FRAME IS DUE WHEN THE BACKEND SAYS, which is what stops a loop that can draw
		// from drawing as fast as the machine allows and calling that a frame rate.
		self.next_frame = match (self.timing.preferred_deadline, self.timing.refresh_interval) {
			(Some(deadline), _) => deadline,
			(None, Some(interval)) => now.saturating_add(interval),
			(None, None) => now.saturating_add(UNPACED_INTERVAL_NS),
		};
	}

	/// One present settled, with the outcome and the timing it carried.
	pub fn on_completion(&mut self, complete: &PresentComplete) {
		self.settle(complete.serial);
		self.last_outcome = Some(complete.outcome);
		self.timing = Timing { preferred_deadline: complete.timing.preferred_deadline, refresh_interval: complete.timing.refresh_interval };
	}

	/// PRESENT_DONE carried the release of one present: the same fact by the other route.
	///
	/// THERE ARE TWO COMPLETION PAIRS AND A LOOP MAY READ EITHER OR BOTH. Settling by SERIAL is what
	/// makes reading both safe: the second route finds the serial already gone and changes nothing.
	pub fn on_release(&mut self, serial: u64) {
		self.settle(serial);
	}

	/// Drop one accepted present, whichever route said so. Idempotent by construction.
	fn settle(&mut self, serial: u64) {
		if let Some(at) = self.pending.iter().position(|pending| *pending == serial) {
			self.pending.remove(at);
			// AN IMAGE CAME BACK WITH IT. A settled present releases the image it consumed, which is
			// the half a loop waiting on PRESENT_DONE is waiting for.
			self.image_available = true;
		}
	}

	/// The acquire answered `again`: the queue had nothing to give.
	///
	/// RECORDED RATHER THAN RETRIED. `again` means "wait for `image-available`", and a loop that
	/// asked again immediately would be the busy spin the non-blocking acquire exists to avoid.
	pub fn on_again(&mut self) {
		self.image_available = false;
	}

	pub fn on_out_of_date(&mut self) {
		self.announced = self.acknowledged.wrapping_add(1);
	}

	pub fn on_not_visible(&mut self) {
		self.visible = false;
	}

	/// What to do at `now`, in monotonic nanoseconds.
	pub fn step(&self, now: u64) -> Step {
		// A CONFIGURATION THAT MOVED COMES FIRST. Everything else is about frames, and there is no
		// legal frame for a configuration this loop has not acknowledged.
		if self.announced != self.acknowledged {
			return Step::Rebuild;
		}
		// A HIDDEN LOOP THROTTLES RATHER THAN DRAWS. Frames accepted while hidden are discarded in
		// order and never reach a screen, so drawing them is work thrown away.
		if !self.visible {
			return Step::Idle { until: Some(now.saturating_add(BACKGROUND_INTERVAL_NS)) };
		}
		// AT MOST THE NEGOTIATED FRAMES IN FLIGHT. More than that is a loop waiting for an image it
		// is itself holding.
		if self.pending.len() >= self.limit {
			return Step::AwaitCompletion;
		}
		if !self.image_available {
			return Step::AwaitCompletion;
		}
		if now < self.next_frame {
			return Step::Idle { until: Some(self.next_frame) };
		}
		Step::Draw
	}

	pub fn in_flight(&self) -> usize {
		self.pending.len()
	}

	pub fn limit(&self) -> usize {
		self.limit
	}

	pub fn generation(&self) -> u64 {
		self.generation
	}

	pub fn acknowledged(&self) -> u64 {
		self.acknowledged
	}

	pub fn visible(&self) -> bool {
		self.visible
	}

	pub fn focused(&self) -> bool {
		self.focused
	}

	pub fn timing(&self) -> Timing {
		self.timing
	}

	pub fn last_outcome(&self) -> Option<PresentOutcome> {
		self.last_outcome
	}

	/// The application was ASKED to close, which is a request and never a teardown: it is what makes
	/// an unsaved-changes prompt possible.
	pub fn close_requested(&self) -> bool {
		self.close_requested
	}
}

//! EVERY RULE THE FRAME LOOP OWES, AS A FIXTURE.
//!
//! The policy is a value, so each of these is arithmetic over state rather than something a reader
//! has to boot a machine to believe. What is NOT here is the syscall half - acquiring, mapping,
//! presenting - which is exercised against a real DisplayService in the guest.

use crate::pacing::{BACKGROUND_INTERVAL_NS, Pacing, Step, UNPACED_INTERVAL_NS};
use display_proto::generated::liber::display::v1::{FrameTiming, OutputColour, OutputTransform, PresentComplete, PresentOutcome, ScaleRatio, SubpixelLayout, SurfaceConfiguration, SurfaceEvent, TimestampEvidence};
use display_proto::generated::liber::graphics::v1::{ColorSpace, Extent2d, PixelFormat};

fn configuration(serial: u64, generation: u64, width: u32, height: u32, visible: bool) -> SurfaceConfiguration {
	SurfaceConfiguration { serial, generation, logical_extent: Extent2d { width, height }, physical_extent: Extent2d { width, height }, scale: ScaleRatio { numerator: 1, denominator: 1 }, transform: OutputTransform::Normal, output: 0, format: PixelFormat::B8g8r8x8Unorm, colour: OutputColour { space: ColorSpace::Srgb, sdr_white_nits: None, min_nits: None, max_nits: None, max_frame_average_nits: None }, subpixel: SubpixelLayout::Unknown, visible, focused: visible }
}

// A BACKEND THAT CAN SAY NOTHING ABOUT TIMING, which is the one this tree has: the current path
// acknowledges a transfer and a flush and observes no vblank at all.
fn unpaced() -> FrameTiming {
	FrameTiming { preferred_deadline: None, refresh_interval: None }
}

fn completion(serial: u64, outcome: PresentOutcome, timing: FrameTiming) -> PresentComplete {
	PresentComplete { serial, outcome, evidence: TimestampEvidence::Unavailable, timing }
}

fn adopted(images: u32) -> Pacing {
	let mut pacing = Pacing::new(images);
	pacing.adopt(&configuration(1, 1, 64, 64, true), images);
	pacing
}

#[test]
fn a_loop_keeps_at_most_the_negotiated_frames_in_flight() {
	// THE BOUND IS THE QUEUE'S OWN COUNT. A loop that kept more in flight than it has images would
	// be waiting for an image it is itself holding, which is a deadlock it wrote for itself.
	let mut pacing = adopted(2);
	assert_eq!(pacing.step(0), Step::Draw, "a fresh queue has something to draw into");
	pacing.on_present(1, 0);
	assert_eq!(pacing.in_flight(), 1);
	assert_eq!(pacing.step(0), Step::Idle { until: Some(UNPACED_INTERVAL_NS) }, "a backend that reports no timing still paces");
	pacing.on_present(2, 0);
	assert_eq!(pacing.in_flight(), 2);
	// AND NOW THE LIMIT IS WHAT ANSWERS, at any time at all - a deadline that has passed does not
	// make an image appear.
	assert_eq!(pacing.step(u64::MAX), Step::AwaitCompletion, "both images are with the service");
	pacing.on_completion(&completion(1, PresentOutcome::Displayed, unpaced()));
	assert_eq!(pacing.in_flight(), 1, "one came back");
	assert_eq!(pacing.step(u64::MAX), Step::Draw, "and the loop may draw again");
}

#[test]
fn the_same_present_settling_twice_does_not_release_two_images() {
	// THERE ARE TWO COMPLETION PAIRS AND A LOOP MAY READ BOTH: the event carries the outcome, and
	// PRESENT_DONE carries the release a loop waits on WITHOUT a dispatch. A counter would be
	// decremented twice by a loop that reads both, and a loop that believed it had more images than
	// it does presents into one the service is still reading.
	let mut pacing = adopted(2);
	pacing.on_present(7, 0);
	pacing.on_present(8, 0);
	assert_eq!(pacing.in_flight(), 2);
	pacing.on_release(7);
	pacing.on_completion(&completion(7, PresentOutcome::Displayed, unpaced()));
	assert_eq!(pacing.in_flight(), 1, "the same present settled by both routes is one present");
	// AND A SERIAL THIS LOOP NEVER SENT CHANGES NOTHING, which is what an idempotent settle means.
	pacing.on_release(999);
	assert_eq!(pacing.in_flight(), 1);
}

#[test]
fn a_loop_paces_against_the_timing_contract_rather_than_spinning() {
	// AN APPLICATION THAT SLEPT ON SIXTEEN MILLISECONDS IS WRONG ON EVERY DISPLAY THAT IS NOT SIXTY
	// HERTZ, and one that busy-spins because the backend said nothing is wrong on all of them and
	// costs a core. So the backend's own numbers win where it has them.
	let mut pacing = adopted(3);
	pacing.on_present(1, 0);
	pacing.on_completion(&completion(1, PresentOutcome::Displayed, FrameTiming { preferred_deadline: None, refresh_interval: Some(8_000_000) }));
	pacing.on_present(2, 1_000);
	assert_eq!(pacing.step(1_000), Step::Idle { until: Some(8_001_000) }, "the refresh interval sets the next frame");
	assert_eq!(pacing.step(8_001_000), Step::Draw, "and the loop draws when it arrives and not before");

	// A DEADLINE THE BACKEND NAMED IS THE ANSWER, not a deadline computed from an interval: the two
	// disagree whenever a frame was late, and the one that matters is when the NEXT frame is due.
	pacing.on_completion(&completion(2, PresentOutcome::Displayed, FrameTiming { preferred_deadline: Some(50_000_000), refresh_interval: Some(8_000_000) }));
	pacing.on_present(3, 8_001_000);
	assert_eq!(pacing.step(8_001_000), Step::Idle { until: Some(50_000_000) }, "the named deadline wins over the interval");

	// AND BEFORE THE DEADLINE THERE IS NO ANSWER THAT MEANS "ASK AGAIN IMMEDIATELY".
	for now in [0, 1, 49_999_999] {
		assert_eq!(pacing.step(now), Step::Idle { until: Some(50_000_000) }, "a loop before its deadline waits rather than spinning");
	}
}

#[test]
fn a_background_loop_throttles_and_does_not_draw() {
	// FRAMES ACCEPTED WHILE HIDDEN ARE DISCARDED IN ORDER AND NEVER REACH A SCREEN, so drawing them
	// is work thrown away - and a loop that stopped entirely would not notice becoming visible again.
	let mut pacing = adopted(2);
	pacing.on_event(&SurfaceEvent::VisibilityChanged(false));
	assert_eq!(pacing.step(1_000), Step::Idle { until: Some(1_000 + BACKGROUND_INTERVAL_NS) }, "a hidden loop waits rather than drawing");
	assert!(!pacing.visible());
	// A SERVICE THAT ANSWERS `not-visible` SAYS THE SAME THING, and the loop reaches the same state
	// whether it learned it from an event or from an acquire.
	let mut told = adopted(2);
	told.on_not_visible();
	assert_eq!(told.step(0), Step::Idle { until: Some(BACKGROUND_INTERVAL_NS) });
	// AND IT COMES BACK. Visibility is an event, so nothing has to be polled for it.
	told.on_event(&SurfaceEvent::VisibilityChanged(true));
	assert_eq!(told.step(0), Step::Draw, "a loop that becomes visible again draws again");
}

#[test]
fn a_configuration_that_moved_is_a_rebuild_and_not_a_frame() {
	// NO IMAGE EVER CROSSES A GENERATION and no frame is legal against a serial this loop has not
	// acknowledged, so a configuration that arrived is the ONLY thing the loop may act on until it
	// has rebuilt - ahead of the limit, ahead of the deadline, ahead of everything.
	let mut pacing = adopted(2);
	pacing.on_present(1, 0);
	pacing.on_present(2, 0);
	assert_eq!(pacing.step(0), Step::AwaitCompletion);
	pacing.on_event(&SurfaceEvent::Configure(configuration(2, 2, 128, 128, true)));
	assert_eq!(pacing.step(0), Step::Rebuild, "a new configuration comes before a frame");
	assert_eq!(pacing.step(u64::MAX), Step::Rebuild, "and no amount of waiting makes it a frame");
	// AN `out-of-date` ACQUIRE SAYS THE SAME THING by the other route.
	let mut told = adopted(2);
	told.on_out_of_date();
	assert_eq!(told.step(0), Step::Rebuild);

	// AND A REBUILD FORGETS EVERY FRAME THAT WAS IN FLIGHT, because the images they were drawn into
	// went with the generation they belonged to. A loop that carried the count across would spend
	// the new generation waiting for completions that can never arrive.
	pacing.adopt(&configuration(2, 2, 128, 128, true), 2);
	assert_eq!(pacing.in_flight(), 0, "the old generation's frames are not the new one's");
	assert_eq!(pacing.generation(), 2);
	assert_eq!(pacing.acknowledged(), 2);
	assert_eq!(pacing.step(0), Step::Draw);
}

#[test]
fn an_acquire_that_answered_again_waits_for_the_event_rather_than_asking_again() {
	// `again` MEANS "WAIT FOR `image-available`", and a loop that asked again immediately would be
	// the busy spin the non-blocking acquire exists to avoid - against a service that is deliberately
	// not blocking, which makes it a spin that costs a core and never ends on its own.
	let mut pacing = adopted(3);
	pacing.on_again();
	assert_eq!(pacing.step(0), Step::AwaitCompletion, "a queue with nothing to give is waited on");
	pacing.on_event(&SurfaceEvent::ImageAvailable);
	assert_eq!(pacing.step(0), Step::Draw, "and the event is what ends the wait");
}

#[test]
fn a_queue_of_none_is_still_a_queue_that_can_answer() {
	// A SERVICE THAT NEGOTIATED NOTHING WOULD OTHERWISE LEAVE A LOOP ANSWERING `AwaitCompletion` FOR
	// EVER, waiting for a completion of a present it could never make.
	let mut pacing = Pacing::new(0);
	pacing.adopt(&configuration(1, 1, 8, 8, true), 0);
	assert_eq!(pacing.limit(), 1);
	assert_eq!(pacing.step(0), Step::Draw);
}

#[test]
fn every_outcome_and_the_close_request_reach_the_application() {
	// THE FOUR OUTCOMES ARE NOT ONE. A background client's frame that was accepted and discarded is
	// a different fact from one the driver lost, and an application that reports its own frame count
	// needs to be able to say which - which a bare completion cannot carry.
	let mut pacing = adopted(3);
	for (serial, outcome) in [(1u64, PresentOutcome::Displayed), (2, PresentOutcome::DiscardedOccluded), (3, PresentOutcome::ReplacedByResize)] {
		pacing.on_present(serial, 0);
		pacing.on_completion(&completion(serial, outcome, unpaced()));
		assert_eq!(pacing.last_outcome(), Some(outcome));
	}
	pacing.on_present(4, 0);
	pacing.on_completion(&completion(4, PresentOutcome::DriverLost, unpaced()));
	assert_eq!(pacing.last_outcome(), Some(PresentOutcome::DriverLost), "a backend that went away is not a frame that was shown");

	// A CLOSE IS A REQUEST AND NEVER A TEARDOWN, which is what makes an unsaved-changes prompt
	// possible: the application decides, and the loop's job is to have told it.
	assert!(!pacing.close_requested());
	pacing.on_event(&SurfaceEvent::CloseRequested);
	assert!(pacing.close_requested(), "the application is told and decides");
	assert_eq!(pacing.step(UNPACED_INTERVAL_NS), Step::Draw, "and is still drawing until it does");
}

#[test]
fn focus_follows_the_surface_and_is_not_visibility() {
	// TWO EVENTS AND NOT ONE: visibility says whether to keep drawing and focus says where input
	// goes. A loop that folded them together would stop drawing a visible window that lost focus.
	let mut pacing = adopted(2);
	assert!(pacing.visible() && pacing.focused());
	pacing.on_event(&SurfaceEvent::FocusChanged(false));
	assert!(pacing.visible(), "losing focus is not becoming hidden");
	assert!(!pacing.focused());
	assert_eq!(pacing.step(0), Step::Draw, "and a visible window without focus still draws");
}

#[test]
fn fallback_counts_rendering_and_blocking_present_inside_the_interval() {
	let mut pacing = adopted(1);
	pacing.on_present_started(1, 5_000_000, Some(0));
	assert_eq!(pacing.step(5_000_000), Step::AwaitCompletion, "a due time cannot release an image");
	pacing.on_release(1);
	for now in [5_000_000, 8_000_000, 15_999_999] {
		pacing.on_event(&SurfaceEvent::ImageAvailable);
		assert_eq!(pacing.step(now), Step::Idle { until: Some(16_000_000) }, "early events do not restart or finish the deadline");
	}
	assert_eq!(pacing.step(16_000_000), Step::Draw);
	pacing.on_present_started(2, 56_000_000, Some(16_000_000));
	assert_eq!(pacing.step(56_000_000), Step::AwaitCompletion);
	pacing.on_release(2);
	assert_eq!(pacing.step(56_000_000), Step::Draw, "forty milliseconds of useful frame work need no extra sixteen");
	pacing.on_present_started(3, 61_000_000, Some(56_000_000));
	pacing.on_release(3);
	assert_eq!(pacing.step(61_000_000), Step::Idle { until: Some(72_000_000) }, "a subsequent cheap frame waits again, without catching up");
}

#[test]
fn a_late_wake_starts_a_new_cadence_and_does_not_catch_up() {
	let mut pacing = adopted(2);
	pacing.on_present_started(1, 90_000_000, Some(84_000_000));
	pacing.on_release(1);
	assert_eq!(pacing.step(120_000_000), Step::Draw);
	pacing.on_present_started(2, 125_000_000, Some(120_000_000));
	pacing.on_release(2);
	assert_eq!(pacing.step(125_000_000), Step::Idle { until: Some(136_000_000) });
}

#[test]
fn acquired_images_keep_independent_starts_when_presented_out_of_order() {
	use crate::frame_starts::FrameStarts;
	let mut starts = FrameStarts::new(2, 1).unwrap();
	let mut pacing = adopted(2);
	starts.step(Step::Draw, 100_000_000);
	let a = starts.begin_acquire();
	starts.acquired(0, 0x1000, a);
	starts.step(Step::Draw, 110_000_000);
	let b = starts.begin_acquire();
	starts.acquired(1, 0x2000, b);
	assert_eq!(starts.take(0, 0x9999, 1), None, "a different mapping does not spend A's start");
	pacing.on_present_started(2, 115_000_000, starts.take(1, 0x2000, 1));
	pacing.on_present_started(1, 116_000_000, starts.take(0, 0x1000, 1));
	assert_eq!(pacing.step(116_000_000), Step::AwaitCompletion);
	pacing.on_release(2);
	assert_eq!(pacing.step(116_000_000), Step::Idle { until: Some(126_000_000) }, "A's earlier start does not shorten B's cadence");
	assert_eq!(starts.take(0, 0x1000, 1), None);
	assert_eq!(starts.take(1, 0x2000, 1), None);
}

#[test]
fn a_direct_acquire_deadline_is_not_shortened_by_an_older_started_image() {
	use crate::frame_starts::FrameStarts;
	let mut starts = FrameStarts::new(2, 1).unwrap();
	let mut pacing = adopted(2);
	starts.step(Step::Draw, 0);
	let a = starts.begin_acquire();
	starts.acquired(0, 0x1000, a);
	let b = starts.begin_acquire();
	assert_eq!(b, None, "a direct acquire has no invented frame-start time");
	starts.acquired(1, 0x2000, b);
	pacing.on_present_started(2, 15_000_000, starts.take(1, 0x2000, 1));
	pacing.on_present_started(1, 20_000_000, starts.take(0, 0x1000, 1));
	pacing.on_release(2);
	assert_eq!(pacing.step(20_000_000), Step::Idle { until: Some(31_000_000) });
	// The public entry point still chooses exactly now + interval, even when called directly
	// with a time older than a prior test frame; it does not inherit the private max guard.
	pacing.on_release(1);
	pacing.on_present(3, 1_000_000);
	assert_eq!(pacing.step(1_000_000), Step::Idle { until: Some(17_000_000) });
}

#[test]
fn repeated_draw_queries_and_refused_acquires_do_not_move_work_to_another_image() {
	use crate::frame_starts::FrameStarts;
	let mut starts = FrameStarts::new(2, 1).unwrap();
	starts.step(Step::Draw, 10);
	starts.step(Step::Draw, 20);
	assert_eq!(starts.begin_acquire(), Some(10), "querying Draw twice does not erase work already begun");
	// That attempt was refused: no acquired slot is assigned.
	assert_eq!(starts.begin_acquire(), None);
	starts.step(Step::AwaitCompletion, 30);
	starts.step(Step::Draw, 40);
	let next = starts.begin_acquire();
	starts.acquired(1, 0x2000, next);
	assert_eq!(starts.take(1, 0x2000, 1), Some(40));
	starts.step(Step::Draw, 50);
	starts.step(Step::Idle { until: Some(80) }, 60);
	assert_eq!(starts.begin_acquire(), None, "a non-Draw decision cancels unacquired work");
}

#[test]
fn abandoning_or_failing_one_present_consumes_only_that_images_start() {
	use crate::frame_starts::FrameStarts;
	let mut starts = FrameStarts::new(3, 1).unwrap();
	for (index, addr, time) in [(0, 0x1000, 10), (1, 0x2000, 20)] {
		starts.step(Step::Draw, time);
		let began = starts.begin_acquire();
		starts.acquired(index, addr, began);
	}
	starts.step(Step::Draw, 30);
	assert_eq!(starts.take(0, 0x1000, 1), Some(10), "abandon consumes A");
	assert_eq!(starts.take(0, 0x1000, 1), None);
	assert_eq!(starts.take(1, 0x2000, 1), Some(20), "a present takes B before its RPC can fail");
	assert_eq!(starts.take(1, 0x2000, 1), None, "failure cannot reuse B's timestamp");
	let c = starts.begin_acquire();
	assert_eq!(c, Some(30), "neither operation consumes an unrelated pending Draw");
	starts.acquired(2, 0x3000, c);
	assert_eq!(starts.take(2, 0x3000, 1), Some(30));
}

#[test]
fn hidden_and_rebuild_transitions_do_not_leave_a_pending_start() {
	use crate::frame_starts::FrameStarts;
	let mut starts = FrameStarts::new(2, 1).unwrap();
	let mut pacing = adopted(2);
	starts.step(Step::Draw, 10);
	let began = starts.begin_acquire();
	starts.acquired(0, 0x1000, began);
	starts.step(Step::Draw, 20);
	// These are the same adapter operations as a false visibility event, even if a true event
	// follows before the application next asks step(). An already acquired image stays its own.
	starts.cancel_pending();
	pacing.on_event(&SurfaceEvent::VisibilityChanged(false));
	assert_eq!(starts.begin_acquire(), None);
	pacing.on_present_started(1, 30, starts.take(0, 0x1000, 1));
	assert_eq!(pacing.step(30), Step::Idle { until: Some(30 + BACKGROUND_INTERVAL_NS) });
	pacing.on_release(1);
	pacing.on_event(&SurfaceEvent::VisibilityChanged(true));
	starts.step(Step::Draw, 40);
	let began = starts.begin_acquire();
	starts.acquired(0, 0x1000, began);
	starts.step(Step::Draw, 50);
	starts.invalidate();
	assert_eq!(starts.take(0, 0x1000, 1), None, "even a failed rebuild discarded old mapping metadata");
	assert_eq!(starts.begin_acquire(), None);
	starts.reset(3, 2).unwrap();
	pacing.adopt(&configuration(2, 2, 128, 128, true), 3);
	assert_eq!(pacing.step(0), Step::Draw, "the new generation is immediately drawable");
	starts.step(Step::Draw, 100);
	let began = starts.begin_acquire();
	starts.acquired(0, 0x1000, began);
	assert_eq!(starts.take(0, 0x1000, 1), None, "an old generation cannot consume fresh metadata");
	assert_eq!(starts.take(0, 0x1000, 2), Some(100));
}

#[test]
fn real_timing_replaces_the_frame_start_fallback_without_leaking_a_deadline() {
	let mut pacing = adopted(3);
	pacing.on_present_started(1, 5_000_000, Some(0));
	pacing.on_completion(&completion(1, PresentOutcome::Displayed, FrameTiming { preferred_deadline: Some(500_000_000), refresh_interval: Some(8_000_000) }));
	pacing.on_present_started(2, 10_000_000, Some(6_000_000));
	assert_eq!(pacing.step(10_000_000), Step::Idle { until: Some(500_000_000) });
	pacing.on_completion(&completion(2, PresentOutcome::Displayed, unpaced()));
	pacing.on_present_started(3, 15_000_000, Some(11_000_000));
	assert_eq!(pacing.step(15_000_000), Step::Idle { until: Some(27_000_000) }, "an obsolete real deadline never enters the fallback guard");
	pacing.on_completion(&completion(3, PresentOutcome::Displayed, FrameTiming { preferred_deadline: None, refresh_interval: Some(8_000_000) }));
	pacing.on_present_started(4, 20_000_000, Some(12_000_000));
	assert_eq!(pacing.step(20_000_000), Step::Idle { until: Some(28_000_000) }, "a real interval still starts at present return");
	pacing.on_completion(&completion(4, PresentOutcome::Displayed, unpaced()));
	pacing.on_present_started(5, 21_000_000, Some(12_000_000));
	assert_eq!(pacing.step(21_000_000), Step::Idle { until: Some(28_000_000) });
}

#[test]
fn frame_start_metadata_reservation_failure_discards_old_evidence() {
	use crate::frame_starts::FrameStarts;
	assert!(FrameStarts::new(usize::MAX, 1).is_err(), "the setup allocation refuses an impossible table");
	let mut starts = FrameStarts::new(2, 1).unwrap();
	starts.step(Step::Draw, 10);
	let began = starts.begin_acquire();
	starts.acquired(0, 0x1000, began);
	assert!(starts.reset(usize::MAX, 2).is_err());
	assert_eq!(starts.take(0, 0x1000, 1), None, "a failed reconfiguration must not resurrect prior metadata");
	assert_eq!(starts.begin_acquire(), None);
}

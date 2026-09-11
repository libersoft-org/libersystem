//! The two loss responses, kept apart, and the growth between them.

use super::*;

const MSS: u32 = 1460;

#[test]
fn the_initial_window_is_the_formula_and_not_ten_segments() {
	// RFC 6928 section 2: `min(10*SMSS, max(2*SMSS, 14600))`. At the common 1460-byte segment the
	// byte cap binds first, which is the half an implementation reading "ten segments" gets wrong.
	assert_eq!(initial_window(1460), 14_600, "ten segments of 1460 is 14600, exactly the cap");
	assert_eq!(initial_window(1220), 12_200, "under the cap, ten segments");
	assert_eq!(initial_window(9000), 18_000, "over it, two segments - because 2*SMSS exceeds the cap");
	assert_eq!(initial_window(536), 5_360);

	let window = CongestionWindow::new(MSS);
	assert_eq!(window.cwnd(), initial_window(MSS));
	assert!(window.in_slow_start(), "a connection starts in slow start");
	assert!(!window.in_recovery());
}

#[test]
fn a_timeout_drops_the_window_to_one_segment_and_not_to_half_of_anything() {
	// THE ASSERTION THAT SEPARATES THE TWO RESPONSES. An implementation with one "halve the window"
	// rule puts half a window back on a path that just stopped delivering; this asserts the policy,
	// not merely that something shrank.
	let mut window = CongestionWindow::new(MSS);
	let flight: u32 = 20 * MSS;
	window.on_timeout(flight);
	assert_eq!(window.cwnd(), MSS, "one segment, and recovery re-enters slow start from the bottom");
	assert_eq!(window.ssthresh(), flight / 2);
	assert!(window.in_slow_start());
	assert!(!window.in_recovery());

	// The floor under `ssthresh` is two segments, so a tiny flight cannot drive it to nothing.
	let mut small = CongestionWindow::new(MSS);
	small.on_timeout(MSS);
	assert_eq!(small.ssthresh(), 2 * MSS);
}

#[test]
fn three_duplicate_acknowledgements_retransmit_and_inflate_rather_than_restarting() {
	let mut window = CongestionWindow::new(MSS);
	let flight: u32 = 20 * MSS;
	assert_eq!(window.on_duplicate_ack(flight, 10_000), DuplicateAction::Wait, "one is reordering");
	assert_eq!(window.on_duplicate_ack(flight, 10_000), DuplicateAction::Wait, "two is reordering");
	assert_eq!(window.cwnd(), initial_window(MSS), "and neither moved the window");

	assert_eq!(window.on_duplicate_ack(flight, 10_000), DuplicateAction::FastRetransmit);
	assert_eq!(window.ssthresh(), flight / 2);
	assert_eq!(window.cwnd(), flight / 2 + 3 * MSS, "ssthresh plus the three segments that left the network");
	assert!(window.in_recovery());

	// EACH FURTHER DUPLICATE IS ONE MORE SEGMENT THAT ARRIVED, so one more may be sent.
	let inflated: u32 = window.cwnd();
	assert_eq!(window.on_duplicate_ack(flight, 10_000), DuplicateAction::Inflate);
	assert_eq!(window.cwnd(), inflated + MSS);
}

#[test]
fn recovery_ends_only_on_the_acknowledgement_that_covers_what_was_outstanding() {
	let mut window = CongestionWindow::new(MSS);
	let flight: u32 = 20 * MSS;
	for _ in 0..DUPLICATE_ACK_THRESHOLD {
		window.on_duplicate_ack(flight, 10_000);
	}
	let ssthresh: u32 = window.ssthresh();

	// A PARTIAL ACKNOWLEDGEMENT STILL LEAVES THE HOLE. Ending recovery here would deflate the window
	// while the retransmission it exists for is still outstanding.
	window.on_new_ack(MSS, 5_000);
	assert!(window.in_recovery(), "the recovery point has not been reached");
	assert!(window.cwnd() > ssthresh, "so the window stays inflated");

	window.on_new_ack(5 * MSS, 10_000);
	assert!(!window.in_recovery());
	assert_eq!(window.cwnd(), ssthresh, "and deflates to exactly ssthresh");
}

#[test]
fn slow_start_adds_a_segment_per_acknowledgement_and_avoidance_adds_one_per_round_trip() {
	let mut window = CongestionWindow::new(MSS);
	let start: u32 = window.cwnd();
	window.on_new_ack(MSS, 1_000);
	assert_eq!(window.cwnd(), start + MSS);
	// AND NEVER MORE THAN ONE SEGMENT PER ACKNOWLEDGEMENT, however much it covered - a single
	// acknowledgement for ten segments is still one segment of growth.
	window.on_new_ack(10 * MSS, 20_000);
	assert_eq!(window.cwnd(), start + 2 * MSS);

	// Past `ssthresh` the growth is one segment per WINDOW acknowledged, not per acknowledgement.
	let mut avoiding = CongestionWindow::new(MSS);
	avoiding.on_timeout(40 * MSS);
	assert_eq!(avoiding.cwnd(), MSS);
	while avoiding.in_slow_start() {
		avoiding.on_new_ack(MSS, 1_000);
	}
	let at_threshold: u32 = avoiding.cwnd();
	avoiding.on_new_ack(MSS, 2_000);
	assert_eq!(avoiding.cwnd(), at_threshold, "one segment of a window is not a round trip");
	let mut acknowledged: u32 = MSS;
	while acknowledged < at_threshold {
		avoiding.on_new_ack(MSS, 3_000);
		acknowledged += MSS;
	}
	assert_eq!(avoiding.cwnd(), at_threshold + MSS, "one segment per round trip");
}

#[test]
fn the_flight_is_bounded_by_the_smaller_of_the_two_windows() {
	let window = CongestionWindow::new(MSS);
	// The peer is the tighter of the two here, so it decides.
	assert_eq!(window.usable(0, 4_000), 4_000);
	// And the path is, here.
	assert_eq!(window.usable(0, 100_000), window.cwnd());
	// What is already outstanding comes off whichever won.
	assert_eq!(window.usable(3_000, 100_000), window.cwnd() - 3_000);
	// A FLIGHT PAST THE WINDOW SENDS NOTHING rather than wrapping into a large number, which is the
	// arithmetic that turns a closed window into a flood.
	assert_eq!(window.usable(100_000, 100_000), 0);
	assert_eq!(window.usable(0, 0), 0, "a zero window is closed, and persist is what reopens it");
}

#[test]
fn a_smaller_path_mtu_moves_the_floor_the_loss_responses_drop_to() {
	let mut window = CongestionWindow::new(MSS);
	window.set_smss(1220);
	window.on_timeout(20 * MSS);
	assert_eq!(window.cwnd(), 1220, "one segment, of the size the path now takes");
	assert_eq!(window.smss(), 1220);
}

#[test]
fn the_recovery_point_comparison_survives_the_sequence_wrap() {
	// A COMPARISON AND NOT A SUBTRACTION ORDER. Around the wrap `ack >= point` is wrong for half the
	// space, and recovery would either never end or end immediately.
	assert!(sequence_reaches(10, 10));
	assert!(sequence_reaches(11, 10));
	assert!(!sequence_reaches(9, 10));
	assert!(sequence_reaches(5, u32::MAX - 5), "past the wrap is still past");
	assert!(!sequence_reaches(u32::MAX - 5, 5), "and before it is still before");
}

#[test]
fn a_zero_window_starts_a_probe_schedule_and_reopening_ends_it() {
	let mut persist = Persist::new();
	assert!(!persist.running());

	// A zero window with nothing waiting is not a deadlock: there is nothing an update would release.
	persist.on_window(0, 0, 1_000, 1_000);
	assert!(!persist.running());

	persist.on_window(0, 4096, 1_000, 1_000);
	assert_eq!(persist.due_ms(), Some(2_000), "one RTO after the window shut");

	persist.on_window(8192, 4096, 3_000, 1_000);
	assert!(!persist.running(), "an open window ends the schedule");
	assert_eq!(persist.interval_ms(), 0);
}

#[test]
fn the_probe_schedule_backs_off_and_never_gives_up() {
	// A PEER ADVERTISING A ZERO WINDOW IS ANSWERING, and is entitled to keep its window shut for as
	// long as its reader is busy - so this schedule has no retry limit, unlike retransmission.
	let mut persist = Persist::new();
	persist.on_window(0, 4096, 0, 1_000);
	assert!(!persist.fire(999), "not before it is due");
	assert!(persist.fire(1_000));
	assert_eq!(persist.due_ms(), Some(3_000), "and the next wait is twice as long");
	assert!(persist.fire(3_000));
	assert_eq!(persist.due_ms(), Some(7_000));

	for step in 0..20 {
		let now: u64 = 10_000 + step * 100_000;
		persist.fire(now);
	}
	assert_eq!(persist.interval_ms(), crate::tcp_rto::MAX_RTO_MS, "the backoff stops at the ceiling");
	assert!(persist.running(), "and it is still probing");
}

#[test]
fn the_probe_interval_respects_the_same_floor_the_retransmission_timer_does() {
	let mut persist = Persist::new();
	persist.on_window(0, 1, 0, 1);
	assert_eq!(persist.interval_ms(), crate::tcp_rto::MIN_RTO_MS, "a tiny RTO does not become a busy loop");
}

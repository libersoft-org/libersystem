//! The three closing paths, and the one that a rule phrased on "the first FIN" loses.

use super::*;

#[test]
fn the_ordinary_active_close_waits_out_two_maximum_segment_lifetimes() {
	let mut closing = Closing::new();
	closing.on_fin_sent(0);
	assert_eq!(closing.state(), CloseState::FinWait1);
	closing.on_fin_acknowledged(100);
	assert_eq!(closing.state(), CloseState::FinWait2);
	assert!(closing.on_peer_fin(200), "the peer's FIN is acknowledged");
	assert_eq!(closing.state(), CloseState::TimeWait { until_ms: 200 + TIME_WAIT_MS });
	assert_eq!(closing.deadline_ms(), Some(200 + TIME_WAIT_MS));
	assert!(closing.retained(), "and the control block is still held");
}

#[test]
fn the_passive_side_closes_on_the_acknowledgement_and_waits_for_nothing() {
	// THE EXEMPTION, STATED CORRECTLY: a side whose own FIN is the LAST segment of the exchange is
	// acknowledged by the peer's final acknowledgement and is finished on it.
	let mut closing = Closing::new();
	assert!(closing.on_peer_fin(0));
	assert_eq!(closing.state(), CloseState::CloseWait);
	assert!(closing.peer_finished());
	closing.on_fin_sent(50);
	assert_eq!(closing.state(), CloseState::LastAck);
	closing.on_fin_acknowledged(100);
	assert_eq!(closing.state(), CloseState::Closed);
	assert!(!closing.retained(), "nothing is held afterwards");
	assert_eq!(closing.deadline_ms(), None);
}

#[test]
fn a_simultaneous_close_puts_both_endpoints_through_closing_into_time_wait() {
	// THE CASE A RULE PHRASED ON "THE FIRST FIN" LOSES. Neither endpoint can observe a global first,
	// so both would conclude they are not it and free immediately - losing the recovery on BOTH.
	let mut left = Closing::new();
	let mut right = Closing::new();
	left.on_fin_sent(0);
	right.on_fin_sent(0);
	assert_eq!(left.state(), CloseState::FinWait1);
	assert_eq!(right.state(), CloseState::FinWait1);

	// The FINs cross: each arrives while the other's own is still unacknowledged.
	assert!(left.on_peer_fin(10));
	assert!(right.on_peer_fin(10));
	assert_eq!(left.state(), CloseState::Closing);
	assert_eq!(right.state(), CloseState::Closing);

	left.on_fin_acknowledged(20);
	right.on_fin_acknowledged(20);
	assert_eq!(left.state(), CloseState::TimeWait { until_ms: 20 + TIME_WAIT_MS });
	assert_eq!(right.state(), CloseState::TimeWait { until_ms: 20 + TIME_WAIT_MS });
	// BOTH retain the control block, the four-tuple, the budget entry and the one deadline. A
	// simultaneous close costs two retained blocks where an ordinary close costs one.
	assert!(left.retained() && right.retained());
	assert_eq!(left.deadline_ms(), Some(20 + TIME_WAIT_MS));
	assert_eq!(right.deadline_ms(), Some(20 + TIME_WAIT_MS));
}

#[test]
fn a_retransmitted_fin_arriving_in_time_wait_is_answered_and_restarts_the_timer() {
	// THE LOST-FINAL-ACK RECOVERY, which is the reason the state exists at all: the peer never saw
	// the last acknowledgement and is asking again.
	let mut closing = Closing::new();
	closing.on_fin_sent(0);
	closing.on_fin_acknowledged(100);
	closing.on_peer_fin(200);
	let first_deadline: u64 = 200 + TIME_WAIT_MS;
	assert_eq!(closing.deadline_ms(), Some(first_deadline));

	assert!(closing.on_peer_fin(first_deadline - 1_000), "it is answered rather than ignored");
	assert_eq!(closing.deadline_ms(), Some(first_deadline - 1_000 + TIME_WAIT_MS), "and the wait starts again");
	assert!(closing.retained());
	// The original deadline passing is no longer enough, because the timer moved.
	assert!(!closing.tick(first_deadline));
	assert!(closing.retained());
}

#[test]
fn the_wait_releases_the_control_block_after_exactly_two_maximum_segment_lifetimes_and_not_before() {
	let mut closing = Closing::new();
	closing.on_fin_sent(0);
	closing.on_fin_acknowledged(0);
	closing.on_peer_fin(0);
	assert!(!closing.tick(TIME_WAIT_MS - 1), "not a millisecond early");
	assert!(closing.retained());
	assert!(closing.tick(TIME_WAIT_MS));
	assert_eq!(closing.state(), CloseState::Closed);
	assert!(!closing.retained(), "and the budget entry goes with it");
	assert_eq!(closing.deadline_ms(), None);
	// A FIN arriving after the block is gone is not this connection's to answer.
	assert!(!closing.on_peer_fin(TIME_WAIT_MS + 1));
}

#[test]
fn two_maximum_segment_lifetimes_is_sixty_seconds_in_this_profile() {
	// The number is fixed here rather than left to the implementation, for the same reason every
	// other number in this profile is: two implementations would otherwise both be "correct" and
	// disagree about how long a tuple is reserved.
	assert_eq!(MSL_MS, 30_000);
	assert_eq!(TIME_WAIT_MS, 60_000);
}

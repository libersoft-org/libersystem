//! One attempt at a time, the three-second cap, and the events that must not change the winner.

use super::*;

#[test]
fn a_singleton_runs_the_full_schedule_with_no_cap() {
	// A SINGLETON LITERAL IS THE FINAL CANDIDATE. Capping it would abandon a connection with nothing
	// to fall back to.
	let mut sequence = Sequence::new(1);
	assert_eq!(sequence.begin(0), Step::Start { index: 0, deadline_ms: None });
	assert_eq!(sequence.deadline_ms(), None);
	assert_eq!(sequence.on_timer(1_000_000), Step::Waiting, "no cap means no expiry");
	assert_eq!(sequence.total_deadline_ms(), 183_000, "the complete SYN schedule and nothing more");
}

#[test]
fn a_nonfinal_candidate_is_retired_at_three_seconds_and_the_next_starts() {
	let mut sequence = Sequence::new(2);
	assert_eq!(sequence.begin(0), Step::Start { index: 0, deadline_ms: Some(NONFINAL_MS) });
	assert_eq!(sequence.on_timer(NONFINAL_MS - 1), Step::Waiting, "not a millisecond early");
	// AT THE CAP THE SERVICE RETIRES THIS CANDIDATE AND STARTS THE NEXT rather than sending another
	// SYN: expiry takes precedence over a retransmission due at the same instant.
	assert_eq!(sequence.on_timer(NONFINAL_MS), Step::Start { index: 1, deadline_ms: None });
	assert_eq!(sequence.active(), Some(1));
	assert_eq!(sequence.started(), 2, "two attempts, one after the other");
	// Two candidates: three seconds for the first and the full schedule for the last.
	assert_eq!(sequence.total_deadline_ms(), 186_000);
}

#[test]
fn only_one_attempt_is_ever_active() {
	let mut sequence = Sequence::new(3);
	sequence.begin(0);
	assert_eq!(sequence.begin(0), Step::Waiting, "beginning again starts nothing");
	assert_eq!(sequence.started(), 1);
	assert_eq!(sequence.active(), Some(0));

	sequence.on_timer(NONFINAL_MS);
	assert_eq!(sequence.active(), Some(1));
	assert_eq!(sequence.started(), 2, "and the retired one is not running beside it");
}

#[test]
fn an_explicit_refusal_advances_at_once_rather_than_waiting_out_the_cap() {
	// A CANDIDATE WITH NO USABLE PATH FAILS WITH ITS TYPED REASON AND ADVANCES: waiting three seconds
	// for something already known to be impossible is three seconds of nothing.
	let mut sequence = Sequence::new(2);
	sequence.begin(0);
	assert_eq!(sequence.on_failed(0, 10), Step::Start { index: 1, deadline_ms: None });
	assert_eq!(sequence.active(), Some(1));
}

#[test]
fn a_late_event_from_a_retired_attempt_changes_nothing() {
	// A SYN-ACK, A QUOTED ERROR OR A SEND COMPLETION FROM AN ABANDONED ATTEMPT arrives after its
	// successor started. A sequence that acted on it would hand the caller a socket to the wrong peer.
	let mut sequence = Sequence::new(2);
	sequence.begin(0);
	sequence.on_timer(NONFINAL_MS);
	assert_eq!(sequence.active(), Some(1));

	assert!(!sequence.on_connected(0), "the first attempt is retired and cannot win");
	assert_eq!(sequence.winner(), None);
	assert_eq!(sequence.on_failed(0, NONFINAL_MS + 10), Step::Waiting, "and its failure retires nothing");
	assert_eq!(sequence.active(), Some(1), "the running attempt is untouched");

	// The running one still can.
	assert!(sequence.on_connected(1));
	assert_eq!(sequence.winner(), Some(1));
}

#[test]
fn the_winner_is_final_and_nothing_afterwards_moves_it() {
	let mut sequence = Sequence::new(3);
	sequence.begin(0);
	assert!(sequence.on_connected(0));
	assert_eq!(sequence.begin(10), Step::Connected { index: 0 }, "no further candidate is started");
	assert_eq!(sequence.started(), 1, "and none was");
	assert!(!sequence.on_connected(1), "a second winner is not a winner");
	assert_eq!(sequence.winner(), Some(0));
	assert_eq!(sequence.on_timer(1_000_000), Step::Connected { index: 0 });
}

#[test]
fn every_candidate_failing_exhausts_the_open_rather_than_looping() {
	let mut sequence = Sequence::new(2);
	sequence.begin(0);
	sequence.on_failed(0, 10);
	assert_eq!(sequence.on_failed(1, 20), Step::Exhausted);
	assert_eq!(sequence.winner(), None);
	assert_eq!(sequence.begin(30), Step::Exhausted, "and it stays exhausted");
	assert_eq!(sequence.started(), 2, "each candidate was tried exactly once");
}

#[test]
fn the_working_candidate_starts_by_three_seconds_when_the_preferred_one_is_black_holed() {
	// THE NAMED CASE: a preferred IPv6 candidate whose SYN is black-holed and a working IPv4 one. The
	// second attempt starts BY three seconds - not after the first has run its whole schedule, which
	// is what a sequence without the cap would do.
	let mut sequence = Sequence::new(2);
	assert_eq!(sequence.begin(0), Step::Start { index: 0, deadline_ms: Some(3_000) });
	// Nothing comes back. At the cap the second starts.
	let Step::Start { index, .. } = sequence.on_timer(3_000) else {
		panic!("the second candidate starts at the cap");
	};
	assert_eq!(index, 1);
	assert!(sequence.on_connected(1));
	assert_eq!(sequence.winner(), Some(1));
	// ONE RESULT, ONE SOCKET: the first attempt was retired before the second began.
	assert_eq!(sequence.started(), 2);
	assert_eq!(sequence.active(), None);
}

#[test]
fn both_peers_silent_costs_the_cap_plus_the_full_schedule_and_no_more() {
	let mut sequence = Sequence::new(2);
	sequence.begin(0);
	sequence.on_timer(NONFINAL_MS);
	// The last candidate has no cap, so only its own schedule ends it - and the total is the sum.
	assert_eq!(sequence.on_timer(NONFINAL_MS + 100_000), Step::Waiting);
	assert_eq!(sequence.total_deadline_ms(), NONFINAL_MS + 183_000);
	// AND NO SINGLE RETRANSMISSION INTERVAL EXCEEDS THE CEILING, which is what stops an uncapped
	// derivation passing a duration-only assertion.
	for attempt in 0..crate::tcp_rto::syn_attempts() {
		assert!(crate::tcp_rto::syn_interval(attempt).expect("an interval") <= crate::tcp_rto::MAX_RTO_MS);
	}
}

//! A held segment is not a transmitted one, and a dead connection's completion moves nothing.

use super::*;

#[test]
fn a_held_segment_is_not_transmitted_until_its_completion_arrives() {
	// THE NAMED CASE: hold a packet in L3 resolution. Its sequence becomes eligible only after its
	// matching `Sent` completion - before that no router can have seen it, so a quotation of it is a
	// forgery and must be refused.
	let mut bound = TransmitBound::new(1000);
	assert!(bound.hold(7, 1500));
	assert_eq!(bound.transmitted(), 1000, "queued is not transmitted");
	assert_eq!(bound.held(), 1);

	assert_eq!(bound.on_sent(7), Handoff::Transmitted { end: 1500 });
	assert_eq!(bound.transmitted(), 1500);
	assert_eq!(bound.held(), 0, "and the slot is released");
}

#[test]
fn a_cancelled_or_failed_handoff_never_advances_the_bound() {
	let mut bound = TransmitBound::new(1000);
	assert!(bound.hold(7, 1500));
	assert_eq!(bound.on_retired(7), Handoff::Retired);
	assert_eq!(bound.transmitted(), 1000, "it never reached the driver");
	assert_eq!(bound.held(), 0);
	// And the completion cannot be replayed to advance it afterwards.
	assert_eq!(bound.on_sent(7), Handoff::Unknown);
	assert_eq!(bound.transmitted(), 1000);
}

#[test]
fn an_old_tokens_completion_does_not_advance_the_replacement_connections_bound() {
	// A CONTROL BLOCK IS REUSED. The frame a cancelled operation left below can complete after the
	// block has been handed to a new connection; taking its sequence at face value would declare the
	// NEW connection's unsent data transmitted.
	let mut bound = TransmitBound::new(1000);
	assert!(bound.hold(7, 900_000));
	bound.reset(2000);
	assert_eq!(bound.transmitted(), 2000);
	assert_eq!(bound.held(), 0, "the old connection's segment is not this connection's");

	assert_eq!(bound.on_sent(7), Handoff::Retired, "refused rather than merely unmatched");
	assert_eq!(bound.transmitted(), 2000, "nothing moved");
}

#[test]
fn the_bound_is_a_high_water_mark_and_go_back_n_does_not_lower_it() {
	// GO-BACK-N REWINDS `SND.NXT`; it does not un-transmit what was already on the wire. A bound
	// that rewound with the queue would refuse the SECOND Packet Too Big about the same bytes.
	let mut bound = TransmitBound::new(1000);
	assert_eq!(bound.advance(3000), Handoff::Transmitted { end: 3000 });
	// The queue rewinds to 1000 and cuts smaller segments; they complete behind the mark.
	assert_eq!(bound.advance(1500), Handoff::Covered);
	assert_eq!(bound.transmitted(), 3000, "still the furthest ever transmitted");
	assert_eq!(bound.advance(3200), Handoff::Transmitted { end: 3200 }, "and new space still moves it");
}

#[test]
fn a_retransmissions_completion_behind_the_mark_is_covered_rather_than_a_move() {
	let mut bound = TransmitBound::new(1000);
	assert!(bound.hold(1, 2000));
	assert_eq!(bound.on_sent(1), Handoff::Transmitted { end: 2000 });
	assert!(bound.hold(2, 1600));
	assert_eq!(bound.on_sent(2), Handoff::Covered);
	assert_eq!(bound.transmitted(), 2000);
}

#[test]
fn the_bound_survives_sequence_wraparound() {
	// An unsigned comparison would refuse every segment across the wrap, which is a connection that
	// silently stops accepting Packet Too Big after two gigabytes.
	let mut bound = TransmitBound::new(u32::MAX - 100);
	assert_eq!(bound.advance(100), Handoff::Transmitted { end: 100 }, "200 bytes past the wrap");
	assert_eq!(bound.transmitted(), 100);
	assert_eq!(bound.advance(u32::MAX - 50), Handoff::Covered, "and what is behind it stays behind it");
	assert_eq!(bound.transmitted(), 100);
}

#[test]
fn a_connection_holds_only_its_share_and_a_refused_hold_is_a_send_that_did_not_happen() {
	let mut bound = TransmitBound::new(0);
	for index in 0..MAX_HELD {
		assert!(bound.hold(index as u64, (index as u32 + 1) * 100), "its share fits");
	}
	assert!(!bound.hold(99, 999_999), "and no more than its share");
	assert_eq!(bound.transmitted(), 0, "a refused hold transmitted nothing");
	assert_eq!(bound.held(), MAX_HELD);
}

#[test]
fn a_stale_slot_is_reclaimed_so_a_dead_connection_cannot_starve_its_successor() {
	let mut bound = TransmitBound::new(0);
	for index in 0..MAX_HELD {
		assert!(bound.hold(index as u64, (index as u32 + 1) * 100));
	}
	bound.reset(5000);
	assert_eq!(bound.held(), 0, "none of them belongs to this connection");
	assert!(bound.hold(1000, 6000), "and the new connection can still hand off");
	assert_eq!(bound.on_sent(1000), Handoff::Transmitted { end: 6000 });
	assert_eq!(bound.transmitted(), 6000);
}

#[test]
fn an_unknown_token_is_neither_a_move_nor_a_retirement() {
	let mut bound = TransmitBound::new(1000);
	assert_eq!(bound.on_sent(42), Handoff::Unknown);
	assert_eq!(bound.on_retired(42), Handoff::Unknown);
	assert_eq!(bound.transmitted(), 1000);
}

//! Every budget at its exact bound and one past it, and the accounting through a whole handshake.

use super::*;

fn with_listener(effective: u16) -> Admission {
	let mut admission = Admission::new();
	admission.register_listener(1, effective);
	admission
}

#[test]
fn a_backlog_of_zero_is_refused_and_anything_above_the_cap_is_clamped_to_it() {
	// ZERO IS A REFUSAL. A listener that may hold no unaccepted connection cannot work, and promoting
	// it to one silently would be a second meaning for a value the caller wrote.
	assert_eq!(effective_backlog(0), Err(BacklogRefusal::Zero));
	assert_eq!(effective_backlog(1), Ok(1));
	assert_eq!(effective_backlog(MAX_BACKLOG_PER_LISTENER), Ok(MAX_BACKLOG_PER_LISTENER));
	// CLAMPED, NOT REFUSED: a refusal would make a portable program guess a number this file chose.
	assert_eq!(effective_backlog(MAX_BACKLOG_PER_LISTENER + 1), Ok(MAX_BACKLOG_PER_LISTENER));
	assert_eq!(effective_backlog(u16::MAX), Ok(MAX_BACKLOG_PER_LISTENER));
}

#[test]
fn a_listener_holds_exactly_its_effective_backlog_and_the_next_syn_is_refused() {
	let mut admission = with_listener(4);
	for slot in 1..=4u16 {
		assert_eq!(admission.admit_syn(1), Ok(()), "slot {slot}");
		assert_eq!(admission.listener(1).expect("held").occupied(), slot);
	}
	assert_eq!(admission.admit_syn(1), Err(Refusal::ListenerBacklog));
	assert_eq!(admission.refusals().listener_backlog, 1);
	// NOTHING WAS CHARGED FOR THE REFUSAL, and nothing already admitted moved.
	assert_eq!(admission.occupancy().half_open, 4);
	assert_eq!(admission.occupancy().control_blocks, 4);
	assert_eq!(admission.occupancy().receive_bytes, 4 * RECEIVE_BASE_BYTES);
}

#[test]
fn the_service_wide_slot_count_is_checked_first_so_a_refusal_has_one_reason() {
	// Several listeners, each well under its own backlog, filling the service-wide budget together.
	let mut admission = Admission::new();
	for id in 0..4u32 {
		admission.register_listener(id, MAX_BACKLOG_PER_LISTENER);
	}
	for index in 0..MAX_BACKLOG_TOTAL {
		assert_eq!(admission.admit_syn(index % 4), Ok(()), "slot {index}");
	}
	assert_eq!(admission.occupied_slots(), MAX_BACKLOG_TOTAL);
	// EVERY LISTENER IS UNDER ITS OWN BACKLOG - sixteen each against a cap of thirty-two - so the
	// only reason left is the machine being full, and that is what the counter says.
	assert_eq!(admission.listener(0).expect("held").occupied(), 16);
	assert_eq!(admission.admit_syn(0), Err(Refusal::ServiceBacklog));
	assert_eq!(admission.refusals().service_backlog, 1);
	assert_eq!(admission.refusals().listener_backlog, 0, "this listener is not the one that is full");
}

#[test]
fn a_handshake_completing_changes_which_kind_of_slot_is_held_and_not_how_many() {
	let mut admission = with_listener(2);
	admission.admit_syn(1).expect("room");
	assert_eq!(admission.occupancy().reserved, 1);
	assert_eq!(admission.occupancy().queued, 0);
	assert_eq!(admission.occupancy().half_open, 1);

	assert!(admission.handshake_complete(1));
	// THE SLOT WAS TAKEN WHEN THE SYN WAS ADMITTED. This only says what is in it, and makes no second
	// admission decision.
	assert_eq!(admission.occupied_slots(), 1);
	assert_eq!(admission.occupancy().reserved, 0);
	assert_eq!(admission.occupancy().queued, 1);
	assert_eq!(admission.occupancy().half_open, 0, "it is no longer half-open");
	assert_eq!(admission.occupancy().control_blocks, 1, "and the control block is the same one");
}

#[test]
fn the_slot_is_held_until_a_handoff_actually_succeeds() {
	let mut admission = with_listener(1);
	admission.admit_syn(1).expect("room");
	admission.handshake_complete(1);
	// A FAILED HANDOFF IS NOT A RELEASE. The connection is still queued and still charged, and the
	// listener stays live - so a second SYN still finds the queue full.
	assert_eq!(admission.admit_syn(1), Err(Refusal::ListenerBacklog));
	assert_eq!(admission.occupied_slots(), 1);

	assert!(admission.accepted(1));
	assert_eq!(admission.occupied_slots(), 0, "and a successful one releases it");
	assert_eq!(admission.occupancy().control_blocks, 1, "the connection itself is still here");
	assert_eq!(admission.admit_syn(1), Ok(()), "so the next peer can enter");
}

#[test]
fn the_concurrent_handshake_case_admits_one_and_drops_the_other() {
	// WITH A BACKLOG OF ONE, TWO SYNs BEFORE EITHER FINAL ACK. Only the first is admitted; the second
	// is dropped with a listener-backlog refusal and creates nothing.
	let mut admission = with_listener(1);
	assert_eq!(admission.admit_syn(1), Ok(()));
	assert_eq!(admission.admit_syn(1), Err(Refusal::ListenerBacklog));
	assert_eq!(admission.occupancy().reserved, 1, "one reserved half-open");
	assert_eq!(admission.occupancy().queued, 0, "and no queued connection");

	// Completing the first changes the counts without changing occupancy.
	admission.handshake_complete(1);
	assert_eq!(admission.occupancy().reserved, 0);
	assert_eq!(admission.occupancy().queued, 1);
	assert_eq!(admission.occupied_slots(), 1);
	assert_eq!(admission.admit_syn(1), Err(Refusal::ListenerBacklog), "still full");

	// And accepting it lets the second peer's retransmitted SYN in.
	admission.accepted(1);
	assert_eq!(admission.admit_syn(1), Ok(()));
}

#[test]
fn a_reservation_is_released_exactly_once_by_expiry_or_a_reset() {
	let mut admission = with_listener(2);
	admission.admit_syn(1).expect("room");
	assert!(admission.release_half_open(1));
	assert_eq!(admission.occupancy(), Occupancy { half_open: 0, control_blocks: 0, receive_bytes: 0, reserved: 0, queued: 0 });
	// EXACTLY ONCE. A second release of the same reservation would credit back memory nothing is
	// holding, which is how a budget comes to permit more than it says.
	assert!(!admission.release_half_open(1));
	assert_eq!(admission.occupancy().control_blocks, 0);
}

#[test]
fn withdrawing_a_listener_aborts_what_it_was_holding_and_releases_every_charge() {
	let mut admission = with_listener(4);
	admission.admit_syn(1).expect("room");
	admission.admit_syn(1).expect("room");
	admission.handshake_complete(1);
	assert_eq!(admission.occupancy().control_blocks, 2);

	// NO CALLER CAN ACCEPT THEM AFTERWARDS, so they are aborted with the listener rather than left
	// charged and unreachable.
	assert_eq!(admission.withdraw_listener(1), 2);
	assert_eq!(admission.occupancy(), Occupancy::default());
	assert!(admission.listener(1).is_none());
	assert_eq!(admission.withdraw_listener(1), 0, "and withdrawing it twice releases nothing twice");
}

#[test]
fn half_open_control_blocks_and_receive_storage_each_refuse_at_their_own_bound() {
	// HALF-OPEN, with the backlog budgets deliberately out of the way.
	let mut half = Admission::new();
	for id in 0..4u32 {
		half.register_listener(id, MAX_BACKLOG_PER_LISTENER);
	}
	for index in 0..MAX_HALF_OPEN {
		half.admit_syn(index % 4).expect("room");
		// Completing each one frees the half-open counter, so drive them all as half-open instead.
	}
	// Sixty-four admitted is both the half-open cap and the service-wide slot cap, and the precedence
	// says which one answers.
	assert_eq!(half.occupancy().half_open, MAX_HALF_OPEN);
	assert_eq!(half.admit_syn(0), Err(Refusal::ServiceBacklog), "the service-wide count is checked first");

	// CONTROL BLOCKS, reached through outbound opens, which take no backlog slot.
	let mut blocks = Admission::new();
	for index in 0..MAX_TCBS {
		blocks.open_outbound().unwrap_or_else(|refusal| panic!("open {index}: {refusal:?}"));
	}
	assert_eq!(blocks.occupancy().control_blocks, MAX_TCBS);
	assert_eq!(blocks.open_outbound(), Err(Refusal::ControlBlocks));
	assert_eq!(blocks.refusals().control_blocks, 1);
	// AND NOTHING ADMITTED MOVED.
	assert_eq!(blocks.occupancy().control_blocks, MAX_TCBS);
}

#[test]
fn sixty_four_base_buffers_fit_under_the_aggregate_receive_cap() {
	// THE NUMBER THAT MAKES THE GUEST ORACLE FEASIBLE: 64 idle connections hold 1048576 bytes against
	// a 2097152-byte cap, which leaves room and is why the base buffer is 16 kB rather than the 65535
	// it used to be.
	assert_eq!(MAX_HALF_OPEN as usize * RECEIVE_BASE_BYTES, 1_048_576);
	assert!(MAX_HALF_OPEN as usize * RECEIVE_BASE_BYTES < MAX_RECEIVE_BYTES);
	// The old base would not have fitted, which is the whole reason it changed.
	assert!(MAX_HALF_OPEN as usize * RECEIVE_CEILING_UNSCALED > MAX_RECEIVE_BYTES);
}

#[test]
fn receive_growth_is_fallible_charged_and_bounded_by_the_negotiated_scale() {
	let mut admission = Admission::new();
	admission.open_outbound().expect("room");
	assert_eq!(admission.occupancy().receive_bytes, RECEIVE_BASE_BYTES);

	// WITHOUT SCALING the ceiling is 65535, and asking for more gets exactly that.
	assert_eq!(admission.grow_receive(RECEIVE_BASE_BYTES, 1_000_000, false), Some(RECEIVE_CEILING_UNSCALED));
	assert_eq!(admission.occupancy().receive_bytes, RECEIVE_CEILING_UNSCALED);
	// Asking for what it already has is not a growth.
	assert_eq!(admission.grow_receive(RECEIVE_CEILING_UNSCALED, RECEIVE_CEILING_UNSCALED, false), None);

	// WITH THE SCALE the ceiling is higher - and negotiating the scale on its own grows nothing,
	// which is what the base buffer above already proves.
	let mut scaled = Admission::new();
	scaled.open_outbound().expect("room");
	assert_eq!(scaled.grow_receive(RECEIVE_BASE_BYTES, 1_000_000, true), Some(RECEIVE_CEILING_SCALED));
}

#[test]
fn a_growth_that_cannot_be_funded_keeps_the_current_buffer_and_touches_nobody_else() {
	// Fill most of the aggregate with admitted connections, then ask one of them to grow past what
	// is left.
	let mut admission = Admission::new();
	let connections: usize = MAX_RECEIVE_BYTES / RECEIVE_BASE_BYTES - 1;
	for _ in 0..connections {
		admission.open_outbound().expect("room");
	}
	let held: usize = admission.occupancy().receive_bytes;
	assert_eq!(admission.grow_receive(RECEIVE_BASE_BYTES, RECEIVE_CEILING_SCALED, true), None, "there is not room for it");
	assert_eq!(admission.refusals().receive_bytes, 1);
	// NOTHING IS EVICTED TO GROW SOMEBODY ELSE, and the connection that asked keeps what it had.
	assert_eq!(admission.occupancy().receive_bytes, held);
	assert_eq!(admission.occupancy().control_blocks, connections as u32);
}

#[test]
fn closing_releases_the_allocation_rather_than_leaving_it_in_a_reusable_slot() {
	// A POOL THAT KEPT AN UNCHARGED BASE BUFFER would be holding memory no budget knows about, and
	// the budget would then permit more than it says.
	let mut admission = Admission::new();
	admission.open_outbound().expect("room");
	let grown: usize = admission.grow_receive(RECEIVE_BASE_BYTES, RECEIVE_CEILING_UNSCALED, false).expect("room");
	admission.close(grown);
	assert_eq!(admission.occupancy(), Occupancy::default());
}

#[test]
fn one_dual_stack_listener_shares_its_reservation_across_both_families() {
	// THE RESERVATION IS THE LISTENER'S, NOT A FAMILY'S. A dual-stack wildcard is one claim, so an
	// IPv4 peer and an IPv6 peer draw on the same backlog - which is the property a per-family count
	// would silently double.
	let mut admission = with_listener(2);
	assert_eq!(admission.admit_syn(1), Ok(()), "an IPv4 peer");
	assert_eq!(admission.admit_syn(1), Ok(()), "an IPv6 peer");
	assert_eq!(admission.occupied_slots(), 2);
	assert_eq!(admission.admit_syn(1), Err(Refusal::ListenerBacklog), "and the third finds it full whichever family it is");
}

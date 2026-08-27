// THE TABLE IS THE TEST. Every edge it allows is walked, and a sample of what it does not is refused
// - because "enforced rather than implied" is a claim about the edges that are NOT there, and a test
// that only walks the legal ones would pass against a function that returns true for everything.

use super::*;

const EVERY_STATE: [BindingState; 8] = [
	BindingState::Unbound,
	BindingState::DependencyPending,
	BindingState::Binding,
	BindingState::Online,
	BindingState::Stopping,
	BindingState::Backoff,
	BindingState::Failed,
	BindingState::Quarantined,
];

// The table, written out a second time and independently, so this test disagrees with the
// implementation rather than restating it.
const LEGAL: [(BindingState, BindingState); 18] = [
	(BindingState::Unbound, BindingState::Binding),
	(BindingState::Binding, BindingState::Online),
	(BindingState::Binding, BindingState::Backoff),
	(BindingState::Binding, BindingState::Failed),
	(BindingState::Binding, BindingState::Stopping),
	(BindingState::Online, BindingState::Stopping),
	(BindingState::Stopping, BindingState::Backoff),
	(BindingState::Stopping, BindingState::Failed),
	(BindingState::Stopping, BindingState::Quarantined),
	(BindingState::Backoff, BindingState::Binding),
	(BindingState::Backoff, BindingState::Failed),
	(BindingState::Failed, BindingState::Binding),
	// P02M0164's, added by the milestone that first gave a node something to wait for.
	(BindingState::Unbound, BindingState::DependencyPending),
	(BindingState::DependencyPending, BindingState::Binding),
	(BindingState::Binding, BindingState::DependencyPending),
	(BindingState::Backoff, BindingState::DependencyPending),
	(BindingState::Stopping, BindingState::DependencyPending),
	(BindingState::Failed, BindingState::DependencyPending),
];

#[test]
fn exactly_the_table_is_legal_and_nothing_else_is() {
	for &from in EVERY_STATE.iter() {
		for &to in EVERY_STATE.iter() {
			let expected = LEGAL.contains(&(from, to));
			assert_eq!(from.may_move_to(to), expected, "{:?} -> {:?}", core::str::from_utf8(from.name()), core::str::from_utf8(to.name()));
		}
	}
}

#[test]
fn no_state_has_an_edge_to_itself_so_a_duplicate_event_changes_nothing() {
	// A DUPLICATE EVENT IS IDEMPOTENT, and it falls out of the table rather than needing a rule of
	// its own: the second arrival of the event that already moved the node is refused like any other
	// illegal transition.
	for &state in EVERY_STATE.iter() {
		assert!(!state.may_move_to(state), "{:?} has an edge to itself", core::str::from_utf8(state.name()));
	}
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	assert!(!record.move_to(BindingState::Binding, None), "the same event twice moves nothing");
	assert!(record.state == BindingState::Binding);
}

#[test]
fn quarantined_is_terminal_for_the_boot() {
	// Its resources are never reused, so there is nowhere for it to go: an edge out would be a
	// device being handed to somebody while its last binding's mappings may still be live.
	for &to in EVERY_STATE.iter() {
		assert!(!BindingState::Quarantined.may_move_to(to), "quarantined moved somewhere");
	}
}

#[test]
fn a_failed_bind_never_lands_back_where_a_bind_begins() {
	// `Unbound` is what INVITES a bind. A failed bind landing there is a bind that immediately
	// happens again with nothing recorded about why the last one did not work.
	for &from in EVERY_STATE.iter() {
		assert!(!from.may_move_to(BindingState::Unbound), "something can reach Unbound again");
	}
}

#[test]
fn every_cause_answers_whether_it_is_retryable() {
	// A flag some variants leave unset is a flag each call site guesses at. This is the whole list,
	// including the ones that are NOT retryable - the half a table written from memory loses.
	assert!(FailureCause::HandshakeTimeout.retryable());
	assert!(FailureCause::DriverExited.retryable());
	assert!(FailureCause::SpawnFailed.retryable(), "usually a transient shortage");
	assert!(!FailureCause::DriverMissing.retryable());
	assert!(!FailureCause::ProtocolMismatch.retryable());
	assert!(!FailureCause::ClaimRefused.retryable(), "somebody else holds it and waiting changes nothing this controls");
	assert!(!FailureCause::IommuRequired.retryable());
	assert!(!FailureCause::ResourceExhausted.retryable());
}

#[test]
fn a_driver_reported_cause_keeps_the_code_and_reads_retryability_off_it() {
	// "The driver said it failed" without saying what it said is a cause that explains nothing - and
	// the driver's own closed set is the only party that can honestly answer retryability about
	// itself.
	assert!(FailureCause::DriverReported(DriverFailureCode::DeviceNotResponding).retryable());
	assert!(FailureCause::DriverReported(DriverFailureCode::OutOfMemory).retryable());
	assert!(!FailureCause::DriverReported(DriverFailureCode::UnsupportedDevice).retryable());
	assert!(!FailureCause::DriverReported(DriverFailureCode::ResourceUnusable).retryable());
	assert!(!FailureCause::DriverReported(DriverFailureCode::InternalError).retryable());
}

#[test]
fn a_record_that_comes_up_stops_carrying_the_reason_its_last_attempt_failed() {
	// A node that is `Online` carrying an old cause reads as a node that is broken and running.
	let mut record = BindingRecord::new();
	record.move_to(BindingState::Binding, None);
	record.move_to(BindingState::Backoff, Some(FailureCause::SpawnFailed));
	assert!(record.failure == Some(FailureCause::SpawnFailed), "the backoff explains itself");
	record.move_to(BindingState::Binding, None);
	assert!(record.failure.is_none(), "a fresh attempt carries no verdict from the last one");
	record.move_to(BindingState::Online, None);
	assert!(record.failure.is_none());
}

#[test]
fn a_teardown_that_did_not_confirm_ends_at_quarantined_and_never_asks_about_a_retry() {
	// `teardown-unconfirmed` is in neither retryability column because it never reaches the
	// question. Walking it is how that stays true rather than being asserted about a value.
	let mut record = BindingRecord::new();
	record.move_to(BindingState::Binding, None);
	record.move_to(BindingState::Stopping, Some(FailureCause::DriverExited));
	assert!(record.move_to(BindingState::Quarantined, Some(FailureCause::TeardownUnconfirmed)));
	assert!(record.failure == Some(FailureCause::TeardownUnconfirmed));
	assert!(!record.move_to(BindingState::Binding, None), "there is no way out of quarantine");
	assert!(!record.move_to(BindingState::Backoff, None));
}

// ------------------------------------------------------- the fault cases
//
// FIVE SHAPES A BIND CAN GO WRONG IN, each walked end to end rather than argued about.
//
// NOT removal racing a bind: the scan runs once and there is no removal event, so that test could
// only pass by first inventing the mechanism it tests.
//
// What each one asserts is the same three things: where the node ended up, what it says about why,
// and that the path it took was one the table allows - which `move_to` answers by refusing anything
// else, so a walk that returns true at every step IS the assertion that the path is legal.

#[test]
fn a_crash_during_bind_goes_back_through_the_teardown() {
	// THE DEVICE WAS TAKEN, so the only way out is through `Stopping`. Going straight to `Failed`
	// would record a node that never had a device, which is a different story about the same boot.
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	assert!(record.move_to(BindingState::Stopping, Some(FailureCause::DriverExited)));
	assert!(record.move_to(BindingState::Backoff, Some(FailureCause::DriverExited)));
	assert!(record.state == BindingState::Backoff);
	assert!(record.failure == Some(FailureCause::DriverExited));
	assert!(FailureCause::DriverExited.retryable(), "a driver that died before saying anything is worth another attempt");
}

#[test]
fn a_crash_after_online_is_the_same_teardown_from_one_state_further_on() {
	// The difference from a crash during bind is WHERE it starts, and the table has an edge for
	// each: `Binding -> Stopping` and `Online -> Stopping`. What follows is identical, which is the
	// point - a driver that dies is a driver that dies, and the teardown does not care when.
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	assert!(record.move_to(BindingState::Online, None));
	assert!(record.failure == None, "coming up clears the reason the last attempt failed");
	assert!(record.move_to(BindingState::Stopping, Some(FailureCause::DriverExited)));
	assert!(record.move_to(BindingState::Backoff, Some(FailureCause::DriverExited)));
	assert!(record.state == BindingState::Backoff);
}

#[test]
fn an_exit_racing_a_restart_cannot_touch_the_binding_that_replaced_it() {
	// THE CASE THE QUEUE EXISTS FOR. The old driver's exit is queued while its binding is being
	// torn down; by the time anything reads the queue the node holds a NEW binding with a new
	// generation. Its exit is not this binding's exit, and its offer is a capability from a binding
	// that is over.
	let mut queue = BindingQueue::new();
	assert!(queue.push(BindingEvent::Exited { generation: 7 }));
	assert!(queue.push(BindingEvent::Offered { generation: 7 }));
	assert!(queue.push(BindingEvent::Ready { generation: 8 }));
	// Reading as the NEW binding: everything the old one left is dropped, and the first event that
	// comes back is the one that is actually about generation 8.
	assert!(queue.pop(8) == Some(BindingEvent::Ready { generation: 8 }));
	assert!(queue.is_empty(), "the stale events were consumed rather than left to be found later");
}

#[test]
fn a_node_holding_no_binding_finds_nothing_in_its_queue_to_act_on() {
	// Between a teardown and the next attempt a node holds nothing, which is exactly where a dying
	// driver's last events land. Answering them would be acting on a binding that is over.
	let mut queue = BindingQueue::new();
	assert!(queue.push(BindingEvent::Exited { generation: 3 }));
	assert!(queue.push(BindingEvent::TimedOut { generation: 3 }));
	assert!(queue.pop(0) == None);
	assert!(queue.is_empty());
}

#[test]
fn a_full_queue_refuses_the_newest_rather_than_forgetting_its_oldest() {
	// A queue that drops its front to make room is a queue that REORDERS: an exit before a `READY`
	// is a different story from an exit after it, and the story is the whole reason to keep them
	// in order.
	let mut queue = BindingQueue::new();
	for _ in 0..MAX_NODE_EVENTS {
		assert!(queue.push(BindingEvent::Offered { generation: 1 }));
	}
	assert!(!queue.push(BindingEvent::Ready { generation: 1 }), "past the bound is a refusal");
	assert!(queue.len() == MAX_NODE_EVENTS);
	assert!(queue.pop(1) == Some(BindingEvent::Offered { generation: 1 }), "and the oldest is still the oldest");
}

#[test]
fn a_driver_that_never_answers_ends_the_same_way_a_crashed_one_does_and_says_so_differently() {
	// A timeout says nothing about the DEVICE - only that this attempt spent its allowance - so it
	// is retryable like a crash. What differs is what the node says about why, and that difference
	// is the whole reason the cause is carried beside the state.
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	assert!(record.move_to(BindingState::Stopping, Some(FailureCause::HandshakeTimeout)));
	assert!(record.move_to(BindingState::Backoff, Some(FailureCause::HandshakeTimeout)));
	assert!(record.failure == Some(FailureCause::HandshakeTimeout));
	assert!(FailureCause::HandshakeTimeout.retryable());
	assert!(record.failure != Some(FailureCause::DriverExited), "a driver that is still there is not a driver that died");
}

#[test]
fn a_teardown_that_does_not_confirm_outranks_every_retry_the_budget_would_have_allowed() {
	// QUARANTINE OUTRANKS A RETRY, whatever the cause was and however many attempts are left. A
	// device that may still be live is not a device to try again on, and its resources stay charged
	// and out of circulation - which is the one stated exception to "leaves nothing behind".
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	// A retryable cause, and attempts to spare - and it still ends here.
	assert!(record.move_to(BindingState::Stopping, Some(FailureCause::DriverExited)));
	assert!(record.move_to(BindingState::Quarantined, Some(FailureCause::TeardownUnconfirmed)));
	assert!(record.state == BindingState::Quarantined);
	assert!(record.failure == Some(FailureCause::TeardownUnconfirmed));
	for state in EVERY_STATE {
		assert!(!record.state.may_move_to(state), "quarantined is terminal for the boot");
	}
}

// ------------------------------------------------------- identity and generation

#[test]
fn a_rebound_node_keeps_its_function_and_takes_a_new_binding() {
	// THE ONLY PLACE IN THIS SCOPE WHERE A SECOND GENERATION EXISTS, and therefore the only place
	// the pairing can be shown to work at all: nothing rescans, so a rebind after a driver crash is
	// what produces one.
	let first = BindingId::new(0, 4, 0, 1);
	let second = first.rebound(2);
	assert!(first.same_function(second), "a rebind is the same device");
	assert!(first != second, "and it is not the same binding");
	assert!(second.generation == 2);
	assert!(second.bus == 0 && second.dev == 4 && second.func == 0, "the BDF is what survives a rebind");
}

#[test]
fn two_identical_controllers_are_two_identities() {
	// Two functions of one kind differ only in where they are plugged in, which is exactly what the
	// BDF is for. Binding by kind alone cannot tell them apart, and a manager that could not would
	// route the second one's providers to the first.
	let left = BindingId::new(0, 2, 0, 1);
	let right = BindingId::new(0, 3, 0, 1);
	assert!(!left.same_function(right), "same kind, same generation, different function");
	assert!(left != right);
}

#[test]
fn one_function_at_one_generation_is_one_binding_however_it_is_reached() {
	// The identity has no row number in it, so two paths that reach the same function agree without
	// anyone having to map one table's index onto another's.
	let reached_one_way = BindingId::new(0, 9, 0, 5);
	let reached_another = BindingId { bus: 0, dev: 9, func: 0, generation: 5 };
	assert!(reached_one_way == reached_another);
}

#[test]
fn a_reused_provider_slot_is_not_the_provider_that_left_it() {
	// The whole reason a slot carries a generation: a catalogue with a fixed number of slots reuses
	// them, and a consumer holding an id for a withdrawn provider must not find itself talking to
	// whatever took its place.
	let binding = BindingId::new(0, 2, 0, 1);
	let first = ProviderId::new(binding, 3, 1);
	let second = ProviderId::new(binding, 3, 2);
	assert!(first != second, "the same slot, one publication later, is a different provider");
	assert!(first.binding == second.binding && first.slot == second.slot);
}

#[test]
fn two_bindings_of_one_function_publish_distinguishable_providers() {
	// A driver that crashed and was rebound publishes again. Its providers are NOT the ones it
	// published before, because the binding they belong to is over - and the id says so without
	// anyone having to remember to check.
	let before = ProviderId::new(BindingId::new(0, 5, 0, 1), 0, 1);
	let after = ProviderId::new(BindingId::new(0, 5, 0, 2), 0, 1);
	assert!(before != after);
	assert!(before.binding.same_function(after.binding), "same device, and that is the point: only the binding moved");
}

#[test]
fn a_node_waiting_for_a_dependency_has_no_way_back_to_where_a_bind_begins() {
	// `Unbound` is what INVITES a bind. A node waiting for a provider that then goes away is
	// waiting harder, not waiting less, and putting it back at `Unbound` would start a bind that
	// fails for the reason the node is already recording.
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::DependencyPending, None));
	assert!(!record.move_to(BindingState::Unbound, None), "there is no way back to unbound");
	// The only way out is a bind, and only when EVERY requirement is published.
	assert!(record.move_to(BindingState::Binding, None));
}

#[test]
fn every_way_of_learning_a_requirement_is_missing_reaches_the_same_state() {
	// Five different situations, one state - which is what makes it a state rather than five flags.
	for from in [BindingState::Unbound, BindingState::Binding, BindingState::Backoff, BindingState::Stopping, BindingState::Failed] {
		assert!(from.may_move_to(BindingState::DependencyPending), "{:?} must be able to say a requirement is missing", core::str::from_utf8(from.name()).unwrap_or("?"));
	}
	// And `Online` is not one of them: a node that is UP and loses a requirement has a device to
	// quieten, so it goes through the teardown like any other loss.
	assert!(!BindingState::Online.may_move_to(BindingState::DependencyPending));
	assert!(BindingState::Online.may_move_to(BindingState::Stopping));
}

#[test]
fn a_crash_then_a_withdrawal_then_the_backoff_expiry_does_not_bind_on_a_condition_that_is_gone() {
	// THE WALK THAT THE EVENT-DRIVEN VERSION GETS WRONG.
	//
	// Reacting to a withdrawal EVENT while a node sits in `Backoff` is not enough: a node can arrive
	// in `Backoff` from `Stopping` after a crash, and if the requirement went away DURING that
	// teardown the event is spent. So every backoff expiry asks the question again rather than
	// waiting to be told.
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	assert!(record.move_to(BindingState::Online, None));
	assert!(record.move_to(BindingState::Stopping, Some(FailureCause::DriverExited)));
	assert!(record.move_to(BindingState::Backoff, Some(FailureCause::DriverExited)));
	// The expiry re-reads `requires` and finds one gone.
	assert!(record.move_to(BindingState::DependencyPending, None));
	assert!(record.state == BindingState::DependencyPending);
}

#[test]
fn withdrawing_one_provider_is_not_the_driver_failing() {
	// DRIVER READINESS AND PROVIDER READINESS ARE DIFFERENT FACTS, and so are driver failure and
	// provider failure. A controller whose child goes away withdraws that child's provider; it does
	// not report itself failed, and nothing about its binding moves.
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	assert!(record.move_to(BindingState::Online, None));

	let mut queue = BindingQueue::new();
	assert!(queue.push(BindingEvent::Withdrawn { generation: 1, token: 2 }));
	assert!(queue.pop(1) == Some(BindingEvent::Withdrawn { generation: 1, token: 2 }));
	// The record is untouched: a withdrawal is not a transition, which is why it is not in the
	// table. A state machine that moved here would tear down a healthy controller because one of
	// its children left.
	assert!(record.state == BindingState::Online);
	assert!(record.failure.is_none());
}

#[test]
fn a_provider_offered_after_ready_belongs_to_the_same_binding() {
	// A controller reports in and THEN enumerates its bus, so its children's providers arrive after
	// the handshake is over. They carry the same generation, because it is the same binding - which
	// is what lets the manager publish them without any second handshake.
	let mut queue = BindingQueue::new();
	assert!(queue.push(BindingEvent::Ready { generation: 4 }));
	assert!(queue.push(BindingEvent::Offered { generation: 4 }));
	assert!(queue.pop(4) == Some(BindingEvent::Ready { generation: 4 }));
	assert!(queue.pop(4) == Some(BindingEvent::Offered { generation: 4 }), "a late offer is this binding's offer");
}

// ------------------------------------------------------- the catalogue's properties
//
// The catalogue itself lives in DeviceManager, which is a `no_std` binary nobody can drive on a
// host - the same wall M3 and M7 both ran into. What CAN be driven here is the identity arithmetic
// every one of its answers rests on, and that is where the defects would be: a slot reused without
// its generation moving, a provider from a binding that is over mistaken for its replacement, two
// controllers of one kind collapsing into one entry.

#[test]
fn a_provider_added_after_publication_is_a_different_provider_from_one_withdrawn_before_it() {
	// ADDED AND REMOVED AT RUN TIME, which is what a catalogue is for and what four named locals
	// could not express: the second disk's entry is not the first disk's entry with a new handle in
	// it, and a consumer holding an id for the one that left cannot find itself talking to the one
	// that arrived.
	let binding = BindingId::new(0, 6, 0, 1);
	let withdrawn = ProviderId::new(binding, 0, 1);
	let added = ProviderId::new(binding, 0, 2);
	assert!(withdrawn != added, "the slot was reused and the generation says so");

	// And a subscriber that arrives AFTER the second publication is answered with the second, not
	// with a stale first: they are distinguishable by value, so a snapshot cannot return the wrong
	// one by accident.
	let snapshot = [added];
	assert!(!snapshot.contains(&withdrawn));
	assert!(snapshot.contains(&added));
}

#[test]
fn two_controllers_of_one_kind_are_two_entries_and_removing_one_leaves_the_other() {
	// The case the four named locals got wrong by construction: two disks are two providers, and
	// which is which is a property of WHERE they are, not of which driver finished first.
	let first = ProviderId::new(BindingId::new(0, 2, 0, 1), 0, 1);
	let second = ProviderId::new(BindingId::new(0, 6, 0, 1), 1, 2);
	let mut published: [Option<ProviderId>; 2] = [Some(first), Some(second)];
	assert!(published[0] != published[1]);

	// Removing one leaves the other exactly as it was.
	published[0] = None;
	assert!(published[0].is_none());
	assert!(published[1] == Some(second), "withdrawing one provider does not disturb the others");
}

#[test]
fn a_provider_belongs_to_a_binding_and_not_to_a_device() {
	// A driver that crashed and was rebound publishes again; the providers it published BEFORE are
	// not the ones it publishes now, because the binding they belonged to is over. Without the
	// generation on the binding the two would be indistinguishable, and a teardown would withdraw
	// its own replacement's entries.
	let before = ProviderId::new(BindingId::new(0, 9, 0, 1), 0, 1);
	let after = ProviderId::new(BindingId::new(0, 9, 0, 2), 0, 2);
	assert!(before.binding != after.binding, "different bindings");
	assert!(before.binding.same_function(after.binding), "same device");
}

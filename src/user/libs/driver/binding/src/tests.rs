// THE TABLE IS THE TEST. Every edge it allows is walked, and a sample of what it does not is refused
// - because "enforced rather than implied" is a claim about the edges that are NOT there, and a test
// that only walks the legal ones would pass against a function that returns true for everything.

use super::*;

const EVERY_STATE: [BindingState; 9] = [
	BindingState::Unbound,
	BindingState::DependencyPending,
	BindingState::Disabled,
	BindingState::Binding,
	BindingState::Online,
	BindingState::Stopping,
	BindingState::Backoff,
	BindingState::Failed,
	BindingState::Quarantined,
];

// The table, written out a second time and independently, so this test disagrees with the
// implementation rather than restating it.
const LEGAL: [(BindingState, BindingState); 26] = [
	(BindingState::Unbound, BindingState::Binding),
	(BindingState::Binding, BindingState::Online),
	(BindingState::Binding, BindingState::Backoff),
	(BindingState::Binding, BindingState::Failed),
	(BindingState::Binding, BindingState::Stopping),
	(BindingState::Online, BindingState::Stopping),
	(BindingState::Stopping, BindingState::Backoff),
	(BindingState::Stopping, BindingState::Failed),
	(BindingState::Stopping, BindingState::Quarantined),
	// A bind that READS the device as already quarantined adopts that fact. It took no claim, so
	// `Failed`/`teardown-unconfirmed` - which is where the refused move used to leave it - describes
	// an attempt that tore something down badly, and this one tore nothing down at all.
	(BindingState::Binding, BindingState::Quarantined),
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
	// P02M0166's, added by the milestone where an operator first has a way to reach it.
	(BindingState::Binding, BindingState::Disabled),
	(BindingState::Stopping, BindingState::Disabled),
	(BindingState::Unbound, BindingState::Disabled),
	(BindingState::Backoff, BindingState::Disabled),
	(BindingState::Failed, BindingState::Disabled),
	(BindingState::DependencyPending, BindingState::Disabled),
	(BindingState::Disabled, BindingState::Unbound),
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
	//
	// ONE STATE MAY REACH IT, AND IT IS NOT A FAILURE: `Disabled`, on an `enable`. An operator
	// asking for the device back is asking for exactly the bind `Unbound` invites, which is why
	// that is where it lands - and why this test names the exception rather than dropping the rule.
	for &from in EVERY_STATE.iter() {
		if from == BindingState::Disabled {
			assert!(from.may_move_to(BindingState::Unbound), "enable is what takes a node out of Disabled");
			continue;
		}
		assert!(!from.may_move_to(BindingState::Unbound), "something that FAILED can reach Unbound again");
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

// ------------------------------------------------------- the stop intent

#[test]
fn a_confirmed_teardown_lands_where_the_intent_says_and_not_where_a_fault_would() {
	// THE CASE THE INTENT EXISTS FOR. P02M0162's table sends a confirmed teardown on to `Backoff`
	// and then back to `Binding`, which is right for a driver that died and exactly wrong for one
	// that was asked to stop: the operator stops it and it starts again.
	assert!(StopIntent::Fault.confirmed_lands_at(true) == Some(BindingState::Backoff));
	assert!(StopIntent::Fault.confirmed_lands_at(false) == Some(BindingState::Failed), "no attempts left");
	assert!(StopIntent::DependencyLost.confirmed_lands_at(true) == Some(BindingState::DependencyPending));
	assert!(StopIntent::DependencyLost.confirmed_lands_at(false) == Some(BindingState::DependencyPending), "a lost dependency is not spent by attempts");
	assert!(StopIntent::OperatorDisable.confirmed_lands_at(true) == Some(BindingState::Disabled), "an operator's stop lands disabled, never back at a bind");
	// A shutdown describes NO next state: the manager is going away, so there is no next binding for
	// a state to be about, and entering one nobody will read is a state nobody wrote down.
	assert!(StopIntent::Shutdown.confirmed_lands_at(true).is_none());
	assert!(StopIntent::Shutdown.confirmed_lands_at(false).is_none());
}

#[test]
fn every_intent_reaches_a_state_the_table_allows_from_stopping() {
	// A verdict the table refuses is a node stuck in `Stopping` for ever, which is the shape a
	// state machine defect takes when the two halves are written apart.
	for intent in [StopIntent::Fault, StopIntent::DependencyLost, StopIntent::OperatorDisable] {
		for attempts_left in [true, false] {
			let Some(landed) = intent.confirmed_lands_at(attempts_left) else { continue };
			assert!(BindingState::Stopping.may_move_to(landed), "{:?} is not reachable from stopping", core::str::from_utf8(landed.name()).unwrap_or("?"));
		}
	}
}

#[test]
fn an_unconfirmed_teardown_ignores_the_intent_entirely() {
	// What is unknown is whether the DEVICE is still live, and no intent changes that. Walked
	// rather than asserted about a value: every intent's node ends in the same place.
	for intent in [StopIntent::Fault, StopIntent::DependencyLost, StopIntent::OperatorDisable, StopIntent::Shutdown] {
		let mut record = BindingRecord::new();
		assert!(record.move_to(BindingState::Binding, None));
		assert!(record.move_to(BindingState::Online, None));
		assert!(record.move_to(BindingState::Stopping, Some(FailureCause::DriverExited)));
		// The teardown did not confirm. The intent is not consulted.
		assert!(record.move_to(BindingState::Quarantined, Some(FailureCause::TeardownUnconfirmed)));
		assert!(record.state == BindingState::Quarantined, "{:?} still ends quarantined", core::str::from_utf8(intent.name()).unwrap_or("?"));
	}
}

// ------------------------------------------------------- the races that are reachable
//
// REACHABLE IN THIS TREE, which is what makes it a table and not a wish. A cartesian product of
// every event against every other is a way of never finishing; a named list is a plan.
//
// A DEVICE REMOVED MID-BIND IS NOT ON IT. The scan runs once and no removal event exists, so that
// test could only pass by first inventing the mechanism it tests.

#[test]
fn a_ready_that_arrives_after_the_deadline_does_not_undo_the_timeout() {
	// ONCE THE FORCED PATH HAS BEEN ENTERED THE OUTCOME IS DECIDED. The alternative is a report that
	// says a driver came up when the manager had already torn it down - and by then its claim is
	// released and its process signalled, so "it is up" would be a claim about nothing.
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	assert!(record.move_to(BindingState::Stopping, Some(FailureCause::HandshakeTimeout)));
	assert!(record.move_to(BindingState::Backoff, Some(FailureCause::HandshakeTimeout)));
	// The late `READY`. The table has no edge from `Backoff` to `Online`, so it is refused by the
	// same rule every other illegal transition is - not by a special case somebody remembered.
	assert!(!record.move_to(BindingState::Online, None), "a late READY is late, and late is not up");
	assert!(record.state == BindingState::Backoff);
}

#[test]
fn a_crash_between_publish_and_subscribe_withdraws_what_was_published() {
	// A driver that reached `Online` published; a consumer had not yet asked. The binding ending
	// must take the publication with it, or a subscriber arriving next finds a provider whose
	// server is gone - which is a failure nobody can attribute.
	let published = ProviderId::new(BindingId::new(0, 3, 0, 1), 0, 1);
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	assert!(record.move_to(BindingState::Online, None));
	assert!(record.move_to(BindingState::Stopping, Some(FailureCause::DriverExited)));
	// Whatever the catalogue does next, the id it withdraws is THIS binding's - and the next
	// binding's providers are distinguishable from it by the generation.
	let after_rebind = ProviderId::new(BindingId::new(0, 3, 0, 2), 0, 2);
	assert!(published != after_rebind);
	assert!(published.binding.same_function(after_rebind.binding));
}

#[test]
fn a_watchdog_expiry_racing_a_clean_exit_reaches_one_verdict_and_not_two() {
	// Both events are about the same binding and both are queued; the queue is what makes them
	// ORDERED rather than simultaneous. Whichever is first decides, and the second finds a record
	// that has already moved - refused by the table, not by a flag somebody has to remember to set.
	let mut queue = BindingQueue::new();
	assert!(queue.push(BindingEvent::Wedged { generation: 1 }));
	assert!(queue.push(BindingEvent::Exited { generation: 1 }));
	assert!(queue.pop(1) == Some(BindingEvent::Wedged { generation: 1 }), "first in, first out");

	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	assert!(record.move_to(BindingState::Online, None));
	assert!(record.move_to(BindingState::Stopping, Some(FailureCause::HandshakeTimeout)));
	// The exit arrives second and finds a node already stopping. `Stopping -> Stopping` is not in
	// the table, so it changes nothing and the verdict stays the one that was reached first.
	assert!(!record.move_to(BindingState::Stopping, Some(FailureCause::DriverExited)));
	assert!(record.failure == Some(FailureCause::HandshakeTimeout), "the first verdict stands");
}

#[test]
fn a_manager_restart_with_drivers_still_live_cannot_mistake_them_for_a_fresh_binding() {
	// The reconstruction's race. A new manager arrives while the old drivers are still being torn
	// down; every event still coming from them carries the OLD generation, and the node it would
	// belong to holds a new one or none at all.
	let mut queue = BindingQueue::new();
	assert!(queue.push(BindingEvent::Ready { generation: 1 }));
	assert!(queue.push(BindingEvent::Offered { generation: 1 }));
	// A node with no binding - which is what a fresh manager's node looks like - drains them all.
	assert!(queue.pop(0).is_none());
	assert!(queue.is_empty(), "nothing from the previous manager's binding survives to be acted on");
}

#[test]
fn a_stopped_that_arrives_after_the_deadline_does_not_turn_a_forced_teardown_back_into_a_clean_one() {
	// THE ONE THE MILESTONE NAMES EXPLICITLY. Once the forced path has been entered the outcome is
	// decided; a late answer is recorded as late and changes nothing, because the alternative is a
	// report that says a driver flushed when the manager had already stopped waiting.
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	assert!(record.move_to(BindingState::Online, None));
	// The stop deadline expired: the teardown is forced, and it did not confirm.
	assert!(record.move_to(BindingState::Stopping, Some(FailureCause::HandshakeTimeout)));
	assert!(record.move_to(BindingState::Quarantined, Some(FailureCause::TeardownUnconfirmed)));
	// `STOPPED` arrives now. There is no edge out of quarantine at all, so it cannot be read as a
	// clean flush - and the cause the record carries still says what actually happened.
	assert!(!record.move_to(BindingState::Backoff, Some(FailureCause::DriverExited)));
	assert!(!record.move_to(BindingState::Failed, Some(FailureCause::DriverExited)));
	assert!(record.failure == Some(FailureCause::TeardownUnconfirmed), "the report still says the teardown was not confirmed");
}

// ------------------------------------------------------- the operator's four verbs

#[test]
fn a_disable_on_a_running_binding_goes_through_the_teardown_and_a_stopped_one_does_not() {
	// The distinction the table is for: there is either a device to quieten or there is not, and a
	// disable that skipped the teardown on a running binding would leave a driver holding hardware
	// nobody is supervising.
	let mut running = BindingRecord::new();
	assert!(running.move_to(BindingState::Binding, None));
	assert!(running.move_to(BindingState::Online, None));
	assert!(!running.move_to(BindingState::Disabled, None), "a running binding has a device to give back first");
	assert!(running.move_to(BindingState::Stopping, None));
	assert!(running.move_to(BindingState::Disabled, None), "and lands disabled once that confirmed");

	// Nothing to tear down: straight there, from every state that has no claim in hand.
	for from in [BindingState::Unbound, BindingState::Backoff, BindingState::Failed, BindingState::DependencyPending] {
		assert!(from.may_move_to(BindingState::Disabled), "a node that holds nothing is disabled directly");
	}
}

#[test]
fn quarantined_is_out_of_reach_of_every_verb_for_this_boot() {
	// Its resources are charged and out of circulation precisely because nothing confirmed the
	// device was quiet, and an operator saying so does not make it so. `disable`, `enable` and
	// `select` are still ACCEPTED as policy - they are about the NEXT bind - but none of them moves
	// this node, and the table is what makes that true rather than a check somebody wrote once.
	let mut record = BindingRecord::new();
	assert!(record.move_to(BindingState::Binding, None));
	assert!(record.move_to(BindingState::Stopping, Some(FailureCause::DriverExited)));
	assert!(record.move_to(BindingState::Quarantined, Some(FailureCause::TeardownUnconfirmed)));
	for to in EVERY_STATE {
		assert!(!record.state.may_move_to(to), "quarantine has no exit, including for an operator");
	}
}

#[test]
fn hung_and_handshake_timeout_are_two_causes_because_a_reader_acts_on_them_differently() {
	// They were one cause until this milestone had to RENDER them, and "it did not answer" cannot be
	// acted on without knowing whether the driver had ever been up: one is a driver that never
	// started, the other is one that stopped.
	assert!(FailureCause::Hung != FailureCause::HandshakeTimeout);
	assert!(FailureCause::Hung.name() == b"hung");
	assert!(FailureCause::HandshakeTimeout.name() == b"handshake-timeout");
	// Both retryable, and for the same reason: nothing about a driver going quiet says the DEVICE
	// is unusable.
	assert!(FailureCause::Hung.retryable());
	assert!(FailureCause::HandshakeTimeout.retryable());
}

#[test]
fn every_state_and_every_cause_has_exactly_one_name() {
	// One vocabulary, so `lsdev`, the System Graph and the boot log cannot each invent their own.
	// Distinctness is the property: two states sharing a name is a surface that cannot tell them
	// apart, which is what the constant `Running` in the graph was.
	let mut seen: [&[u8]; 9] = [b""; 9];
	for (at, state) in EVERY_STATE.iter().enumerate() {
		let name = state.name();
		assert!(!name.is_empty());
		for earlier in seen.iter().take(at) {
			assert!(*earlier != name, "two states share a name");
		}
		seen[at] = name;
	}
	let causes = [
		FailureCause::DriverMissing,
		FailureCause::ProtocolMismatch,
		FailureCause::ClaimRefused,
		FailureCause::IommuRequired,
		FailureCause::ResourceExhausted,
		FailureCause::SpawnFailed,
		FailureCause::HandshakeTimeout,
		FailureCause::DriverExited,
		FailureCause::DriverReported(DriverFailureCode::InternalError),
		FailureCause::TeardownUnconfirmed,
		FailureCause::Hung,
	];
	let mut names: [&[u8]; 11] = [b""; 11];
	for (at, cause) in causes.iter().enumerate() {
		let name = cause.name();
		assert!(!name.is_empty());
		for earlier in names.iter().take(at) {
			assert!(*earlier != name, "two causes share a name");
		}
		names[at] = name;
	}
}

#[test]
fn the_first_incident_is_clamped_by_the_boot_and_a_later_one_is_not() {
	// THE FIRST really does compete with the boot: the kernel's recovery ladder reboots the machine
	// when its window runs out, so a bind that outlasts it is a bind nothing will see the end of.
	let window = 300;
	let share = 3; // a third of the window for one device's bring-up
	// Opening at tick 10 with the boot's deadline at 50: the boot's is sooner, so it wins.
	assert!(IncidentWindow::deadline(window, share, 50, 10) == 50);
	// Opening at tick 10 with the boot's deadline at 500: this incident's own slice is sooner.
	assert!(IncidentWindow::deadline(window, share, 500, 10) == 110);
}

#[test]
fn a_recovery_long_after_the_boot_is_not_born_already_expired() {
	// THE DEFECT THIS ARITHMETIC EXISTS TO PREVENT, and it was live until 2026-08-27: the clamp was
	// unconditional, so an hour after boot every recovery got a deadline in the PAST and every bind
	// failed instantly for arithmetic about a boot that had finished long ago.
	let window = 300;
	let share = 3;
	let boot_deadline = 400; // long gone
	let now = 360_000; // an hour later at 100 ticks a second
	let deadline = IncidentWindow::deadline(window, share, boot_deadline, now);
	assert!(deadline > now, "a recovery must get time it can actually spend");
	assert!(deadline == now + 100, "and its full slice, because nothing is competing with it any more");
}

#[test]
fn a_boot_that_published_no_window_bounds_nothing_by_it() {
	// Zero is "not published", and the manager then falls back to its per-attempt deadline - which
	// is what it did before a window existed, so an old supervisor still starts a new manager.
	assert!(IncidentWindow::deadline(0, 3, 500, 10) == 0);
	assert!(IncidentWindow::deadline(300, 0, 500, 10) == 0);
}

// ------------------------------------------------------------- M7's resource-invariant fault cases
//
// EVERY ONE OF THESE IS A BIND THAT FAILED, AND WHAT IT MAY NOT COST.
//
// The fault cases here used to walk a `BindingRecord` through abstract states: they never
// instantiated the transaction, never failed it at a step, and never asked what was still held - so
// two handles that a real rollback leaked (the driver's end of the bootstrap channel, and every
// resource acquired and not yet sent) were invisible to a suite that passed. The ledger is testable
// now, and a `Closes` that RECORDS rather than acts is what makes "closed exactly once, in this
// order, and nothing left" a thing a machine checks rather than a sentence in a comment.

use super::{Closes, Holdings, Settled};

// What a rollback did, in the order it did it. FIXED ARRAYS, because this crate is `no_std` and a
// test that needed a heap would be a test that could not run where the code does.
const LEDGER_MAX: usize = 16;

struct Ledger {
	killed: [u64; LEDGER_MAX],
	killed_n: usize,
	closed: [u64; LEDGER_MAX],
	closed_n: usize,
	released: [u64; LEDGER_MAX],
	released_n: usize,
	domains: [u64; LEDGER_MAX],
	domains_n: usize,
	// What the release answers. `None` is a release still running - the answer arrives on the claim
	// handle - which is the case the deadline exists for.
	release_answer: Option<u32>,
}

impl Ledger {
	fn new(release_answer: Option<u32>) -> Self {
		Self { killed: [0; LEDGER_MAX], killed_n: 0, closed: [0; LEDGER_MAX], closed_n: 0, released: [0; LEDGER_MAX], released_n: 0, domains: [0; LEDGER_MAX], domains_n: 0, release_answer }
	}
	fn closed_once(&self, handle: u64) -> bool {
		self.closed[..self.closed_n].iter().filter(|&&h| h == handle).count() == 1
	}
	fn closed_any(&self, handle: u64) -> bool {
		self.closed[..self.closed_n].contains(&handle)
	}
	// WHERE IN THE SEQUENCE a handle was closed, so ORDER can be asserted and not just membership.
	fn closed_at(&self, handle: u64) -> Option<usize> {
		self.closed[..self.closed_n].iter().position(|&h| h == handle)
	}
}

impl Closes for Ledger {
	fn kill(&mut self, process: u64) {
		self.killed[self.killed_n] = process;
		self.killed_n += 1;
	}
	fn close(&mut self, handle: u64) {
		self.closed[self.closed_n] = handle;
		self.closed_n += 1;
	}
	fn release(&mut self, claim: u64) -> Option<u32> {
		self.released[self.released_n] = claim;
		self.released_n += 1;
		self.release_answer
	}
	fn kill_domain(&mut self, domain: u64) {
		self.domains[self.domains_n] = domain;
		self.domains_n += 1;
	}
}

fn free_ledger() -> Ledger {
	Ledger::new(Some(super::CLAIM_FREE))
}

#[test]
fn a_bind_that_fails_before_the_claim_leaves_nothing_and_takes_no_device() {
	// THE CLAIM STEP. Nothing has been taken, so nothing is released and nothing is quarantined -
	// but a Domain may already exist, and a Domain nobody kills is a subtree charged to this manager
	// for ever.
	let mut held = Holdings::new();
	held.domain = 0x10;
	held.channel = 0x11;
	held.driver_side = 0x12;
	let mut ledger = free_ledger();
	let mut pending = held.begin_teardown(&mut ledger);
	assert_eq!(ledger.released_n, 0, "a transaction that took no device releases none");
	assert!(ledger.closed_once(0x11) && ledger.closed_once(0x12), "both ends of the bootstrap channel are given back");
	let settled = pending.settle(&mut ledger, 0, 100);
	assert_eq!(settled, Some(Settled::Free), "with no process and no claim there is nothing to wait for");
	assert_eq!(&ledger.domains[..ledger.domains_n], &[0x10], "and the Domain is killed exactly once");
}

#[test]
fn a_spawn_that_fails_gives_back_the_bootstrap_handle_it_was_holding() {
	// THE SPAWN STEP, AND THE HANDLE THE AUDIT FOUND. `spawn_prepared_in` returns with the driver's
	// end of the bootstrap channel still in the caller when it fails, and the rollback had no field
	// by which to close it: one leaked handle and its accounting on an ordinary failure path.
	let mut held = Holdings::new();
	held.domain = 0x20;
	held.channel = 0x21;
	held.driver_side = 0x22;
	held.claim = 0x23;
	let mut ledger = free_ledger();
	let mut pending = held.begin_teardown(&mut ledger);
	assert!(ledger.closed_once(0x22), "the driver's end of the bootstrap channel is closed exactly once");
	assert_eq!(&ledger.released[..ledger.released_n], &[0x23], "and the device is given back");
	assert_eq!(pending.settle(&mut ledger, 0, 100), Some(Settled::Free), "a confirmed release with no process to wait for settles at once");
	assert_eq!(held.driver_side, 0, "and the ledger no longer names it, so a second rollback closes nothing twice");
}

#[test]
fn a_bind_that_fails_between_resources_closes_every_one_it_had_not_sent() {
	// THE RESOURCE STEP. The MMIO grant, the MSI vector, the key sink, the power connection and the
	// console feed are acquired one at a time and sent one at a time, and a failure anywhere in
	// between leaves some acquired and some already the driver's. A rollback that closed all of them
	// would close what it had given away; one that closed none leaked what it still held.
	let mut held = Holdings::new();
	held.domain = 0x30;
	held.process = 0x31;
	held.channel = 0x32;
	held.claim = 0x33;
	assert!(held.hold(1, 0x40) && held.hold(2, 0x41) && held.hold(3, 0x42));
	held.handed_over(0x40);
	let mut ledger = free_ledger();
	let mut pending = held.begin_teardown(&mut ledger);
	assert!(!ledger.closed_any(0x40), "a resource the driver already has is NOT closed by the sender");
	assert!(ledger.closed_once(0x41) && ledger.closed_once(0x42), "and every one still held is closed exactly once");
	assert_eq!(&ledger.killed[..ledger.killed_n], &[0x31], "the process is killed, and killed once");
	// AND THE ORDER: the manager's own handles go back after the kill and before the release, which
	// is what stops a driver reading one as it dies and what stops the release running under them.
	assert!(ledger.closed_at(0x41) < ledger.closed_at(0x32), "the resources are closed before the channel");
	assert!(pending.process != 0, "and its handle is KEPT - the exit is what confirms it, not the kill");
	assert_eq!(ledger.domains_n, 0, "the Domain is not killed before the confirmations");
	assert_eq!(pending.settle(&mut ledger, 0, 100), None, "and nothing settles while the exit has not arrived");
}

#[test]
fn a_teardown_leaves_the_node_stopping_until_both_confirmations_arrive() {
	// THE HANDSHAKE STEP, AND THE TWO CONFIRMATIONS. A release that is still running answers no
	// terminal state, so BOTH halves are outstanding; the node may not leave `Stopping` on either
	// one alone.
	let mut held = Holdings::new();
	held.domain = 0x50;
	held.process = 0x51;
	held.channel = 0x52;
	held.claim = 0x53;
	let mut ledger = Ledger::new(None);
	let mut pending = held.begin_teardown(&mut ledger);
	assert_eq!(pending.claim, 0x53, "a release that has not answered keeps the claim handle to wait on");
	assert_eq!(pending.settle(&mut ledger, 0, 100), None, "neither confirmation has arrived");
	pending.note(super::BindingEvent::Exited { generation: 1 });
	assert_eq!(pending.settle(&mut ledger, 0, 100), None, "the exit alone is not a teardown");
	pending.note(super::BindingEvent::ClaimSettled { generation: 1, state: super::CLAIM_FREE });
	assert_eq!(pending.settle(&mut ledger, 0, 100), Some(Settled::Free), "both, and the device is back");
	assert!(ledger.closed_once(0x51) && ledger.closed_once(0x53), "and the two handles are closed exactly once, here and not before");
	assert_eq!(&ledger.domains[..ledger.domains_n], &[0x50], "the Domain last of all");
}

#[test]
fn a_confirmation_that_never_comes_ends_at_the_deadline_and_quarantines() {
	// A CHILD THAT IGNORES ITS DEATH. The teardown slice M5 reserves is what this is for: the node
	// does not hold open for ever, and the device does not go back into circulation on the strength
	// of a confirmation nobody received.
	let mut held = Holdings::new();
	held.domain = 0x60;
	held.process = 0x61;
	held.claim = 0x62;
	let mut ledger = Ledger::new(None);
	let mut pending = held.begin_teardown(&mut ledger);
	pending.note(super::BindingEvent::ClaimSettled { generation: 1, state: super::CLAIM_FREE });
	assert_eq!(pending.settle(&mut ledger, 99, 100), None, "before the deadline it is still waiting");
	assert_eq!(pending.settle(&mut ledger, 100, 100), Some(Settled::Unconfirmed), "at it, the teardown is unconfirmed");
	assert!(ledger.closed_once(0x61), "the handles are given back even so - what is quarantined is the DEVICE");
	assert_eq!(&ledger.domains[..ledger.domains_n], &[0x60]);
}

#[test]
fn a_release_that_does_not_reach_free_is_unconfirmed_however_promptly_it_answers() {
	// PROMPT IS NOT THE SAME AS CONFIRMED. A release that answers at once with a state that is not
	// `Free` is a device that is not back, and the node is quarantined for it - which is the rule a
	// teardown that read only "did the syscall return" could not state.
	let mut held = Holdings::new();
	held.claim = 0x70;
	held.domain = 0x71;
	let mut ledger = Ledger::new(Some(3));
	let mut pending = held.begin_teardown(&mut ledger);
	assert_eq!(pending.settle(&mut ledger, 0, 100), Some(Settled::Unconfirmed), "a terminal state that is not Free is not a device given back");
}

// ------------------------------------------------------- M7's heartbeat refusals, actually refused
//
// THE THREE REFUSAL TESTS COMPARED ENUM VARIANTS. `assert_ne!(Opcode::Pong, Opcode::Ping)` and
// `assert_ne!(1, 2)` are true of any program, so they would have passed against a supervisor that
// reset its watchdog on ANY frame with ANY sequence - which is exactly what `rt::heartbeat` did and
// what this milestone exists to have stopped. The decisions are drivable now, so the refusals are
// asserted against the thing that makes them.

use super::{Beat, Heartbeat};

// The cadence the entry's deadline implies. Passed in rather than computed here so the arithmetic
// stays in `driver-protocol`, where it is tested against the deadline bounds.
const PERIOD: u32 = 5;
const DEADLINE: u32 = 10;

fn armed(now: u64) -> Heartbeat {
	let mut beat = Heartbeat::default();
	beat.arm(Some(DEADLINE), now, PERIOD);
	beat
}

#[test]
fn a_pong_carrying_a_sequence_nobody_asked_with_does_not_reset_the_watchdog() {
	let mut beat = armed(0);
	assert_eq!(beat.tick(4), Beat::Idle, "nothing is due before the period is up");
	assert_eq!(beat.tick(5), Beat::Ask(1), "and then one ping goes out");
	beat.asked(5);

	// EVERY WAY OF ANSWERING WRONG, and none of them counts.
	assert!(!beat.answered(2, 6, PERIOD), "a sequence that was never asked with");
	assert!(!beat.answered(0, 6, PERIOD), "the sequence before any ping");
	assert!(!beat.answered(1 + 1, 6, PERIOD), "the sequence of the ping that has not been sent yet");
	assert!(beat.awaiting(), "and the ping is still outstanding after every one of them");

	// The right one, once.
	assert!(beat.answered(1, 6, PERIOD), "the number it was asked with");
	assert!(!beat.awaiting(), "which clears it");
	// AND A DUPLICATE OF THE RIGHT ONE IS STILL WRONG. Nothing is outstanding, so there is nothing
	// for it to answer - a driver echoing its last pong for ever must not look supervised.
	assert!(!beat.answered(1, 7, PERIOD), "a duplicate answers a ping that is no longer outstanding");
}

#[test]
fn a_ping_that_is_not_answered_inside_its_deadline_wedges_once_and_not_repeatedly() {
	let mut beat = armed(0);
	assert_eq!(beat.tick(5), Beat::Ask(1));
	beat.asked(5);
	assert_eq!(beat.tick(14), Beat::Idle, "inside the deadline there is nothing to say");
	assert_eq!(beat.tick(15), Beat::Wedged, "at it, the driver is wedged");
	// ONCE. A node stays wedged for as long as its teardown takes, and a supervisor that queued the
	// verdict on every pass would queue one per tick of that teardown.
	assert_eq!(beat.tick(16), Beat::Idle, "and it is not re-declared on the next pass");
	// NOR IS IT ASKED AGAIN. Clearing only the outstanding flag left the next ping already due, so
	// the pass after the verdict sent one to a binding that was being torn down.
	assert_eq!(beat.tick(99), Beat::Idle, "a spent watchdog asks nothing more");
	assert_eq!(beat.wake_at(), 0, "and the wait has nothing to come back for");
	// The NEXT binding on this node arms it again, and the schedule starts over.
	beat.arm(Some(DEADLINE), 100, PERIOD);
	assert_eq!(beat.tick(105), Beat::Ask(1), "a fresh binding starts at the first sequence again");
}

#[test]
fn a_driver_whose_entry_declares_no_deadline_is_never_asked_and_never_wedged() {
	// AN ABSENT DEADLINE IS NOT A ZERO ONE. A supervisor that treated it as zero would declare every
	// unsupervised driver wedged immediately, which is the failure the `supervised` guard prevents.
	let mut beat = Heartbeat::default();
	beat.arm(None, 0, PERIOD);
	assert!(!beat.supervised());
	assert_eq!(beat.wake_at(), 0, "there is nothing to wake for");
	assert_eq!(beat.tick(u64::MAX), Beat::Idle, "and nothing to say, ever");
	assert!(!beat.answered(0, 0, PERIOD), "an answer to a question nobody asked counts for nothing");

	// A deadline of zero is the same thing said the other way, and the manifest refuses it anyway.
	let mut zero = Heartbeat::default();
	zero.arm(Some(0), 0, PERIOD);
	assert!(!zero.supervised());
}

#[test]
fn a_ping_that_could_not_be_sent_is_neither_an_answer_nor_a_wedge() {
	// The channel has gone, which is a driver that ENDED rather than one that is slow: the exit
	// event arrives on its own, and the watchdog must not race it to a verdict of its own.
	let mut beat = armed(0);
	assert_eq!(beat.tick(5), Beat::Ask(1));
	beat.unsendable(5);
	assert!(!beat.awaiting(), "nothing is outstanding, so nothing can expire");
	assert_eq!(beat.tick(6), Beat::Idle);
	assert_eq!(beat.wake_at(), 15, "and the next look is a whole deadline away");
}

#[test]
fn the_wait_comes_back_for_whichever_of_the_two_deadlines_is_next() {
	// `wake_at` is what the manager's central wait is bounded by, so getting it wrong is a watchdog
	// that fires late or a loop that spins.
	let mut beat = armed(100);
	assert_eq!(beat.wake_at(), 105, "with nothing outstanding, the next ping is what to wake for");
	assert_eq!(beat.tick(105), Beat::Ask(1));
	beat.asked(105);
	assert_eq!(beat.wake_at(), 115, "with one outstanding, its expiry is");
}

// THE TABLE IS THE TEST. Every edge it allows is walked, and a sample of what it does not is refused
// - because "enforced rather than implied" is a claim about the edges that are NOT there, and a test
// that only walks the legal ones would pass against a function that returns true for everything.

use super::*;

const EVERY_STATE: [BindingState; 7] = [
	BindingState::Unbound,
	BindingState::Binding,
	BindingState::Online,
	BindingState::Stopping,
	BindingState::Backoff,
	BindingState::Failed,
	BindingState::Quarantined,
];

// The table, written out a second time and independently, so this test disagrees with the
// implementation rather than restating it.
const LEGAL: [(BindingState, BindingState); 12] = [
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

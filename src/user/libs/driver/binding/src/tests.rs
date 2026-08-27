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

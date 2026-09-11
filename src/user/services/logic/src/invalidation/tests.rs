//! The table, one row per authority.

use super::*;
use crate::tcp_bind::{BindMode, BindRefusal, BindTable, Binding, Local};

#[test]
fn an_unsent_operation_with_an_automatically_chosen_source_reselects_silently() {
	// NOTHING HAS LEFT THE MACHINE, so choosing again is not a change anyone can observe - and the
	// deadline is the one it was admitted with, not a fresh one.
	let outcome = on_invalidation(OperationState::UnsentAutomaticSource);
	assert_eq!(outcome, Outcome::Reselect);
	assert_eq!(outcome.reported(), None, "nothing is reported to the caller");
	assert!(outcome.keeps_deadline(), "a reselection is not a new operation");
}

#[test]
fn an_unsent_operation_whose_source_the_caller_named_fails_with_address_unavailable() {
	// THE ROW THAT SAID "RESELECT" FOR BOTH. An override that survives until the moment it matters
	// and is then discarded is worse than not having one: the caller is told the connection
	// succeeded and it went out of the address they were avoiding.
	let outcome = on_invalidation(OperationState::UnsentNamedSource);
	assert_eq!(outcome, Outcome::Fail);
	assert_eq!(outcome.reported(), Some(Error::AddressUnavailable));
	assert!(!outcome.keeps_deadline(), "it is over");
}

#[test]
fn a_wildcard_listener_survives_its_address_going_away() {
	// It is bound to a port and a mode, not to a route.
	let outcome = on_invalidation(OperationState::WildcardListener);
	assert_eq!(outcome, Outcome::Keep);
	assert_eq!(outcome.reported(), None);
	assert!(!outcome.releases_port(), "and it keeps its port");

	let mut table = BindTable::new();
	let wildcard = Binding { mode: BindMode::Ipv6Only, address: Local::V6([0; 16]), port: 8080 };
	let id = table.bind(wildcard).expect("a wildcard bind");
	assert_eq!(table.len(), 1);
	// The address went away; the claim did not.
	assert_eq!(table.claims(), &[(id, wildcard)]);
	assert_eq!(table.bind(wildcard), Err(BindRefusal::InUse), "the port is still held");
}

#[test]
fn a_listener_bound_to_a_specific_address_is_withdrawn_and_its_port_released() {
	// LEFT PUBLISHED IT COULD NEVER ACCEPT AGAIN, while holding a port against the binds that could.
	let outcome = on_invalidation(OperationState::SpecificListener);
	assert_eq!(outcome, Outcome::Withdraw);
	assert_eq!(outcome.reported(), Some(Error::AddressUnavailable), "delivered on the listener channel");
	assert!(outcome.releases_port());

	let mut table = BindTable::new();
	let mut address = [0u8; 16];
	address[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
	address[15] = 0x11;
	let specific = Binding { mode: BindMode::Ipv6Only, address: Local::V6(address), port: 8080 };
	let id = table.bind(specific).expect("a specific bind");
	// The address goes away and the withdrawal releases the port.
	assert!(table.unbind(id));
	assert!(table.is_empty());
	// WHICH A CALLER MAY THEN BIND AGAIN, in either shape, once the address returns.
	assert!(table.bind(specific).is_ok());
	assert!(table.unbind(id + 1));
	assert!(table.bind(Binding { mode: BindMode::Ipv6Only, address: Local::V6([0; 16]), port: 8080 }).is_ok());
}

#[test]
fn a_sent_request_awaiting_a_reply_fails_rather_than_retrying_as_a_new_tuple() {
	// DNS, SNTP, DHCP AND THE DIAGNOSTICS ALL SIT HERE. The peer will answer to the tuple it was
	// asked from; a silent change of source makes the answer unrecognisable and may make the peer
	// act twice.
	let outcome = on_invalidation(OperationState::AwaitingReply);
	assert_eq!(outcome, Outcome::Fail);
	assert_eq!(outcome.reported(), Some(Error::AddressUnavailable));
}

#[test]
fn a_connecting_tcp_attempt_is_retired_and_the_open_may_try_what_is_left() {
	// The handshake cannot complete from an address the machine no longer holds, but the OPEN has
	// not failed while a candidate remains - and the caller must not be told twice.
	let more = on_invalidation(OperationState::TcpConnecting { named_source: false, candidates_left: true });
	assert_eq!(more, Outcome::RetireAttempt { try_next: true });
	assert_eq!(more.reported(), None, "the open is still running");

	let last = on_invalidation(OperationState::TcpConnecting { named_source: false, candidates_left: false });
	assert_eq!(last, Outcome::RetireAttempt { try_next: false });
	assert_eq!(last.reported(), Some(Error::AddressUnavailable), "and now there is nothing left to try");
}

#[test]
fn a_caller_named_source_fails_the_whole_open_rather_than_one_attempt() {
	// NEITHER FALLBACK NOR RE-SELECTION MAY SUBSTITUTE ANOTHER SOURCE. The remaining candidates
	// would all have to be opened from an address the caller did not choose.
	for candidates_left in [true, false] {
		let outcome = on_invalidation(OperationState::TcpConnecting { named_source: true, candidates_left });
		assert_eq!(outcome, Outcome::Fail, "candidates_left={candidates_left}");
		assert_eq!(outcome.reported(), Some(Error::AddressUnavailable));
	}
}

#[test]
fn an_established_connection_is_closed_after_flushing_what_is_acknowledged() {
	// A CONNECTION IS ITS TUPLE. A new source is a new connection the peer knows nothing about, so
	// there is nothing to reselect to.
	let outcome = on_invalidation(OperationState::TcpEstablished);
	assert_eq!(outcome, Outcome::Close);
	assert_eq!(outcome.reported(), Some(Error::AddressUnavailable));
	assert!(!outcome.keeps_deadline());
}

#[test]
fn only_a_listener_withdrawal_releases_a_port() {
	// The port accounting is the part a caller can observe from outside, so it is worth stating
	// against every row rather than only the two that mention listeners.
	let every_state = [
		OperationState::UnsentAutomaticSource,
		OperationState::UnsentNamedSource,
		OperationState::AwaitingReply,
		OperationState::TcpConnecting { named_source: false, candidates_left: true },
		OperationState::TcpConnecting { named_source: true, candidates_left: false },
		OperationState::TcpEstablished,
		OperationState::WildcardListener,
		OperationState::SpecificListener,
	];
	for state in every_state {
		let outcome = on_invalidation(state);
		assert_eq!(outcome.releases_port(), state == OperationState::SpecificListener, "{state:?}");
	}
}

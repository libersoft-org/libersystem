//! Which reply is admissible in which phase, and what an admitted one does.

use super::*;

const XID: u32 = 0xdead_beef;
const CHADDR: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
const SERVER: [u8; 4] = [10, 0, 2, 2];
const OTHER: [u8; 4] = [10, 0, 2, 9];
const ADDRESS: [u8; 4] = [10, 0, 2, 15];

fn reply(message_type: u8, server: Option<[u8; 4]>, destination: Destination) -> Reply {
	Reply { message_type, xid: XID, chaddr: CHADDR, server, yiaddr: ADDRESS, destination }
}

fn selecting() -> Transaction {
	Transaction { phase: Phase::Selecting, ..Transaction::new(XID, CHADDR) }
}

fn requesting() -> Transaction {
	Transaction { phase: Phase::Requesting, server: Some(SERVER), requested: Some(ADDRESS), ..Transaction::new(XID, CHADDR) }
}

fn renewing() -> Transaction {
	Transaction { phase: Phase::Renewing, ..requesting() }
}

#[test]
fn a_foreign_transaction_or_client_is_refused_before_the_phase_is_consulted() {
	let txn = selecting();
	let mut foreign = reply(OFFER, Some(SERVER), Destination::Broadcast);
	foreign.xid = XID.wrapping_add(1);
	assert_eq!(txn.admit(&foreign), Admit::Refused(Refusal::ForeignTransaction));

	let mut other_client = reply(OFFER, Some(SERVER), Destination::Broadcast);
	other_client.chaddr = [0xaa; 6];
	assert_eq!(txn.admit(&other_client), Admit::Refused(Refusal::ForeignClient));
}

#[test]
fn selecting_takes_offers_and_nothing_else() {
	let txn = selecting();
	assert_eq!(txn.admit(&reply(OFFER, Some(SERVER), Destination::Broadcast)), Admit::SelectOffer);
	// A NAK IN SELECTING IS NOT ADMISSIBLE: there is no lease to reject and no server to reject it.
	assert_eq!(txn.admit(&reply(NAK, Some(SERVER), Destination::Broadcast)), Admit::Refused(Refusal::WrongPhase));
	assert_eq!(txn.admit(&reply(ACK, Some(SERVER), Destination::Broadcast)), Admit::Refused(Refusal::WrongPhase));
}

#[test]
fn a_competing_offer_after_one_was_chosen_changes_nothing() {
	// `xid` AND `chaddr` CANNOT PICK AN OFFER: every legitimate server answering the same discover
	// shares both. What refuses this is the phase having moved once one was selected.
	let txn = requesting();
	assert_eq!(txn.admit(&reply(OFFER, Some(OTHER), Destination::Broadcast)), Admit::Refused(Refusal::WrongPhase));
	assert_eq!(txn.admit(&reply(OFFER, Some(SERVER), Destination::Broadcast)), Admit::Refused(Refusal::WrongPhase));
}

#[test]
fn an_ack_from_a_server_that_was_not_selected_is_refused() {
	let txn = requesting();
	assert_eq!(txn.admit(&reply(ACK, Some(OTHER), Destination::Broadcast)), Admit::Refused(Refusal::ForeignServer));
	assert_eq!(txn.admit(&reply(ACK, Some(SERVER), Destination::Broadcast)), Admit::CommitLease);

	// And an ACK for an address other than the one requested.
	let mut wrong = reply(ACK, Some(SERVER), Destination::Broadcast);
	wrong.yiaddr = [10, 0, 2, 99];
	assert_eq!(txn.admit(&wrong), Admit::Refused(Refusal::WrongAddress));
}

#[test]
fn a_renewal_takes_a_unicast_ack_from_its_own_server() {
	let txn = renewing();
	assert_eq!(txn.admit(&reply(ACK, Some(SERVER), Destination::Unicast)), Admit::CommitLease);
	// A broadcast ACK is not the form a renewal's reply takes.
	assert_eq!(txn.admit(&reply(ACK, Some(SERVER), Destination::Broadcast)), Admit::Refused(Refusal::WrongDestination));
	assert_eq!(txn.admit(&reply(ACK, Some(OTHER), Destination::Unicast)), Admit::Refused(Refusal::ForeignServer));
}

#[test]
fn rebinding_is_the_one_phase_where_a_new_server_is_legitimate() {
	let txn = Transaction { phase: Phase::Rebinding, ..requesting() };
	assert_eq!(txn.admit(&reply(ACK, Some(OTHER), Destination::Broadcast)), Admit::CommitLease);
	assert_eq!(txn.admit(&reply(NAK, Some(OTHER), Destination::Broadcast)), Admit::ClearLease);
}

#[test]
fn an_admissible_nak_clears_the_lease_in_each_phase_that_admits_one() {
	// THREE POSITIVE TRANSITIONS, not three refusals: a NAK the client is allowed to act on means
	// the server has declared the lease invalid, and the client discards it and starts again.
	assert_eq!(requesting().admit(&reply(NAK, Some(SERVER), Destination::Broadcast)), Admit::ClearLease);
	assert_eq!(renewing().admit(&reply(NAK, Some(SERVER), Destination::Broadcast)), Admit::ClearLease);
	assert_eq!(Transaction { phase: Phase::Rebinding, ..requesting() }.admit(&reply(NAK, Some(OTHER), Destination::Broadcast)), Admit::ClearLease);
}

#[test]
fn a_broadcast_nak_answering_a_unicast_renewal_is_accepted() {
	// THE CASE NEITHER GENERIC SET REACHES. RFC 2131 section 4.1 requires a server to BROADCAST every
	// DHCPNAK when `giaddr` is zero - a renewal whose REQUEST was unicast included. A client that
	// admits only unicast replies while renewing passes every other fixture here and fails this one,
	// then goes on using a lease the server has just rejected.
	let txn = renewing();
	assert_eq!(txn.admit(&reply(NAK, Some(SERVER), Destination::Broadcast)), Admit::ClearLease);
	// And a NAK from a server that is not the selected one is still refused while renewing.
	assert_eq!(txn.admit(&reply(NAK, Some(OTHER), Destination::Broadcast)), Admit::Refused(Refusal::ForeignServer));
}

#[test]
fn a_late_reply_arriving_after_the_phase_moved_on_changes_nothing() {
	// The ACK arrives after the lease is bound and nothing is in flight.
	let bound = Transaction { phase: Phase::Bound, ..requesting() };
	assert_eq!(bound.admit(&reply(ACK, Some(SERVER), Destination::Unicast)), Admit::Refused(Refusal::WrongPhase));
	assert_eq!(bound.admit(&reply(NAK, Some(SERVER), Destination::Broadcast)), Admit::Refused(Refusal::WrongPhase));
	assert_eq!(bound.admit(&reply(OFFER, Some(SERVER), Destination::Broadcast)), Admit::Refused(Refusal::WrongPhase));

	// And nothing at all is admissible before a discover has gone out.
	let init = Transaction::new(XID, CHADDR);
	assert_eq!(init.admit(&reply(OFFER, Some(SERVER), Destination::Broadcast)), Admit::Refused(Refusal::WrongPhase));
}

#[test]
fn an_offer_without_a_server_identifier_names_nobody_to_request_from() {
	let txn = selecting();
	assert_eq!(txn.admit(&reply(OFFER, None, Destination::Broadcast)), Admit::Refused(Refusal::ForeignServer));
}

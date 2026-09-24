// THE FLOW, CHECKED AGAINST A SCRIPTED PEER. The derivations themselves are held to the
// specification's own sample data in `smp`'s tests; what these hold is the ORDER, and the four checks
// that decide whether the order was kept - so the scripted peer computes its side with the same
// verified functions, and every test that expects a failure breaks exactly one thing.
use super::{AUTH_BONDING, AUTH_SECURE_CONNECTIONS, Address, IO_NO_INPUT_NO_OUTPUT, Initiator, KEY_SIZE, Step, code, reason};
use crate::hci_codec::{public_key_x, reverse16};
use crate::smp::{f4, f5, f6};

const LOCAL: Address = [0x00, 0x56, 0x12, 0x37, 0x37, 0xbf, 0xce];
const PEER: Address = [0x00, 0xa7, 0x13, 0x70, 0x2d, 0xcf, 0xc1];
const NA: [u8; 16] = [0x11; 16];
const NB: [u8; 16] = [0x22; 16];
const DHKEY_MSB: [u8; 32] = [0x33; 32];

fn pka() -> [u8; 64] {
	core::array::from_fn(|at| at as u8)
}

fn pkb() -> [u8; 64] {
	core::array::from_fn(|at| 0x80 | at as u8)
}

fn response() -> alloc::vec::Vec<u8> {
	alloc::vec![code::PAIRING_RESPONSE, IO_NO_INPUT_NO_OUTPUT, 0x00, AUTH_BONDING | AUTH_SECURE_CONNECTIONS, KEY_SIZE, 0x00, 0x00]
}

fn pdu(kind: u8, value: &[u8]) -> alloc::vec::Vec<u8> {
	let mut out = alloc::vec![kind];
	out.extend_from_slice(value);
	out
}

// The peer's confirm value over its nonce, in wire order.
fn peer_confirm() -> alloc::vec::Vec<u8> {
	let cb = f4(&public_key_x(&pkb()), &public_key_x(&pka()), &NB, 0);
	pdu(code::PAIRING_CONFIRM, &reverse16(&cb))
}

// The peer's check value, computed as the peer computes it: over the exchange in ITS order.
fn peer_check() -> alloc::vec::Vec<u8> {
	let keys = f5(&DHKEY_MSB, &NA, &NB, &LOCAL, &PEER);
	let io_b = [AUTH_BONDING | AUTH_SECURE_CONNECTIONS, 0x00, IO_NO_INPUT_NO_OUTPUT];
	let eb = f6(&keys.mac_key, &NB, &NA, &[0u8; 16], &io_b, &PEER, &LOCAL);
	pdu(code::PAIRING_DHKEY_CHECK, &reverse16(&eb))
}

fn dhkey_wire() -> [u8; 32] {
	let mut out = DHKEY_MSB;
	out.reverse();
	out
}

// Drive a pairing to the point where both the peer's nonce and the DH key are in hand.
fn to_check(dhkey_first: bool) -> (Initiator, alloc::vec::Vec<Step>) {
	let (mut pairing, first) = Initiator::start(LOCAL, PEER, pka(), NA);
	assert!(matches!(first, Step::Send(ref request) if request[0] == code::PAIRING_REQUEST));
	assert!(matches!(pairing.on_pdu(&response())[..], [Step::Send(ref key)] if key[0] == code::PAIRING_PUBLIC_KEY && key.len() == 65));
	assert_eq!(pairing.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pkb())), alloc::vec![Step::GenerateDhKey(pkb())]);
	let random = pairing.on_pdu(&peer_confirm());
	assert_eq!(random, alloc::vec![Step::Send(pdu(code::PAIRING_RANDOM, &reverse16(&NA)))]);
	if dhkey_first {
		assert!(pairing.on_dhkey(&dhkey_wire()).is_empty(), "the key alone is not enough: the peer's nonce is still owed");
		let steps = pairing.on_pdu(&pdu(code::PAIRING_RANDOM, &reverse16(&NB)));
		(pairing, steps)
	} else {
		assert!(pairing.on_pdu(&pdu(code::PAIRING_RANDOM, &reverse16(&NB))).is_empty(), "the nonce alone is not enough: the controller has not answered");
		let steps = pairing.on_dhkey(&dhkey_wire());
		(pairing, steps)
	}
}

#[test]
// THE WHOLE EXCHANGE, TO THE KEY. The check value this host sends is the one the peer computes for
// itself over the same exchange, and the key it ends with is the one the peer derives.
fn a_just_works_pairing_completes_with_the_key_the_peer_derives() {
	let (mut pairing, steps) = to_check(false);
	let keys = f5(&DHKEY_MSB, &NA, &NB, &LOCAL, &PEER);
	let io_a = [AUTH_BONDING | AUTH_SECURE_CONNECTIONS, 0x00, IO_NO_INPUT_NO_OUTPUT];
	let ea = f6(&keys.mac_key, &NA, &NB, &[0u8; 16], &io_a, &LOCAL, &PEER);
	assert_eq!(steps, alloc::vec![Step::Send(pdu(code::PAIRING_DHKEY_CHECK, &reverse16(&ea)))]);
	assert_eq!(pairing.on_pdu(&peer_check()), alloc::vec![Step::Encrypt(keys.ltk)]);
	assert!(pairing.is_done());
	assert_eq!(pairing.ltk(), Some(keys.ltk));
}

#[test]
// THE PEER'S NONCE AND THE CONTROLLER'S KEY ARRIVE IN EITHER ORDER, because one comes from the radio
// and the other from the controller - and a host that assumed one order would stall in the other.
fn the_dh_key_may_arrive_before_or_after_the_peers_nonce() {
	let (mut early, steps) = to_check(true);
	assert!(matches!(steps[..], [Step::Send(ref check)] if check[0] == code::PAIRING_DHKEY_CHECK));
	assert!(matches!(early.on_pdu(&peer_check())[..], [Step::Encrypt(_)]));
	let (mut late, steps) = to_check(false);
	assert!(matches!(steps[..], [Step::Send(ref check)] if check[0] == code::PAIRING_DHKEY_CHECK));
	assert_eq!(
		early.ltk(),
		{
			late.on_pdu(&peer_check());
			late.ltk()
		},
		"the same key either way"
	);
}

#[test]
// NO DOWNGRADE. A peer that does not set Secure Connections is refused rather than paired under the
// legacy model, which a passive listener breaks from a recording.
fn a_peer_without_secure_connections_is_refused_rather_than_paired_the_old_way() {
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, pka(), NA);
	let mut legacy = response();
	legacy[3] = AUTH_BONDING;
	assert_eq!(pairing.on_pdu(&legacy), alloc::vec![Step::Send(alloc::vec![code::PAIRING_FAILED, reason::AUTHENTICATION_REQUIREMENTS]), Step::Failed(reason::AUTHENTICATION_REQUIREMENTS)]);
	assert!(pairing.is_failed());
	assert_eq!(pairing.ltk(), None);
	// And a key shorter than 128 bits is refused the same way.
	let (mut short, _) = Initiator::start(LOCAL, PEER, pka(), NA);
	let mut seven = response();
	seven[4] = 7;
	assert!(matches!(short.on_pdu(&seven).last(), Some(Step::Failed(reason::ENCRYPTION_KEY_SIZE))));
}

#[test]
// THE CONFIRM VALUE IS A COMMITMENT, and a peer whose revealed nonce does not match what it committed
// to chose that nonce after seeing this host's - which is what the commitment exists to prevent.
fn a_nonce_that_does_not_match_its_commitment_fails_the_pairing() {
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, pka(), NA);
	pairing.on_pdu(&response());
	pairing.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pkb()));
	pairing.on_pdu(&peer_confirm());
	let mut other = NB;
	other[0] ^= 1;
	let steps = pairing.on_pdu(&pdu(code::PAIRING_RANDOM, &reverse16(&other)));
	assert_eq!(steps.last(), Some(&Step::Failed(reason::CONFIRM_VALUE_FAILED)));
	assert!(pairing.is_failed());
}

#[test]
// THE PEER'S CHECK VALUE IS ITS PROOF THAT IT HOLDS THE SAME DIFFIE-HELLMAN KEY, and a wrong one is a
// peer that does not - which is the man-in-the-middle Secure Connections can detect.
fn a_check_value_that_does_not_verify_fails_rather_than_encrypting() {
	let (mut pairing, _) = to_check(false);
	let mut wrong = peer_check();
	wrong[5] ^= 0xff;
	let steps = pairing.on_pdu(&wrong);
	assert_eq!(steps.last(), Some(&Step::Failed(reason::DHKEY_CHECK_FAILED)));
	assert!(!steps.iter().any(|step| matches!(step, Step::Encrypt(_))), "no key is offered for encryption");
	assert_eq!(pairing.ltk(), None);
}

#[test]
// A PEER ANSWERING WITH THIS HOST'S OWN PUBLIC KEY IS REFLECTING IT, and a Diffie-Hellman key computed
// against one's own key is one the reflector can compute too.
fn a_reflected_public_key_is_refused_before_the_controller_is_asked() {
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, pka(), NA);
	pairing.on_pdu(&response());
	let steps = pairing.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pka()));
	assert!(!steps.iter().any(|step| matches!(step, Step::GenerateDhKey(_))));
	assert_eq!(steps.last(), Some(&Step::Failed(reason::INVALID_PARAMETERS)));
	// A key of the wrong length is refused the same way.
	let (mut short, _) = Initiator::start(LOCAL, PEER, pka(), NA);
	short.on_pdu(&response());
	assert_eq!(short.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &[0u8; 63])).last(), Some(&Step::Failed(reason::INVALID_PARAMETERS)));
	// And a controller that refuses the peer's point ends the attempt.
	let (mut refused, _) = Initiator::start(LOCAL, PEER, pka(), NA);
	refused.on_pdu(&response());
	refused.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pkb()));
	assert_eq!(refused.on_dhkey_failed().last(), Some(&Step::Failed(reason::INVALID_PARAMETERS)));
}

#[test]
// A WELL-FORMED PDU IN THE WRONG STATE IS A PEER NOT FOLLOWING THE EXCHANGE, and deriving keys from an
// order that is not the specification's is deriving them outside its security argument.
fn a_pdu_out_of_order_fails_the_pairing() {
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, pka(), NA);
	assert_eq!(pairing.on_pdu(&peer_confirm()).last(), Some(&Step::Failed(reason::UNSPECIFIED)), "a confirm before the response");
	let (mut early, _) = Initiator::start(LOCAL, PEER, pka(), NA);
	early.on_pdu(&response());
	early.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pkb()));
	assert_eq!(early.on_pdu(&pdu(code::PAIRING_RANDOM, &reverse16(&NB))).last(), Some(&Step::Failed(reason::UNSPECIFIED)), "a nonce before its commitment");
	assert_eq!(Initiator::start(LOCAL, PEER, pka(), NA).0.on_pdu(&[]).last(), Some(&Step::Failed(reason::INVALID_PARAMETERS)), "an empty PDU");
}

#[test]
// THE PEER'S FAILURE IS REPORTED AND NOT ANSWERED, and nothing after a failure moves the exchange -
// a late PDU from an attempt that has ended is not the start of a new one.
fn a_failure_from_the_peer_ends_the_attempt_and_nothing_revives_it() {
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, pka(), NA);
	assert_eq!(pairing.on_pdu(&[code::PAIRING_FAILED, reason::PAIRING_NOT_SUPPORTED]), alloc::vec![Step::Failed(reason::PAIRING_NOT_SUPPORTED)]);
	assert!(pairing.is_failed());
	assert!(pairing.on_pdu(&response()).is_empty());
	assert!(pairing.on_dhkey(&dhkey_wire()).is_empty());
	assert_eq!(pairing.ltk(), None);
}

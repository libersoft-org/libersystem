// THE FLOW, CHECKED AGAINST A SCRIPTED PEER. The derivations themselves are held to the
// specification's own sample data in `smp`'s tests; what these hold is the ORDER, and the four checks
// that decide whether the order was kept - so the scripted peer computes its side with the same
// verified functions, and every test that expects a failure breaks exactly one thing.
use super::{AUTH_BONDING, AUTH_CT2, AUTH_MITM, AUTH_SECURE_CONNECTIONS, Address, Answer, Bonded, BredrResponder, DIST_ENCRYPTION, DIST_IDENTITY, DIST_LINK_KEY, IO_DISPLAY_ONLY, IO_DISPLAY_YES_NO, IO_KEYBOARD_DISPLAY, IO_KEYBOARD_ONLY, IO_NO_INPUT_NO_OUTPUT, Initiator, KEY_SIZE, Options, Question, Step, code, nonce, passkey_of, passkey_value, reason};
use crate::hci_codec::{public_key_x, reverse16};
use crate::smp::{c1, f4, f5, f6, g2, s1};

const LOCAL: Address = [0x00, 0x56, 0x12, 0x37, 0x37, 0xbf, 0xce];
const PEER: Address = [0x00, 0xa7, 0x13, 0x70, 0x2d, 0xcf, 0xc1];
const NA: [u8; 16] = [0x11; 16];
const NB: [u8; 16] = [0x22; 16];
const DHKEY_MSB: [u8; 32] = [0x33; 32];

fn options() -> Options {
	Options { io: IO_NO_INPUT_NO_OUTPUT, legacy: false, irk: [0x44; 16], identity: LOCAL, cross_transport: false }
}

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
	let (mut pairing, first) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options());
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
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options());
	let mut legacy = response();
	legacy[3] = AUTH_BONDING;
	assert_eq!(pairing.on_pdu(&legacy), alloc::vec![Step::Send(alloc::vec![code::PAIRING_FAILED, reason::AUTHENTICATION_REQUIREMENTS]), Step::Failed(reason::AUTHENTICATION_REQUIREMENTS)]);
	assert!(pairing.is_failed());
	assert_eq!(pairing.ltk(), None);
	// And a key shorter than 128 bits is refused the same way.
	let (mut short, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options());
	let mut seven = response();
	seven[4] = 7;
	assert!(matches!(short.on_pdu(&seven).last(), Some(Step::Failed(reason::ENCRYPTION_KEY_SIZE))));
}

#[test]
// THE CONFIRM VALUE IS A COMMITMENT, and a peer whose revealed nonce does not match what it committed
// to chose that nonce after seeing this host's - which is what the commitment exists to prevent.
fn a_nonce_that_does_not_match_its_commitment_fails_the_pairing() {
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options());
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
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options());
	pairing.on_pdu(&response());
	let steps = pairing.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pka()));
	assert!(!steps.iter().any(|step| matches!(step, Step::GenerateDhKey(_))));
	assert_eq!(steps.last(), Some(&Step::Failed(reason::INVALID_PARAMETERS)));
	// A key of the wrong length is refused the same way.
	let (mut short, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options());
	short.on_pdu(&response());
	assert_eq!(short.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &[0u8; 63])).last(), Some(&Step::Failed(reason::INVALID_PARAMETERS)));
	// And a controller that refuses the peer's point ends the attempt.
	let (mut refused, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options());
	refused.on_pdu(&response());
	refused.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pkb()));
	assert_eq!(refused.on_dhkey_failed().last(), Some(&Step::Failed(reason::INVALID_PARAMETERS)));
}

#[test]
// A WELL-FORMED PDU IN THE WRONG STATE IS A PEER NOT FOLLOWING THE EXCHANGE, and deriving keys from an
// order that is not the specification's is deriving them outside its security argument.
fn a_pdu_out_of_order_fails_the_pairing() {
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options());
	assert_eq!(pairing.on_pdu(&peer_confirm()).last(), Some(&Step::Failed(reason::UNSPECIFIED)), "a confirm before the response");
	let (mut early, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options());
	early.on_pdu(&response());
	early.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pkb()));
	assert_eq!(early.on_pdu(&pdu(code::PAIRING_RANDOM, &reverse16(&NB))).last(), Some(&Step::Failed(reason::UNSPECIFIED)), "a nonce before its commitment");
	assert_eq!(Initiator::start(LOCAL, PEER, Some(pka()), NA, options()).0.on_pdu(&[]).last(), Some(&Step::Failed(reason::INVALID_PARAMETERS)), "an empty PDU");
}

#[test]
// THE PEER'S FAILURE IS REPORTED AND NOT ANSWERED, and nothing after a failure moves the exchange -
// a late PDU from an attempt that has ended is not the start of a new one.
fn a_failure_from_the_peer_ends_the_attempt_and_nothing_revives_it() {
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options());
	assert_eq!(pairing.on_pdu(&[code::PAIRING_FAILED, reason::PAIRING_NOT_SUPPORTED]), alloc::vec![Step::Failed(reason::PAIRING_NOT_SUPPORTED)]);
	assert!(pairing.is_failed());
	assert!(pairing.on_pdu(&response()).is_empty());
	assert!(pairing.on_dhkey(&dhkey_wire()).is_empty());
	assert_eq!(pairing.ltk(), None);
}

#[test]
// THE TRANSIENT KEYS DO NOT OUTLIVE THEIR WORK: once the peer's check value verifies, the Diffie-Hellman
// key is gone and the MacKey zeroed, and only the LTK is kept - and an attempt that failed keeps neither.
fn only_the_ltk_outlives_a_completed_exchange_and_nothing_a_failed_one() {
	let (mut pairing, _) = to_check(false);
	pairing.on_pdu(&peer_check());
	assert!(pairing.dhkey.is_none(), "the Diffie-Hellman key is let go");
	let keys = pairing.keys.expect("the LTK is kept for encryption");
	assert_eq!(keys.mac_key, [0u8; 16], "the MacKey is zeroed");
	assert_eq!(pairing.ltk(), Some(keys.ltk));
	let (mut failed, _) = to_check(false);
	let mut wrong = peer_check();
	wrong[5] ^= 0xff;
	failed.on_pdu(&wrong);
	assert!(failed.dhkey.is_none() && failed.keys.is_none(), "a failed attempt keeps no key");
}

// ------------------------------------------------------------------ the models a person answers

fn watched() -> Options {
	Options { io: IO_KEYBOARD_DISPLAY, legacy: false, irk: [0x44; 16], identity: LOCAL, cross_transport: false }
}

fn response_from(io: u8, auth: u8, init: u8, resp: u8) -> alloc::vec::Vec<u8> {
	alloc::vec![code::PAIRING_RESPONSE, io, 0x00, auth, KEY_SIZE, init, resp]
}

// THE REQUEST THIS HOST SENDS with a watcher: KeyboardDisplay, protection asked for, its identity offered and the
// peer's asked for.
#[test]
fn with_a_watcher_the_request_asks_for_protection_and_identities() {
	let (_, first) = Initiator::start(LOCAL, PEER, Some(pka()), NA, watched());
	assert_eq!(first, Step::Send(alloc::vec![code::PAIRING_REQUEST, IO_KEYBOARD_DISPLAY, 0, AUTH_BONDING | AUTH_MITM | AUTH_SECURE_CONNECTIONS, KEY_SIZE, DIST_IDENTITY, DIST_ENCRYPTION | DIST_IDENTITY]));
}

// NUMERIC COMPARISON: the six digits are g2's, and nothing is sent until a person says yes.
#[test]
fn numeric_comparison_waits_for_the_yes() {
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, watched());
	let auth = AUTH_BONDING | AUTH_MITM | AUTH_SECURE_CONNECTIONS;
	pairing.on_pdu(&response_from(IO_DISPLAY_YES_NO, auth, 0, 0));
	pairing.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pkb()));
	pairing.on_dhkey(&dhkey_wire());
	pairing.on_pdu(&peer_confirm());
	let asked = pairing.on_pdu(&pdu(code::PAIRING_RANDOM, &reverse16(&NB)));
	let digits = g2(&public_key_x(&pka()), &public_key_x(&pkb()), &NA, &NB) % 1_000_000;
	assert_eq!(asked, alloc::vec![Step::Ask(Question::Compare(digits))]);
	assert!(pairing.authenticated());
	let checked = pairing.answer(Answer::Yes);
	assert!(matches!(checked[..], [Step::Send(ref check)] if check[0] == code::PAIRING_DHKEY_CHECK));
	// A NO ENDS IT with the specification's reason.
	let (mut refused, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, watched());
	refused.on_pdu(&response_from(IO_DISPLAY_YES_NO, auth, 0, 0));
	refused.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pkb()));
	refused.on_pdu(&peer_confirm());
	refused.on_pdu(&pdu(code::PAIRING_RANDOM, &reverse16(&NB)));
	assert_eq!(refused.answer(Answer::No).last(), Some(&Step::Failed(reason::NUMERIC_COMPARISON_FAILED)));
}

// A RESPONDER IN PASSKEY ENTRY, scripted with the verified functions: its nonce each round, and its commitment to the
// round's bit.
fn passkey_rounds(pairing: &mut Initiator, passkey: u32, mut first: alloc::vec::Vec<Step>) -> alloc::vec::Vec<Step> {
	for round in 0..20u8 {
		let Some(Step::Send(confirm)) = first.iter().find(|step| matches!(step, Step::Send(bytes) if bytes[0] == code::PAIRING_CONFIRM)).cloned() else { panic!("round {round}: no commitment") };
		let na = nonce(&NA, round + 1);
		let bit = 0x80 | ((passkey >> round) & 1) as u8;
		assert_eq!(confirm[1..], reverse16(&f4(&public_key_x(&pka()), &public_key_x(&pkb()), &na, bit)), "round {round}: the commitment is to the passkey's bit");
		let nb = [round; 16];
		let cb = f4(&public_key_x(&pkb()), &public_key_x(&pka()), &nb, bit);
		let random = pairing.on_pdu(&pdu(code::PAIRING_CONFIRM, &reverse16(&cb)));
		assert_eq!(random, alloc::vec![Step::Send(pdu(code::PAIRING_RANDOM, &reverse16(&na)))]);
		first = pairing.on_pdu(&pdu(code::PAIRING_RANDOM, &reverse16(&nb)));
	}
	first
}

// PASSKEY ENTRY, THIS HOST SHOWING: twenty rounds, one bit each, and the check values keyed with the passkey.
#[test]
fn passkey_entry_shown_runs_twenty_rounds_and_checks_with_the_passkey() {
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, watched());
	let auth = AUTH_BONDING | AUTH_MITM | AUTH_SECURE_CONNECTIONS;
	pairing.on_pdu(&response_from(IO_KEYBOARD_ONLY, auth, 0, 0));
	let passkey = passkey_of(&NA);
	let steps = pairing.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pkb()));
	assert!(steps.contains(&Step::Ask(Question::ShowPasskey(passkey))));
	pairing.on_dhkey(&dhkey_wire());
	let check = passkey_rounds(&mut pairing, passkey, steps);
	let na = nonce(&NA, 20);
	let nb = [19u8; 16];
	let keys = f5(&DHKEY_MSB, &na, &nb, &LOCAL, &PEER);
	let io_a = [auth, 0x00, IO_KEYBOARD_DISPLAY];
	let ea = f6(&keys.mac_key, &na, &nb, &passkey_value(passkey), &io_a, &LOCAL, &PEER);
	assert_eq!(check, alloc::vec![Step::Send(pdu(code::PAIRING_DHKEY_CHECK, &reverse16(&ea)))]);
	let io_b = [auth, 0x00, IO_KEYBOARD_ONLY];
	let eb = f6(&keys.mac_key, &nb, &na, &passkey_value(passkey), &io_b, &PEER, &LOCAL);
	assert_eq!(pairing.on_pdu(&pdu(code::PAIRING_DHKEY_CHECK, &reverse16(&eb))), alloc::vec![Step::Encrypt(keys.ltk)]);
	assert!(pairing.authenticated());
}

// PASSKEY ENTRY, THIS HOST TYPING what a display-only peer shows: the rounds begin with the person's digits; a wrong
// bit fails the round. (Two keyboard-and-display sides compare numbers instead, under Secure Connections.)
#[test]
fn passkey_entry_typed_begins_with_the_answer() {
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, Some(pka()), NA, watched());
	let auth = AUTH_BONDING | AUTH_MITM | AUTH_SECURE_CONNECTIONS;
	pairing.on_pdu(&response_from(IO_DISPLAY_ONLY, auth, 0, 0));
	let steps = pairing.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pkb()));
	assert!(steps.contains(&Step::Ask(Question::EnterPasskey)));
	let first = pairing.answer(Answer::Passkey(123456));
	assert!(matches!(first[..], [Step::Send(ref confirm)] if confirm[0] == code::PAIRING_CONFIRM));
	// The responder commits to a different passkey's bit: the round fails at its nonce.
	let nb = [7u8; 16];
	let wrong_bit = 0x80 | (((123456u32 >> 0) & 1) ^ 1) as u8;
	let cb = f4(&public_key_x(&pkb()), &public_key_x(&pka()), &nb, wrong_bit);
	pairing.on_pdu(&pdu(code::PAIRING_CONFIRM, &reverse16(&cb)));
	assert_eq!(pairing.on_pdu(&pdu(code::PAIRING_RANDOM, &reverse16(&nb))).last(), Some(&Step::Failed(reason::CONFIRM_VALUE_FAILED)));
}

// LEGACY ONLY ON THE OPERATOR'S WORD: c1 commitments, s1's short-term key, and the keys the peer distributes.
#[test]
fn legacy_pairing_on_request_distributes_its_keys() {
	let mut legacy = options();
	legacy.legacy = true;
	let (mut pairing, _) = Initiator::start(LOCAL, PEER, None, NA, legacy);
	let response = response_from(IO_NO_INPUT_NO_OUTPUT, AUTH_BONDING, DIST_IDENTITY, DIST_ENCRYPTION | DIST_IDENTITY);
	let confirm = pairing.on_pdu(&response);
	let mut preq = [code::PAIRING_REQUEST, IO_NO_INPUT_NO_OUTPUT, 0, AUTH_BONDING, KEY_SIZE, DIST_IDENTITY, DIST_ENCRYPTION | DIST_IDENTITY];
	preq.reverse();
	let mut pres: [u8; 7] = response.clone().try_into().unwrap();
	pres.reverse();
	let tk = [0u8; 16];
	let ia: [u8; 6] = LOCAL[1..].try_into().unwrap();
	let ra: [u8; 6] = PEER[1..].try_into().unwrap();
	assert_eq!(confirm, alloc::vec![Step::Send(pdu(code::PAIRING_CONFIRM, &reverse16(&c1(&tk, &NA, &preq, &pres, 0, &ia, 0, &ra))))]);
	let sconfirm = c1(&tk, &NB, &preq, &pres, 0, &ia, 0, &ra);
	assert_eq!(pairing.on_pdu(&pdu(code::PAIRING_CONFIRM, &reverse16(&sconfirm))), alloc::vec![Step::Send(pdu(code::PAIRING_RANDOM, &reverse16(&NA)))]);
	assert_eq!(pairing.on_pdu(&pdu(code::PAIRING_RANDOM, &reverse16(&NB))), alloc::vec![Step::Encrypt(s1(&tk, &NB, &NA))]);
	assert!(pairing.on_encrypted().is_empty(), "nothing is given before the peer's keys arrive");
	let ltk = [0x5a; 16];
	assert!(pairing.on_pdu(&pdu(code::ENCRYPTION_INFORMATION, &ltk)).is_empty());
	assert!(pairing.on_pdu(&pdu(code::MASTER_IDENTIFICATION, &[0x34, 0x12, 1, 2, 3, 4, 5, 6, 7, 8])).is_empty());
	assert!(pairing.on_pdu(&pdu(code::IDENTITY_INFORMATION, &[0x77; 16])).is_empty());
	let mut identity = alloc::vec![0x01];
	identity.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0xc6]);
	let done = pairing.on_pdu(&pdu(code::IDENTITY_ADDRESS_INFORMATION, &identity));
	assert!(matches!(done[0], Step::Send(ref info) if info[0] == code::IDENTITY_INFORMATION));
	assert!(matches!(done[1], Step::Send(ref address) if address[0] == code::IDENTITY_ADDRESS_INFORMATION));
	let Some(Step::Bonded(bonded)) = done.last() else { panic!("the bond ends it") };
	assert_eq!(**bonded, Bonded { ltk: reverse16(&ltk), ediv: 0x1234, rand: [1, 2, 3, 4, 5, 6, 7, 8], irk: Some([0x77; 16]), identity: Some([0x01, 0xc6, 0x55, 0x44, 0x33, 0x22, 0x11]), secure_connections: false, authenticated: false, link_key: None });
}

// A PRIVATE ADDRESS GIVEN AS AN IDENTITY is refused.
#[test]
fn a_private_identity_is_refused() {
	let (mut pairing, _) = to_check(false);
	let keys = f5(&DHKEY_MSB, &NA, &NB, &LOCAL, &PEER);
	let _ = keys;
	pairing.response[6] = DIST_IDENTITY;
	pairing.owed = DIST_IDENTITY;
	pairing.on_pdu(&peer_check());
	assert!(pairing.on_encrypted().is_empty());
	pairing.on_pdu(&pdu(code::IDENTITY_INFORMATION, &[0x77; 16]));
	let mut identity = alloc::vec![0x01];
	identity.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x46]);
	assert_eq!(pairing.on_pdu(&pdu(code::IDENTITY_ADDRESS_INFORMATION, &identity)).last(), Some(&Step::Failed(reason::INVALID_PARAMETERS)));
}

// ------------------------------------------------------------------ cross-transport key derivation

// A Just Works pairing on LE with `cross_transport` set, the peer answering `auth` and `link_key` in both fields: run to
// the bond and hand it back.
fn le_with_cross_transport(auth: u8, link_key: u8) -> (alloc::vec::Vec<u8>, Bonded) {
	let options = Options { cross_transport: true, ..options() };
	let (mut pairing, first) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options);
	let Step::Send(request) = first else { panic!("the request goes first") };
	pairing.on_pdu(&response_from(IO_NO_INPUT_NO_OUTPUT, auth, link_key, link_key));
	pairing.on_pdu(&pdu(code::PAIRING_PUBLIC_KEY, &pkb()));
	pairing.on_pdu(&peer_confirm());
	pairing.on_pdu(&pdu(code::PAIRING_RANDOM, &reverse16(&NB)));
	pairing.on_dhkey(&dhkey_wire());
	let keys = f5(&DHKEY_MSB, &NA, &NB, &LOCAL, &PEER);
	let io_b = [auth, 0x00, IO_NO_INPUT_NO_OUTPUT];
	let eb = f6(&keys.mac_key, &NB, &NA, &[0u8; 16], &io_b, &PEER, &LOCAL);
	assert!(matches!(pairing.on_pdu(&pdu(code::PAIRING_DHKEY_CHECK, &reverse16(&eb)))[..], [Step::Encrypt(_)]));
	let done = pairing.on_encrypted();
	let Some(Step::Bonded(bonded)) = done.last() else { panic!("the bond ends it: {done:?}") };
	(request, (**bonded).clone())
}

// ON LE, a dual-mode host asks for the BR/EDR link key with CT2, and a peer that asks too gets the one `h7` and `h6`
// make from the LTK - held as HCI carries it, least significant octet first.
#[test]
fn on_le_the_link_key_both_sides_asked_for_is_derived_from_the_ltk() {
	let auth = AUTH_BONDING | AUTH_SECURE_CONNECTIONS | AUTH_CT2;
	let (request, bonded) = le_with_cross_transport(auth, DIST_LINK_KEY);
	assert_eq!(request[3] & AUTH_CT2, AUTH_CT2, "CT2 is asked for");
	assert_eq!(request[5] & DIST_LINK_KEY, DIST_LINK_KEY);
	assert_eq!(request[6] & DIST_LINK_KEY, DIST_LINK_KEY);
	let mut expected = crate::bt_keys::link_key_from_ltk(&bonded.ltk, true);
	expected.reverse();
	assert_eq!(bonded.link_key, Some(expected));
	// WITHOUT CT2 FROM THE PEER, h6 makes the intermediate key, and the result differs.
	let (_, without) = le_with_cross_transport(AUTH_BONDING | AUTH_SECURE_CONNECTIONS, DIST_LINK_KEY);
	let mut h6_path = crate::bt_keys::link_key_from_ltk(&without.ltk, false);
	h6_path.reverse();
	assert_eq!(without.link_key, Some(h6_path));
	assert_ne!(without.link_key, Some(expected));
}

// A PEER THAT DOES NOT ASK gets nothing derived, and neither does a host that is not dual-mode.
#[test]
fn on_le_no_link_key_is_derived_unless_both_sides_ask() {
	let (_, bonded) = le_with_cross_transport(AUTH_BONDING | AUTH_SECURE_CONNECTIONS | AUTH_CT2, 0);
	assert_eq!(bonded.link_key, None);
	let (_, first) = Initiator::start(LOCAL, PEER, Some(pka()), NA, options());
	assert!(matches!(first, Step::Send(ref request) if request[3] & AUTH_CT2 == 0 && request[5] & DIST_LINK_KEY == 0 && request[6] & DIST_LINK_KEY == 0));
	// AND NOT ON LEGACY, which is no source: without this host's public key the request asks for neither.
	let (_, legacy) = Initiator::start(LOCAL, PEER, None, NA, Options { cross_transport: true, legacy: true, ..options() });
	assert!(matches!(legacy, Step::Send(ref request) if request[3] & AUTH_CT2 == 0 && request[6] & DIST_LINK_KEY == 0));
}

// OVER BR/EDR, this host the central: the LTK both sides derive from the link key, at the link key's own level, and the
// identities exchanged - the peer's first.
#[test]
fn over_bredr_the_ltk_is_derived_from_the_link_key_at_its_level() {
	let link_key: [u8; 16] = core::array::from_fn(|at| 0x30 + at as u8);
	let (mut pairing, first) = Initiator::over_bredr(LOCAL, PEER, link_key, true, options());
	assert_eq!(first, Step::Send(alloc::vec![code::PAIRING_REQUEST, 0, 0, AUTH_CT2, KEY_SIZE, DIST_ENCRYPTION | DIST_IDENTITY, DIST_ENCRYPTION | DIST_IDENTITY]));
	assert!(pairing.on_pdu(&response_from(0, AUTH_CT2, DIST_ENCRYPTION | DIST_IDENTITY, DIST_ENCRYPTION | DIST_IDENTITY)).is_empty(), "the peer's identity comes first");
	assert!(pairing.on_pdu(&pdu(code::IDENTITY_INFORMATION, &[0x66; 16])).is_empty());
	let mut identity = alloc::vec![0x00];
	identity.extend_from_slice(&[0x02, 0x00, 0x20, 0xdc, 0x1b, 0x00]);
	let done = pairing.on_pdu(&pdu(code::IDENTITY_ADDRESS_INFORMATION, &identity));
	assert!(matches!(done[0], Step::Send(ref info) if info[0] == code::IDENTITY_INFORMATION));
	assert!(matches!(done[1], Step::Send(ref address) if address[0] == code::IDENTITY_ADDRESS_INFORMATION));
	let Some(Step::Bonded(bonded)) = done.last() else { panic!("the bond ends it") };
	let mut source = link_key;
	source.reverse();
	assert_eq!(bonded.ltk, crate::bt_keys::ltk_from_link_key(&source, true));
	assert!(bonded.secure_connections && bonded.authenticated, "the derived key keeps its source's level");
	assert_eq!(bonded.identity, Some([0x00, 0x00, 0x1b, 0xdc, 0x20, 0x00, 0x02]));
	assert_eq!(bonded.link_key, None, "over BR/EDR the LTK is the derived key");
	// AN UNAUTHENTICATED SOURCE MAKES AN UNAUTHENTICATED KEY.
	let (mut plain, _) = Initiator::over_bredr(LOCAL, PEER, link_key, false, options());
	let done = plain.on_pdu(&response_from(0, 0, DIST_ENCRYPTION, DIST_ENCRYPTION));
	let Some(Step::Bonded(bonded)) = done.last() else { panic!("no identity owed, so the bond is at once") };
	assert!(!bonded.authenticated);
	assert_eq!(bonded.ltk, crate::bt_keys::ltk_from_link_key(&source, false), "without CT2 from the peer, h6 makes the intermediate key");
}

// A PEER THAT DOES NOT ASK FOR THE DERIVATION over BR/EDR ends the exchange with nothing stored and nothing sent.
#[test]
fn over_bredr_a_peer_that_does_not_ask_ends_it_with_nothing_sent() {
	let (mut pairing, _) = Initiator::over_bredr(LOCAL, PEER, [0x42; 16], true, options());
	assert_eq!(pairing.on_pdu(&response_from(0, 0, DIST_IDENTITY, DIST_IDENTITY)), alloc::vec![Step::Failed(reason::PAIRING_NOT_SUPPORTED)]);
	assert!(pairing.is_failed());
	assert!(pairing.bredr.is_some_and(|(key, _)| key == [0; 16]), "the link key copy is cleared");
}

// THE RESPONDER OVER BR/EDR, for a link the peer is the central of: it answers what both may give, gives its identity
// first, and derives the same LTK the initiator does.
#[test]
fn over_bredr_the_responder_answers_gives_first_and_derives_the_same_key() {
	let link_key: [u8; 16] = core::array::from_fn(|at| 0x70 ^ at as u8);
	let (mut initiator, Step::Send(request)) = Initiator::over_bredr(PEER, LOCAL, link_key, true, options()) else { panic!("a request") };
	let (responder, steps) = BredrResponder::on_request(&request, link_key, true, &Options { identity: PEER, ..options() });
	let mut responder = responder.expect("the request is taken");
	let sent: alloc::vec::Vec<alloc::vec::Vec<u8>> = steps.iter().filter_map(|step| if let Step::Send(pdu) = step { Some(pdu.clone()) } else { None }).collect();
	assert_eq!(sent.len(), 3, "the response, then this side's identity and address");
	assert_eq!(sent[0], alloc::vec![code::PAIRING_RESPONSE, 0, 0, AUTH_CT2, KEY_SIZE, DIST_ENCRYPTION | DIST_IDENTITY, DIST_ENCRYPTION | DIST_IDENTITY]);
	let mut initiator_steps = alloc::vec::Vec::new();
	for pdu in &sent {
		initiator_steps.extend(initiator.on_pdu(pdu));
	}
	let Some(Step::Bonded(theirs)) = initiator_steps.last() else { panic!("the initiator bonds once the responder's identity is in") };
	let mut ours = alloc::vec::Vec::new();
	for step in &initiator_steps {
		if let Step::Send(pdu) = step {
			ours.extend(responder.on_pdu(pdu));
		}
	}
	let Some(Step::Bonded(mine)) = ours.last() else { panic!("the responder bonds once the initiator's identity is in") };
	assert_eq!(mine.ltk, theirs.ltk, "both sides derive one key");
	assert_eq!(mine.identity, Some(LOCAL), "the responder learns the initiator's identity");
	assert_eq!(theirs.identity, Some(PEER), "and the initiator the responder's");
	assert!(mine.authenticated && responder.is_finished());
	// A REQUEST ASKING FOR NO DERIVATION is answered that there is nothing to do.
	let (none, refused) = BredrResponder::on_request(&[code::PAIRING_REQUEST, 0, 0, 0, KEY_SIZE, DIST_IDENTITY, DIST_IDENTITY], link_key, true, &options());
	assert!(none.is_none());
	assert_eq!(refused.last(), Some(&Step::Failed(reason::PAIRING_NOT_SUPPORTED)));
}

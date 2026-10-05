//! THE SECURITY MANAGER, AS THE INITIATOR OF AN LE PAIRING - every association model a person can answer, and the
//! keys the two sides distribute once the link is encrypted.
//!
//! This is the whole exchange that turns an unencrypted link into a bonded one, driven by the PDUs the peer sends,
//! the two values the controller computes - this host's public key, and the Diffie-Hellman key once the peer's is
//! known - and, where the model needs one, a person's answer. Every derivation is `smp`'s, checked against the
//! specification's own sample data; what is here is the ORDER, and the checks that decide whether it was kept.
//!
//! THE MODELS. LE Secure Connections is the rule: Just Works where neither side asks for protection against a man
//! in the middle or nobody can answer, Numeric Comparison where both sides can show and confirm six digits, and
//! Passkey Entry where one side types what the other shows - twenty rounds of commitments, one bit of the passkey
//! each. LE LEGACY PAIRING runs only when the operator asked for it, for a device that has nothing better: a
//! passive listener breaks it from a recording, and a bond made with it is kept at the legacy level it earns.
//!
//! JUST WORKS IS ENCRYPTED AND NOT AUTHENTICATED, and the state machine does not pretend otherwise: the confirm
//! value proves the peer committed to its nonce before seeing this host's - which defeats a passive attacker - and
//! proves nothing about WHICH radio answered.
//!
//! THE KEYS DISTRIBUTED AFTER ENCRYPTION. This host asks the peer for its identity - the identity resolving key
//! that resolves its private addresses, and its identity address - and on legacy its long-term key with the EDIV
//! and Rand that name it; it gives its own identity in return.
//!
//! CROSS-TRANSPORT KEY DERIVATION. A dual-mode peer bonded on one radio has a key on the other derived from the first,
//! never from anything weaker: only a Secure Connections key is a source, and the derived key keeps its source's level.
//! On LE both sides set the LinkKey bit in both key distribution fields and the BR/EDR link key comes out of the LTK
//! (`bt_keys::link_key_from_ltk`); on BR/EDR this host - the link's central - runs the exchange over the BR/EDR
//! Security Manager's fixed channel, both sides set EncKey, and the LTK comes out of the link key
//! (`bt_keys::ltk_from_link_key`). The CT2 bit both set chooses `h7` over `h6` for the first step. A BR/EDR link key
//! is held as HCI carries it, least significant octet first; the derivations take it most significant first.
//!
//! EVERY VALUE ON THE SMP WIRE IS LITTLE-ENDIAN and every `smp` function takes its values most significant first.
//! The reversal happens at the boundary, here, once, and is named.

use crate::aes::Key;
use crate::hci_codec::{public_key_x, reverse16};
use crate::smp::{Keys, c1, f4, f5, f6, g2, s1};
use alloc::boxed::Box;
use alloc::vec::Vec;

/// PDU codes.
pub mod code {
	pub const PAIRING_REQUEST: u8 = 0x01;
	pub const PAIRING_RESPONSE: u8 = 0x02;
	pub const PAIRING_CONFIRM: u8 = 0x03;
	pub const PAIRING_RANDOM: u8 = 0x04;
	pub const PAIRING_FAILED: u8 = 0x05;
	pub const ENCRYPTION_INFORMATION: u8 = 0x06;
	pub const MASTER_IDENTIFICATION: u8 = 0x07;
	pub const IDENTITY_INFORMATION: u8 = 0x08;
	pub const IDENTITY_ADDRESS_INFORMATION: u8 = 0x09;
	pub const SIGNING_INFORMATION: u8 = 0x0a;
	pub const PAIRING_PUBLIC_KEY: u8 = 0x0c;
	pub const PAIRING_DHKEY_CHECK: u8 = 0x0d;
	pub const KEYPRESS_NOTIFICATION: u8 = 0x0e;
}

/// Why a pairing failed, as the reason code the Pairing Failed PDU carries.
pub mod reason {
	pub const PASSKEY_ENTRY_FAILED: u8 = 0x01;
	pub const AUTHENTICATION_REQUIREMENTS: u8 = 0x03;
	pub const CONFIRM_VALUE_FAILED: u8 = 0x04;
	pub const PAIRING_NOT_SUPPORTED: u8 = 0x05;
	pub const ENCRYPTION_KEY_SIZE: u8 = 0x06;
	pub const UNSPECIFIED: u8 = 0x08;
	pub const INVALID_PARAMETERS: u8 = 0x0a;
	pub const DHKEY_CHECK_FAILED: u8 = 0x0b;
	pub const NUMERIC_COMPARISON_FAILED: u8 = 0x0c;
}

/// IO capabilities, as SMP names them.
pub const IO_DISPLAY_ONLY: u8 = 0x00;
pub const IO_DISPLAY_YES_NO: u8 = 0x01;
pub const IO_KEYBOARD_ONLY: u8 = 0x02;
pub const IO_NO_INPUT_NO_OUTPUT: u8 = 0x03;
pub const IO_KEYBOARD_DISPLAY: u8 = 0x04;

/// Authentication requirements: bonding, protection against a man in the middle, Secure Connections, and CT2 - the
/// `h7` derivation for cross-transport keys.
pub const AUTH_BONDING: u8 = 0x01;
pub const AUTH_MITM: u8 = 0x04;
pub const AUTH_SECURE_CONNECTIONS: u8 = 0x08;
pub const AUTH_CT2: u8 = 0x20;

/// The key distribution bits: the encryption key (legacy's LTK, EDIV and Rand - and over BR/EDR, the LTK derived from
/// the link key), the identity key and address, and the BR/EDR link key derived from an LE Secure Connections LTK.
pub const DIST_ENCRYPTION: u8 = 0x01;
pub const DIST_IDENTITY: u8 = 0x02;
pub const DIST_LINK_KEY: u8 = 0x08;

/// The only key size this host accepts.
pub const KEY_SIZE: u8 = 16;

/// A device address as the derivations take it: the address type and then the six bytes, most
/// significant first. Type 0 is public and 1 is random.
pub type Address = [u8; 7];

/// How this host pairs this time: what it declares, whether the operator asked for legacy pairing, and the identity
/// it gives the peer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
	/// The IO capability declared: KeyboardDisplay with a prompt watcher, NoInputNoOutput without.
	pub io: u8,
	/// The operator named this device for legacy pairing.
	pub legacy: bool,
	/// This host's identity resolving key and identity address, distributed to the peer.
	pub irk: [u8; 16],
	pub identity: Address,
	/// This host has the other radio: on LE it asks for the BR/EDR link key to be derived, with CT2.
	pub cross_transport: bool,
}

/// What a person is asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Question {
	/// Do the six digits match what the peer shows?
	Compare(u32),
	/// Type these six digits on the peer.
	ShowPasskey(u32),
	/// Type the six digits the peer shows.
	EnterPasskey,
}

/// What a person answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
	Yes,
	No,
	Passkey(u32),
}

/// What a finished pairing leaves: the long-term key and, on legacy, the EDIV and Rand that name it; the peer's
/// identity where it gave it; and the level the model earned.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bonded {
	pub ltk: [u8; 16],
	pub ediv: u16,
	pub rand: [u8; 8],
	pub irk: Option<[u8; 16]>,
	pub identity: Option<Address>,
	pub secure_connections: bool,
	pub authenticated: bool,
	/// THE OTHER RADIO'S KEY, derived across transports where both sides asked for it: on LE, the BR/EDR link key as HCI
	/// carries it, least significant octet first. `None` over BR/EDR, where the LTK above is the derived key.
	pub link_key: Option<[u8; 16]>,
}

/// What the service must do next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
	/// Send this SMP PDU to the peer.
	Send(Vec<u8>),
	/// Ask the controller for the Diffie-Hellman key against this peer public key, in wire order.
	GenerateDhKey([u8; 64]),
	/// Start encryption with this key, most significant first: Secure Connections' LTK, or legacy's STK.
	Encrypt([u8; 16]),
	/// A person must answer this; `answer` takes the reply.
	Ask(Question),
	/// The keys are distributed: the bond.
	Bonded(Box<Bonded>),
	/// Pairing has failed with this reason. Anything the caller holds for it is to be discarded.
	Failed(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Model {
	JustWorks,
	Numeric,
	/// This host shows the passkey; the peer types it.
	PasskeyShown,
	/// The peer shows it; this host's person types it.
	PasskeyTyped,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
	AwaitResponse,
	AwaitPublicKey,
	/// A person's passkey, before the commitments can begin.
	AwaitPasskey,
	AwaitConfirm,
	AwaitRandom,
	/// The nonces are exchanged; a person's yes to the comparison, or the Diffie-Hellman key, is still awaited.
	AwaitDhKey,
	AwaitCheck,
	/// Legacy: this host's confirm is sent and the peer's awaited, then its random.
	LegacyConfirm,
	LegacyRandom,
	/// The link key is in hand and encryption is next; then the keys.
	Encrypting,
	Distributing,
	Done,
	Failed,
}

/// One pairing attempt, from the request to the bond.
#[derive(Clone)]
pub struct Initiator {
	state: State,
	options: Options,
	local: Address,
	peer: Address,
	/// The healthy randomness every nonce, the passkey and legacy's random value come from.
	seed: [u8; 16],
	/// This host's nonce in the current round, most significant first.
	na: [u8; 16],
	/// This host's public key, in wire order.
	pka: [u8; 64],
	pkb: Option<[u8; 64]>,
	/// The peer's confirm value, most significant first.
	cb: Option<[u8; 16]>,
	nb: Option<[u8; 16]>,
	dhkey: Option<[u8; 32]>,
	keys: Option<Keys>,
	request: [u8; 7],
	response: [u8; 7],
	secure_connections: bool,
	model: Model,
	passkey: Option<u32>,
	round: u8,
	confirmed: bool,
	// Legacy: the temporary key and the short-term key it makes.
	tk: [u8; 16],
	// What the peer distributes, as it arrives, and what is still owed.
	ltk: Option<[u8; 16]>,
	ediv: u16,
	rand: [u8; 8],
	irk: Option<[u8; 16]>,
	identity: Option<Address>,
	owed: u8,
	given: u8,
	// OVER BR/EDR: the link key the LTK is derived from, as HCI carries it, and whether its pairing was authenticated.
	bredr: Option<([u8; 16], bool)>,
}

// THE SIX DIGITS a passkey is, from the healthy randomness: what this host shows when the peer is the keyboard.
fn passkey_of(seed: &[u8; 16]) -> u32 {
	let block = Key::new(seed).block(&[0xff; 16]);
	u32::from_be_bytes([block[0], block[1], block[2], block[3]]) % 1_000_000
}

// A ROUND'S NONCE: AES under the seed over the round number, so twenty rounds need one healthy random value.
fn nonce(seed: &[u8; 16], round: u8) -> [u8; 16] {
	if round == 0 {
		return *seed;
	}
	let mut counter = [0u8; 16];
	counter[15] = round;
	Key::new(seed).block(&counter)
}

// THE ASSOCIATION MODEL from both sides' IO capabilities, this host the initiator, when protection is asked for -
// the specification's table, the Secure Connections column where it differs.
fn model_of(ours: u8, theirs: u8, secure_connections: bool) -> Model {
	match (ours, theirs) {
		(IO_NO_INPUT_NO_OUTPUT, _) | (_, IO_NO_INPUT_NO_OUTPUT) => Model::JustWorks,
		(IO_DISPLAY_ONLY, IO_DISPLAY_ONLY | IO_DISPLAY_YES_NO) | (IO_DISPLAY_YES_NO, IO_DISPLAY_ONLY) => Model::JustWorks,
		(IO_DISPLAY_YES_NO | IO_KEYBOARD_DISPLAY, IO_DISPLAY_YES_NO | IO_KEYBOARD_DISPLAY) if secure_connections => Model::Numeric,
		(IO_DISPLAY_YES_NO, IO_DISPLAY_YES_NO) => Model::JustWorks,
		(IO_KEYBOARD_DISPLAY, IO_DISPLAY_YES_NO) | (IO_DISPLAY_YES_NO, IO_KEYBOARD_DISPLAY) => Model::PasskeyShown,
		(IO_KEYBOARD_DISPLAY, IO_KEYBOARD_DISPLAY) => Model::PasskeyTyped,
		(IO_KEYBOARD_ONLY | IO_KEYBOARD_DISPLAY, IO_DISPLAY_ONLY | IO_DISPLAY_YES_NO | IO_KEYBOARD_DISPLAY) => Model::PasskeyTyped,
		(IO_DISPLAY_ONLY | IO_DISPLAY_YES_NO | IO_KEYBOARD_DISPLAY, IO_KEYBOARD_ONLY | IO_KEYBOARD_DISPLAY) => Model::PasskeyShown,
		(IO_KEYBOARD_ONLY, IO_KEYBOARD_ONLY) => Model::PasskeyTyped,
		_ => Model::JustWorks,
	}
}

fn pdu(kind: u8, value: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![kind];
	out.extend_from_slice(value);
	out
}

// A passkey as the 128-bit value the check functions take, most significant first.
fn passkey_value(passkey: u32) -> [u8; 16] {
	let mut out = [0u8; 16];
	out[12..].copy_from_slice(&passkey.to_be_bytes());
	out
}

impl Initiator {
	/// Begin: the first PDU is the request. `seed` must come from healthy system randomness; this module cannot
	/// check that and the caller's refusal to pair without it is what enforces it. `public_key` is this host's
	/// P-256 key from the controller, where it has one; without it only legacy pairing can run.
	pub fn start(local: Address, peer: Address, public_key: Option<[u8; 64]>, seed: [u8; 16], options: Options) -> (Initiator, Step) {
		// PROTECTION AGAINST A MAN IN THE MIDDLE IS ASKED FOR ONLY WHEN SOMEONE CAN PROVIDE IT: asking without a person
		// to answer would be a claim this exchange cannot honour.
		let mitm = if options.io == IO_NO_INPUT_NO_OUTPUT { 0 } else { AUTH_MITM };
		let sc = if public_key.is_some() { AUTH_SECURE_CONNECTIONS } else { 0 };
		// THE OTHER RADIO'S KEY IS ASKED FOR only on Secure Connections, the one source it may come from.
		let (ct2, link_key) = if options.cross_transport && public_key.is_some() { (AUTH_CT2, DIST_LINK_KEY) } else { (0, 0) };
		let request = [code::PAIRING_REQUEST, options.io, 0x00, AUTH_BONDING | mitm | sc | ct2, KEY_SIZE, DIST_IDENTITY | link_key, DIST_ENCRYPTION | DIST_IDENTITY | link_key];
		let pairing = Initiator { state: State::AwaitResponse, options, local, peer, seed, na: seed, pka: public_key.unwrap_or([0; 64]), pkb: None, cb: None, nb: None, dhkey: None, keys: None, request, response: [0; 7], secure_connections: false, model: Model::JustWorks, passkey: None, round: 0, confirmed: false, tk: [0; 16], ltk: None, ediv: 0, rand: [0; 8], irk: None, identity: None, owed: 0, given: 0, bredr: None };
		(pairing, Step::Send(request.to_vec()))
	}

	/// BEGIN OVER BR/EDR: cross-transport key derivation on the BR/EDR Security Manager's channel, this host the link's
	/// central, the link already encrypted with `link_key` - a Secure Connections key, which the caller checked - as HCI
	/// carries it. The request asks for the LTK to be derived (EncKey) and for both identities; IO capabilities, OOB and
	/// the protection bits mean nothing here and are zero.
	pub fn over_bredr(local: Address, peer: Address, link_key: [u8; 16], authenticated: bool, options: Options) -> (Initiator, Step) {
		let request = [code::PAIRING_REQUEST, 0x00, 0x00, AUTH_CT2, KEY_SIZE, DIST_ENCRYPTION | DIST_IDENTITY, DIST_ENCRYPTION | DIST_IDENTITY];
		let (mut pairing, _) = Initiator::start(local, peer, None, [0; 16], options);
		pairing.request = request;
		pairing.bredr = Some((link_key, authenticated));
		(pairing, Step::Send(request.to_vec()))
	}

	/// Whether the exchange has reached its key.
	pub fn is_done(&self) -> bool {
		matches!(self.state, State::Encrypting | State::Distributing | State::Done)
	}

	/// Whether it ended without one.
	pub fn is_failed(&self) -> bool {
		self.state == State::Failed
	}

	/// The Secure Connections key, once there is one.
	pub fn ltk(&self) -> Option<[u8; 16]> {
		if self.is_done() && self.secure_connections { self.keys.map(|keys| keys.ltk) } else { None }
	}

	/// Whether the model a person answers - numeric comparison or a passkey - ran: what makes the bond authenticated.
	pub fn authenticated(&self) -> bool {
		match self.bredr {
			// OVER BR/EDR the level is the link key's: the derived key is worth what its source was.
			Some((_, authenticated)) => authenticated,
			None => self.model != Model::JustWorks,
		}
	}

	fn fail(&mut self, why: u8) -> Vec<Step> {
		self.state = State::Failed;
		// THE TRANSIENT KEYS ARE CLEARED ON FAILURE, not only on completion. A failed attempt that left its
		// Diffie-Hellman key and MacKey in memory would leave key material behind for the life of this object.
		self.forget_keys();
		alloc::vec![Step::Send(alloc::vec![code::PAIRING_FAILED, why]), Step::Failed(why)]
	}

	// OVERWRITTEN WHERE THEY LIE, THEN LET GO. Setting an `Option` to `None` rewrites its tag and leaves the key
	// bytes behind it exactly as they were, in memory this object keeps or frees.
	fn forget_keys(&mut self) {
		if let Some(dhkey) = self.dhkey.as_mut() {
			clear(dhkey);
		}
		if let Some(keys) = self.keys.as_mut() {
			clear(&mut keys.mac_key);
			clear(&mut keys.ltk);
		}
		if let Some(ltk) = self.ltk.as_mut() {
			clear(ltk);
		}
		if let Some((link_key, _)) = self.bredr.as_mut() {
			clear(link_key);
		}
		clear(&mut self.tk);
		clear(&mut self.seed);
		clear(&mut self.na);
		self.dhkey = None;
		self.keys = None;
		self.ltk = None;
	}

	/// One PDU from the peer.
	pub fn on_pdu(&mut self, pdu_bytes: &[u8]) -> Vec<Step> {
		if self.state == State::Done || self.state == State::Failed {
			return Vec::new();
		}
		let Some(&kind) = pdu_bytes.first() else { return self.fail(reason::INVALID_PARAMETERS) };
		if kind == code::PAIRING_FAILED {
			// THE PEER'S REASON IS REPORTED AND NOT ANSWERED: a failed PDU is not replied to with another.
			self.state = State::Failed;
			self.forget_keys();
			return alloc::vec![Step::Failed(pdu_bytes.get(1).copied().unwrap_or(reason::UNSPECIFIED))];
		}
		// A KEYPRESS NOTIFICATION says the peer's person is typing; it moves nothing.
		if kind == code::KEYPRESS_NOTIFICATION {
			return Vec::new();
		}
		match (self.state, kind) {
			(State::AwaitResponse, code::PAIRING_RESPONSE) => self.on_response(pdu_bytes),
			(State::AwaitPublicKey, code::PAIRING_PUBLIC_KEY) => {
				if pdu_bytes.len() != 65 {
					return self.fail(reason::INVALID_PARAMETERS);
				}
				let mut pkb = [0u8; 64];
				pkb.copy_from_slice(&pdu_bytes[1..]);
				// A PEER ANSWERING WITH THIS HOST'S OWN KEY is reflecting it, and a Diffie-Hellman key computed against
				// one's own public key is one the reflector can compute too.
				if pkb == self.pka {
					return self.fail(reason::INVALID_PARAMETERS);
				}
				self.pkb = Some(pkb);
				let mut steps = alloc::vec![Step::GenerateDhKey(pkb)];
				match self.model {
					// THIS HOST SHOWS THE PASSKEY and the commitments begin at once: the peer answers each only once its
					// person has typed it.
					Model::PasskeyShown => {
						let passkey = passkey_of(&self.seed);
						self.passkey = Some(passkey);
						steps.push(Step::Ask(Question::ShowPasskey(passkey)));
						steps.extend(self.commit());
					}
					Model::PasskeyTyped => {
						self.state = State::AwaitPasskey;
						steps.push(Step::Ask(Question::EnterPasskey));
					}
					Model::JustWorks | Model::Numeric => self.state = State::AwaitConfirm,
				}
				steps
			}
			(State::AwaitConfirm, code::PAIRING_CONFIRM) => {
				let Ok(cb) = <[u8; 16]>::try_from(&pdu_bytes[1..]) else { return self.fail(reason::INVALID_PARAMETERS) };
				self.cb = Some(reverse16(&cb));
				self.state = State::AwaitRandom;
				alloc::vec![Step::Send(pdu(code::PAIRING_RANDOM, &reverse16(&self.na)))]
			}
			(State::AwaitRandom, code::PAIRING_RANDOM) => {
				let Ok(nb) = <[u8; 16]>::try_from(&pdu_bytes[1..]) else { return self.fail(reason::INVALID_PARAMETERS) };
				let nb = reverse16(&nb);
				let (Some(pkb), Some(cb)) = (self.pkb, self.cb) else { return self.fail(reason::UNSPECIFIED) };
				// THE CONFIRM VALUE IS CHECKED AGAINST THE NONCE IT COMMITTED TO: a Cb that does not match f4 over the Nb
				// now revealed is a peer that chose its nonce after seeing this host's.
				let z = self.passkey_bit();
				if f4(&public_key_x(&pkb), &public_key_x(&self.pka), &nb, z) != cb {
					return self.fail(reason::CONFIRM_VALUE_FAILED);
				}
				self.nb = Some(nb);
				if matches!(self.model, Model::PasskeyShown | Model::PasskeyTyped) {
					self.round += 1;
					if self.round < 20 {
						return self.commit();
					}
				}
				self.state = State::AwaitDhKey;
				if self.model == Model::Numeric {
					let value = g2(&public_key_x(&self.pka), &public_key_x(&pkb), &self.na, &nb) % 1_000_000;
					return alloc::vec![Step::Ask(Question::Compare(value))];
				}
				self.confirmed = true;
				self.check_value()
			}
			(State::AwaitCheck, code::PAIRING_DHKEY_CHECK) => {
				let Ok(eb) = <[u8; 16]>::try_from(&pdu_bytes[1..]) else { return self.fail(reason::INVALID_PARAMETERS) };
				let (Some(keys), Some(nb)) = (self.keys.as_ref(), self.nb) else { return self.fail(reason::UNSPECIFIED) };
				let io_b = [self.response[3], self.response[2], self.response[1]];
				// THE PEER'S CHECK VALUE IS ITS PROOF THAT IT HOLDS THE SAME DIFFIE-HELLMAN KEY.
				if f6(&keys.mac_key, &nb, &self.na, &self.r(), &io_b, &self.peer, &self.local) != reverse16(&eb) {
					return self.fail(reason::DHKEY_CHECK_FAILED);
				}
				let ltk = keys.ltk;
				self.state = State::Encrypting;
				// The Diffie-Hellman key and the MacKey have done their work; the LTK is what outlives the exchange.
				if let Some(dhkey) = self.dhkey.as_mut() {
					clear(dhkey);
				}
				self.dhkey = None;
				if let Some(keys) = self.keys.as_mut() {
					clear(&mut keys.mac_key);
				}
				alloc::vec![Step::Encrypt(ltk)]
			}
			(State::LegacyConfirm, code::PAIRING_CONFIRM) => {
				let Ok(confirm) = <[u8; 16]>::try_from(&pdu_bytes[1..]) else { return self.fail(reason::INVALID_PARAMETERS) };
				self.cb = Some(reverse16(&confirm));
				self.state = State::LegacyRandom;
				alloc::vec![Step::Send(pdu(code::PAIRING_RANDOM, &reverse16(&self.na)))]
			}
			(State::LegacyRandom, code::PAIRING_RANDOM) => {
				let Ok(srand) = <[u8; 16]>::try_from(&pdu_bytes[1..]) else { return self.fail(reason::INVALID_PARAMETERS) };
				let srand = reverse16(&srand);
				let Some(sconfirm) = self.cb else { return self.fail(reason::UNSPECIFIED) };
				if self.legacy_confirm(&srand) != sconfirm {
					return self.fail(reason::CONFIRM_VALUE_FAILED);
				}
				let stk = s1(&self.tk, &srand, &self.na);
				clear(&mut self.tk);
				self.state = State::Encrypting;
				alloc::vec![Step::Encrypt(stk)]
			}
			(State::Distributing, _) => self.on_distributed(pdu_bytes),
			// A PDU THAT IS WELL FORMED AND ARRIVES IN THE WRONG STATE is a peer that is not following the exchange.
			_ => self.fail(reason::UNSPECIFIED),
		}
	}

	fn on_response(&mut self, pdu_bytes: &[u8]) -> Vec<Step> {
		let Ok(response) = <[u8; 7]>::try_from(pdu_bytes) else { return self.fail(reason::INVALID_PARAMETERS) };
		if response[4] != KEY_SIZE {
			return self.fail(reason::ENCRYPTION_KEY_SIZE);
		}
		if response[1] > IO_KEYBOARD_DISPLAY {
			return self.fail(reason::INVALID_PARAMETERS);
		}
		self.response = response;
		if let Some((link_key, _)) = self.bredr {
			return self.derive_over_bredr(&link_key);
		}
		let mitm = (self.request[3] | response[3]) & AUTH_MITM != 0;
		self.secure_connections = self.request[3] & response[3] & AUTH_SECURE_CONNECTIONS != 0;
		self.model = if mitm { model_of(self.options.io, response[1], self.secure_connections) } else { Model::JustWorks };
		// The keys each side gives: what both asked for.
		self.owed = self.request[6] & response[6] & if self.secure_connections { DIST_IDENTITY } else { DIST_ENCRYPTION | DIST_IDENTITY };
		self.given = self.request[5] & response[5] & DIST_IDENTITY;
		if self.secure_connections {
			self.state = State::AwaitPublicKey;
			return alloc::vec![Step::Send(pdu(code::PAIRING_PUBLIC_KEY, &self.pka))];
		}
		// NO DOWNGRADE WITHOUT THE OPERATOR'S WORD: a peer without Secure Connections is refused, not honoured under
		// the legacy model, unless the operator named it for exactly that.
		if !self.options.legacy {
			return self.fail(reason::AUTHENTICATION_REQUIREMENTS);
		}
		// LEGACY HAS NO NUMERIC COMPARISON: two screens that could compare make it Just Works.
		if self.model == Model::Numeric {
			self.model = Model::JustWorks;
		}
		match self.model {
			Model::PasskeyShown => {
				let passkey = passkey_of(&self.seed);
				self.passkey = Some(passkey);
				self.tk = passkey_value(passkey);
				let mut steps = alloc::vec![Step::Ask(Question::ShowPasskey(passkey))];
				steps.extend(self.legacy_commit());
				steps
			}
			Model::PasskeyTyped => {
				self.state = State::AwaitPasskey;
				alloc::vec![Step::Ask(Question::EnterPasskey)]
			}
			_ => self.legacy_commit(),
		}
	}

	// THE LTK FROM THE LINK KEY, where both sides set EncKey in both fields; then the identities, the peer's first.
	// A peer that asks for no derivation leaves this host with nothing to store: the exchange ends, failed, with
	// nothing sent - the peer refused nothing.
	fn derive_over_bredr(&mut self, link_key: &[u8; 16]) -> Vec<Step> {
		let (request, response) = (self.request, self.response);
		if request[5] & response[5] & request[6] & response[6] & DIST_ENCRYPTION == 0 {
			self.state = State::Failed;
			self.forget_keys();
			return alloc::vec![Step::Failed(reason::PAIRING_NOT_SUPPORTED)];
		}
		let ct2 = request[3] & response[3] & AUTH_CT2 != 0;
		let mut source = *link_key;
		source.reverse();
		let ltk = crate::bt_keys::ltk_from_link_key(&source, ct2);
		clear(&mut source);
		self.keys = Some(Keys { mac_key: [0; 16], ltk });
		self.secure_connections = true;
		self.owed = request[6] & response[6] & DIST_IDENTITY;
		self.given = request[5] & response[5] & DIST_IDENTITY;
		self.state = State::Distributing;
		self.after_distribution()
	}

	/// A person's answer to the question this exchange asked.
	pub fn answer(&mut self, answer: Answer) -> Vec<Step> {
		match (self.state, self.model, answer) {
			(State::AwaitPasskey, Model::PasskeyTyped, Answer::Passkey(passkey)) if passkey <= 999_999 => {
				self.passkey = Some(passkey);
				if self.secure_connections {
					self.commit()
				} else {
					self.tk = passkey_value(passkey);
					self.legacy_commit()
				}
			}
			(State::AwaitPasskey, _, _) => self.fail(reason::PASSKEY_ENTRY_FAILED),
			(State::AwaitDhKey, Model::Numeric, Answer::Yes) if !self.confirmed => {
				self.confirmed = true;
				self.check_value()
			}
			(State::AwaitDhKey, Model::Numeric, _) if !self.confirmed => self.fail(reason::NUMERIC_COMPARISON_FAILED),
			// A shown passkey is answered no only to give up.
			(_, Model::PasskeyShown, Answer::No) if !self.is_done() => self.fail(reason::PASSKEY_ENTRY_FAILED),
			_ => Vec::new(),
		}
	}

	// The passkey bit this round commits to, as `f4`'s z: 0x80 with the bit, or zero outside Passkey Entry.
	fn passkey_bit(&self) -> u8 {
		match (self.model, self.passkey) {
			(Model::PasskeyShown | Model::PasskeyTyped, Some(passkey)) => 0x80 | ((passkey >> self.round) & 1) as u8,
			_ => 0,
		}
	}

	// One Passkey Entry round's commitment: this round's nonce, committed with the passkey's bit.
	fn commit(&mut self) -> Vec<Step> {
		let Some(pkb) = self.pkb else { return self.fail(reason::UNSPECIFIED) };
		self.na = nonce(&self.seed, self.round + 1);
		let ca = f4(&public_key_x(&self.pka), &public_key_x(&pkb), &self.na, self.passkey_bit());
		self.state = State::AwaitConfirm;
		alloc::vec![Step::Send(pdu(code::PAIRING_CONFIRM, &reverse16(&ca)))]
	}

	fn legacy_confirm(&self, random: &[u8; 16]) -> [u8; 16] {
		let mut ia = [0u8; 6];
		ia.copy_from_slice(&self.local[1..]);
		let mut ra = [0u8; 6];
		ra.copy_from_slice(&self.peer[1..]);
		// THE TWO PDUs AS THEY WENT ON THE WIRE, most significant octet first: their bytes reversed.
		let mut preq = self.request;
		preq.reverse();
		let mut pres = self.response;
		pres.reverse();
		c1(&self.tk, random, &preq, &pres, self.local[0], &ia, self.peer[0], &ra)
	}

	fn legacy_commit(&mut self) -> Vec<Step> {
		let confirm = self.legacy_confirm(&self.na);
		self.state = State::LegacyConfirm;
		alloc::vec![Step::Send(pdu(code::PAIRING_CONFIRM, &reverse16(&confirm)))]
	}

	// The `r` the check values take: the passkey in Passkey Entry, zero otherwise.
	fn r(&self) -> [u8; 16] {
		match (self.model, self.passkey) {
			(Model::PasskeyShown | Model::PasskeyTyped, Some(passkey)) => passkey_value(passkey),
			_ => [0u8; 16],
		}
	}

	/// The Diffie-Hellman key the controller computed, in wire order.
	pub fn on_dhkey(&mut self, wire: &[u8; 32]) -> Vec<Step> {
		if self.state == State::Done || self.state == State::Failed {
			return Vec::new();
		}
		self.dhkey = Some(crate::hci_codec::dhkey(wire));
		self.check_value()
	}

	/// The controller could not compute the Diffie-Hellman key - it refused the peer's point, which is what a
	/// controller does with a key that is not on the curve.
	pub fn on_dhkey_failed(&mut self) -> Vec<Step> {
		if self.state == State::Done || self.state == State::Failed {
			return Vec::new();
		}
		self.fail(reason::INVALID_PARAMETERS)
	}

	// Stage two, once the peer's nonce, the Diffie-Hellman key and - for numeric comparison - a person's yes are in
	// hand: they arrive in any order.
	fn check_value(&mut self) -> Vec<Step> {
		let (Some(dhkey), Some(nb)) = (self.dhkey.as_ref(), self.nb) else { return Vec::new() };
		if self.state != State::AwaitDhKey || !self.confirmed {
			return Vec::new();
		}
		let keys = f5(dhkey, &self.na, &nb, &self.local, &self.peer);
		let io_a = [self.request[3], self.request[2], self.request[1]];
		let ea = f6(&keys.mac_key, &self.na, &nb, &self.r(), &io_a, &self.local, &self.peer);
		self.keys = Some(keys);
		self.state = State::AwaitCheck;
		alloc::vec![Step::Send(pdu(code::PAIRING_DHKEY_CHECK, &reverse16(&ea)))]
	}

	/// THE LINK IS ENCRYPTED: the keys are distributed now - the peer's first, then this host's.
	pub fn on_encrypted(&mut self) -> Vec<Step> {
		if self.state != State::Encrypting {
			return Vec::new();
		}
		self.state = State::Distributing;
		self.after_distribution()
	}

	fn on_distributed(&mut self, pdu_bytes: &[u8]) -> Vec<Step> {
		let value = &pdu_bytes[1..];
		match pdu_bytes[0] {
			code::ENCRYPTION_INFORMATION if value.len() == 16 && self.owed & DIST_ENCRYPTION != 0 => {
				let mut ltk = [0u8; 16];
				ltk.copy_from_slice(value);
				self.ltk = Some(reverse16(&ltk));
				clear(&mut ltk);
			}
			code::MASTER_IDENTIFICATION if value.len() == 10 && self.owed & DIST_ENCRYPTION != 0 && self.ltk.is_some() => {
				self.ediv = u16::from_le_bytes([value[0], value[1]]);
				self.rand.copy_from_slice(&value[2..10]);
				self.owed &= !DIST_ENCRYPTION;
			}
			code::IDENTITY_INFORMATION if value.len() == 16 && self.owed & DIST_IDENTITY != 0 => {
				let mut irk = [0u8; 16];
				irk.copy_from_slice(value);
				self.irk = Some(reverse16(&irk));
			}
			code::IDENTITY_ADDRESS_INFORMATION if value.len() == 7 && self.owed & DIST_IDENTITY != 0 && self.irk.is_some() => {
				let mut identity = [0u8; 7];
				identity[0] = value[0];
				for (at, byte) in value[1..].iter().rev().enumerate() {
					identity[1 + at] = *byte;
				}
				// AN IDENTITY ADDRESS IS PUBLIC OR STATIC RANDOM: a private address named as an identity is no identity.
				if identity[0] > 1 || (identity[0] == 1 && identity[1] >> 6 != 0b11) {
					return self.fail(reason::INVALID_PARAMETERS);
				}
				self.identity = Some(identity);
				self.owed &= !DIST_IDENTITY;
			}
			// A signing key was not asked for; one sent anyway changes nothing.
			code::SIGNING_INFORMATION => {}
			_ => return self.fail(reason::UNSPECIFIED),
		}
		self.after_distribution()
	}

	// When the peer has given everything it owes, this host gives its identity, and the bond is made.
	fn after_distribution(&mut self) -> Vec<Step> {
		if self.owed != 0 {
			return Vec::new();
		}
		let mut steps = Vec::new();
		if self.given & DIST_IDENTITY != 0 {
			steps.push(Step::Send(pdu(code::IDENTITY_INFORMATION, &reverse16(&self.options.irk))));
			let mut address = alloc::vec![self.options.identity[0]];
			address.extend(self.options.identity[1..].iter().rev());
			steps.push(Step::Send(pdu(code::IDENTITY_ADDRESS_INFORMATION, &address)));
		}
		let ltk = if self.secure_connections { self.keys.map(|keys| keys.ltk) } else { self.ltk };
		let Some(ltk) = ltk else { return self.fail(reason::UNSPECIFIED) };
		// THE BR/EDR LINK KEY FROM AN LE SECURE CONNECTIONS LTK, where both sides set LinkKey in both fields.
		let derive = self.bredr.is_none() && self.secure_connections && self.request[5] & self.response[5] & self.request[6] & self.response[6] & DIST_LINK_KEY != 0;
		let link_key = derive.then(|| {
			let mut key = crate::bt_keys::link_key_from_ltk(&ltk, self.request[3] & self.response[3] & AUTH_CT2 != 0);
			key.reverse();
			key
		});
		let bonded = Bonded { ltk, ediv: self.ediv, rand: self.rand, irk: self.irk, identity: self.identity, secure_connections: self.secure_connections, authenticated: self.authenticated(), link_key };
		self.state = State::Done;
		self.forget_keys();
		steps.push(Step::Bonded(Box::new(bonded)));
		steps
	}
}

/// THE RESPONDER OVER BR/EDR, for a link the peer holds the central role on: cross-transport key derivation needs no
/// person and no exchange of values, only both sides' word that they want it - so this side answers the request with
/// what both may give, gives its identity first as a responder does, and takes the peer's.
pub struct BredrResponder {
	request: [u8; 7],
	response: [u8; 7],
	link_key: [u8; 16],
	authenticated: bool,
	owed: u8,
	irk: Option<[u8; 16]>,
	identity: Option<Address>,
	finished: bool,
}

impl BredrResponder {
	/// The peer's Pairing Request over the BR/EDR Security Manager's channel, on a link encrypted with `link_key` - a
	/// Secure Connections key, which the caller checked - as HCI carries it. `None` with the steps that refuse it.
	pub fn on_request(request_bytes: &[u8], link_key: [u8; 16], authenticated: bool, options: &Options) -> (Option<BredrResponder>, Vec<Step>) {
		let refuse = |why: u8| (None, alloc::vec![Step::Send(alloc::vec![code::PAIRING_FAILED, why]), Step::Failed(why)]);
		let Ok(request) = <[u8; 7]>::try_from(request_bytes) else { return refuse(reason::INVALID_PARAMETERS) };
		if request[0] != code::PAIRING_REQUEST {
			return refuse(reason::UNSPECIFIED);
		}
		if request[4] != KEY_SIZE {
			return refuse(reason::ENCRYPTION_KEY_SIZE);
		}
		// A PEER THAT ASKS FOR NO DERIVATION is answered that there is nothing to do here.
		if request[5] & request[6] & DIST_ENCRYPTION == 0 {
			return refuse(reason::PAIRING_NOT_SUPPORTED);
		}
		let response = [
			code::PAIRING_RESPONSE,
			0x00,
			0x00,
			request[3] & AUTH_CT2,
			KEY_SIZE,
			request[5] & (DIST_ENCRYPTION | DIST_IDENTITY),
			request[6] & (DIST_ENCRYPTION | DIST_IDENTITY),
		];
		let mut steps = alloc::vec![Step::Send(response.to_vec())];
		// THE RESPONDER GIVES FIRST: its identity, where the initiator asked for it.
		if response[6] & DIST_IDENTITY != 0 {
			steps.push(Step::Send(pdu(code::IDENTITY_INFORMATION, &reverse16(&options.irk))));
			let mut address = alloc::vec![options.identity[0]];
			address.extend(options.identity[1..].iter().rev());
			steps.push(Step::Send(pdu(code::IDENTITY_ADDRESS_INFORMATION, &address)));
		}
		let mut responder = BredrResponder { request, response, link_key, authenticated, owed: response[5] & DIST_IDENTITY, irk: None, identity: None, finished: false };
		steps.extend(responder.finish_if_owed_nothing());
		(Some(responder), steps)
	}

	/// Whether the exchange is over, bonded or not.
	pub fn is_finished(&self) -> bool {
		self.finished
	}

	/// One PDU from the initiator: its identity.
	pub fn on_pdu(&mut self, pdu_bytes: &[u8]) -> Vec<Step> {
		if self.finished {
			return Vec::new();
		}
		let Some(&kind) = pdu_bytes.first() else { return self.fail(reason::INVALID_PARAMETERS) };
		let value = &pdu_bytes[1..];
		match kind {
			code::PAIRING_FAILED => {
				self.finished = true;
				clear(&mut self.link_key);
				return alloc::vec![Step::Failed(value.first().copied().unwrap_or(reason::UNSPECIFIED))];
			}
			code::IDENTITY_INFORMATION if value.len() == 16 && self.owed & DIST_IDENTITY != 0 => {
				let mut irk = [0u8; 16];
				irk.copy_from_slice(value);
				self.irk = Some(reverse16(&irk));
			}
			code::IDENTITY_ADDRESS_INFORMATION if value.len() == 7 && self.owed & DIST_IDENTITY != 0 && self.irk.is_some() => {
				let mut identity = [0u8; 7];
				identity[0] = value[0];
				for (at, byte) in value[1..].iter().rev().enumerate() {
					identity[1 + at] = *byte;
				}
				if identity[0] > 1 || (identity[0] == 1 && identity[1] >> 6 != 0b11) {
					return self.fail(reason::INVALID_PARAMETERS);
				}
				self.identity = Some(identity);
				self.owed &= !DIST_IDENTITY;
			}
			code::SIGNING_INFORMATION => {}
			_ => return self.fail(reason::UNSPECIFIED),
		}
		self.finish_if_owed_nothing()
	}

	fn fail(&mut self, why: u8) -> Vec<Step> {
		self.finished = true;
		clear(&mut self.link_key);
		alloc::vec![Step::Send(alloc::vec![code::PAIRING_FAILED, why]), Step::Failed(why)]
	}

	fn finish_if_owed_nothing(&mut self) -> Vec<Step> {
		if self.owed != 0 || self.finished {
			return Vec::new();
		}
		self.finished = true;
		let ct2 = self.request[3] & self.response[3] & AUTH_CT2 != 0;
		let mut source = self.link_key;
		source.reverse();
		let ltk = crate::bt_keys::ltk_from_link_key(&source, ct2);
		clear(&mut source);
		clear(&mut self.link_key);
		let bonded = Bonded { ltk, ediv: 0, rand: [0; 8], irk: self.irk, identity: self.identity, secure_connections: true, authenticated: self.authenticated, link_key: None };
		alloc::vec![Step::Bonded(Box::new(bonded))]
	}
}

impl Drop for BredrResponder {
	fn drop(&mut self) {
		clear(&mut self.link_key);
	}
}

// AN ATTEMPT THAT IS DROPPED TAKES ITS KEYS WITH IT, however it ends: the service lets go of an initiator on
// encryption, on failure, on cancellation and with the link, and none of those may leave the Diffie-Hellman key or
// the LTK behind in the memory it frees.
impl Drop for Initiator {
	fn drop(&mut self) {
		self.forget_keys();
	}
}

// Zero key bytes. `black_box` stands for a reader of what was written, so a store the optimiser could prove dead -
// the value is about to be discarded, which is exactly when these run - is still made. This crate holds no
// `unsafe`, so it is this rather than a volatile write.
fn clear(bytes: &mut [u8]) {
	bytes.fill(0);
	core::hint::black_box(bytes);
}

#[cfg(test)]
mod tests;

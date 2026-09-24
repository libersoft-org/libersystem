//! THE SECURITY MANAGER, AS THE INITIATOR OF A JUST WORKS LE SECURE CONNECTIONS PAIRING.
//!
//! This is the whole exchange that turns an unencrypted link into a bonded one, driven by the PDUs
//! the peer sends and the two values the controller computes: this host's public key, and the
//! Diffie-Hellman key once the peer's is known. Every derivation is `smp`'s, checked against the
//! specification's own sample data; what is here is the ORDER, and the checks that decide whether
//! the order was kept.
//!
//! NO DOWNGRADE. A peer whose pairing response does not set the Secure Connections bit is refused -
//! not paired under the legacy model, which a passive listener can break from a recording. A peer
//! that cannot do Secure Connections cannot bond to this machine, and that is the milestone's own
//! scope rather than a limitation of this code.
//!
//! JUST WORKS IS ENCRYPTED AND NOT AUTHENTICATED, and the state machine does not pretend otherwise:
//! the confirm value it checks proves that the peer committed to its nonce before seeing this host's
//! - which defeats a passive attacker - and proves nothing about WHICH radio answered. A radio that
//! answered first completes this exchange exactly as the mouse would.
//!
//! EVERY VALUE ON THE SMP WIRE IS LITTLE-ENDIAN and every `smp` function takes its values most
//! significant first. The reversal happens at the boundary, here, once, and is named.

use crate::hci_codec::{public_key_x, reverse16};
use crate::smp::{Keys, f4, f5, f6};

/// PDU codes.
pub mod code {
	pub const PAIRING_REQUEST: u8 = 0x01;
	pub const PAIRING_RESPONSE: u8 = 0x02;
	pub const PAIRING_CONFIRM: u8 = 0x03;
	pub const PAIRING_RANDOM: u8 = 0x04;
	pub const PAIRING_FAILED: u8 = 0x05;
	pub const PAIRING_PUBLIC_KEY: u8 = 0x0c;
	pub const PAIRING_DHKEY_CHECK: u8 = 0x0d;
}

/// Why a pairing failed, as the reason code the Pairing Failed PDU carries.
pub mod reason {
	pub const AUTHENTICATION_REQUIREMENTS: u8 = 0x03;
	pub const CONFIRM_VALUE_FAILED: u8 = 0x04;
	pub const PAIRING_NOT_SUPPORTED: u8 = 0x05;
	pub const ENCRYPTION_KEY_SIZE: u8 = 0x06;
	pub const UNSPECIFIED: u8 = 0x08;
	pub const INVALID_PARAMETERS: u8 = 0x0a;
	pub const DHKEY_CHECK_FAILED: u8 = 0x0b;
}

/// IO capability: no input and no output, which is what a host pairing a mouse with no screen to
/// compare a number on is - and what makes the association model Just Works.
pub const IO_NO_INPUT_NO_OUTPUT: u8 = 0x03;

/// Authentication requirements: bonding, and Secure Connections. MITM is NOT requested, because
/// nothing here can provide it; asking for it would be a claim this exchange cannot honour.
pub const AUTH_BONDING: u8 = 0x01;
pub const AUTH_SECURE_CONNECTIONS: u8 = 0x08;

/// The only key size this host accepts.
pub const KEY_SIZE: u8 = 16;

/// A device address as the derivations take it: the address type and then the six bytes, most
/// significant first. Type 0 is public and 1 is random.
pub type Address = [u8; 7];

/// What the service must do next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
	/// Send this SMP PDU to the peer.
	Send(alloc::vec::Vec<u8>),
	/// Ask the controller for the Diffie-Hellman key against this peer public key, in wire order.
	GenerateDhKey([u8; 64]),
	/// Pairing has completed: start encryption with this long-term key, most significant first.
	Encrypt([u8; 16]),
	/// Pairing has failed with this reason. Anything the caller holds for it is to be discarded.
	Failed(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
	/// The request has been sent and the response is awaited.
	AwaitResponse,
	/// This host's public key has been sent and the peer's is awaited.
	AwaitPublicKey,
	/// The peer's confirm value is awaited.
	AwaitConfirm,
	/// This host's nonce has been sent and the peer's is awaited.
	AwaitRandom,
	/// The peer's nonce has been checked and the Diffie-Hellman key is still being computed.
	AwaitDhKey,
	/// This host's check value has been sent and the peer's is awaited.
	AwaitCheck,
	/// Bonded, as far as the exchange goes: encryption is next.
	Done,
	Failed,
}

/// One pairing attempt, from the request to the key.
#[derive(Clone)]
pub struct Initiator {
	state: State,
	local: Address,
	peer: Address,
	/// This host's nonce, most significant first.
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
}

impl Initiator {
	/// Begin: the first PDU is the request. `na` must come from healthy system randomness; this
	/// module cannot check that and the caller's refusal to pair without it is what enforces it.
	pub fn start(local: Address, peer: Address, public_key: [u8; 64], na: [u8; 16]) -> (Initiator, Step) {
		let request = [code::PAIRING_REQUEST, IO_NO_INPUT_NO_OUTPUT, 0x00, AUTH_BONDING | AUTH_SECURE_CONNECTIONS, KEY_SIZE, 0x00, 0x00];
		let pairing = Initiator { state: State::AwaitResponse, local, peer, na, pka: public_key, pkb: None, cb: None, nb: None, dhkey: None, keys: None, request, response: [0; 7] };
		(pairing, Step::Send(request.to_vec()))
	}

	/// Whether the exchange has reached its key.
	pub fn is_done(&self) -> bool {
		self.state == State::Done
	}

	/// Whether it ended without one.
	pub fn is_failed(&self) -> bool {
		self.state == State::Failed
	}

	/// The key, once there is one.
	pub fn ltk(&self) -> Option<[u8; 16]> {
		if self.state == State::Done { self.keys.map(|keys| keys.ltk) } else { None }
	}

	fn fail(&mut self, why: u8) -> alloc::vec::Vec<Step> {
		self.state = State::Failed;
		// THE TRANSIENT KEYS ARE CLEARED ON FAILURE, not only on completion. A failed attempt that
		// left its Diffie-Hellman key and MacKey in memory would leave key material behind for the
		// life of this object, which outlives the attempt.
		self.dhkey = None;
		self.keys = None;
		alloc::vec![Step::Send(alloc::vec![code::PAIRING_FAILED, why]), Step::Failed(why)]
	}

	/// One PDU from the peer.
	pub fn on_pdu(&mut self, pdu: &[u8]) -> alloc::vec::Vec<Step> {
		if self.state == State::Done || self.state == State::Failed {
			return alloc::vec::Vec::new();
		}
		let Some(&kind) = pdu.first() else { return self.fail(reason::INVALID_PARAMETERS) };
		if kind == code::PAIRING_FAILED {
			// THE PEER'S REASON IS REPORTED AND NOT ANSWERED: a failed PDU is not replied to with
			// another, which would be two sides each telling the other it had failed.
			self.state = State::Failed;
			self.dhkey = None;
			self.keys = None;
			return alloc::vec![Step::Failed(pdu.get(1).copied().unwrap_or(reason::UNSPECIFIED))];
		}
		match (self.state, kind) {
			(State::AwaitResponse, code::PAIRING_RESPONSE) => {
				let Ok(response) = <[u8; 7]>::try_from(pdu) else { return self.fail(reason::INVALID_PARAMETERS) };
				// NO DOWNGRADE: a response without Secure Connections is refused, not honoured under
				// the legacy model.
				if response[3] & AUTH_SECURE_CONNECTIONS == 0 {
					return self.fail(reason::AUTHENTICATION_REQUIREMENTS);
				}
				if response[4] != KEY_SIZE {
					return self.fail(reason::ENCRYPTION_KEY_SIZE);
				}
				if response[1] > 0x04 {
					return self.fail(reason::INVALID_PARAMETERS);
				}
				self.response = response;
				self.state = State::AwaitPublicKey;
				let mut key = alloc::vec![code::PAIRING_PUBLIC_KEY];
				key.extend_from_slice(&self.pka);
				alloc::vec![Step::Send(key)]
			}
			(State::AwaitPublicKey, code::PAIRING_PUBLIC_KEY) => {
				if pdu.len() != 65 {
					return self.fail(reason::INVALID_PARAMETERS);
				}
				let mut pkb = [0u8; 64];
				pkb.copy_from_slice(&pdu[1..]);
				// A PEER ANSWERING WITH THIS HOST'S OWN KEY is reflecting it, and a Diffie-Hellman key
				// computed against one's own public key is one the reflector can compute too.
				if pkb == self.pka {
					return self.fail(reason::INVALID_PARAMETERS);
				}
				self.pkb = Some(pkb);
				self.state = State::AwaitConfirm;
				alloc::vec![Step::GenerateDhKey(pkb)]
			}
			(State::AwaitConfirm, code::PAIRING_CONFIRM) => {
				let Ok(cb) = <[u8; 16]>::try_from(&pdu[1..]) else { return self.fail(reason::INVALID_PARAMETERS) };
				self.cb = Some(reverse16(&cb));
				self.state = State::AwaitRandom;
				let mut random = alloc::vec![code::PAIRING_RANDOM];
				random.extend_from_slice(&reverse16(&self.na));
				alloc::vec![Step::Send(random)]
			}
			(State::AwaitRandom, code::PAIRING_RANDOM) => {
				let Ok(nb) = <[u8; 16]>::try_from(&pdu[1..]) else { return self.fail(reason::INVALID_PARAMETERS) };
				let nb = reverse16(&nb);
				let (Some(pkb), Some(cb)) = (self.pkb, self.cb) else { return self.fail(reason::UNSPECIFIED) };
				// THE CONFIRM VALUE IS CHECKED AGAINST THE NONCE IT COMMITTED TO. The peer sent Cb
				// before it saw Na; a Cb that does not match f4 over the Nb it now reveals is a peer
				// that chose its nonce after seeing this host's, which is what the commitment exists
				// to prevent.
				if f4(&public_key_x(&pkb), &public_key_x(&self.pka), &nb, 0) != cb {
					return self.fail(reason::CONFIRM_VALUE_FAILED);
				}
				self.nb = Some(nb);
				self.state = State::AwaitDhKey;
				self.check_value()
			}
			(State::AwaitCheck, code::PAIRING_DHKEY_CHECK) => {
				let Ok(eb) = <[u8; 16]>::try_from(&pdu[1..]) else { return self.fail(reason::INVALID_PARAMETERS) };
				let (Some(keys), Some(nb)) = (self.keys, self.nb) else { return self.fail(reason::UNSPECIFIED) };
				let io_b = [self.response[3], self.response[2], self.response[1]];
				// THE PEER'S CHECK VALUE IS ITS PROOF THAT IT HOLDS THE SAME DIFFIE-HELLMAN KEY: it
				// is keyed with the MacKey both sides derived, over the exchange in its own order.
				if f6(&keys.mac_key, &nb, &self.na, &[0u8; 16], &io_b, &self.peer, &self.local) != reverse16(&eb) {
					return self.fail(reason::DHKEY_CHECK_FAILED);
				}
				self.state = State::Done;
				// The MacKey has done its work; the LTK is what outlives the exchange.
				let ltk = keys.ltk;
				self.dhkey = None;
				self.keys = Some(Keys { mac_key: [0; 16], ltk });
				alloc::vec![Step::Encrypt(ltk)]
			}
			// A PDU THAT IS WELL FORMED AND ARRIVES IN THE WRONG STATE is a peer that is not
			// following the exchange, and continuing would mean deriving keys from an order that is
			// not the one the specification's security argument is about.
			_ => self.fail(reason::UNSPECIFIED),
		}
	}

	/// The Diffie-Hellman key the controller computed, in wire order.
	pub fn on_dhkey(&mut self, wire: &[u8; 32]) -> alloc::vec::Vec<Step> {
		if self.state == State::Done || self.state == State::Failed {
			return alloc::vec::Vec::new();
		}
		self.dhkey = Some(crate::hci_codec::dhkey(wire));
		self.check_value()
	}

	/// The controller could not compute the Diffie-Hellman key - it refused the peer's point, which is
	/// what a controller does with a key that is not on the curve.
	pub fn on_dhkey_failed(&mut self) -> alloc::vec::Vec<Step> {
		if self.state == State::Done || self.state == State::Failed {
			return alloc::vec::Vec::new();
		}
		self.fail(reason::INVALID_PARAMETERS)
	}

	// Stage two, once BOTH the peer's nonce and the Diffie-Hellman key are in hand - they arrive in
	// either order, because one comes from the peer and the other from the controller.
	fn check_value(&mut self) -> alloc::vec::Vec<Step> {
		let (Some(dhkey), Some(nb)) = (self.dhkey, self.nb) else { return alloc::vec::Vec::new() };
		if self.state != State::AwaitDhKey {
			return alloc::vec::Vec::new();
		}
		let keys = f5(&dhkey, &self.na, &nb, &self.local, &self.peer);
		let io_a = [self.request[3], self.request[2], self.request[1]];
		let ea = f6(&keys.mac_key, &self.na, &nb, &[0u8; 16], &io_a, &self.local, &self.peer);
		self.keys = Some(keys);
		self.state = State::AwaitCheck;
		let mut check = alloc::vec![code::PAIRING_DHKEY_CHECK];
		check.extend_from_slice(&reverse16(&ea));
		alloc::vec![Step::Send(check)]
	}
}

#[cfg(test)]
mod tests;

//! THE LE SECURE CONNECTIONS KEY DERIVATIONS, which are four uses of AES-CMAC and one subtraction.
//!
//! WHAT THE HOST DOES AND WHAT THE CONTROLLER DOES is the division this module is shaped by. The
//! elliptic-curve half - the P-256 key pair and the Diffie-Hellman shared secret - belongs to the
//! CONTROLLER, which this milestone requires to support the HCI public-key and DHKey commands. So
//! there is no curve arithmetic here and there must not be: a host implementing P-256 beside a
//! controller that already has it would be a second implementation of the one operation whose
//! mistakes are silent, for no capability gained.
//!
//! WHAT IS LEFT IS EXACTLY FOUR FUNCTIONS, and each is a message laid out in a fixed order and
//! passed through CMAC. The mistakes available are all of the same shape - a field in the wrong
//! order, a counter on the wrong derivation, a length that is not the one the specification names -
//! and every one of them produces sixteen plausible bytes. Only the specification's own sample data
//! says which sixteen, which is why every function here has its vector from Appendix D beside it.
//!
//! THIS SLICE IS JUST WORKS AND SAYS SO. The association model that compares numbers on two screens
//! needs `g2` and a person; a mouse has neither. What Just Works buys is encryption against a
//! passive listener and NOT authentication against an active one - a radio that answered first can
//! complete this exchange, and nothing in these functions can tell. `g2` is here because the
//! derivation is the same family and its vector checks the same code paths, not because this
//! milestone uses it.

use crate::aes::{BLOCK, Key};
use crate::cmac::mac;

/// The salt `f5` mixes the Diffie-Hellman secret with before deriving anything from it.
///
/// ITS VALUE IS CONFIRMED BY THE SPECIFICATION'S OWN VECTOR rather than by this comment: Appendix
/// D.3 prints the intermediate `T` for a known `W`, and the test below computes it from this
/// constant. A wrong salt produces a wrong `T`, a wrong MacKey and a wrong LTK, and the pairing
/// fails at the check value with nothing saying why.
const F5_SALT: [u8; BLOCK] = [0x6c, 0x88, 0x83, 0x91, 0xaa, 0xf5, 0xa5, 0x38, 0x60, 0x37, 0x0b, 0xdb, 0x5a, 0x60, 0x83, 0xbe];

/// The key identifier `f5` puts in every derivation, which is ASCII `btle`.
const F5_KEY_ID: [u8; 4] = [0x62, 0x74, 0x6c, 0x65];

/// The key length `f5` names, 256 bits, big-endian.
const F5_LENGTH: [u8; 2] = [0x01, 0x00];

/// A public key's X coordinate, which is what every one of these functions takes rather than the
/// whole point: the Y coordinate adds nothing to the derivation and carrying it would be one more
/// thing to get the byte order of wrong.
pub type PublicKeyX = [u8; 32];

/// A 128-bit nonce, key or confirm value.
pub type Value = [u8; BLOCK];

/// A device address as these functions take it: the address type byte and then the six address
/// bytes, most significant first.
pub type Address = [u8; 7];

/// `f4`, the confirm value. `z` is zero for Just Works and for numeric comparison; it carries a
/// passkey bit in the models this slice does not implement.
pub fn f4(u: &PublicKeyX, v: &PublicKeyX, x: &Value, z: u8) -> Value {
	let mut message = [0u8; 32 + 32 + 1];
	message[..32].copy_from_slice(u);
	message[32..64].copy_from_slice(v);
	message[64] = z;
	mac(&Key::new(x), &message)
}

/// The two keys `f5` derives: the one that authenticates the exchange and the one that encrypts the
/// link.
///
/// THE COUNTER IS WHAT SEPARATES THEM AND IT IS THE ONLY THING THAT DOES. Counter zero is the
/// MacKey and counter one is the LTK; swapped, the pairing still completes - both sides derive the
/// same pair in the same wrong order - and the link is encrypted with the key that was meant to
/// authenticate it. Nothing observable distinguishes that from a correct pairing until a peer that
/// got it right refuses to talk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Keys {
	pub mac_key: Value,
	pub ltk: Value,
}

/// `f5`, from the Diffie-Hellman secret the controller computed.
pub fn f5(w: &[u8; 32], n1: &Value, n2: &Value, a1: &Address, a2: &Address) -> Keys {
	// THE SECRET IS NOT THE KEY. `T` is what a derivation is keyed with, and using `W` directly
	// would hand every derived key the same relationship to the secret that the secret has to
	// itself.
	let t = mac(&Key::new(&F5_SALT), w);
	let key = Key::new(&t);
	let mut message = [0u8; 1 + 4 + BLOCK + BLOCK + 7 + 7 + 2];
	message[1..5].copy_from_slice(&F5_KEY_ID);
	message[5..21].copy_from_slice(n1);
	message[21..37].copy_from_slice(n2);
	message[37..44].copy_from_slice(a1);
	message[44..51].copy_from_slice(a2);
	message[51..53].copy_from_slice(&F5_LENGTH);
	message[0] = 0;
	let mac_key = mac(&key, &message);
	message[0] = 1;
	let ltk = mac(&key, &message);
	Keys { mac_key, ltk }
}

/// `f6`, the check value each side sends and verifies.
pub fn f6(w: &Value, n1: &Value, n2: &Value, r: &Value, io_cap: &[u8; 3], a1: &Address, a2: &Address) -> Value {
	let mut message = [0u8; BLOCK * 3 + 3 + 7 + 7];
	message[..16].copy_from_slice(n1);
	message[16..32].copy_from_slice(n2);
	message[32..48].copy_from_slice(r);
	message[48..51].copy_from_slice(io_cap);
	message[51..58].copy_from_slice(a1);
	message[58..65].copy_from_slice(a2);
	mac(&Key::new(w), &message)
}

/// `g2`, the numeric comparison value, as the specification's thirty-two bits.
///
/// THE SIX-DIGIT NUMBER A PERSON READS IS THIS VALUE MODULO A MILLION, and that reduction is the
/// caller's: a comparison this slice does not perform should not have a number formatted for it.
pub fn g2(u: &PublicKeyX, v: &PublicKeyX, x: &Value, y: &Value) -> u32 {
	let mut message = [0u8; 32 + 32 + BLOCK];
	message[..32].copy_from_slice(u);
	message[32..64].copy_from_slice(v);
	message[64..80].copy_from_slice(y);
	let tag = mac(&Key::new(x), &message);
	u32::from_be_bytes([tag[12], tag[13], tag[14], tag[15]])
}

/// The six digits a person would be shown, for the association model this slice does not implement.
pub const fn comparison_digits(g2: u32) -> u32 {
	g2 % 1_000_000
}

#[cfg(test)]
mod tests;

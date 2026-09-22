//! AES-CMAC, RFC 4493, which is the one primitive every Bluetooth key derivation is built from.
//!
//! CMAC IS CBC-MAC WITH THE LAST BLOCK FIXED. Plain CBC-MAC is forgeable when messages may differ in
//! length - given the tag of one message, an attacker can produce the tag of a longer one - and the
//! two subkeys exist to close exactly that. A host that wrote CBC-MAC and called it CMAC would
//! produce tags that pass its own tests and fail every peer, which is the good case; the bad one is
//! a peer that accepts them.
//!
//! THE SUBKEY DERIVATION IS A SHIFT AND A CONDITIONAL XOR, and the condition is on a key bit. It is
//! written branchless for the same reason `aes::xtime` is - it costs nothing - while the module as a
//! whole makes no constant-time claim, for the reasons `aes` states.

use crate::aes::{BLOCK, Key};

/// The constant the shift reduces by, which is the Rb of RFC 4493 for a 128-bit block.
const RB: u8 = 0x87;

/// Shift a block left by one bit, and reduce when the bit that fell off was set.
fn subkey(block: &[u8; BLOCK]) -> [u8; BLOCK] {
	let overflow = block[0] >> 7;
	let mut out = [0u8; BLOCK];
	for at in 0..BLOCK {
		let next = if at + 1 < BLOCK { block[at + 1] >> 7 } else { 0 };
		out[at] = (block[at] << 1) | next;
	}
	out[BLOCK - 1] ^= RB & overflow.wrapping_neg();
	out
}

/// The two subkeys RFC 4493 derives from the cipher key.
///
/// PUBLIC BECAUSE THE STANDARD PUBLISHES THEM, and a test that can check them separately can say
/// which half of the construction is wrong rather than only that the tag is.
pub fn subkeys(key: &Key) -> ([u8; BLOCK], [u8; BLOCK]) {
	let l = key.block(&[0u8; BLOCK]);
	let k1 = subkey(&l);
	let k2 = subkey(&k1);
	(k1, k2)
}

/// The AES-CMAC of `message` under `key`.
///
/// THE EMPTY MESSAGE IS A MESSAGE and has a tag of its own - it takes the padded path, because a
/// zero-length message is not a whole number of blocks. A construction that special-cased it into
/// the unpadded path would agree with nothing.
pub fn mac(key: &Key, message: &[u8]) -> [u8; BLOCK] {
	let (k1, k2) = subkeys(key);
	let whole = !message.is_empty() && message.len() % BLOCK == 0;
	let blocks = if message.is_empty() { 1 } else { message.len().div_ceil(BLOCK) };
	let mut state = [0u8; BLOCK];
	for at in 0..blocks {
		let start = at * BLOCK;
		let mut block = [0u8; BLOCK];
		if at + 1 < blocks {
			block.copy_from_slice(&message[start..start + BLOCK]);
		} else if whole {
			block.copy_from_slice(&message[start..start + BLOCK]);
			for (byte, k) in block.iter_mut().zip(k1.iter()) {
				*byte ^= *k;
			}
		} else {
			// THE PADDING IS A ONE BIT AND THEN ZEROS, so a message and that message with trailing
			// zeros appended are different messages. Padding with zeros alone would make them one.
			let tail = &message[start..];
			block[..tail.len()].copy_from_slice(tail);
			block[tail.len()] = 0x80;
			for (byte, k) in block.iter_mut().zip(k2.iter()) {
				*byte ^= *k;
			}
		}
		for (byte, s) in block.iter_mut().zip(state.iter()) {
			*byte ^= *s;
		}
		state = key.block(&block);
	}
	state
}

#[cfg(test)]
mod tests;

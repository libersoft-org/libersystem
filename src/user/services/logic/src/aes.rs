//! AES-128, ENCRYPTION ONLY, because that is all anything above it needs.
//!
//! CMAC is built from the forward direction alone, and every Bluetooth key derivation is built from
//! CMAC - so the inverse cipher, the inverse S-box and the inverse mix-columns are absent rather
//! than written and unused. Code that exists and is never exercised is code nothing would notice
//! breaking.
//!
//! THIS IS NOT CONSTANT-TIME AGAINST A LOCAL ATTACKER and says so here rather than implying
//! otherwise by silence. The S-box is a table lookup, so its access pattern depends on the key, and
//! a process that could measure this one's cache could learn from it. What it is for is a link key
//! derivation inside a service whose Domain holds nothing else, against a peer on a radio - and a
//! peer on a radio cannot measure a cache. A table-free implementation is a different piece of work
//! with a different justification, and pretending this one has that property would be worse than
//! either.

/// The block and key size this implementation has. AES-192 and AES-256 differ in the key schedule
/// only, and are absent for the same reason the inverse cipher is.
pub const BLOCK: usize = 16;
pub const KEY: usize = 16;

/// The number of rounds AES-128 performs.
const ROUNDS: usize = 10;

/// The forward S-box, FIPS-197 figure 7.
const SBOX: [u8; 256] = [
	0x63,
	0x7c,
	0x77,
	0x7b,
	0xf2,
	0x6b,
	0x6f,
	0xc5,
	0x30,
	0x01,
	0x67,
	0x2b,
	0xfe,
	0xd7,
	0xab,
	0x76,
	0xca,
	0x82,
	0xc9,
	0x7d,
	0xfa,
	0x59,
	0x47,
	0xf0,
	0xad,
	0xd4,
	0xa2,
	0xaf,
	0x9c,
	0xa4,
	0x72,
	0xc0,
	0xb7,
	0xfd,
	0x93,
	0x26,
	0x36,
	0x3f,
	0xf7,
	0xcc,
	0x34,
	0xa5,
	0xe5,
	0xf1,
	0x71,
	0xd8,
	0x31,
	0x15,
	0x04,
	0xc7,
	0x23,
	0xc3,
	0x18,
	0x96,
	0x05,
	0x9a,
	0x07,
	0x12,
	0x80,
	0xe2,
	0xeb,
	0x27,
	0xb2,
	0x75,
	0x09,
	0x83,
	0x2c,
	0x1a,
	0x1b,
	0x6e,
	0x5a,
	0xa0,
	0x52,
	0x3b,
	0xd6,
	0xb3,
	0x29,
	0xe3,
	0x2f,
	0x84,
	0x53,
	0xd1,
	0x00,
	0xed,
	0x20,
	0xfc,
	0xb1,
	0x5b,
	0x6a,
	0xcb,
	0xbe,
	0x39,
	0x4a,
	0x4c,
	0x58,
	0xcf,
	0xd0,
	0xef,
	0xaa,
	0xfb,
	0x43,
	0x4d,
	0x33,
	0x85,
	0x45,
	0xf9,
	0x02,
	0x7f,
	0x50,
	0x3c,
	0x9f,
	0xa8,
	0x51,
	0xa3,
	0x40,
	0x8f,
	0x92,
	0x9d,
	0x38,
	0xf5,
	0xbc,
	0xb6,
	0xda,
	0x21,
	0x10,
	0xff,
	0xf3,
	0xd2,
	0xcd,
	0x0c,
	0x13,
	0xec,
	0x5f,
	0x97,
	0x44,
	0x17,
	0xc4,
	0xa7,
	0x7e,
	0x3d,
	0x64,
	0x5d,
	0x19,
	0x73,
	0x60,
	0x81,
	0x4f,
	0xdc,
	0x22,
	0x2a,
	0x90,
	0x88,
	0x46,
	0xee,
	0xb8,
	0x14,
	0xde,
	0x5e,
	0x0b,
	0xdb,
	0xe0,
	0x32,
	0x3a,
	0x0a,
	0x49,
	0x06,
	0x24,
	0x5c,
	0xc2,
	0xd3,
	0xac,
	0x62,
	0x91,
	0x95,
	0xe4,
	0x79,
	0xe7,
	0xc8,
	0x37,
	0x6d,
	0x8d,
	0xd5,
	0x4e,
	0xa9,
	0x6c,
	0x56,
	0xf4,
	0xea,
	0x65,
	0x7a,
	0xae,
	0x08,
	0xba,
	0x78,
	0x25,
	0x2e,
	0x1c,
	0xa6,
	0xb4,
	0xc6,
	0xe8,
	0xdd,
	0x74,
	0x1f,
	0x4b,
	0xbd,
	0x8b,
	0x8a,
	0x70,
	0x3e,
	0xb5,
	0x66,
	0x48,
	0x03,
	0xf6,
	0x0e,
	0x61,
	0x35,
	0x57,
	0xb9,
	0x86,
	0xc1,
	0x1d,
	0x9e,
	0xe1,
	0xf8,
	0x98,
	0x11,
	0x69,
	0xd9,
	0x8e,
	0x94,
	0x9b,
	0x1e,
	0x87,
	0xe9,
	0xce,
	0x55,
	0x28,
	0xdf,
	0x8c,
	0xa1,
	0x89,
	0x0d,
	0xbf,
	0xe6,
	0x42,
	0x68,
	0x41,
	0x99,
	0x2d,
	0x0f,
	0xb0,
	0x54,
	0xbb,
	0x16,
];

/// The round constants, which are `x^(i-1)` in GF(2^8). Ten of them, one per round after the first.
const RCON: [u8; ROUNDS] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

/// Multiply by `x` in GF(2^8) with the AES reduction polynomial.
///
/// BRANCHLESS BECAUSE THE BRANCH IS ON A KEY BIT. The reduction happens exactly when the high bit is
/// set, and an `if` there is a branch whose direction is data - which is the one timing property
/// that is cheap to keep and costs nothing to write.
const fn xtime(byte: u8) -> u8 {
	(byte << 1) ^ (0x1b & (((byte >> 7) & 1).wrapping_neg()))
}

/// An expanded AES-128 key: eleven round keys of sixteen bytes.
#[derive(Clone, Copy)]
pub struct Key {
	round: [[u8; BLOCK]; ROUNDS + 1],
}

impl Key {
	/// Expand a 128-bit key.
	pub fn new(key: &[u8; KEY]) -> Key {
		let mut round = [[0u8; BLOCK]; ROUNDS + 1];
		round[0] = *key;
		for at in 1..=ROUNDS {
			let previous = round[at - 1];
			// The last word, rotated, substituted, and the round constant on its first byte.
			let mut word = [previous[13], previous[14], previous[15], previous[12]];
			for byte in &mut word {
				*byte = SBOX[*byte as usize];
			}
			word[0] ^= RCON[at - 1];
			let mut next = [0u8; BLOCK];
			for column in 0..4 {
				for row in 0..4 {
					let at_byte = column * 4 + row;
					next[at_byte] = previous[at_byte] ^ if column == 0 { word[row] } else { next[at_byte - 4] };
				}
			}
			round[at] = next;
		}
		Key { round }
	}

	/// Encrypt one block in place.
	pub fn encrypt(&self, block: &mut [u8; BLOCK]) {
		add_round_key(block, &self.round[0]);
		for at in 1..ROUNDS {
			sub_bytes(block);
			shift_rows(block);
			mix_columns(block);
			add_round_key(block, &self.round[at]);
		}
		// THE LAST ROUND HAS NO MIX-COLUMNS, which is the one thing every wrong AES has wrong: the
		// cipher still looks like a cipher, and only a known-answer test says otherwise.
		sub_bytes(block);
		shift_rows(block);
		add_round_key(block, &self.round[ROUNDS]);
	}

	/// Encrypt one block, answering a new one.
	pub fn block(&self, input: &[u8; BLOCK]) -> [u8; BLOCK] {
		let mut out = *input;
		self.encrypt(&mut out);
		out
	}
}

fn add_round_key(block: &mut [u8; BLOCK], key: &[u8; BLOCK]) {
	for (byte, k) in block.iter_mut().zip(key.iter()) {
		*byte ^= *k;
	}
}

fn sub_bytes(block: &mut [u8; BLOCK]) {
	for byte in block.iter_mut() {
		*byte = SBOX[*byte as usize];
	}
}

/// The state is column-major: byte `4c + r` is row `r` of column `c`. Row `r` rotates left by `r`.
fn shift_rows(block: &mut [u8; BLOCK]) {
	let original = *block;
	for row in 1..4 {
		for column in 0..4 {
			block[column * 4 + row] = original[((column + row) % 4) * 4 + row];
		}
	}
}

fn mix_columns(block: &mut [u8; BLOCK]) {
	for column in 0..4 {
		let at = column * 4;
		let (a0, a1, a2, a3) = (block[at], block[at + 1], block[at + 2], block[at + 3]);
		let all = a0 ^ a1 ^ a2 ^ a3;
		block[at] = a0 ^ all ^ xtime(a0 ^ a1);
		block[at + 1] = a1 ^ all ^ xtime(a1 ^ a2);
		block[at + 2] = a2 ^ all ^ xtime(a2 ^ a3);
		block[at + 3] = a3 ^ all ^ xtime(a3 ^ a0);
	}
}

#[cfg(test)]
mod tests;

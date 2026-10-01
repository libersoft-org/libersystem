use super::*;

fn hex_digit(byte: u8) -> u8 {
	match byte {
		b'0'..=b'9' => byte - b'0',
		b'a'..=b'f' => byte - b'a' + 10,
		_ => panic!("invalid hexadecimal test vector"),
	}
}

fn digest_hex(text: &[u8]) -> [u8; 32] {
	let mut out = [0u8; 32];
	for (index, pair) in text.chunks_exact(2).enumerate() {
		out[index] = (hex_digit(pair[0]) << 4) | hex_digit(pair[1]);
	}
	out
}

#[test]
fn standard_vectors_match() {
	assert_eq!(digest(b""), digest_hex(b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"));
	assert_eq!(digest(b"abc"), digest_hex(b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
	assert_eq!(digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"), digest_hex(b"248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"));
}

#[test]
fn parts_fed_in_turn_digest_as_their_concatenation() {
	let whole: [u8; 300] = core::array::from_fn(|index| (index * 7 + 3) as u8);
	// EVERY SPLIT, including the empty parts and the ones that land on and across a block's edge.
	for first in 0..whole.len() {
		for second in [first, (first + 1).min(whole.len()), (first + 64).min(whole.len()), whole.len()] {
			let mut hasher = Sha256::new();
			hasher.update(&whole[..first]);
			hasher.update(&whole[first..second]);
			hasher.update(&whole[second..]);
			assert_eq!(hasher.finish(), digest(&whole), "split at {first} and {second}");
		}
	}
	for length in [0, 1, 55, 56, 63, 64, 65, 119, 120, 128] {
		let mut hasher = Sha256::new();
		for byte in &whole[..length] {
			hasher.update(core::slice::from_ref(byte));
		}
		assert_eq!(hasher.finish(), digest(&whole[..length]), "{length} bytes one at a time");
	}
	let mut hasher = Sha256::new();
	hasher.update(b"ab");
	hasher.update(b"c");
	assert_eq!(hasher.finish(), digest_hex(b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
}

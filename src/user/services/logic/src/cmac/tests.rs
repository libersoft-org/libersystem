use super::{Stream, mac, subkeys};
use crate::aes::{BLOCK, Key};

// RFC 4493's own examples, typed from the document. An expected value produced by this code would
// prove only that it agrees with itself.
fn hex(text: &str) -> alloc::vec::Vec<u8> {
	let clean: alloc::string::String = text.chars().filter(|c| !c.is_whitespace()).collect();
	(0..clean.len() / 2).map(|at| u8::from_str_radix(&clean[at * 2..at * 2 + 2], 16).expect("hex")).collect()
}

fn block(text: &str) -> [u8; BLOCK] {
	let bytes = hex(text);
	let mut out = [0u8; BLOCK];
	out.copy_from_slice(&bytes);
	out
}

// The key every example in RFC 4493 uses.
fn rfc_key() -> Key {
	Key::new(&block("2b7e1516 28aed2a6 abf71588 09cf4f3c"))
}

// The 64-byte message the examples take prefixes of.
const MESSAGE: &str = "6bc1bee2 2e409f96 e93d7e11 7393172a
                       ae2d8a57 1e03ac9c 9eb76fac 45af8e51
                       30c81c46 a35ce411 e5fbc119 1a0a52ef
                       f69f2445 df4f9b17 ad2b417b e66c3710";

#[test]
// RFC 4493 SECTION 4: the two subkeys the standard prints, checked on their own so a failure says
// which half of the construction is wrong rather than only that the tag is.
fn the_two_subkeys_are_the_ones_the_standard_prints() {
	let (k1, k2) = subkeys(&rfc_key());
	assert_eq!(k1, block("fbeed618 35713366 7c85e08f 7236a8de"));
	assert_eq!(k2, block("f7ddac30 6ae266cc f90bc11e e46d513b"));
}

#[test]
// RFC 4493 EXAMPLE 1: the empty message, which takes the padded path because zero bytes are not a
// whole number of blocks. A construction that special-cased it into the unpadded path would agree
// with nothing.
fn the_empty_message_has_a_tag_of_its_own() {
	assert_eq!(mac(&rfc_key(), &[]), block("bb1d6929 e9593728 7fa37d12 9b756746"));
}

#[test]
// RFC 4493 EXAMPLE 2: exactly one block, which is the unpadded path and the first subkey.
fn one_whole_block_takes_the_first_subkey() {
	let message = hex(MESSAGE);
	assert_eq!(mac(&rfc_key(), &message[..16]), block("070a16b4 6b4d4144 f79bdd9d d04a287c"));
}

#[test]
// RFC 4493 EXAMPLE 3: forty bytes, which is two whole blocks and a partial one - the padded path
// with a real message in front of it.
fn a_partial_last_block_takes_the_second_subkey_and_the_padding() {
	let message = hex(MESSAGE);
	assert_eq!(mac(&rfc_key(), &message[..40]), block("dfa66747 de9ae630 30ca3261 1497c827"));
}

#[test]
// RFC 4493 EXAMPLE 4: sixty-four bytes, four whole blocks.
fn four_whole_blocks() {
	let message = hex(MESSAGE);
	assert_eq!(message.len(), 64);
	assert_eq!(mac(&rfc_key(), &message), block("51f0bebf 7e3b9d92 fc497417 79363cfe"));
}

#[test]
// THE PADDING IS A ONE BIT AND THEN ZEROS, so a message and that message with a trailing zero are
// different messages. Padded with zeros alone they would have the same tag, which is a forgery that
// needs no key at all - and every one of the vectors above still passes with that mistake in place,
// because none of them is a prefix of another with zeros between.
fn a_trailing_zero_is_not_the_same_message() {
	let key = rfc_key();
	assert_ne!(mac(&key, &[1, 2, 3]), mac(&key, &[1, 2, 3, 0]));
	assert_ne!(mac(&key, &[]), mac(&key, &[0]));
	// And length alone is not what distinguishes them: the same bytes give the same tag.
	assert_eq!(mac(&key, &[1, 2, 3]), mac(&key, &[1, 2, 3]));
}

#[test]
// THE STREAM IS THE SAME TAG IN PIECES: the RFC's four messages, each split at every point, and a long message fed in
// uneven pieces, agree with the one-shot tag - the empty message and the whole-block ending included, which are where
// a stream that decided "last block" too early would part from it.
fn a_stream_fed_in_any_pieces_gives_the_one_shot_tag() {
	let key = rfc_key();
	let message = hex(MESSAGE);
	for length in [0usize, 16, 40, 64] {
		let whole = mac(&key, &message[..length]);
		for split in 0..=length {
			let mut stream = Stream::new(&key);
			stream.update(&message[..split]);
			stream.update(&message[split..length]);
			assert_eq!(stream.finish(), whole, "length {length} split at {split}");
		}
	}
	let long: alloc::vec::Vec<u8> = (0..5000u32).map(|at| (at * 7 + 3) as u8).collect();
	let mut stream = Stream::new(&key);
	for piece in long.chunks(333) {
		stream.update(piece);
	}
	assert_eq!(stream.finish(), mac(&key, &long));
}

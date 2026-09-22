use super::{BLOCK, Key};

// The vectors here are FIPS-197's own and are typed from the standard rather than produced by this
// code. A test whose expected value came from the implementation proves the implementation agrees
// with itself, which is the one thing a cipher's test must not do.
fn bytes(hex: &str) -> [u8; BLOCK] {
	let mut out = [0u8; BLOCK];
	for (at, slot) in out.iter_mut().enumerate() {
		*slot = u8::from_str_radix(&hex[at * 2..at * 2 + 2], 16).expect("hex");
	}
	out
}

#[test]
// FIPS-197 APPENDIX B, the worked example the standard walks through byte by byte.
fn the_standards_own_worked_example_encrypts_to_the_value_it_prints() {
	let key = Key::new(&bytes("2b7e151628aed2a6abf7158809cf4f3c"));
	assert_eq!(key.block(&bytes("3243f6a8885a308d313198a2e0370734")), bytes("3925841d02dc09fbdc118597196a0b32"));
}

#[test]
// FIPS-197 APPENDIX C.1, the AES-128 known-answer vector.
fn the_standards_known_answer_vector_for_a_hundred_and_twenty_eight_bit_key() {
	let key = Key::new(&bytes("000102030405060708090a0b0c0d0e0f"));
	assert_eq!(key.block(&bytes("00112233445566778899aabbccddeeff")), bytes("69c4e0d86a7b0430d8cdb78070b4c55a"));
}

#[test]
// THE BLUETOOTH CORE SPECIFICATION'S OWN `e` FUNCTION SAMPLE, which is AES-128 under another name -
// and is the vector that matters here, because it is the one the layers above this call.
//
// THE SPECIFICATION PRINTS ITS VALUES MOST-SIGNIFICANT-BYTE FIRST while the wire carries them the
// other way round. They are written here in the standard's own order and the byte order is the
// caller's problem, which is where it belongs: a cipher that swapped bytes for its caller's
// convenience would be a cipher nobody could check against the document.
fn the_bluetooth_sample_for_the_security_function_e() {
	let key = Key::new(&bytes("4C68384139F574D836BCF34E9DFB01BF"));
	assert_eq!(key.block(&bytes("0213243546576879acbdcedfe0f10213")), bytes("99ad1b5226a37e3e058e3b8e27c2c666"));
}

#[test]
// THE LAST ROUND HAS NO MIX-COLUMNS, and this is what says so. Encrypting the all-zero block under
// the all-zero key is a value the standard's test suite carries, and a cipher with the final
// mix-columns left in still produces sixteen plausible bytes - just not these.
fn the_all_zero_block_under_the_all_zero_key() {
	let key = Key::new(&[0u8; BLOCK]);
	assert_eq!(key.block(&[0u8; BLOCK]), bytes("66e94bd4ef8a2c3b884cfa59ca342b2e"));
}

#[test]
// ENCRYPTING IN PLACE AND ENCRYPTING INTO A NEW BLOCK ARE THE SAME OPERATION, which is worth one
// assertion because the two are separate entry points and a caller picks by convenience.
fn in_place_and_by_value_agree() {
	let key = Key::new(&bytes("000102030405060708090a0b0c0d0e0f"));
	let input = bytes("00112233445566778899aabbccddeeff");
	let mut in_place = input;
	key.encrypt(&mut in_place);
	assert_eq!(in_place, key.block(&input));
}

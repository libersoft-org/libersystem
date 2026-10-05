use super::*;

fn hex16(text: &str) -> [u8; 16] {
	let digits: alloc::vec::Vec<u8> = text.bytes().filter(|byte| !byte.is_ascii_whitespace()).collect();
	let mut out = [0u8; 16];
	for (at, pair) in digits.chunks(2).enumerate() {
		out[at] = u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap();
	}
	out
}

const W: &str = "ec0234a3 57c8ad05 341010a6 0a397d9b";

#[test]
fn ah_matches_appendix_d_7() {
	assert_eq!(ah(&hex16(W), 0x708194), 0x0dfbaa);
}

#[test]
fn h6_matches_appendix_d_8() {
	assert_eq!(h6(&hex16(W), b"lebr"), hex16("2d9ae102 e76dc91c e8d3a9e2 80b16399"));
}

#[test]
fn h7_matches_appendix_d_9() {
	assert_eq!(h7(&SALT_TMP1, &hex16(W)), hex16("fb173597 c6a3c0ec d2998c2a 75a57011"));
}

#[test]
fn a_private_address_resolves_with_its_key_and_no_other() {
	let irk = hex16(W);
	let address = private_address(&irk, 0x12_3456);
	assert_eq!(address[0] >> 6, 0b01, "resolvable");
	assert!(resolves(&irk, &address));
	let other = hex16("00112233 44556677 8899aabb ccddeeff");
	assert!(!resolves(&other, &address));
	let mut public = address;
	public[0] &= 0x3F;
	assert!(!resolves(&irk, &public), "an address that is not resolvable is never resolved");
}

#[test]
fn the_cross_transport_derivations_are_the_two_step_h6_and_h7_chains() {
	let key = hex16(W);
	assert_eq!(link_key_from_ltk(&key, false), h6(&h6(&key, b"tmp1"), b"lebr"));
	assert_eq!(link_key_from_ltk(&key, true), h6(&h7(&SALT_TMP1, &key), b"lebr"));
	assert_eq!(ltk_from_link_key(&key, true), h6(&h7(&SALT_TMP2, &key), b"brle"));
	assert_ne!(ltk_from_link_key(&key, true), link_key_from_ltk(&key, true));
}

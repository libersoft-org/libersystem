// Each ATR below is built by hand from the grammar, with its check byte worked out in the comment, so
// the expectation is the standard's and not the parser's.

use super::*;

#[test]
fn a_t0_only_atr_has_no_check_byte() {
	// 3B 02 14 50: direct convention, no interface bytes, two historical bytes, T=0 implied.
	let atr = parse(&[0x3b, 0x02, 0x14, 0x50]).unwrap();
	assert_eq!((atr.protocols, atr.first, atr.historical_at, atr.historical_len), (1, 0, 2, 2));
	assert!(atr.offers(0) && !atr.offers(1));
	assert_eq!(atr.ta1, None);
	// A check byte where none belongs is a trailing byte.
	assert_eq!(parse(&[0x3b, 0x02, 0x14, 0x50, 0x00]), Err(Refusal::Trailing));
}

#[test]
fn a_t1_atr_carries_a_check_byte_that_zeroes_the_xor() {
	// 3B 88 80 01 + "LIBERPIV" + TCK. T0 = 0x88: TD1 present, eight historical bytes. TD1 = 0x80:
	// TD2 present, protocol T=0. TD2 = 0x01: nothing follows, protocol T=1. So T=0 first, T=1 too, and
	// a check byte: 0x88 ^ 0x80 ^ 0x01 ^ 'L' ^ 'I' ^ 'B' ^ 'E' ^ 'R' ^ 'P' ^ 'I' ^ 'V'.
	let mut bytes = alloc_vec(&[0x3b, 0x88, 0x80, 0x01]);
	bytes.extend_from_slice(b"LIBERPIV");
	let tck = [0x88u8, 0x80, 0x01, b'L', b'I', b'B', b'E', b'R', b'P', b'I', b'V'].iter().fold(0, |a, b| a ^ b);
	assert_eq!(check_byte(&bytes), tck);
	bytes.push(tck);
	let atr = parse(&bytes).unwrap();
	assert_eq!(atr.protocols, 0b11);
	assert_eq!(atr.first, 0);
	assert_eq!((atr.historical_at, atr.historical_len), (4, 8));
	assert_eq!(&bytes[atr.historical_at..atr.historical_at + atr.historical_len], b"LIBERPIV");
	// Any flipped bit breaks the check.
	let mut flipped = bytes.clone();
	flipped[5] ^= 0x01;
	assert_eq!(parse(&flipped), Err(Refusal::Check));
	// And a missing check byte is a truncation.
	assert_eq!(parse(&bytes[..bytes.len() - 1]), Err(Refusal::Truncated));
}

fn alloc_vec(bytes: &[u8]) -> Vec<u8> {
	bytes.to_vec()
}

#[test]
fn ta1_and_a_t1_first_card_are_read() {
	// 3B 90 11 01 + TCK: TA1 = 0x11 and TD1 = 0x01 present (Y1 = 0x9), no historical bytes, T=1 only.
	// TCK = 0x90 ^ 0x11 ^ 0x01 = 0x80.
	let atr = parse(&[0x3b, 0x90, 0x11, 0x01, 0x80]).unwrap();
	assert_eq!((atr.ta1, atr.first, atr.protocols), (Some(0x11), 1, 0b10));
	assert!(!atr.offers(0));
}

#[test]
fn t15_is_not_a_transmission_protocol() {
	// 3B 80 8F 01 + TCK: TD1 names T=15 (global bytes follow), TD2 names T=1.
	// TCK = 0x80 ^ 0x8F ^ 0x01 = 0x0E.
	let atr = parse(&[0x3b, 0x80, 0x8f, 0x01, 0x0e]).unwrap();
	assert_eq!((atr.protocols, atr.first), (0b10, 1));
}

#[test]
fn inverse_convention_is_recognised_and_anything_else_refused() {
	assert!(parse(&[0x3f, 0x00]).unwrap().inverse);
	assert_eq!(parse(&[0x3c, 0x00]), Err(Refusal::Convention));
}

#[test]
fn every_declared_length_is_checked_against_the_bytes_that_are_there() {
	assert_eq!(parse(&[0x3b]), Err(Refusal::Length));
	assert_eq!(parse(&[0x3b; 34]), Err(Refusal::Length), "past 33 bytes is not an ATR");
	// T0 promises TA1, TB1, TC1 and TD1 and none follow.
	assert_eq!(parse(&[0x3b, 0xf0]), Err(Refusal::Truncated));
	// Five historical bytes promised, two present.
	assert_eq!(parse(&[0x3b, 0x05, 0x01, 0x02]), Err(Refusal::Truncated));
	// A TD chain that never ends within the bytes.
	assert_eq!(parse(&[0x3b, 0x80, 0x80, 0x80]), Err(Refusal::Truncated));
	// A chain longer than the standard's levels.
	let mut chain = alloc_vec(&[0x3b, 0x80]);
	chain.extend_from_slice(&[0x80; 9]);
	chain.push(0x00);
	assert_eq!(parse(&chain), Err(Refusal::Chain));
}

use super::{Address, Keys, PublicKeyX, Value, comparison_digits, f4, f5, f6, g2};
use crate::aes::{BLOCK, Key};
use crate::cmac::mac;

// Every value in this file is typed from the Bluetooth Core Specification's Appendix D, "Sample data
// for the Security Manager", in the specification's own order - most significant byte on the left.
// NONE OF THEM CAME FROM THIS CODE. A derivation tested against its own output proves that the
// implementation agrees with itself, which is the one property a key derivation must not be
// verified by: every mistake available here - a field in the wrong order, a counter on the wrong
// derivation, a length that is not the one the document names - produces sixteen plausible bytes.
fn hex<const N: usize>(text: &str) -> [u8; N] {
	let clean: alloc::string::String = text.chars().filter(|c| !c.is_whitespace()).collect();
	assert_eq!(clean.len(), N * 2, "the literal is not {N} bytes");
	let mut out = [0u8; N];
	for (at, slot) in out.iter_mut().enumerate() {
		*slot = u8::from_str_radix(&clean[at * 2..at * 2 + 2], 16).expect("hex");
	}
	out
}

fn u() -> PublicKeyX {
	hex("20b003d2 f297be2c 5e2c83a7 e9f9a5b9 eff49111 acf4fddb cc030148 0e359de6")
}

fn v() -> PublicKeyX {
	hex("55188b3d 32f6bb9a 900afcfb eed4e72a 59cb9ac2 f19d7cfb 6b4fdd49 f47fc5fd")
}

fn n1() -> Value {
	hex("d5cb8454 d177733e ffffb2ec 712baeab")
}

fn n2() -> Value {
	hex("a6e8e7cc 25a75f6e 216583f7 ff3dc4cf")
}

fn a1() -> Address {
	hex("00561237 37bfce")
}

fn a2() -> Address {
	hex("00a71370 2dcfc1")
}

#[test]
// APPENDIX D.2: the confirm value. `Z` is zero, which is Just Works and numeric comparison both.
fn f4_matches_the_specifications_confirm_value_sample() {
	assert_eq!(f4(&u(), &v(), &n1(), 0x00), hex::<BLOCK>("f2c916f1 07a9bd1c f1eda1be a974872d"));
}

#[test]
// APPENDIX D.3, AND THE SALT IS CONFIRMED RATHER THAN ASSERTED. The document prints the
// intermediate `T` for a known secret; this computes it from the constant in the module. A wrong
// salt gives a wrong `T`, a wrong MacKey and a wrong LTK, and the pairing fails at the check value
// with nothing saying why.
fn the_f5_salt_produces_the_intermediate_the_specification_prints() {
	let w: [u8; 32] = hex("ec0234a3 57c8ad05 341010a6 0a397d9b 99796b13 b4f866f1 868d34f3 73bfa698");
	let t = mac(&Key::new(&super::F5_SALT), &w);
	assert_eq!(t, hex::<BLOCK>("3c128f20 de883288 97624bdb 8dac6989"));
}

#[test]
// APPENDIX D.3: both keys, and THE COUNTER IS WHAT SEPARATES THEM. Counter zero is the MacKey and
// counter one is the LTK; swapped, a pairing between two implementations that made the same mistake
// still completes, and the link is encrypted with the key that was meant to authenticate it.
fn f5_matches_both_of_the_specifications_key_samples() {
	let w: [u8; 32] = hex("ec0234a3 57c8ad05 341010a6 0a397d9b 99796b13 b4f866f1 868d34f3 73bfa698");
	let keys = f5(&w, &n1(), &n2(), &a1(), &a2());
	assert_eq!(keys, Keys { mac_key: hex::<BLOCK>("2965f176 a1084a02 fd3f6a20 ce636e20"), ltk: hex::<BLOCK>("69867911 69d7cd23 980522b5 94750a38") });
	// AND THE TWO ARE NOT THE SAME KEY, which is the property the counter exists for and the one a
	// swapped implementation still has. It is asserted so that a reader sees the difference is
	// checked rather than assumed by the pair above.
	assert_ne!(keys.mac_key, keys.ltk);
}

#[test]
// APPENDIX D.4: the check value, keyed with the MacKey `f5` produced above - which is what ties the
// two samples together and is why the document prints the same number in both.
fn f6_matches_the_specifications_check_value_sample() {
	let mac_key = hex::<BLOCK>("2965f176 a1084a02 fd3f6a20 ce636e20");
	let r = hex::<BLOCK>("12a3343b b453bb54 08da42d2 0c2d0fc8");
	let io_cap: [u8; 3] = hex("010102");
	assert_eq!(f6(&mac_key, &n1(), &n2(), &r, &io_cap, &a1(), &a2()), hex::<BLOCK>("e3c47398 9cd0e8c5 d26c0b09 da958f61"));
}

#[test]
// APPENDIX D.5: the numeric comparison value, which is the low thirty-two bits of the tag.
fn g2_matches_the_specifications_numeric_comparison_sample() {
	assert_eq!(g2(&u(), &v(), &n1(), &n2()), 0x2f9ed5ba);
	// The six digits a person would read, which is the caller's reduction and not the function's.
	assert_eq!(comparison_digits(0x2f9ed5ba), 0x2f9ed5ba % 1_000_000);
	assert!(comparison_digits(u32::MAX) < 1_000_000);
}

#[test]
// THE ORDER OF THE TWO ADDRESSES IS PART OF THE DERIVATION AND NOT A DETAIL OF IT. `f5` and `f6`
// each take the initiator's first and the responder's second, and an implementation that swapped
// them derives a different key from the same exchange - so two devices that disagree about which
// they are produce different LTKs and the encryption fails with no error that names the cause.
fn swapping_the_two_addresses_derives_a_different_key() {
	let w: [u8; 32] = hex("ec0234a3 57c8ad05 341010a6 0a397d9b 99796b13 b4f866f1 868d34f3 73bfa698");
	let right = f5(&w, &n1(), &n2(), &a1(), &a2());
	let swapped = f5(&w, &n1(), &n2(), &a2(), &a1());
	assert_ne!(right.ltk, swapped.ltk);
	assert_ne!(right.mac_key, swapped.mac_key);
	// The same for the nonces, which is the other pair an implementation can transpose.
	assert_ne!(right.ltk, f5(&w, &n2(), &n1(), &a1(), &a2()).ltk);
}

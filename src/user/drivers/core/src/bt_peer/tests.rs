// The emulated peer's cryptography is a SEPARATE implementation from the host's, so it is held to the
// same published vectors here - on its own, before the two are ever made to agree in the guest.
use super::{Peer, aes128, cmac, f4, f5, f6, fingerprint};

fn hex<const N: usize>(text: &str) -> [u8; N] {
	let clean: alloc::string::String = text.chars().filter(|c| !c.is_whitespace()).collect();
	let mut out = [0u8; N];
	for (at, slot) in out.iter_mut().enumerate() {
		*slot = u8::from_str_radix(&clean[at * 2..at * 2 + 2], 16).expect("hex");
	}
	out
}

#[test]
// FIPS-197 APPENDIX B AND C.1, and the S-box this module GENERATES rather than types in.
fn the_independent_aes_meets_the_standards_own_vectors() {
	assert_eq!(super::SBOX[0x00], 0x63);
	assert_eq!(super::SBOX[0x01], 0x7c);
	assert_eq!(super::SBOX[0x53], 0xed);
	assert_eq!(super::SBOX[0xff], 0x16);
	assert_eq!(aes128(&hex("2b7e151628aed2a6abf7158809cf4f3c"), &hex("3243f6a8885a308d313198a2e0370734")), hex("3925841d02dc09fbdc118597196a0b32"));
	assert_eq!(aes128(&hex("000102030405060708090a0b0c0d0e0f"), &hex("00112233445566778899aabbccddeeff")), hex("69c4e0d86a7b0430d8cdb78070b4c55a"));
	assert_eq!(aes128(&[0u8; 16], &[0u8; 16]), hex("66e94bd4ef8a2c3b884cfa59ca342b2e"));
}

#[test]
// RFC 4493'S FOUR EXAMPLES.
fn the_independent_cmac_meets_rfc_4493() {
	let key = hex::<16>("2b7e151628aed2a6abf7158809cf4f3c");
	let m: [u8; 64] = hex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710");
	assert_eq!(cmac(&key, &[]), hex("bb1d6929e95937287fa37d129b756746"));
	assert_eq!(cmac(&key, &m[..16]), hex("070a16b46b4d4144f79bdd9dd04a287c"));
	assert_eq!(cmac(&key, &m[..40]), hex("dfa66747de9ae63030ca32611497c827"));
	assert_eq!(cmac(&key, &m), hex("51f0bebf7e3b9d92fc49741779363cfe"));
}

#[test]
// CORE APPENDIX D.2, D.3 AND D.4, against this module's own f4, f5 and f6.
fn the_independent_smp_functions_meet_the_core_sample_data() {
	let u: [u8; 32] = hex("20b003d2f297be2c5e2c83a7e9f9a5b9eff49111acf4fddbcc0301480e359de6");
	let v: [u8; 32] = hex("55188b3d32f6bb9a900afcfbeed4e72a59cb9ac2f19d7cfb6b4fdd49f47fc5fd");
	let n1: [u8; 16] = hex("d5cb8454d177733effffb2ec712baeab");
	let n2: [u8; 16] = hex("a6e8e7cc25a75f6e216583f7ff3dc4cf");
	assert_eq!(f4(&u, &v, &n1, 0), hex("f2c916f107a9bd1cf1eda1bea974872d"));
	let w: [u8; 32] = hex("ec0234a357c8ad05341010a60a397d9b99796b13b4f866f1868d34f373bfa698");
	let a1: [u8; 7] = hex("00561237 37bfce");
	let a2: [u8; 7] = hex("00a71370 2dcfc1");
	let (mac_key, ltk) = f5(&w, &n1, &n2, &a1, &a2);
	assert_eq!(mac_key, hex("2965f176a1084a02fd3f6a20ce636e20"));
	assert_eq!(ltk, hex("6986791169d7cd23980522b594750a38"));
	let r: [u8; 16] = hex("12a3343bb453bb5408da42d20c2d0fc8");
	assert_eq!(f6(&mac_key, &n1, &n2, &r, &[0x01, 0x01, 0x02], &a1, &a2), hex("e3c473989cd0e8c5d26c0b09da958f61"));
}

#[test]
// A FINGERPRINT IDENTIFIES A KEY WITHOUT BEING IT, and two keys have two fingerprints.
fn a_fingerprint_tells_keys_apart_and_is_stable() {
	let a = [0x11u8; 16];
	let b = [0x12u8; 16];
	assert_eq!(fingerprint(&a), fingerprint(&a));
	assert_ne!(fingerprint(&a), fingerprint(&b));
}

#[test]
// THE RESPONDER REFUSES WHAT A DEVICE REFUSES: a request without Secure Connections, and a PDU that is
// not part of the exchange it is in.
fn the_responder_refuses_a_legacy_request_and_an_out_of_order_pdu() {
	let mut peer = Peer::new();
	peer.connected([0, 1, 2, 3, 4, 5, 6]);
	assert_eq!(peer.smp(&[0x01, 0x03, 0x00, 0x01, 16, 0x00, 0x00]), alloc::vec![alloc::vec![0x05, 0x03]]);
	assert_eq!(peer.pairings, 1, "a refused request is still a request the gate can count");
	assert_eq!(peer.smp(&[0x03; 17]), alloc::vec![alloc::vec![0x05, 0x08]]);
	assert_eq!(peer.ltk, None);
}

#[test]
// THE GATT SERVER REFUSES TO TURN REPORTS ON OVER AN UNENCRYPTED LINK, which is HOGP's rule and what
// makes a gate's "input arrived" mean "input arrived over an encrypted link".
fn reports_cannot_be_enabled_before_the_link_is_encrypted() {
	let mut peer = Peer::new();
	let enable = [0x12, 0x0b, 0x00, 0x01, 0x00];
	assert_eq!(peer.att(&enable, false), Some(alloc::vec![0x01, 0x12, 0x0b, 0x00, 0x0f]));
	assert!(!peer.notify);
	assert_eq!(peer.att(&[0x52, 0x08, 0x00, 0x00], true), None);
	assert_eq!(peer.att(&enable, true), Some(alloc::vec![0x13]));
	assert!(peer.boot_mode && peer.notify);
	assert!(peer.report(0).is_some());
}

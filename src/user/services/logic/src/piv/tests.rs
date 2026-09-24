// The commands below are spelled out byte by byte from SP 800-73-4 Part 2, not produced by the code
// under test: a SELECT by name of A0 00 00 03 08 00 00 10 00 01 00, GET DATA with a tag list 5C, and so
// on.

use super::*;

#[test]
fn exactly_the_three_canonical_commands_are_allowed() {
	assert_eq!(classify(&[0x00, 0xa4, 0x04, 0x00, 0x0b, 0xa0, 0x00, 0x00, 0x03, 0x08, 0x00, 0x00, 0x10, 0x00, 0x01, 0x00, 0x00]), Ok(Allowed::SelectPiv));
	assert_eq!(classify(&[0x00, 0xcb, 0x3f, 0xff, 0x03, 0x5c, 0x01, 0x7e, 0x00]), Ok(Allowed::GetDiscovery));
	assert_eq!(classify(&[0x00, 0xcb, 0x3f, 0xff, 0x05, 0x5c, 0x03, 0x5f, 0xc1, 0x05, 0x00]), Ok(Allowed::GetAuthCertificate));
}

#[test]
fn a_near_miss_of_an_allowed_command_is_not_allowed() {
	// The right-truncated AID the card would also accept: not the canonical form.
	assert_eq!(classify(&[0x00, 0xa4, 0x04, 0x00, 0x09, 0xa0, 0x00, 0x00, 0x03, 0x08, 0x00, 0x00, 0x10, 0x00, 0x00]), Err(Refused::Unsupported));
	// Another application.
	assert_eq!(classify(&[0x00, 0xa4, 0x04, 0x00, 0x07, 0xa0, 0x00, 0x00, 0x00, 0x03, 0x10, 0x10, 0x00]), Err(Refused::Unsupported));
	// Another object: the Card Holder Unique Identifier (5FC102).
	assert_eq!(classify(&[0x00, 0xcb, 0x3f, 0xff, 0x05, 0x5c, 0x03, 0x5f, 0xc1, 0x02, 0x00]), Err(Refused::Unsupported));
	// The allowed GET DATA without its Le, or with a chaining bit, or on logical channel 1, or with
	// secure messaging.
	assert_eq!(classify(&[0x00, 0xcb, 0x3f, 0xff, 0x03, 0x5c, 0x01, 0x7e]), Err(Refused::Unsupported));
	for class in [0x10, 0x01, 0x0c, 0x80] {
		assert_eq!(classify(&[class, 0xcb, 0x3f, 0xff, 0x03, 0x5c, 0x01, 0x7e, 0x00]), Err(Refused::Unsupported), "class {class:#04x}");
	}
	// GET RESPONSE is the service's alone.
	assert_eq!(classify(&[0x00, 0xc0, 0x00, 0x00, 0x00]), Err(Refused::Unsupported));
}

#[test]
fn every_command_that_touches_a_secret_is_refused_by_name() {
	// VERIFY with a PIN, the status-only VERIFY (no data), and one behind a chaining bit.
	assert_eq!(classify(&[0x00, 0x20, 0x00, 0x80, 0x08, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0xff, 0xff]), Err(Refused::Forbidden));
	assert_eq!(classify(&[0x00, 0x20, 0x00, 0x80]), Err(Refused::Forbidden));
	assert_eq!(classify(&[0x10, 0x20, 0x00, 0x80, 0x08, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0xff, 0xff]), Err(Refused::Forbidden));
	// CHANGE REFERENCE DATA, RESET RETRY COUNTER, PUT DATA, GENERATE ASYMMETRIC KEY PAIR, GENERAL
	// AUTHENTICATE sent raw, and the import and attestation extensions.
	for instruction in [0x24, 0x2c, 0xdb, 0x47, 0x87, 0xfe, 0xf9, 0xfa] {
		assert_eq!(classify(&[0x00, instruction, 0x00, 0x80, 0x01, 0x00]), Err(Refused::Forbidden), "instruction {instruction:#04x}");
	}
}

#[test]
fn a_command_whose_lengths_do_not_add_up_is_malformed() {
	assert_eq!(classify(&[0x00, 0xa4, 0x04]), Err(Refused::Malformed));
	// Lc says three, two follow; Lc says three, five follow; Lc zero with data.
	assert_eq!(classify(&[0x00, 0xcb, 0x3f, 0xff, 0x03, 0x5c, 0x01]), Err(Refused::Malformed));
	assert_eq!(classify(&[0x00, 0xcb, 0x3f, 0xff, 0x03, 0x5c, 0x01, 0x7e, 0x00, 0x00]), Err(Refused::Malformed));
	assert_eq!(classify(&[0x00, 0xcb, 0x3f, 0xff, 0x00, 0x5c]), Err(Refused::Malformed));
	assert_eq!(classify(&[0u8; MAX_COMMAND + 1]), Err(Refused::Malformed));
}

#[test]
fn the_verification_template_carries_no_pin() {
	let template = verify_template();
	assert_eq!(&template[..5], &[0x00, 0x20, 0x00, 0x80, 0x08], "VERIFY, PIV application PIN, eight bytes");
	assert!(template[5..].iter().all(|&byte| byte == 0xff), "every PIN byte is a placeholder");
	assert!(pinpad_fits(true, true, 8, 4, 8));
	assert!(!pinpad_fits(false, true, 8, 6, 8), "no secure verification");
	assert!(!pinpad_fits(true, false, 8, 6, 8), "not ASCII");
	assert!(!pinpad_fits(true, true, 6, 6, 8), "a block too small");
	assert!(!pinpad_fits(true, true, 8, 7, 8), "cannot take a six-digit PIN");
	assert!(!pinpad_fits(true, true, 8, 6, 6), "cannot take an eight-digit PIN");
}

#[test]
fn general_authenticate_is_the_dynamic_authentication_template() {
	let challenge = [0x5au8; CHALLENGE_BYTES];
	let apdu = general_authenticate(&challenge);
	// 00 87 11 9A: P-256, PIV Authentication key. Lc 0x26 = 38: 7C 24 { 82 00, 81 20 challenge }.
	assert_eq!(&apdu[..11], &[0x00, 0x87, 0x11, 0x9a, 0x26, 0x7c, 0x24, 0x82, 0x00, 0x81, 0x20]);
	assert_eq!(&apdu[11..43], &challenge);
	assert_eq!(apdu[43], 0x00);
	assert_eq!(apdu.len(), 5 + 0x26 + 1);
}

// A DER ECDSA signature of the shape a P-256 key produces: r with a sign byte, s without.
fn signature() -> Vec<u8> {
	let mut der = alloc::vec![0x30, 0x45, 0x02, 0x21, 0x00];
	der.extend_from_slice(&[0x80; 32]);
	der.extend_from_slice(&[0x02, 0x20]);
	der.extend_from_slice(&[0x11; 32]);
	der
}

#[test]
fn a_signature_is_taken_only_from_the_exact_template() {
	let der = signature();
	let mut response = alloc::vec![0x7c, 0x49, 0x82, 0x47];
	response.extend_from_slice(&der);
	assert_eq!(authentication_signature(&response), Ok(der.clone()));
	// The same with long-form lengths.
	let mut long = alloc::vec![0x7c, 0x81, 0x49, 0x82, 0x81, 0x47];
	long.extend_from_slice(&der);
	assert_eq!(authentication_signature(&long), Err(Malformed::Template), "the outer length no longer covers the body");
	let mut long = alloc::vec![0x7c, 0x81, 0x4a, 0x82, 0x81, 0x47];
	long.extend_from_slice(&der);
	assert_eq!(authentication_signature(&long), Ok(der.clone()));
	// A trailing byte, a different tag, a truncated signature.
	let mut trailing = response.clone();
	trailing.push(0x00);
	assert_eq!(authentication_signature(&trailing), Err(Malformed::Template));
	let mut tag = response.clone();
	tag[2] = 0x81;
	assert_eq!(authentication_signature(&tag), Err(Malformed::Template));
	assert_eq!(authentication_signature(&response[..20]), Err(Malformed::Template));
}

#[test]
fn a_signature_that_is_not_der_ecdsa_is_refused() {
	let wrap = |der: &[u8]| {
		let mut response = alloc::vec![0x7c, der.len() as u8 + 2, 0x82, der.len() as u8];
		response.extend_from_slice(der);
		authentication_signature(&response)
	};
	let good = signature();
	assert!(wrap(&good).is_ok());
	// Negative r: the sign byte removed.
	let mut negative = alloc::vec![0x30, 0x44, 0x02, 0x20];
	negative.extend_from_slice(&[0x80; 32]);
	negative.extend_from_slice(&[0x02, 0x20]);
	negative.extend_from_slice(&[0x11; 32]);
	assert_eq!(wrap(&negative), Err(Malformed::Signature));
	// A redundant leading zero on s.
	let mut padded = alloc::vec![0x30, 0x46, 0x02, 0x21, 0x00];
	padded.extend_from_slice(&[0x80; 32]);
	padded.extend_from_slice(&[0x02, 0x21, 0x00]);
	padded.extend_from_slice(&[0x11; 32]);
	assert_eq!(wrap(&padded), Err(Malformed::Signature));
	// A SEQUENCE length that does not match.
	let mut wrong = good.clone();
	wrong[1] ^= 0x01;
	assert_eq!(wrap(&wrong), Err(Malformed::Signature));
}

#[test]
fn a_pin_status_is_one_of_the_typed_outcomes() {
	assert_eq!(pin_status(0x90, 0x00), PinStatus::Verified);
	assert_eq!(pin_status(0x63, 0xc2), PinStatus::Incorrect { retries: 2 });
	assert_eq!(pin_status(0x63, 0xc0), PinStatus::Incorrect { retries: 0 });
	assert_eq!(pin_status(0x69, 0x83), PinStatus::Blocked);
	assert_eq!(pin_status(0x64, 0x00), PinStatus::TimedOut);
	assert_eq!(pin_status(0x64, 0x01), PinStatus::Cancelled);
	assert_eq!(pin_status(0x63, 0x00), PinStatus::Error, "a warning without a counter is not a count");
	assert_eq!(pin_status(0x6a, 0x80), PinStatus::Error);
}

#[test]
fn a_continued_answer_is_bounded() {
	let mut continued = Continued::new();
	let mut first = alloc::vec![0xaa; 256];
	first.extend_from_slice(&[0x61, 0x10]);
	assert_eq!(continued.feed(&first), Next::More([0x00, 0xc0, 0x00, 0x00, 0x10]));
	let mut last = alloc::vec![0xbb; 16];
	last.extend_from_slice(&[0x90, 0x00]);
	assert_eq!(continued.feed(&last), Next::Complete(0x90, 0x00));
	assert_eq!(continued.data().len(), 272);
	// Past sixteen kilobytes.
	let mut long = Continued::new();
	let mut piece = alloc::vec![0x00; 256];
	piece.extend_from_slice(&[0x61, 0x00]);
	for _ in 0..MAX_CONTINUED / 256 {
		assert!(matches!(long.feed(&piece), Next::More(_)));
	}
	assert_eq!(long.feed(&piece), Next::Refused);
	// Past the count, even with nothing in each piece.
	let mut many = Continued::new();
	for _ in 0..MAX_CONTINUATIONS {
		assert!(matches!(many.feed(&[0x61, 0x00]), Next::More(_)));
	}
	assert_eq!(many.feed(&[0x61, 0x00]), Next::Refused);
	assert_eq!(Continued::new().feed(&[0x90]), Next::Refused, "one byte is not a status word");
}

use super::*;

const SELECT: [u8; 17] = [0x00, 0xa4, 0x04, 0x00, 0x0b, 0xa0, 0x00, 0x00, 0x03, 0x08, 0x00, 0x00, 0x10, 0x00, 0x01, 0x00, 0x00];

fn selected() -> Card {
	let mut card = Card::new();
	assert_eq!(card.apdu(&SELECT).last_chunk(), Some(&[0x90, 0x00]));
	card
}

#[test]
fn the_fixture_atr_is_one_a_real_parser_accepts() {
	let parsed = smartcard_model::atr::parse(&atr()).expect("the fixture's ATR is an ATR");
	assert_eq!((parsed.first, parsed.protocols), (0, 0b11));
}

#[test]
fn the_certificate_comes_back_whole_through_get_response() {
	let mut card = selected();
	let first = card.apdu(&[0x00, 0xcb, 0x3f, 0xff, 0x05, 0x5c, 0x03, 0x5f, 0xc1, 0x05, 0x00]);
	assert!(first.len() <= 258);
	let mut object = first[..first.len() - 2].to_vec();
	let mut sw = [first[first.len() - 2], first[first.len() - 1]];
	while sw[0] == 0x61 {
		let next = card.apdu(&[0x00, 0xc0, 0x00, 0x00, sw[1]]);
		assert!(next.len() <= 258);
		object.extend_from_slice(&next[..next.len() - 2]);
		sw = [next[next.len() - 2], next[next.len() - 1]];
	}
	assert_eq!(sw, [0x90, 0x00]);
	// 53 82 LL LL { 70 82 LL LL certificate 71 01 00 FE 00 }
	assert_eq!(object[0], 0x53);
	let certificate = certificate();
	let at = object.windows(certificate.len()).position(|window| window == certificate).expect("the certificate is in the object");
	assert_eq!(&object[at - 4..at], &[0x70, 0x82, (certificate.len() >> 8) as u8, certificate.len() as u8]);
}

#[test]
fn authentication_needs_a_verification_and_a_challenge_the_card_knows() {
	let mut card = selected();
	let (challenge, signature) = known()[0];
	let mut command = alloc::vec![0x00, 0x87, 0x11, 0x9a, 0x26, 0x7c, 0x24, 0x82, 0x00, 0x81, 0x20];
	command.extend_from_slice(challenge);
	command.push(0x00);
	assert_eq!(card.apdu(&command), alloc::vec![0x69, 0x82], "security status not satisfied");
	assert_eq!(card.verify(Pinpad::Verified), alloc::vec![0x90, 0x00]);
	let response = card.apdu(&command);
	assert_eq!(&response[response.len() - 2..], &[0x90, 0x00]);
	assert_eq!(&response[4..response.len() - 2], signature);
	let mut unknown = command.clone();
	unknown[11] ^= 0xff;
	assert_eq!(card.apdu(&unknown), alloc::vec![0x6a, 0x80]);
	// A reset forgets the verification.
	card.reset();
	card.apdu(&SELECT);
	assert_eq!(card.apdu(&command), alloc::vec![0x69, 0x82]);
}

#[test]
fn the_retry_counter_counts_down_and_blocks() {
	let mut card = selected();
	assert_eq!(card.verify(Pinpad::Incorrect), alloc::vec![0x63, 0xc2]);
	assert_eq!(card.verify(Pinpad::Incorrect), alloc::vec![0x63, 0xc1]);
	assert_eq!(card.verify(Pinpad::Incorrect), alloc::vec![0x69, 0x83]);
	assert_eq!(card.verify(Pinpad::Verified), alloc::vec![0x69, 0x83], "blocked stays blocked");
}

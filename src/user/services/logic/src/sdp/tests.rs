use super::*;
use alloc::vec;

#[test]
fn elements_round_trip_with_every_width_and_nesting() {
	let element = Element::Sequence(vec![
		Element::Nil,
		Element::u8(7),
		Element::u16(0x1234),
		Element::u32(0x89AB_CDEF),
		Element::Int(-2, 1),
		Element::uuid16(0x1101),
		Element::Uuid(Uuid::U32(0x1234_5678)),
		Element::Uuid(Uuid::U128(BASE_UUID)),
		Element::text("name"),
		Element::Bool(true),
		Element::Alternative(vec![Element::Url(b"http://x".to_vec())]),
		Element::Text(vec![b'x'; 300]),
	]);
	let mut bytes = Vec::new();
	encode_element(&element, &mut bytes);
	let (back, rest) = decode_element(&bytes).unwrap();
	assert!(rest.is_empty());
	assert_eq!(back, element);
}

#[test]
fn a_malformed_element_is_refused_by_name() {
	assert_eq!(decode_element(&[0x35, 0x05, 0x19, 0x11]), Err(Refusal::Truncated), "a sequence longer than the bytes");
	assert_eq!(decode_element(&[0x01]), Err(Refusal::BadDescriptor(0x01)), "a nil with a size");
	assert_eq!(decode_element(&[0x0B]), Err(Refusal::Truncated), "an eight-byte integer with none of its bytes");
	let mut deep = vec![0x35, 0x00];
	for _ in 0..12 {
		let inner = deep.clone();
		deep = vec![0x35, inner.len() as u8];
		deep.extend_from_slice(&inner);
	}
	assert_eq!(decode_element(&deep), Err(Refusal::TooDeep));
}

#[test]
fn a_short_uuid_compares_with_its_full_form() {
	let mut full = BASE_UUID;
	full[2..4].copy_from_slice(&0x110Bu16.to_be_bytes());
	assert!(Uuid::U16(0x110B).same(&Uuid::U128(full)));
	assert!(!Uuid::U16(0x110A).same(&Uuid::U128(full)));
	assert!(Uuid::U32(0x110B).same(&Uuid::U16(0x110B)));
}

#[test]
fn a_record_names_its_rfcomm_channel_psm_version_and_features() {
	let hfp = hfp_audio_gateway(0x10001, 4, 0x0220);
	assert_eq!(hfp.rfcomm_channel(), Some(4));
	assert_eq!(hfp.profile_version(uuid::HANDSFREE), Some(0x0108));
	assert_eq!(hfp.supported_features(), Some(0x0220));
	let target = avrcp(0x10002, true, 0x0042);
	assert_eq!((target.l2cap_psm(), target.additional_psm()), (Some(0x0017), Some(0x001B)));
	assert!(target.matches(&[Uuid::U16(uuid::AV_REMOTE_CONTROL_TARGET), Uuid::U16(uuid::AVCTP)]));
	assert!(!target.matches(&[Uuid::U16(uuid::AUDIO_SINK)]));
	assert_eq!(opp_server(0x10003, 9, Some(0x1001)).get(attribute::GOEP_L2CAP_PSM), Some(&Element::u16(0x1001)));
}

#[test]
fn a_client_search_continues_until_the_server_is_done_and_reads_the_records() {
	let mut server = Server::new();
	server.offer(hfp_audio_gateway(0x10001, 4, 0x0220));
	server.offer(a2dp(0x10002, false, 0x0001));
	server.offer(a2dp(0x10003, true, 0x0001));
	let (mut search, mut request) = Search::new(1, Uuid::U16(uuid::ADVANCED_AUDIO_DISTRIBUTION));
	// A peer asking for at most 0x400 bytes gets every record at once; make the server continue by asking for fewer.
	let narrow = |request: &[u8]| -> Vec<u8> {
		let (transaction, pdu) = decode(request).unwrap();
		let Pdu::ServiceSearchAttributeRequest { pattern, wanted, continuation, .. } = pdu else { panic!() };
		encode(transaction, &Pdu::ServiceSearchAttributeRequest { pattern, max_bytes: 40, wanted, continuation })
	};
	let mut rounds = 0;
	let records = loop {
		rounds += 1;
		let response = server.answer(&narrow(&request));
		match search.answer(&response) {
			Answer::More(next) => request = next,
			Answer::Records(records) => break records,
			Answer::Failed(code) => panic!("failed {code}"),
		}
		assert!(rounds < 50);
	};
	assert!(rounds > 2, "the answer took several continuations");
	assert_eq!(records.len(), 2, "both A2DP records, not the HFP one");
	assert_eq!(records[0].handle, 0x10002);
	assert_eq!(records[1].profile_version(uuid::ADVANCED_AUDIO_DISTRIBUTION), Some(0x0103));
}

#[test]
fn the_server_refuses_a_forged_continuation_an_unknown_handle_and_bad_syntax() {
	let mut server = Server::new();
	server.offer(hsp_audio_gateway(0x10005, 2));
	let forged = encode(9, &Pdu::ServiceSearchAttributeRequest { pattern: vec![Uuid::U16(uuid::HEADSET)], max_bytes: 100, wanted: vec![Wanted::Range(0, 0xFFFF)], continuation: vec![0, 0, 0, 5] });
	assert_eq!(decode(&server.answer(&forged)).unwrap().1, Pdu::Error { code: error::INVALID_CONTINUATION });
	let unknown = encode(10, &Pdu::ServiceAttributeRequest { handle: 0x99, max_bytes: 100, wanted: vec![Wanted::Id(1)], continuation: Vec::new() });
	assert_eq!(decode(&server.answer(&unknown)).unwrap().1, Pdu::Error { code: error::INVALID_HANDLE });
	assert_eq!(decode(&server.answer(&[0x06, 0, 11, 0, 3, 0x35, 0x00, 0x00])).unwrap(), (11, Pdu::Error { code: error::INVALID_SYNTAX }));
	let search = encode(12, &Pdu::ServiceSearchRequest { pattern: vec![Uuid::U16(uuid::HEADSET_AUDIO_GATEWAY)], max_records: 4, continuation: Vec::new() });
	assert_eq!(decode(&server.answer(&search)).unwrap().1, Pdu::ServiceSearchResponse { total: 1, handles: vec![0x10005], continuation: Vec::new() });
	server.withdraw(0x10005);
	assert_eq!(decode(&server.answer(&search)).unwrap().1, Pdu::ServiceSearchResponse { total: 0, handles: Vec::new(), continuation: Vec::new() });
}

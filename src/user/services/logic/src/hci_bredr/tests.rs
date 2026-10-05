use super::*;
use alloc::vec;

fn event_bytes(code: u8, params: &[u8]) -> Vec<u8> {
	let mut out = vec![code, params.len() as u8];
	out.extend_from_slice(params);
	out
}

const PEER: [u8; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

#[test]
fn commands_are_laid_out_as_the_specification_says() {
	assert_eq!(inquiry(8, 0), vec![0x33, 0x8B, 0x9E, 8, 0]);
	assert_eq!(inquiry(0, 3)[3], 1, "a length of at least one unit");
	let create = create_connection(&PEER, 1, 0x1234);
	assert_eq!(create.len(), 13);
	assert_eq!(&create[..6], &PEER);
	assert_eq!(u16::from_le_bytes([create[6], create[7]]), ACL_PACKET_TYPES);
	assert_eq!(u16::from_le_bytes([create[10], create[11]]), 0x9234, "a clock offset is marked valid");
	assert_eq!(io_capability_reply(&PEER, IoCapability::DisplayYesNo, AUTH_MITM_BONDING)[6..], [1, 0, 5]);
	assert_eq!(passkey_reply(&PEER, 123456)[6..], 123456u32.to_le_bytes());
	assert_eq!(passkey_reply(&PEER, 9_999_999)[6..], 999999u32.to_le_bytes(), "six digits at most");
	assert!(pin_code_reply(&PEER, b"").is_none() && pin_code_reply(&PEER, &[b'1'; 17]).is_none());
	assert_eq!(pin_code_reply(&PEER, b"0000").unwrap()[6], 4);
	let sniff = sniff_mode(0x0042, 5000, 10);
	assert_eq!(u16::from_le_bytes([sniff[2], sniff[3]]), SNIFF_MAX_SLOTS, "the interval bounded");
	assert_eq!(u16::from_le_bytes([sniff[4], sniff[5]]), SNIFF_MIN_SLOTS);
	assert_eq!(local_name(b"LiberSystem").len(), 248);
}

#[test]
fn an_extended_inquiry_result_gives_the_name_and_service_uuids() {
	let mut p = vec![1u8];
	p.extend_from_slice(&PEER);
	p.extend_from_slice(&[1, 0]);
	p.extend_from_slice(&[0x04, 0x25, 0x00]);
	p.extend_from_slice(&0x4321u16.to_le_bytes());
	p.push(0xC8);
	let mut eir = vec![5, eir::NAME_COMPLETE, b'K', b'e', b'y', b's', 5, eir::UUID16_COMPLETE, 0x24, 0x11, 0x00, 0x12, 0];
	eir.resize(240, 0);
	p.extend_from_slice(&eir);
	let Some(Event::Inquiry(found)) = decode(&event_bytes(event::EXTENDED_INQUIRY_RESULT, &p)) else { panic!("an inquiry result") };
	assert_eq!(found[0].address, PEER);
	assert_eq!(found[0].class, 0x002504);
	assert_eq!(found[0].rssi, Some(-56));
	assert_eq!(found[0].eir, Eir { name: b"Keys".to_vec(), uuids: vec![0x1124, 0x1200] });
}

#[test]
fn an_eir_field_running_past_its_end_stops_the_read() {
	assert_eq!(parse_eir(&[3, eir::NAME_SHORT, b'a', b'b', 9, eir::UUID16_COMPLETE, 1]), Eir { name: b"ab".to_vec(), uuids: Vec::new() });
}

#[test]
fn inquiry_results_with_and_without_rssi_read_device_by_device() {
	let mut p = vec![2u8];
	for (n, rssi) in [(1u8, 0xF0u8), (2, 0xE0)] {
		p.extend_from_slice(&[n, 0, 0, 0, 0, 0]);
		p.push(1);
		p.push(0);
		p.extend_from_slice(&[0x0C, 0x02, 0x5A]);
		p.extend_from_slice(&(u16::from(n) * 100).to_le_bytes());
		p.push(rssi);
	}
	let Some(Event::Inquiry(found)) = decode(&event_bytes(event::INQUIRY_RESULT_WITH_RSSI, &p)) else { panic!() };
	assert_eq!(found.len(), 2);
	assert_eq!((found[1].address[0], found[1].clock_offset, found[1].rssi, found[1].class), (2, 200, Some(-32), 0x5A020C));
	assert_eq!(decode(&event_bytes(event::INQUIRY_RESULT_WITH_RSSI, &p[..20])), None, "a count the bytes do not hold");
}

#[test]
fn the_pairing_events_are_read() {
	let mut confirm = PEER.to_vec();
	confirm.extend_from_slice(&654321u32.to_le_bytes());
	assert_eq!(decode(&event_bytes(event::USER_CONFIRMATION_REQUEST, &confirm)), Some(Event::UserConfirmationRequest { address: PEER, value: 654321 }));
	let mut key = PEER.to_vec();
	key.extend_from_slice(&[0xAB; 16]);
	key.push(0x08);
	assert_eq!(decode(&event_bytes(event::LINK_KEY_NOTIFICATION, &key)), Some(Event::LinkKeyNotification { address: PEER, key: [0xAB; 16], kind: KeyType::AuthenticatedP256 }));
	let mut response = PEER.to_vec();
	response.extend_from_slice(&[3, 0, 4]);
	assert_eq!(decode(&event_bytes(event::IO_CAPABILITY_RESPONSE, &response)), Some(Event::IoCapabilityResponse { address: PEER, capability: 3, authentication: 4 }));
	let mut complete = vec![0u8, 0x2A, 0x00];
	complete.extend_from_slice(&PEER);
	complete.extend_from_slice(&[1, 0]);
	assert_eq!(decode(&event_bytes(event::CONNECTION_COMPLETE, &complete)), Some(Event::ConnectionComplete { status: 0, handle: 0x2A, address: PEER, acl: true, encrypted: false }));
	assert_eq!(decode(&[event::CONNECTION_COMPLETE, 11, 0, 0x2A]), None, "a length the bytes do not carry");
}

#[test]
fn feature_bits_and_key_types_are_named() {
	let page0 = [0xA0u8, 0, 0, 0x80, 0, 0, 0x08, 0];
	assert!(feature::role_switch(&page0) && feature::sniff(&page0) && feature::esco(&page0) && feature::simple_pairing(&page0));
	let page1 = [0x09u8, 0, 0, 0, 0, 0, 0, 0];
	assert!(feature::host_simple_pairing(&page1) && feature::host_secure_connections(&page1));
	for value in 0..=8u8 {
		assert_eq!(KeyType::from_value(value).value(), value);
	}
	assert_eq!(IoCapability::from_value(4), None);
}

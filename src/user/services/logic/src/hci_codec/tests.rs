use super::{Event, Refusal, acl, acl_header, address_from_wire, advertised, command, create_connection, enable_encryption, event, opcode, public_key_x, reports, reverse16, subevent};

#[test]
// A COMMAND IS ITS OPCODE LITTLE-ENDIAN, ITS LENGTH, AND ITS PARAMETERS. The opcode is the one field a
// reader checks first, and written the other way round it names a different command that the
// controller answers with a status nobody expected.
fn a_command_is_its_opcode_little_endian_and_its_length() {
	assert_eq!(command(opcode::RESET, &[]), Some(alloc::vec![0x03, 0x0c, 0x00]));
	assert_eq!(command(opcode::LE_SET_SCAN_ENABLE, &[1, 0]), Some(alloc::vec![0x0c, 0x20, 0x02, 0x01, 0x00]));
	assert_eq!(command(opcode::RESET, &[0u8; 256]), None, "a parameter list past the command ceiling is refused");
}

#[test]
// THE TWO BYTE-ORDER BOUNDARIES, AGAINST A KNOWN VALUE. The wire is little-endian and every SMP
// function takes its values most significant first; reversed silently at either boundary, a pairing
// fails at the confirm value with nothing saying why.
fn the_address_and_the_public_key_cross_the_byte_order_boundary_the_right_way_round() {
	// The Core specification's A1 is 00561237 37bfce - a type byte and the address 56:12:37:37:bf:ce.
	// On the wire it is the address least significant byte first.
	let wire = [0xce, 0xbf, 0x37, 0x37, 0x12, 0x56];
	assert_eq!(address_from_wire(&wire), [0x56, 0x12, 0x37, 0x37, 0xbf, 0xce]);
	// A public key's X is the first 32 bytes of the wire's 64, little-endian.
	let mut key = [0u8; 64];
	for (at, byte) in key.iter_mut().enumerate() {
		*byte = at as u8;
	}
	let x = public_key_x(&key);
	assert_eq!(x[0], 31, "the most significant byte is the last of the wire's X");
	assert_eq!(x[31], 0);
	assert_eq!(reverse16(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16])[0], 16);
}

#[test]
// THE KEY GOES ONTO THE WIRE REVERSED, and the diversifier and random number are zero - which is how a
// Secure Connections LTK is used. A key sent in the order it was computed is a key the controller
// encrypts with, successfully, and the peer cannot decrypt.
fn the_encryption_command_carries_the_key_in_wire_order_and_zero_diversifiers() {
	let ltk = [0x69, 0x86, 0x79, 0x11, 0x69, 0xd7, 0xcd, 0x23, 0x98, 0x05, 0x22, 0xb5, 0x94, 0x75, 0x0a, 0x38];
	let params = enable_encryption(0x0040, &ltk);
	assert_eq!(&params[..2], &[0x40, 0x00]);
	assert_eq!(&params[2..12], &[0u8; 10], "random number and diversifier");
	assert_eq!(params[12], 0x38, "the least significant byte of the key first");
	assert_eq!(params[27], 0x69);
	// And the connection command carries the peer address in wire order too.
	let connect = create_connection(0, &[0x56, 0x12, 0x37, 0x37, 0xbf, 0xce]);
	assert_eq!(&connect[6..12], &[0xce, 0xbf, 0x37, 0x37, 0x12, 0x56]);
}

#[test]
// AN EVENT WHOSE DECLARED LENGTH DISAGREES WITH WHAT ARRIVED IS REFUSED RATHER THAN READ. The controller
// chooses every length on this wire.
fn an_event_length_that_disagrees_with_its_bytes_is_refused() {
	assert_eq!(event(&[]), Err(Refusal::Short { len: 0 }));
	assert_eq!(event(&[0x0e, 4, 1, 0x03, 0x0c]), Err(Refusal::Length { declared: 4, got: 3 }));
	assert_eq!(event(&[0x0e, 2, 1, 0x03]), Err(Refusal::Truncated { code: 0x0e, needed: 3 }));
	assert_eq!(event(&[0x99, 0]), Err(Refusal::Unhandled { code: 0x99 }));
	// The ordinary case: a command complete for reset, status zero, one credit back.
	assert_eq!(event(&[0x0e, 4, 1, 0x03, 0x0c, 0x00]), Ok(Event::CommandComplete { credits: 1, opcode: opcode::RESET, params: &[0x00] }));
	assert_eq!(event(&[0x0f, 4, 0x00, 1, 0x0d, 0x20]), Ok(Event::CommandStatus { status: 0, credits: 1, opcode: opcode::LE_CREATE_CONNECTION }));
}

#[test]
// A CONNECTION COMPLETE CARRIES THE PEER IN WIRE ORDER, and the handle is twelve bits - the four above
// it are flags a link does not have, and reading them as part of the handle names a link that does not
// exist.
fn a_connection_complete_is_decoded_with_the_peer_in_this_services_order() {
	let mut body = alloc::vec![super::event::LE_META, 19, subevent::CONNECTION_COMPLETE, 0x00, 0x40, 0xf0, 0x00, 0x00];
	body.extend_from_slice(&[0xce, 0xbf, 0x37, 0x37, 0x12, 0x56]);
	body.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0xa4, 0x01, 0x00]);
	assert_eq!(event(&body), Ok(Event::Connected { status: 0, handle: 0x0040, peer_kind: 0, peer: [0x56, 0x12, 0x37, 0x37, 0xbf, 0xce] }));
}

#[test]
// ONE EVENT MAY CARRY SEVERAL REPORTS, and a report whose data runs past the event makes every report
// after it read at the wrong offset - so the walk refuses at the first one that does.
fn advertising_reports_are_walked_and_a_ragged_list_is_refused() {
	// One report: connectable, public, a name "mouse" and the HID service.
	let data: &[u8] = &[0x06, 0x09, b'm', b'o', b'u', b's', b'e', 0x03, 0x03, 0x12, 0x18];
	let mut list = alloc::vec![0x00, 0x00, 0xce, 0xbf, 0x37, 0x37, 0x12, 0x56, data.len() as u8];
	list.extend_from_slice(data);
	list.push(0xc4);
	let found = reports(1, &list).expect("one well-formed report");
	assert_eq!(found.len(), 1);
	assert_eq!(found[0].address, [0x56, 0x12, 0x37, 0x37, 0xbf, 0xce]);
	assert_eq!(found[0].rssi, -60);
	assert_eq!(advertised(found[0].data()), (Some(&b"mouse"[..]), true));
	// A report claiming more data than the list holds.
	let mut ragged = list.clone();
	ragged[8] = 40;
	assert_eq!(reports(1, &ragged), Err(Refusal::RaggedReports));
	// A count larger than the reports present.
	assert_eq!(reports(2, &list), Err(Refusal::RaggedReports));
	// And bytes left over after the counted reports are not reports.
	let mut trailing = list.clone();
	trailing.push(0);
	assert_eq!(reports(1, &trailing), Err(Refusal::RaggedReports));
}

#[test]
// A NAME IS NOT AN IDENTITY AND A MALFORMED FIELD IS NOT A NAME. The walk stops at a field that runs
// past the data rather than reading past it, and an appearance of "mouse" counts as human-interface.
fn an_advertisement_is_read_for_what_an_operator_sees_and_nothing_else() {
	assert_eq!(advertised(&[]), (None, false));
	assert_eq!(advertised(&[0x03, 0x19, 0xc2, 0x03]), (None, true), "appearance: mouse");
	assert_eq!(advertised(&[0x03, 0x19, 0x40, 0x00]), (None, false), "appearance: a phone");
	assert_eq!(advertised(&[0x09, 0x09, b'x']), (None, false), "a field running past the data ends the walk");
	assert_eq!(advertised(&[0x00, 0x09]), (None, false), "a zero-length field ends it too");
}

#[test]
// AN ACL PACKET'S DECLARED LENGTH IS THE LENGTH THAT ARRIVED, and the handle and boundary flag share
// one word.
fn an_acl_packet_round_trips_and_a_length_disagreement_is_refused() {
	let packet = acl(0x0040, 0b10, &[1, 2, 3]);
	assert_eq!(packet, alloc::vec![0x40, 0x20, 0x03, 0x00, 1, 2, 3]);
	assert_eq!(acl_header(&packet), Some((0x0040, 0b10, &[1u8, 2, 3][..])));
	assert_eq!(acl_header(&packet[..6]), None);
	assert_eq!(acl_header(&[0x40]), None);
}

use super::*;

// A small peer: GAP at 1..5, the human-interface service at 6..0x14 with a protocol mode, a report map, and two
// reports - an input one with id 1 (reference and configuration) and a feature one with id 2 (reference only).
const MAP: [u8; 30] = [
	0x05,
	0x01,
	0x09,
	0x02,
	0xa1,
	0x01,
	0x85,
	0x01,
	0x09,
	0x01,
	0xa1,
	0x00,
	0x05,
	0x09,
	0x19,
	0x01,
	0x29,
	0x03,
	0x15,
	0x00,
	0x25,
	0x01,
	0x95,
	0x03,
	0x75,
	0x01,
	0x81,
	0x02,
	0xc0,
	0xc0,
];

fn group_response(entries: &[(u16, u16, u16)]) -> Vec<u8> {
	let mut out = alloc::vec![op::READ_BY_GROUP_TYPE_RESPONSE, 6];
	for (start, end, kind) in entries {
		out.extend_from_slice(&start.to_le_bytes());
		out.extend_from_slice(&end.to_le_bytes());
		out.extend_from_slice(&kind.to_le_bytes());
	}
	out
}

fn declarations(entries: &[(u16, u8, u16, u16)]) -> Vec<u8> {
	let mut out = alloc::vec![op::READ_BY_TYPE_RESPONSE, 7];
	for (handle, properties, value, kind) in entries {
		out.extend_from_slice(&handle.to_le_bytes());
		out.push(*properties);
		out.extend_from_slice(&value.to_le_bytes());
		out.extend_from_slice(&kind.to_le_bytes());
	}
	out
}

fn information(entries: &[(u16, u16)]) -> Vec<u8> {
	let mut out = alloc::vec![op::FIND_INFORMATION_RESPONSE, 1];
	for (handle, kind) in entries {
		out.extend_from_slice(&handle.to_le_bytes());
		out.extend_from_slice(&kind.to_le_bytes());
	}
	out
}

fn not_found(request: u8, handle: u16) -> Vec<u8> {
	let mut out = alloc::vec![op::ERROR_RESPONSE, request];
	out.extend_from_slice(&handle.to_le_bytes());
	out.push(ATTRIBUTE_NOT_FOUND);
	out
}

fn read_response(value: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![op::READ_RESPONSE];
	out.extend_from_slice(value);
	out
}

#[test]
fn walks_to_the_map_and_the_input_reports() {
	let mtu = 23;
	let (mut walk, first) = Discovery::start(mtu);
	assert_eq!(first, Next::Request(request(op::READ_BY_GROUP_TYPE_REQUEST, &[1, 0xffff, uuid::PRIMARY_SERVICE])));
	let next = walk.on_answer(&group_response(&[(0x0001, 0x0005, 0x1800), (0x0006, 0x0014, 0x1812)]));
	assert_eq!(next, Next::Request(request(op::READ_BY_TYPE_REQUEST, &[6, 0x14, uuid::CHARACTERISTIC])));
	// protocol mode 7/8, report map 9/0xa, input report 0xb/0xc (descriptors 0xd, 0xe), feature report 0xf/0x10
	// (descriptor 0x11).
	let next = walk.on_answer(&declarations(&[(0x0007, 0x06, 0x0008, uuid::PROTOCOL_MODE), (0x0009, 0x02, 0x000a, uuid::REPORT_MAP), (0x000b, 0x12, 0x000c, uuid::REPORT)]));
	assert_eq!(next, Next::Request(request(op::READ_BY_TYPE_REQUEST, &[0x0c, 0x14, uuid::CHARACTERISTIC])));
	let next = walk.on_answer(&declarations(&[(0x000f, 0x0a, 0x0010, uuid::REPORT)]));
	assert_eq!(next, Next::Request(request(op::READ_BY_TYPE_REQUEST, &[0x10, 0x14, uuid::CHARACTERISTIC])));
	let next = walk.on_answer(&not_found(op::READ_BY_TYPE_REQUEST, 0x10));
	// THE FIRST REPORT'S DESCRIPTORS END BEFORE THE NEXT DECLARATION.
	assert_eq!(next, Next::Request(request(op::FIND_INFORMATION_REQUEST, &[0x0d, 0x0e])));
	let next = walk.on_answer(&information(&[(0x000d, uuid::REPORT_REFERENCE), (0x000e, uuid::CLIENT_CONFIGURATION)]));
	assert_eq!(next, Next::Request(request(op::FIND_INFORMATION_REQUEST, &[0x11, 0x14])));
	let next = walk.on_answer(&information(&[(0x0011, uuid::REPORT_REFERENCE)]));
	assert_eq!(next, Next::Request(request(op::FIND_INFORMATION_REQUEST, &[0x12, 0x14])));
	let next = walk.on_answer(&not_found(op::FIND_INFORMATION_REQUEST, 0x12));
	assert_eq!(next, Next::Request(request(op::READ_REQUEST, &[0x0d])));
	let next = walk.on_answer(&read_response(&[1, REPORT_INPUT]));
	assert_eq!(next, Next::Request(request(op::READ_REQUEST, &[0x11])));
	let next = walk.on_answer(&read_response(&[2, 3]));
	// THE MAP, past one packet: 22 bytes, then a blob from 22.
	assert_eq!(next, Next::Request(request(op::READ_REQUEST, &[0x0a])));
	let next = walk.on_answer(&read_response(&MAP[..22]));
	assert_eq!(next, Next::Request(request(op::READ_BLOB_REQUEST, &[0x0a, 22])));
	let mut blob = alloc::vec![op::READ_BLOB_RESPONSE];
	blob.extend_from_slice(&MAP[22..]);
	let next = walk.on_answer(&blob);
	// REPORT PROTOCOL, then the input report's notifications on - and only its.
	let Next::Command(command, then) = next else { panic!("report protocol is selected first") };
	assert_eq!(command, write(op::WRITE_COMMAND, 0x08, &[1]));
	assert_eq!(*then, Next::Request(write(op::WRITE_REQUEST, 0x0e, &NOTIFICATIONS_ON)));
	let ready = walk.on_answer(&[op::WRITE_RESPONSE]);
	let Next::Ready(map) = ready else { panic!("the walk ends ready") };
	assert_eq!(map.descriptor, MAP);
	assert_eq!(map.inputs, [Input { handle: 0x0c, id: 1 }]);
	assert_eq!(map.id_of(0x0c), Some(1));
	assert_eq!(map.id_of(0x10), None);
}

#[test]
fn a_boot_only_device_is_told_apart() {
	let (mut walk, _) = Discovery::start(23);
	walk.on_answer(&group_response(&[(0x0006, 0x000e, 0x1812)]));
	let next = walk.on_answer(&declarations(&[(0x0007, 0x06, 0x0008, uuid::PROTOCOL_MODE), (0x0009, 0x12, 0x000a, 0x2a33)]));
	let next = match next {
		Next::Request(_) => walk.on_answer(&not_found(op::READ_BY_TYPE_REQUEST, 0x0b)),
		other => other,
	};
	assert_eq!(next, Next::Unsupported(Unsupported::NoReportMap));
}

#[test]
fn a_map_past_the_bound_is_refused() {
	let (mut walk, _) = Discovery::start(23);
	walk.on_answer(&group_response(&[(0x0006, 0x0010, 0x1812)]));
	walk.on_answer(&declarations(&[(0x0009, 0x02, 0x000a, uuid::REPORT_MAP), (0x000b, 0x12, 0x000c, uuid::REPORT)]));
	walk.on_answer(&not_found(op::READ_BY_TYPE_REQUEST, 0x0d));
	walk.on_answer(&information(&[(0x000d, uuid::CLIENT_CONFIGURATION)]));
	let mut next = walk.on_answer(&not_found(op::FIND_INFORMATION_REQUEST, 0x0e));
	assert_eq!(next, Next::Request(request(op::READ_REQUEST, &[0x0a])));
	next = walk.on_answer(&read_response(&[0u8; 22]));
	let mut rounds = 0;
	while let Next::Request(_) = next {
		let mut blob = alloc::vec![op::READ_BLOB_RESPONSE];
		blob.extend_from_slice(&[0u8; 22]);
		next = walk.on_answer(&blob);
		rounds += 1;
		assert!(rounds < 40, "the walk must stop at its bound");
	}
	assert_eq!(next, Next::Unsupported(Unsupported::TooLarge));
}

use super::*;

fn characteristic(declaration: u16, value: u16, properties: u8) -> Vec<u8> {
	let mut out = alloc::vec![op::READ_BY_TYPE_RESPONSE, 7];
	out.extend_from_slice(&declaration.to_le_bytes());
	out.push(properties);
	out.extend_from_slice(&value.to_le_bytes());
	out.extend_from_slice(&0xfff1u16.to_le_bytes());
	out
}

fn descriptor(handle: u16, uuid: u16) -> Vec<u8> {
	let mut out = alloc::vec![op::FIND_INFORMATION_RESPONSE, 1];
	out.extend_from_slice(&handle.to_le_bytes());
	out.extend_from_slice(&uuid.to_le_bytes());
	out
}

fn absent(opcode: u8, from: u16) -> Vec<u8> {
	let mut out = alloc::vec![op::ERROR_RESPONSE, opcode];
	out.extend_from_slice(&from.to_le_bytes());
	out.push(ATTRIBUTE_NOT_FOUND);
	out
}

fn descriptors(properties: u8) -> Subscription {
	let mut walk = Subscription::new(1, 40, 3, 100).unwrap();
	assert_eq!(walk.request(), [8, 2, 0, 3, 0, 3, 0x28]);
	assert_eq!(walk.on_response(&characteristic(2, 3, properties), 0), Ok(false));
	assert_eq!(walk.request(), [8, 4, 0, 40, 0, 3, 0x28]);
	assert_eq!(walk.on_response(&absent(op::READ_BY_TYPE_REQUEST, 4), 0), Ok(false));
	assert_eq!(walk.request(), [4, 4, 0, 40, 0]);
	walk
}

#[test]
fn chooses_the_offered_mode_and_requires_the_write_acknowledgment() {
	for (properties, setting) in [(0x10, 1), (0x20, 2), (0x30, 1)] {
		let mut walk = descriptors(properties);
		assert_eq!(walk.on_response(&descriptor(4, 0x2902), 0), Ok(false));
		assert_eq!(walk.request(), [0x12, 4, 0, setting, 0]);
		assert_eq!(walk.value(), 3);
		for wrong in [&[0x0b][..], &[0x13, 0], &[], &[0x01, 0x12]] {
			let mut other = walk.clone();
			assert_eq!(other.on_response(wrong, 0), Err(Error::Io));
			assert!(other.request().is_empty());
		}
		assert_eq!(walk.on_response(&[0x13], 0), Ok(true));
		assert!(walk.request().is_empty());
	}
}

#[test]
fn never_takes_the_next_characteristics_cccd() {
	let mut walk = Subscription::new(1, 40, 3, 100).unwrap();
	assert_eq!(walk.on_response(&characteristic(2, 3, 0x10), 0), Ok(false));
	assert_eq!(walk.on_response(&characteristic(7, 8, 0x10), 0), Ok(false));
	assert_eq!(walk.request(), [4, 4, 0, 6, 0]);
	assert_eq!(walk.on_response(&descriptor(9, 0x2902), 0), Err(Error::Io));
	assert!(walk.request().is_empty());
	let mut walk = Subscription::new(1, 40, 3, 100).unwrap();
	assert_eq!(walk.on_response(&characteristic(2, 3, 0x10), 0), Ok(false));
	assert_eq!(walk.on_response(&characteristic(4, 5, 0x10), 0), Err(Error::Unsupported));
}

#[test]
fn advances_across_characteristic_and_mixed_descriptor_pages() {
	let mut walk = Subscription::new(1, 40, 9, 100).unwrap();
	assert_eq!(walk.on_response(&characteristic(2, 3, 0x10), 0), Ok(false));
	assert_eq!(walk.request(), [8, 4, 0, 9, 0, 3, 0x28]);
	let mut target = characteristic(8, 9, 0x20);
	target[1] = 21;
	target.extend_from_slice(&[0; 14]);
	assert_eq!(walk.on_response(&target, 0), Ok(false));
	assert_eq!(walk.on_response(&characteristic(20, 21, 0x10), 0), Ok(false));
	assert_eq!(walk.request(), [4, 10, 0, 19, 0]);
	let mut vendor = alloc::vec![5, 2, 10, 0];
	vendor.extend_from_slice(&[0xab; 16]);
	assert_eq!(walk.on_response(&vendor, 0), Ok(false));
	assert_eq!(walk.request(), [4, 11, 0, 19, 0]);
	assert_eq!(walk.on_response(&descriptor(11, 0x2901), 0), Ok(false));
	assert_eq!(walk.request(), [4, 12, 0, 19, 0]);
	assert_eq!(walk.on_response(&descriptor(12, 0x2902), 0), Ok(false));
	assert_eq!(walk.request(), [0x12, 12, 0, 2, 0]);
	assert_eq!(walk.on_response(&[0x13], 0), Ok(true));
}

#[test]
fn descriptor_handles_and_the_whole_page_are_validated_before_writing() {
	let original = descriptors(0x10);
	for handle in [0, 1, 3, 41, 0xffff] {
		let mut walk = original.clone();
		assert_eq!(walk.on_response(&descriptor(handle, 0x2902), 0), Err(Error::Io));
		assert!(walk.request().is_empty());
	}
	let mut ragged = descriptor(4, 0x2902);
	ragged.push(0);
	let mut backwards = descriptor(5, 0x2902);
	backwards.extend_from_slice(&descriptor(4, 0x2901)[2..]);
	let mut duplicate = descriptor(4, 0x2902);
	duplicate.extend_from_slice(&descriptor(5, 0x2902)[2..]);
	let mut wrong_opcode = descriptor(4, 0x2902);
	wrong_opcode[0] = op::READ_RESPONSE;
	let mut oversized = descriptor(4, 0x2902);
	oversized.resize(crate::bt_bounds::ATT_MTU + 1, 0);
	for pdu in [ragged, backwards, duplicate, wrong_opcode, oversized] {
		let mut walk = original.clone();
		assert_eq!(walk.on_response(&pdu, 0), Err(Error::Io));
		assert!(walk.request().is_empty());
	}
}

#[test]
fn refuses_wrong_characteristics_and_malformed_declarations() {
	let mut wrong_opcode = characteristic(2, 3, 0x10);
	wrong_opcode[0] = op::READ_RESPONSE;
	let mut ragged = characteristic(2, 3, 0x10);
	ragged.push(0);
	for pdu in [wrong_opcode, ragged, characteristic(0, 3, 0x10), characteristic(4, 5, 0x10), characteristic(2, 2, 0x10), characteristic(2, 41, 0x10)] {
		let mut walk = Subscription::new(1, 40, 3, 100).unwrap();
		assert_eq!(walk.on_response(&pdu, 0), Err(Error::Io));
	}
	for (value, properties) in [(4, 0x10), (3, 0x02)] {
		let mut walk = Subscription::new(1, 40, 3, 100).unwrap();
		assert_eq!(walk.on_response(&characteristic(2, value, properties), 0), Err(Error::Unsupported));
	}
	for (start, end, value) in [(0, 10, 3), (1, 3, 3), (0xffff, 0xffff, 0xffff), (1, 0xffff, 0xffff)] {
		assert!(matches!(Subscription::new(start, end, value, 100), Err(Error::Unsupported)));
	}
}

#[test]
fn maps_real_refusal_and_rejects_error_from_another_request() {
	let original = descriptors(0x20);
	assert_eq!(original.clone().on_response(&[1, 4, 4, 0, 0x05], 0), Err(Error::Denied));
	assert_eq!(original.clone().on_response(&[1, 4, 4, 0, 0x0a], 0), Err(Error::Unsupported));
	for pdu in [&[1, 8, 4, 0, 0x0a][..], &[1, 4, 41, 0, 0x0a], &[1, 4, 4, 0, 0x0a, 0]] {
		assert_eq!(original.clone().on_response(pdu, 0), Err(Error::Io));
	}
}

#[test]
fn bounds_advancing_pages_and_refuses_repeated_handles() {
	let mut walk = Subscription::new(1, 1000, 900, 100).unwrap();
	for index in 0..MAX_DISCOVERED {
		let declaration = 2 + index as u16 * 2;
		assert_eq!(walk.on_response(&characteristic(declaration, declaration + 1, 0x10), 0), if index + 1 == MAX_DISCOVERED { Err(Error::Exhausted) } else { Ok(false) });
	}
	let mut walk = descriptors(0x10);
	for index in 0..MAX_DISCOVERED {
		assert_eq!(walk.on_response(&descriptor(4 + index as u16, 0x2901), 0), if index + 1 == MAX_DISCOVERED { Err(Error::Exhausted) } else { Ok(false) });
	}
	let mut walk = descriptors(0x10);
	assert_eq!(walk.on_response(&descriptor(4, 0x2901), 0), Ok(false));
	assert_eq!(walk.on_response(&descriptor(4, 0x2902), 0), Err(Error::Io));
}

#[test]
fn accepts_the_bluetooth_base_uuid_form_of_cccd() {
	let mut walk = descriptors(0x20);
	let mut page = alloc::vec![5, 2, 4, 0];
	page.extend_from_slice(&[0xfb, 0x34, 0x9b, 0x5f, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x02, 0x29, 0x00, 0x00]);
	assert_eq!(walk.on_response(&page, 0), Ok(false));
	assert_eq!(walk.request(), [0x12, 4, 0, 2, 0]);
}

#[test]
fn one_deadline_covers_all_pages_and_a_late_reply_cannot_complete_the_write() {
	let mut walk = descriptors(0x20);
	assert_eq!(walk.deadline(), 100);
	assert!(!walk.expired(99));
	assert_eq!(walk.on_response(&descriptor(4, 0x2901), 98), Ok(false));
	assert_eq!(walk.on_response(&descriptor(5, 0x2902), 99), Ok(false));
	assert_eq!(walk.request(), [0x12, 5, 0, 2, 0]);
	assert!(walk.expired(100));
	assert_eq!(walk.on_response(&[0x13], 100), Err(Error::TimedOut));
	assert!(walk.request().is_empty());
	assert_eq!(walk.on_response(&[0x13], 101), Err(Error::TimedOut));
}

#[test]
fn the_last_possible_descriptor_handle_does_not_wrap_the_walk() {
	let mut walk = Subscription::new(0xfffc, 0xffff, 0xfffe, 100).unwrap();
	assert_eq!(walk.on_response(&characteristic(0xfffd, 0xfffe, 0x20), 0), Ok(false));
	assert_eq!(walk.on_response(&absent(op::READ_BY_TYPE_REQUEST, 0xffff), 0), Ok(false));
	assert_eq!(walk.request(), [4, 0xff, 0xff, 0xff, 0xff]);
	assert_eq!(walk.on_response(&descriptor(0xffff, 0x2902), 0), Ok(false));
	assert_eq!(walk.request(), [0x12, 0xff, 0xff, 2, 0]);
	assert_eq!(walk.on_response(&[0x13], 0), Ok(true));
}

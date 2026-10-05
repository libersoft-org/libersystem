use super::*;

// The canonical frames every RFCOMM trace shows: the initiator's SABM on DLCI 0 and the responder's UA.
#[test]
fn rfcomm_frames_carry_the_specified_check() {
	assert_eq!(rfcomm_frame(0, true, SABM, true, None, &[]), [0x03, 0x3F, 0x01, 0x1C]);
	assert_eq!(rfcomm_frame(0, true, UA, true, None, &[]), [0x03, 0x73, 0x01, 0xD7]);
}

#[test]
fn inquiry_names_every_device_with_its_classes() {
	let mut world = World::new(7);
	let out = world.command(0x0401, &[0x33, 0x8b, 0x9e, 4, 0]).expect("inquiry is a classic command");
	let results: Vec<&Vec<u8>> = out
		.iter()
		.filter_map(|out| match out {
			Out::Event(bytes) if bytes[0] == 0x2F => Some(bytes),
			_ => None,
		})
		.collect();
	assert_eq!(results.len(), DEVICES.len());
	// One result per event, the address least significant first, and the name in the response.
	let phone = results[0];
	assert_eq!(phone[1] as usize, phone.len() - 2);
	assert_eq!(&phone[3..9], &[0x02, 0x00, 0x20, 0xdc, 0x1b, 0x00]);
	let eir = &phone[17..];
	assert_eq!(eir[1], 0x09);
	assert_eq!(&eir[2..2 + "fixture phone".len()], b"fixture phone");
	assert!(matches!(out.last(), Some(Out::Event(bytes)) if bytes[0] == 0x01));
}

#[test]
fn a_page_needs_page_scan() {
	let mut world = World::new(7);
	let (result, out) = world.act(FIRST_DEVICE, 1, 0).unwrap();
	assert_eq!(result, 0x04);
	assert!(out.is_empty());
	world.command(0x0C1A, &[2]).unwrap();
	let (result, out) = world.act(FIRST_DEVICE, 1, 0).unwrap();
	assert_eq!(result, 0);
	assert!(matches!(&out[0], Out::Event(bytes) if bytes[0] == 0x04));
	assert!(world.take_log().iter().any(|line| line == "scan 2"));
}

fn events(out: &[Out]) -> Vec<u8> {
	out.iter()
		.filter_map(|out| match out {
			Out::Event(bytes) if bytes[0] != 0x0e && bytes[0] != 0x0f => Some(bytes[0]),
			_ => None,
		})
		.collect()
}

// THE HOST PAIRS THE PHONE: no key, both DisplayYesNo, so Numeric Comparison - and a key of the authenticated
// P-256 type once the host confirms.
#[test]
fn numeric_comparison_with_the_phone() {
	let mut world = World::new(7);
	let address = wire(&DEVICES[0].address);
	let out = world.command(0x0405, &with_address(&DEVICES[0].address, &[0x18, 0xcc, 1, 0, 0, 0, 1])).unwrap();
	assert_eq!(events(&out), [0x03]);
	let handle = FIRST_HANDLE;
	assert_eq!(events(&world.command(0x0411, &handle.to_le_bytes()).unwrap()), [0x17]);
	assert_eq!(events(&world.command(0x040C, &address).unwrap()), [0x31]);
	let mut reply = address.to_vec();
	reply.extend_from_slice(&[1, 0, 5]);
	assert_eq!(events(&world.command(0x042B, &reply).unwrap()), [0x32, 0x33]);
	let out = world.command(0x042C, &address).unwrap();
	assert_eq!(events(&out), [0x36, 0x18, 0x06]);
	let key_type = out.iter().find_map(|out| match out {
		Out::Event(bytes) if bytes[0] == 0x18 => Some(bytes[2 + 22]),
		_ => None,
	});
	assert_eq!(key_type, Some(0x08));
	assert_eq!(events(&world.command(0x0413, &[handle as u8, (handle >> 8) as u8, 1]).unwrap()), [0x08]);
}

// THE SERIAL DEVICE'S RECORD names RFCOMM channel 3 under the serial port class.
#[test]
fn the_serial_record_names_its_channel() {
	let record = de_sequence(&[de_u16(0x0004), de_sequence(&[de_sequence(&[de_uuid(0x0100)]), de_sequence(&[de_uuid(0x0003), de_u8(3)])])]);
	let mut uuids = Vec::new();
	uuids_in(&record, &mut uuids);
	assert_eq!(uuids, [0x0100, 0x0003]);
}

// THE KEYBOARD'S DESCRIPTOR is what the shared parser reads as a keyboard with a consumer control, by report id.
#[test]
fn the_keyboard_descriptor_parses_as_a_keyboard_and_a_consumer_control() {
	let layout = crate::hid::parse(&KEYBOARD_DESCRIPTOR);
	assert!(layout.uses_ids());
	assert!(layout.has_keyboard());
	assert!(layout.has_consumer());
	let mut seen = Vec::new();
	layout.keys_diff(1, &[0; 8], &[0x02, 0, 0x05, 0, 0, 0, 0, 0], &mut |usage, down| seen.push((usage, down)));
	assert!(seen.contains(&(0x0007_00e1, true)));
	assert!(seen.contains(&(0x0007_0005, true)));
	let mut consumer = Vec::new();
	layout.keys_diff(2, &[0, 0], &[0x30, 0x00], &mut |usage, down| consumer.push((usage, down)));
	assert_eq!(consumer, [(0x000c_0030, true)]);
}

// TYPING: the script becomes press-and-release reports on the interrupt channel, and a chord is one report down.
#[test]
fn a_script_is_typed_on_the_interrupt_channel() {
	let mut world = World::new(7);
	let keyboard = usize::from(KEYBOARD - FIRST_DEVICE);
	world.command(0x0C1A, &[2]).unwrap();
	world.act(KEYBOARD, 1, 0).unwrap();
	world.command(0x0409, &with_address(&DEVICES[keyboard].address, &[0])).unwrap();
	// The keyboard opens control; the host answers success and both configurations, and interrupt follows.
	let link_handle = FIRST_HANDLE + keyboard as u16;
	let opened = world.act(KEYBOARD, 13, 0).unwrap().1;
	assert!(opened.iter().any(|out| matches!(out, Out::Acl(bytes) if bytes[8] == 0x02 && u16::from_le_bytes([bytes[12], bytes[13]]) == HID_CONTROL)));
	let respond = |world: &mut World, local: u16, remote: u16| {
		// Connection response (success), the host's configuration request, and its response to the device's.
		let mut signals = Vec::new();
		signals.extend_from_slice(&[0x03, 1, 8, 0]);
		signals.extend_from_slice(&remote.to_le_bytes());
		signals.extend_from_slice(&local.to_le_bytes());
		signals.extend_from_slice(&[0, 0, 0, 0]);
		signals.extend_from_slice(&[0x04, 2, 4, 0]);
		signals.extend_from_slice(&local.to_le_bytes());
		signals.extend_from_slice(&[0, 0]);
		signals.extend_from_slice(&[0x05, 2, 6, 0]);
		signals.extend_from_slice(&local.to_le_bytes());
		signals.extend_from_slice(&[0, 0, 0, 0]);
		let mut pdu = (signals.len() as u16).to_le_bytes().to_vec();
		pdu.extend_from_slice(&1u16.to_le_bytes());
		pdu.extend_from_slice(&signals);
		world.acl(link_handle, 0b10, &pdu).unwrap()
	};
	let after_control = respond(&mut world, 0x0040, 0x0050);
	assert!(after_control.iter().any(|out| matches!(out, Out::Acl(bytes) if bytes[8] == 0x02 && u16::from_le_bytes([bytes[12], bytes[13]]) == HID_INTERRUPT)));
	respond(&mut world, 0x0041, 0x0051);
	world.type_text(KEYBOARD, "a\n", 5).unwrap();
	assert!(world.tick(4).is_empty());
	let typed = world.tick(5);
	let reports: Vec<&[u8]> = typed
		.iter()
		.filter_map(|out| match out {
			Out::Acl(bytes) if u16::from_le_bytes([bytes[6], bytes[7]]) == 0x0051 => Some(&bytes[8..]),
			_ => None,
		})
		.collect();
	assert_eq!(reports.len(), 4);
	assert_eq!(reports[0], [HIDP_DATA_INPUT, 1, 0, 0, 0x04, 0, 0, 0, 0, 0]);
	assert_eq!(reports[2], [HIDP_DATA_INPUT, 1, 0, 0, 0x28, 0, 0, 0, 0, 0]);
}

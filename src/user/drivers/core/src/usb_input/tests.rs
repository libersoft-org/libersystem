use super::*;

// A composite configuration as a keyboard with a media-key interface ships it: a boot keyboard (interface 0) with
// its HID class descriptor and interrupt IN 0x81, an alternate setting of interface 0 that must not bind twice, a
// HID interface (1) whose only endpoint is OUT and is therefore not one, a vendor interface (2), and a pointing
// interface (3) with interrupt IN 0x84 every 10 frames.
fn composite() -> Vec<u8> {
	let mut config = alloc::vec![9u8, 0x02, 0, 0, 4, 1, 0, 0xa0, 50];
	config.extend_from_slice(&[9, 0x04, 0, 0, 1, 3, 1, 1, 0]);
	config.extend_from_slice(&[9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63, 0]);
	config.extend_from_slice(&[7, 0x05, 0x81, 0x03, 8, 0, 10]);
	config.extend_from_slice(&[9, 0x04, 0, 1, 1, 3, 1, 1, 0]);
	config.extend_from_slice(&[7, 0x05, 0x85, 0x03, 8, 0, 10]);
	config.extend_from_slice(&[9, 0x04, 1, 0, 1, 3, 0, 0, 0]);
	config.extend_from_slice(&[7, 0x05, 0x02, 0x03, 8, 0, 10]);
	config.extend_from_slice(&[9, 0x04, 2, 0, 1, 0xff, 0, 0, 0]);
	config.extend_from_slice(&[7, 0x05, 0x83, 0x03, 8, 0, 10]);
	config.extend_from_slice(&[9, 0x04, 3, 0, 1, 3, 0, 2, 0]);
	config.extend_from_slice(&[9, 0x21, 0x11, 0x01, 0, 1, 0x22, 0x34, 0x01]);
	config.extend_from_slice(&[7, 0x05, 0x84, 0x03, 4, 0, 10]);
	let total = config.len() as u16;
	config[2..4].copy_from_slice(&total.to_le_bytes());
	config
}

#[test]
// EVERY HID INTERFACE WITH AN INTERRUPT IN ENDPOINT, ONCE: the boot keyboard with its report length, the pointer
// with its own, the alternate setting of interface 0 not a second binding, the OUT-only one and the vendor one not
// HID inputs at all.
fn every_hid_interface_with_an_interrupt_in_endpoint_is_declared_once() {
	let declared = declare(&composite());
	assert_eq!(declared.len(), 2);
	assert_eq!(declared[0], Declared { iface: 0, boot_keyboard: true, desc_len: 63, endpoint: Some(Endpoint { address: 0x81, max_packet: 8, interval: 10 }) });
	assert_eq!(declared[1], Declared { iface: 3, boot_keyboard: false, desc_len: 0x134, endpoint: Some(Endpoint { address: 0x84, max_packet: 4, interval: 10 }) });
}

#[test]
// A TWO-BYTE INTERFACE RECORD IS LEGAL AND IS NOT A HID ONE: its class is not in it, and the byte after it is the
// next record's header.
fn an_interface_record_too_short_to_carry_its_class_is_not_hid() {
	let mut config = alloc::vec![9u8, 0x02, 0, 0, 1, 1, 0, 0x80, 50];
	config.extend_from_slice(&[2, 0x04]);
	config.extend_from_slice(&[7, 0x05, 0x81, 0x03, 8, 0, 10]);
	let total = config.len() as u16;
	config[2..4].copy_from_slice(&total.to_le_bytes());
	assert!(declare(&config).is_empty());
}

#[test]
// FOUR AT MOST, the interfaces the class budget charges a device for.
fn no_more_than_four_interfaces_are_declared() {
	let mut config = alloc::vec![9u8, 0x02, 0, 0, 6, 1, 0, 0x80, 50];
	for iface in 0..6u8 {
		config.extend_from_slice(&[9, 0x04, iface, 0, 1, 3, 0, 0, 0]);
		config.extend_from_slice(&[7, 0x05, 0x81 + iface, 0x03, 8, 0, 10]);
	}
	let total = config.len() as u16;
	config[2..4].copy_from_slice(&total.to_le_bytes());
	let declared = declare(&config);
	assert_eq!(declared.len(), HID_INTERFACES as usize);
	assert_eq!(declared.iter().map(|found| found.iface).collect::<Vec<_>>(), [0, 1, 2, 3]);
}

#[test]
// THE LAYOUT AN INTERFACE DECODES AGAINST: its own report descriptor when that says something the system consumes;
// the boot layout for a boot keyboard whose descriptor does not, once SET_PROTOCOL was taken; nothing for one that
// refused it, and nothing - not even the question - for an interface that is no keyboard.
fn an_unreadable_boot_keyboard_falls_back_to_the_boot_protocol_and_anything_else_to_nothing() {
	// A keyboard report descriptor: eight modifier bits and six key array bytes.
	let keyboard: &[u8] = &[
		0x05,
		0x01,
		0x09,
		0x06,
		0xa1,
		0x01,
		0x05,
		0x07,
		0x19,
		0xe0,
		0x29,
		0xe7,
		0x15,
		0x00,
		0x25,
		0x01,
		0x75,
		0x01,
		0x95,
		0x08,
		0x81,
		0x02,
		0x95,
		0x06,
		0x75,
		0x08,
		0x15,
		0x00,
		0x26,
		0xff,
		0x00,
		0x05,
		0x07,
		0x19,
		0x00,
		0x2a,
		0xff,
		0x00,
		0x81,
		0x00,
		0xc0,
	];
	let own = layout_for(Some(keyboard), false, || panic!("a usable descriptor needs no boot protocol")).expect("a keyboard's own layout");
	assert!(own.has_keyboard());
	let mut asked = false;
	let boot = layout_for(None, true, || {
		asked = true;
		true
	})
	.expect("the boot layout");
	assert!(asked && boot.has_keyboard(), "a boot keyboard with no readable descriptor is asked into the boot protocol");
	assert!(layout_for(None, true, || false).is_none(), "and one that refused it binds nothing");
	assert!(layout_for(Some(&[0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0xc0]), false, || panic!("not a keyboard, so not asked")).is_none(), "a vendor page is nothing the system consumes");
}

#[test]
// `usb VVVV:PPPP port P if I`, and the collection past the first named.
fn a_gamepad_is_labelled_by_its_device_port_and_interface() {
	assert_eq!(pad_label(0x1d6b, 0x0104, 3, 1, 0), b"usb 1d6b:0104 port 3 if 1");
	assert_eq!(pad_label(0x045e, 0x028e, 12, 0, 1), b"usb 045e:028e port 12 if 0 pad 2");
}

use super::*;

// THE TWO DEVICES THE HARNESS MODELS (`vhost-i2c-gpio.py --hid`), byte for byte.
//
// A PRECISION TOUCHPAD: a Mouse collection (report 1), a Touch Pad collection (report 2) and a Device Configuration
// collection (feature report 3, Input Mode).
const TOUCHPAD: &[u8] = &[
	0x05,
	0x01,
	0x09,
	0x02,
	0xA1,
	0x01,
	0x85,
	0x01,
	0x09,
	0x01,
	0xA1,
	0x00,
	0x05,
	0x09,
	0x19,
	0x01,
	0x29,
	0x02,
	0x15,
	0x00,
	0x25,
	0x01,
	0x75,
	0x01,
	0x95,
	0x02,
	0x81,
	0x02,
	0x95,
	0x06,
	0x81,
	0x03,
	0x05,
	0x01,
	0x09,
	0x30,
	0x09,
	0x31,
	0x15,
	0x81,
	0x25,
	0x7F,
	0x75,
	0x08,
	0x95,
	0x02,
	0x81,
	0x06,
	0xC0,
	0xC0, // mouse
	0x05,
	0x0D,
	0x09,
	0x05,
	0xA1,
	0x01,
	0x85,
	0x02,
	0x09,
	0x22,
	0xA1,
	0x02,
	0x09,
	0x42,
	0x15,
	0x00,
	0x25,
	0x01,
	0x75,
	0x01,
	0x95,
	0x01,
	0x81,
	0x02,
	0x95,
	0x07,
	0x81,
	0x03,
	0x09,
	0x51,
	0x25,
	0x0F,
	0x75,
	0x08,
	0x95,
	0x01,
	0x81,
	0x02,
	0x05,
	0x01,
	0x09,
	0x30,
	0x09,
	0x31,
	0x15,
	0x00,
	0x26,
	0xFF,
	0x0F,
	0x75,
	0x10,
	0x95,
	0x02,
	0x81,
	0x02,
	0xC0,
	0x05,
	0x0D,
	0x09,
	0x54,
	0x15,
	0x00,
	0x25,
	0x05,
	0x75,
	0x08,
	0x95,
	0x01,
	0x81,
	0x02,
	0xC0, // touch pad
	0x05,
	0x0D,
	0x09,
	0x0E,
	0xA1,
	0x01,
	0x85,
	0x03,
	0x09,
	0x22,
	0xA1,
	0x02,
	0x09,
	0x52,
	0x15,
	0x00,
	0x25,
	0x0A,
	0x75,
	0x08,
	0x95,
	0x01,
	0xB1,
	0x02,
	0xC0,
	0xC0, // configuration
];

// One finger's logical collection: its tip switch, contact identifier and absolute X and Y over 0..4095.
const FINGER: [u8; 50] = [
	0x05,
	0x0D,
	0x09,
	0x22,
	0xA1,
	0x02,
	0x09,
	0x42,
	0x15,
	0x00,
	0x25,
	0x01,
	0x75,
	0x01,
	0x95,
	0x01,
	0x81,
	0x02,
	0x95,
	0x07,
	0x81,
	0x03,
	0x09,
	0x51,
	0x25,
	0x0F,
	0x75,
	0x08,
	0x95,
	0x01,
	0x81,
	0x02,
	0x05,
	0x01,
	0x09,
	0x30,
	0x09,
	0x31,
	0x15,
	0x00,
	0x26,
	0xFF,
	0x0F,
	0x75,
	0x10,
	0x95,
	0x02,
	0x81,
	0x02,
	0xC0,
];

// A TOUCHSCREEN: a Touch Screen collection (report 1) of two fingers and a contact count.
fn touchscreen() -> alloc::vec::Vec<u8> {
	let mut out = alloc::vec![0x05, 0x0D, 0x09, 0x04, 0xA1, 0x01, 0x85, 0x01];
	for _ in 0..2 {
		out.extend_from_slice(&FINGER);
	}
	out.extend_from_slice(&[0x05, 0x0D, 0x09, 0x54, 0x15, 0x00, 0x25, 0x02, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0xC0]);
	out
}

fn finger(tip: bool, id: u8, x: u16, y: u16) -> [u8; 6] {
	let mut out = [tip as u8, id, 0, 0, 0, 0];
	out[2..4].copy_from_slice(&x.to_le_bytes());
	out[4..6].copy_from_slice(&y.to_le_bytes());
	out
}

// A PRECISION TOUCHPAD PUBLISHES `pointer` ALONE, and its reports go by collection: the mouse report to the pointer,
// the touch pad's finger report nowhere - though its Generic Desktop X and Y would fold into a pointer - and the
// configuration's feature report is no input at all.
#[test]
fn a_precision_touchpad_publishes_pointer_alone_and_its_finger_report_goes_nowhere() {
	let layout = hid::parse(TOUCHPAD);
	assert_eq!(publications(&layout), Publications { pointer: true, touch: false });
	assert_eq!((route(&layout, 1), route(&layout, 2), route(&layout, 3)), (Route::Pointer, Route::Nothing, Route::Nothing));
	let (mut x, mut y, mut buttons, mut wheel) = (0, 0, 0, 0);
	assert!(layout.pointer_fold(2, &[1, 0, 0xE8, 0x03, 0xE8, 0x03, 1], &mut x, &mut y, &mut buttons, &mut wheel), "the finger report WOULD move a pointer - which is why it is routed by its collection");
	// TWO MOVES AND A CLICK, the harness's script in mouse mode.
	let mut pointer = Pointer::default();
	let (id, body) = split(&layout, &[1, 0, 10, 5]).unwrap();
	let moved = pointer.feed(&layout, id, body).expect("the first move");
	assert_eq!((u16::from_le_bytes([moved[0], moved[1]]), u16::from_le_bytes([moved[2], moved[3]]), moved[4]), (10, 5, 0));
	let moved = pointer.feed(&layout, 1, &[0, 10, 5]).expect("the second");
	assert_eq!((u16::from_le_bytes([moved[0], moved[1]]), u16::from_le_bytes([moved[2], moved[3]])), (20, 10));
	assert_eq!(pointer.feed(&layout, 1, &[1, 0, 0]).map(|frame| frame[4]), Some(1), "the button down");
	assert_eq!(pointer.feed(&layout, 1, &[1, 0, 0]), None, "and nothing for a report that changed nothing");
	assert_eq!(pointer.feed(&layout, 1, &[0, 0, 0]).map(|frame| frame[4]), Some(0), "and up");
}

// A TOUCHSCREEN PUBLISHES `touch` ALONE, and its report is contacts: two fingers down and their lift, each scaled into
// the pointer grid, never a pointer.
#[test]
fn a_touchscreen_publishes_touch_alone_and_its_report_is_contacts() {
	let descriptor = touchscreen();
	let layout = hid::parse(&descriptor);
	assert_eq!(publications(&layout), Publications { pointer: false, touch: true });
	assert_eq!(route(&layout, 1), Route::Touch);
	let mut report = alloc::vec![1u8];
	report.extend_from_slice(&finger(true, 0, 1024, 1024));
	report.extend_from_slice(&finger(true, 1, 3072, 3072));
	report.push(2);
	let (id, body) = split(&layout, &report).unwrap();
	let mut frames = [[0u8; 6]; hid::MAX_CONTACTS];
	assert_eq!(contact_frames(&layout, id, body, &mut frames), 2);
	let at = |value: u16| ((value as i64 * hid::NORM_MAX as i64) / 4095) as u16;
	assert_eq!(frames[0], {
		let mut f = [0u8, 1, 0, 0, 0, 0];
		f[2..4].copy_from_slice(&at(1024).to_le_bytes());
		f[4..6].copy_from_slice(&at(1024).to_le_bytes());
		f
	});
	assert_eq!((frames[1][0], frames[1][1], u16::from_le_bytes([frames[1][2], frames[1][3]])), (1, 1, at(3072)));
	let mut lift = alloc::vec![1u8];
	lift.extend_from_slice(&finger(false, 0, 1024, 1024));
	lift.extend_from_slice(&finger(false, 1, 3072, 3072));
	lift.push(2);
	assert_eq!(contact_frames(&layout, 1, &lift[1..], &mut frames), 2);
	assert_eq!((frames[0][1], frames[1][1]), (0, 0), "both lifted");
}

// A DESCRIPTOR WITH NEITHER COLLECTION - a keyboard's - publishes nothing.
#[test]
fn a_descriptor_with_neither_collection_publishes_nothing() {
	let layout = hid::boot_keyboard();
	assert_eq!(publications(&layout), Publications::default());
}

// THE RESET INDICATION is awaited twice at most; the storm rule spends one reset and then fails.
#[test]
fn the_reset_is_retried_once_and_a_storm_resets_once_and_then_fails() {
	let mut handshake = ResetHandshake::new();
	assert_eq!(handshake.timed_out(), ResetNext::Again);
	assert_eq!(handshake.timed_out(), ResetNext::Refuse);
	assert_eq!(handshake.timed_out(), ResetNext::Refuse);
	assert_eq!(RESET_BOUND_TICKS, 5 * rt::TICKS_PER_SECOND);
	let mut storm = Storm::default();
	assert_eq!(storm.empty_event(), StormAnswer::Reset);
	assert_eq!(storm.empty_event(), StormAnswer::Fail);
}

// THE DESCRIPTOR REGISTER, from either firmware: the tree's `hid-descr-addr` cell in the property block, and the
// `_DSM`'s integer answer.
#[test]
fn the_descriptor_register_comes_from_the_tree_s_property_or_the_dsm_s_integer() {
	fn record(kind: u8, depth: u8, name: &[u8], value: &[u8]) -> alloc::vec::Vec<u8> {
		let mut out = alloc::vec![kind, depth];
		out.extend_from_slice(&(name.len() as u16).to_le_bytes());
		out.extend_from_slice(&(value.len() as u32).to_le_bytes());
		out.extend_from_slice(name);
		out.extend_from_slice(value);
		// THE VALUE IS PADDED TO FOUR, and the name is not - as the kernel writes a block.
		out.resize(out.len() + ((value.len() + 3) & !3) - value.len(), 0);
		out
	}
	let mut block = record(rt::DEVICE_PROPERTY_VALUE, 0, b"compatible", b"hid-over-i2c\0");
	block.extend(record(rt::DEVICE_PROPERTY_VALUE, 0, b"reg", &0x2Cu32.to_be_bytes()));
	block.extend(record(rt::DEVICE_PROPERTY_VALUE, 0, b"hid-descr-addr", &0x20u32.to_be_bytes()));
	assert_eq!(tree_descriptor_register(&block), Some(0x20));
	// A CHILD NODE'S property of the same name is not the device's.
	let mut nested = record(rt::DEVICE_PROPERTY_NODE, 1, b"child", &[]);
	nested.extend(record(rt::DEVICE_PROPERTY_VALUE, 1, b"hid-descr-addr", &0x20u32.to_be_bytes()));
	assert_eq!(tree_descriptor_register(&nested), None);
	assert_eq!(tree_descriptor_register(&block[..block.len() - 2]), None, "a cut record is no register");
	assert_eq!(tree_descriptor_register(&record(rt::DEVICE_PROPERTY_VALUE, 0, b"hid-descr-addr", &0x1_0000u32.to_be_bytes())), None, "nor a number past sixteen bits");
	let mut integer = alloc::vec![0x01];
	integer.extend_from_slice(&0x20u64.to_le_bytes());
	assert_eq!(dsm_register(&integer), Some(0x20));
	assert_eq!(dsm_register(&[0x02, 1, 2]), None, "a buffer is not a register");
}

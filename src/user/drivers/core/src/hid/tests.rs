// The HID parser's fixtures. Each holds a rule against a descriptor or a report written out by hand,
// because a parser tested only against the descriptors real devices send is a parser tested against
// the inputs that never caused the defect.

use super::*;

// A descriptor that walks the bit cursor past the bound it is checked against.
//
// THE CURSOR SATURATES AND THE CHECK MUST TOO. A plain `cursor + bits` overflows on exactly the
// descriptor the check exists to refuse: in a debug build it panics, and in a release one it wraps
// to a small number and ADMITS the segment - which then reads bits from wherever the arithmetic
// landed.
#[test]
fn a_descriptor_that_runs_the_bit_cursor_past_the_bound_is_refused_rather_than_wrapping() {
	let mut descriptor: Vec<u8> = Vec::new();
	descriptor.extend_from_slice(&[0x05, 0x01]); // usage page (generic desktop)
	descriptor.extend_from_slice(&[0x09, 0x06]); // usage (keyboard)
	descriptor.extend_from_slice(&[0xa1, 0x01]); // collection
	descriptor.extend_from_slice(&[0x05, 0x07]); // usage page (keyboard)
	descriptor.extend_from_slice(&[0x15, 0x00, 0x25, 0x01]); // logical 0..1
	// Thirty-two bits per field, and the largest count a four-byte item can carry, repeated until
	// the cursor is far past anything that fits in a report.
	for _ in 0..40 {
		descriptor.extend_from_slice(&[0x75, 0x20]); // report size 32
		descriptor.extend_from_slice(&[0x97, 0xff, 0xff, 0xff, 0x7f]); // report count 0x7fffffff
		descriptor.extend_from_slice(&[0x81, 0x02]); // input (data, variable)
	}
	descriptor.push(0xc0);

	let layout = parse(&descriptor);
	// Nothing decodable survives, and the parse returned rather than panicking.
	assert_eq!(layout.segs.len(), 0, "every segment is past the report bound");
	assert!(layout.report_bytes() <= MAX_REPORT_BYTES + 1, "and the report length stays bounded: {}", layout.report_bytes());
}

#[test]
// A SHORT REPORT MUST NOT LEAVE THE PREVIOUS ONE'S TAIL STANDING. A key whose usage lives in the
// dropped tail is otherwise never released - a key that sticks down after a truncated report, which
// is what a device sends when it is unplugged mid-transfer.
fn a_short_report_clears_the_tail_of_the_state_the_next_diff_runs_against() {
	let mut state: [u8; MAX_REPORT_BYTES as usize] = [0; MAX_REPORT_BYTES as usize];
	// A full boot-keyboard report with a key in its last slot.
	remember(&mut state, &[0x00, 0x00, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09]);
	assert_eq!(state[7], 0x09);

	// A truncated one: the tail must be gone, not kept.
	remember(&mut state, &[0x00, 0x00, 0x04]);
	assert_eq!(&state[..3], &[0x00, 0x00, 0x04]);
	assert!(state[3..].iter().all(|byte| *byte == 0), "the tail survived: {:?}", &state[3..10]);

	// And the release is what the diff then emits.
	let layout = boot_keyboard();
	let mut before: [u8; MAX_REPORT_BYTES as usize] = [0; MAX_REPORT_BYTES as usize];
	remember(&mut before, &[0x00, 0x00, 0x04, 0x05, 0x00, 0x00, 0x00, 0x00]);
	let mut after: [u8; MAX_REPORT_BYTES as usize] = [0; MAX_REPORT_BYTES as usize];
	remember(&mut after, &[0x00, 0x00, 0x04]);
	let mut events: Vec<(u32, bool)> = Vec::new();
	layout.keys_diff(0, &before, &after, &mut |usage, down| events.push((usage, down)));
	assert!(events.iter().any(|(usage, down)| *usage & 0xffff == 0x05 && !*down), "the key in the dropped tail was released: {events:?}");
	assert!(!events.iter().any(|(usage, _)| *usage & 0xffff == 0x04), "and the one still present did not move");
}

#[test]
// THE BOOT KEYBOARD IS PARSED BY THE PARSER ITSELF, so the fallback and the general path cannot
// disagree about the report they decode.
fn the_boot_keyboard_layout_decodes_its_own_report() {
	let layout = boot_keyboard();
	assert!(!layout.uses_ids());
	assert_eq!(layout.report_bytes(), 8, "one modifier byte, one pad and six key slots");

	let idle: [u8; MAX_REPORT_BYTES as usize] = [0; MAX_REPORT_BYTES as usize];
	let mut pressed: [u8; MAX_REPORT_BYTES as usize] = [0; MAX_REPORT_BYTES as usize];
	// Left shift held, and the usage for `a`.
	remember(&mut pressed, &[0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00]);
	let mut events: Vec<(u32, bool)> = Vec::new();
	layout.keys_diff(0, &idle, &pressed, &mut |usage, down| events.push((usage, down)));
	assert!(events.iter().any(|(usage, down)| *usage & 0xffff == 0xe1 && *down), "left shift went down: {events:?}");
	assert!(events.iter().any(|(usage, down)| *usage & 0xffff == 0x04 && *down), "and so did the letter");

	// THE ROLLOVER AND ERROR CODES ARE SKIPPED, because a keyboard that reports more keys than its
	// array holds fills every slot with 1 - and emitting that as a key press types a character the
	// person did not.
	let mut rollover: [u8; MAX_REPORT_BYTES as usize] = [0; MAX_REPORT_BYTES as usize];
	remember(&mut rollover, &[0x00, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01]);
	let mut events: Vec<(u32, bool)> = Vec::new();
	layout.keys_diff(0, &idle, &rollover, &mut |usage, down| events.push((usage, down)));
	assert!(events.is_empty(), "a rollover report is not six key presses: {events:?}");
}

#[test]
// A FIELD PAST THE END OF A REPORT READS AS ZERO rather than reading whatever follows the buffer,
// which is what lets a short report be diffed at all.
fn a_field_past_the_end_of_a_body_reads_as_zero() {
	let body: [u8; 2] = [0xff, 0xff];
	assert_eq!(field(&body, 0, 8), 0xff);
	assert_eq!(field(&body, 8, 8), 0xff);
	assert_eq!(field(&body, 16, 8), 0, "past the end");
	assert_eq!(field(&body, 4, 8), 0xff, "and one that straddles two bytes");
	// A field that straddles the end takes what exists and zeroes the rest.
	assert_eq!(field(&body, 12, 8), 0x0f);
	assert_eq!(signed_field(&body, 0, 8), -1, "sign extension over a full byte");
	assert_eq!(signed_field(&body, 0, 4), -1);
	assert_eq!(signed_field(&body, 16, 8), 0);
}

#[test]
// THE ARRAY INDEX ARITHMETIC IS IN `i64` AND CLAMPED. Both operands come from a descriptor the
// DEVICE wrote: a logical minimum of `i32::MIN` against a reading of zero overflows an `i32`
// subtraction, which panics in a debug build and wraps to a plausible-looking usage in a release
// one - a key press the person never made.
fn an_array_field_with_an_extreme_logical_minimum_does_not_overflow() {
	let mut descriptor: Vec<u8> = Vec::new();
	descriptor.extend_from_slice(&[0x05, 0x07]); // usage page (keyboard)
	descriptor.extend_from_slice(&[0xa1, 0x01]); // collection
	// Logical minimum `i32::MIN` and a maximum of 255, as a four-byte signed item.
	descriptor.extend_from_slice(&[0x17, 0x00, 0x00, 0x00, 0x80]);
	descriptor.extend_from_slice(&[0x26, 0xff, 0x00]);
	descriptor.extend_from_slice(&[0x19, 0x00, 0x29, 0xff]); // usage min 0, max 255
	descriptor.extend_from_slice(&[0x75, 0x08, 0x95, 0x06]); // six bytes
	descriptor.extend_from_slice(&[0x81, 0x00]); // input (data, array)
	descriptor.push(0xc0);

	let layout = parse(&descriptor);
	let idle: [u8; MAX_REPORT_BYTES as usize] = [0; MAX_REPORT_BYTES as usize];
	let mut report: [u8; MAX_REPORT_BYTES as usize] = [0; MAX_REPORT_BYTES as usize];
	remember(&mut report, &[0x00, 0x2a, 0x00, 0x00, 0x00, 0x00]);
	let mut events: Vec<(u32, bool)> = Vec::new();
	// The assertion is that this RETURNS. Whatever usages it decides on, it must not overflow on the
	// way - and under the old arithmetic it did.
	layout.keys_diff(0, &idle, &report, &mut |usage, down| events.push((usage, down)));
	assert!(events.iter().all(|(usage, _)| *usage != 0));
}

// A two-contact multi-touch descriptor, built the way a touch surface builds one: an application
// collection holding a contact count, then one LOGICAL COLLECTION PER FINGER, each with its own
// identifier, tip switch and pair of absolute axes.
fn touch_descriptor() -> Vec<u8> {
	let mut d: Vec<u8> = Vec::new();
	d.extend_from_slice(&[0x05, 0x0d]); // usage page (digitizer)
	d.extend_from_slice(&[0x09, 0x04]); // usage (touch screen)
	d.extend_from_slice(&[0xa1, 0x01]); // collection (application)
	d.extend_from_slice(&[0x09, 0x54]); //   usage (contact count)
	d.extend_from_slice(&[0x15, 0x00, 0x25, 0x0a]); //   logical 0..10
	d.extend_from_slice(&[0x75, 0x08, 0x95, 0x01]); //   8 bits x 1
	d.extend_from_slice(&[0x81, 0x02]); //   input (data, variable)
	for _ in 0..2 {
		d.extend_from_slice(&[0xa1, 0x02]); //   collection (logical) - one finger
		d.extend_from_slice(&[0x09, 0x51]); //     usage (contact identifier)
		d.extend_from_slice(&[0x15, 0x00, 0x25, 0x7f]);
		d.extend_from_slice(&[0x75, 0x08, 0x95, 0x01]);
		d.extend_from_slice(&[0x81, 0x02]);
		d.extend_from_slice(&[0x09, 0x42]); //     usage (tip switch)
		d.extend_from_slice(&[0x15, 0x00, 0x25, 0x01]);
		d.extend_from_slice(&[0x75, 0x08, 0x95, 0x01]);
		d.extend_from_slice(&[0x81, 0x02]);
		d.extend_from_slice(&[0x05, 0x01]); //     usage page (generic desktop)
		d.extend_from_slice(&[0x09, 0x30, 0x09, 0x31]); //     usage (x), usage (y)
		d.extend_from_slice(&[0x15, 0x00, 0x26, 0xff, 0x00]); //     logical 0..255
		d.extend_from_slice(&[0x75, 0x08, 0x95, 0x02]);
		d.extend_from_slice(&[0x81, 0x02]);
		d.extend_from_slice(&[0x05, 0x0d]); //     back to the digitizer page
		d.extend_from_slice(&[0xc0]); //   end collection
	}
	d.extend_from_slice(&[0xc0]); // end collection
	d
}

#[test]
fn a_digitizer_is_not_flattened_into_a_mouse() {
	// The parser kept segments on FOUR pages and dropped every other one before it was decoded -
	// which is what flattening a tablet into a pointer is here: not a lossy mapping downstream, but
	// a filter upstream that never let the fields exist.
	let layout = parse(&touch_descriptor());
	assert!(layout.has_digitizer(), "the digitizer page survives the parse");
	// And the collections were counted: application, then one per contact.
	assert_eq!(layout.collection_depth(), 2, "a finger's fields are one collection inside the application one");
}

#[test]
fn each_contact_identifier_begins_a_contact_and_its_axes_are_its_own() {
	// A reader that took every X on the page would give every finger the LAST one's position, which
	// is one pointer that jumps rather than two fingers.
	let layout = parse(&touch_descriptor());
	// count=2, then (id 7, tip 1, x 0x10, y 0x20), (id 9, tip 1, x 0xF0, y 0x80)
	let report = [2u8, 7, 1, 0x10, 0x20, 9, 1, 0xF0, 0x80];
	let mut out = [Contact::default(); MAX_CONTACTS];
	assert_eq!(layout.contacts(0, &report, &mut out), 2, "two contacts");
	assert_eq!(out[0].id, 7);
	assert_eq!(out[1].id, 9, "the identifiers are the device's own and they persist");
	assert!(out[0].tip && out[1].tip);
	// The axes are scaled into the same grid the pointer path uses, and they DIFFER.
	assert!(out[0].x < out[1].x, "each finger keeps its own X: {} then {}", out[0].x, out[1].x);
	assert!(out[0].y < out[1].y);
}

#[test]
fn contact_count_bounds_what_is_reported_so_an_untouched_slot_is_not_a_phantom_finger() {
	// A digitizer declares slots for every finger it can ever report and leaves the unused ones
	// holding whatever was there before. A reader that takes every declared slot reports contacts at
	// stale positions that nothing is touching - and the count, in the same report, is the answer.
	let layout = parse(&touch_descriptor());
	let report = [1u8, 7, 1, 0x10, 0x20, 9, 0, 0xF0, 0x80];
	let mut out = [Contact::default(); MAX_CONTACTS];
	assert_eq!(layout.contacts(0, &report, &mut out), 1, "one finger is down, whatever the second slot still holds");
	assert_eq!(out[0].id, 7);
	// AND A COUNT LARGER THAN THE REPORT CARRIES DOES NOT INVENT ONE.
	let lying = [9u8, 7, 1, 0x10, 0x20, 9, 1, 0xF0, 0x80];
	assert_eq!(layout.contacts(0, &lying, &mut out), 2, "the count bounds what was decoded and does not extend it");
}

#[test]
fn a_descriptor_that_never_closes_its_collections_is_bounded_rather_than_nesting_for_ever() {
	// `Collection` and `End Collection` are bytes the DEVICE chose, and nothing counted them at all.
	let mut d: Vec<u8> = Vec::new();
	d.extend_from_slice(&[0x05, 0x0d, 0x09, 0x04]);
	for _ in 0..64 {
		d.extend_from_slice(&[0xa1, 0x01]);
	}
	d.extend_from_slice(&[0x09, 0x51]);
	d.extend_from_slice(&[0x15, 0x00, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02]);
	let layout = parse(&d);
	assert!(layout.collection_depth() <= MAX_COLLECTION_DEPTH, "nothing past the bound is kept, and the depth says so");
	// The fields declared past the bound are dropped rather than the parse panicking or looping.
	assert!(!layout.has_digitizer(), "a field declared inside a runaway nesting is not a field this parser keeps");
}

#[test]
fn the_unit_exponent_is_a_signed_nibble_and_not_a_byte() {
	// 0x0F is MINUS ONE. Read as unsigned it scales a tablet's position by ten to the fifteenth,
	// and the number is small either way - which is what makes the mistake survive a glance.
	assert_eq!(nibble_exponent(0x00), 0);
	assert_eq!(nibble_exponent(0x07), 7);
	assert_eq!(nibble_exponent(0x08), -8, "eight is the first negative one");
	assert_eq!(nibble_exponent(0x0F), -1);
	// And the high bits of the item's data are not part of it.
	assert_eq!(nibble_exponent(0xFF0F), -1);
}

#[test]
fn a_declared_unit_is_readable_and_an_undeclared_one_says_so() {
	let mut d: Vec<u8> = Vec::new();
	d.extend_from_slice(&[0x05, 0x01, 0x09, 0x02, 0xa1, 0x01]);
	d.extend_from_slice(&[0x09, 0x30]);
	d.extend_from_slice(&[0x55, 0x0d]); // unit exponent -3
	d.extend_from_slice(&[0x65, 0x11]); // unit: centimetres
	d.extend_from_slice(&[0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02]);
	d.extend_from_slice(&[0xc0]);
	let layout = parse(&d);
	assert_eq!(layout.unit_for(PAGE_GENERIC_DESKTOP, 0x30), Some((0x11, -3)), "a tablet reports position IN A UNIT");
	// A device that declared none says none, which is different from declaring a dimensionless one.
	assert_eq!(parse(&boot_keyboard_descriptor()).unit_for(PAGE_GENERIC_DESKTOP, 0x30), None);
}

// The boot keyboard's own descriptor, for the regression bar the item states: what the keyboard and
// pointer paths already produce must not change.
fn boot_keyboard_descriptor() -> Vec<u8> {
	alloc::vec![
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
		0x01,
		0x75,
		0x08,
		0x81,
		0x03,
		0x05,
		0x07,
		0x19,
		0x00,
		0x29,
		0x65,
		0x15,
		0x00,
		0x25,
		0x65,
		0x75,
		0x08,
		0x95,
		0x06,
		0x81,
		0x00,
		0xc0,
	]
}

#[test]
fn the_keyboard_path_is_unchanged_by_everything_above() {
	// The item's own regression bar, asserted rather than assumed.
	let layout = parse(&boot_keyboard_descriptor());
	assert!(layout.has_keyboard());
	assert!(!layout.has_pointer());
	assert!(!layout.has_digitizer());
	let mut pressed: Vec<u32> = Vec::new();
	layout.keys_diff(0, &[0u8; 8], &[0x02, 0, 0x04, 0, 0, 0, 0, 0], &mut |usage, down| {
		if down {
			pressed.push(usage);
		}
	});
	assert!(pressed.contains(&((PAGE_KEYBOARD as u32) << 16 | 0xe1)), "left shift, from the modifier bitmap");
	assert!(pressed.contains(&((PAGE_KEYBOARD as u32) << 16 | 0x04)), "and the key in the array");
}

#[test]
fn an_absolute_axis_is_read_in_the_signedness_its_descriptor_declared() {
	// A NON-NEGATIVE LOGICAL MINIMUM MEANS UNSIGNED. Read signed, an eight-bit axis over 0..255
	// reports MINUS SIXTEEN for 0xF0 - a touch near the right-hand edge - and `scale` clamps that to
	// the LEFT edge. The right-hand half of the surface reads as the left one, and nothing refuses
	// anything.
	let layout = parse(&touch_descriptor());
	let far_right = [1u8, 3, 1, 0xF0, 0xF0];
	let mut out = [Contact::default(); MAX_CONTACTS];
	assert_eq!(layout.contacts(0, &far_right, &mut out), 1);
	assert!(out[0].x > NORM_MAX / 2, "0xF0 of 0..255 is most of the way across, not the left edge: {}", out[0].x);
	assert!(out[0].y > NORM_MAX / 2);

	// AND A SIGNED AXIS IS STILL SIGNED. A descriptor whose logical minimum is negative means what
	// it says, and reading that unsigned would put every negative reading at the far end.
	let mut d: Vec<u8> = Vec::new();
	d.extend_from_slice(&[0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x30]);
	d.extend_from_slice(&[0x15, 0x81, 0x25, 0x7f]); // logical -127..127
	d.extend_from_slice(&[0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0xc0]);
	let signed = parse(&d);
	let (mut x, mut y, mut buttons, mut wheel) = (0i32, 0i32, 0u8, 0i32);
	assert!(signed.pointer_fold(0, &[0xF0], &mut x, &mut y, &mut buttons, &mut wheel));
	assert!(x < NORM_MAX / 2, "0xF0 of -127..127 is minus sixteen, near the low end: {x}");
}

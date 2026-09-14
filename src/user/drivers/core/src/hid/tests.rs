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

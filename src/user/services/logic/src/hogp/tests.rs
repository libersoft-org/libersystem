use super::{BUTTON_LEFT, BUTTON_MASK, BUTTON_MIDDLE, BUTTON_RIGHT, MAX_REPORT, Mode, ModeAction, NOTIFICATIONS_OFF, NOTIFICATIONS_ON, Refusal, Report, decodable, mode, report};

#[test]
// THE DISPLACEMENTS ARE SIGNED, AND THIS IS THE DEFECT THE MODULE IS WRITTEN AGAINST. Read
// unsigned, one pixel to the LEFT is 0xff - two hundred and fifty-five to the right - so a mouse
// pushed left walks to the right edge and stays there with no error anywhere.
fn a_leftward_move_is_minus_one_and_not_two_hundred_and_fifty_five() {
	assert_eq!(report(&[0, 0xff, 0xff]), Ok(Report { buttons: 0, dx: -1, dy: -1, wheel: 0 }));
	assert_eq!(report(&[0, 0x01, 0x01]), Ok(Report { buttons: 0, dx: 1, dy: 1, wheel: 0 }));
	assert_eq!(report(&[0, 0x80, 0x7f]), Ok(Report { buttons: 0, dx: -128, dy: 127, wheel: 0 }), "the ends of the signed range");
	// And the wheel, which is the same mistake one byte further along: a scroll up is negative.
	assert_eq!(report(&[0, 0, 0, 0xff]), Ok(Report { buttons: 0, dx: 0, dy: 0, wheel: -1 }));
	assert_eq!(report(&[0, 0, 0, 0x01]), Ok(Report { buttons: 0, dx: 0, dy: 0, wheel: 1 }));
	assert_eq!(report(&[0, 0, 0]).map(|r| r.wheel), Ok(0), "the three-byte form has no wheel and reports none");
}

#[test]
// THREE BITS AND NOT A BYTE. A host taking the whole first byte as a button mask would report
// buttons four through eight whenever a device set a reserved bit - which several do.
fn the_buttons_are_the_three_defined_bits_and_the_reserved_ones_are_not_buttons() {
	assert_eq!(report(&[BUTTON_LEFT, 0, 0]).map(|r| r.buttons), Ok(BUTTON_LEFT));
	assert_eq!(report(&[BUTTON_LEFT | BUTTON_RIGHT | BUTTON_MIDDLE, 0, 0]).map(|r| r.buttons), Ok(BUTTON_MASK));
	assert_eq!(report(&[0xff, 0, 0]).map(|r| r.buttons), Ok(BUTTON_MASK), "the reserved bits above are not buttons");
	assert_eq!(report(&[0xf8, 0, 0]).map(|r| r.buttons), Ok(0), "and a report that sets only reserved bits holds no button");
}

#[test]
// A DEVICE SENDING FIVE BYTES IS NOT IN BOOT MODE, whatever its protocol-mode characteristic says.
// Decoding the first four would be reading a report map this milestone does not parse as though it
// were the fixed layout that predates one.
fn a_report_that_is_not_the_boot_layout_is_refused_rather_than_decoded_from_its_first_bytes() {
	assert_eq!(report(&[]), Err(Refusal::Short { len: 0 }));
	assert_eq!(report(&[0, 0]), Err(Refusal::Short { len: 2 }));
	assert_eq!(report(&[0, 0, 0, 0, 0]), Err(Refusal::NotBootLayout { len: 5 }));
	assert_eq!(report(&[0u8; 64]), Err(Refusal::NotBootLayout { len: 64 }));
	assert!(report(&[0, 0, 0]).is_ok() && report(&[0; MAX_REPORT]).is_ok(), "three and four are the layout");
}

#[test]
// A MODE IS ONE BYTE AND A LONGER VALUE IS NOT A LONGER MODE. A device answering with two bytes has
// answered with something else, and taking the first would read a field with no basis for meaning.
fn the_protocol_mode_is_one_defined_byte_and_says_what_to_do_about_itself() {
	assert_eq!(mode(&[0]), Ok(ModeAction::Ready));
	assert_eq!(mode(&[1]), Ok(ModeAction::SelectBoot), "a device in report mode is asked to change");
	assert_eq!(mode(&[2]), Err(Refusal::UnknownMode(2)));
	assert_eq!(mode(&[0xff]), Err(Refusal::UnknownMode(0xff)));
	assert_eq!(mode(&[]), Err(Refusal::Short { len: 0 }));
	assert_eq!(mode(&[0, 0]), Err(Refusal::Short { len: 2 }), "two bytes is not a mode");
	assert_eq!(Mode::from_byte(0).map(Mode::byte), Some(0));
	assert_eq!(Mode::from_byte(1).map(Mode::byte), Some(1));
}

#[test]
// A DEVICE THAT CHANGED MODE WITHOUT BEING ASKED ARRIVES AS A REPORT, not as a mode read - so
// whether a report may be decoded at all is a separate question from what to do about a mode.
fn a_device_in_report_mode_is_a_typed_refusal_and_not_a_decode() {
	assert_eq!(decodable(Mode::Boot), Ok(()));
	assert_eq!(decodable(Mode::Report), Err(Refusal::ReportMode));
}

#[test]
// BIT ONE IS INDICATIONS AND THIS PROFILE MUST NOT SET IT. An indication is acknowledged, and a
// device waiting for an acknowledgement this client never sends stops sending reports - which reads
// as a mouse that worked once and then went quiet.
fn the_configuration_value_turns_on_notifications_and_not_indications() {
	assert_eq!(NOTIFICATIONS_ON, [0x01, 0x00]);
	assert_eq!(NOTIFICATIONS_ON[0] & 0x02, 0, "indications are not set");
	assert_eq!(NOTIFICATIONS_OFF, [0x00, 0x00]);
}

#[test]
// THE MOUSE REPORTS MOTION AND THE CONSUMER READS A POSITION, and the fold is the drivers' own:
// saturating then clamped, so a very large move reaches the edge rather than wrapping to the other one.
fn motion_folds_onto_a_position_that_stops_at_the_edges() {
	use super::{NORMALISED_MAX, Pointer, RELATIVE_RANGE};
	let mut pointer = Pointer::new();
	let (x0, y0) = pointer.fold(0, 0);
	let (x1, _) = pointer.fold(10, 0);
	assert!(x1 > x0, "a move to the right moves right");
	let (x2, _) = pointer.fold(-20, 0);
	assert!(x2 < x0, "and a move to the left moves left - the sign survives the fold");
	assert_eq!(pointer.fold(i32::MAX, i32::MAX), (NORMALISED_MAX as u16, NORMALISED_MAX as u16), "the far edge, not a wrap");
	assert_eq!(pointer.fold(i32::MIN, i32::MIN), (0, 0), "and the near one");
	// Half the range is half the screen, to within the one unit integer division rounds away: the
	// range is odd, so its half is not a whole number of steps.
	let half = pointer.fold(RELATIVE_RANGE / 2, 0).0 as i64;
	assert!((half - (NORMALISED_MAX / 2) as i64).abs() <= 1, "half the range is half the screen, got {half}");
	let _ = y0;
}

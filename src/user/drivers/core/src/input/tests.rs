// The input driver's decoding, held against the device's own numbers.
use super::{AXIS_X, AXIS_Y, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, EV_ABS, EV_KEY, EV_REL, EV_SYN, NORM_MAX, Pointer, REL_RANGE, REL_WHEEL, axis_bound, axis_max, fold, normalize};

#[test]
// THE AXIS RANGE IS THE DEVICE'S CLAIM AND THE POINTER'S WHOLE COORDINATE SYSTEM. A block shorter
// than the field does not contain it, and a maximum that is zero or negative once it is signed is not
// a range - the clamp becomes an empty interval and the pointer sits in the corner whatever the
// device reports.
fn an_absolute_axis_range_is_believed_only_when_it_is_one() {
	assert_eq!(axis_max(20, 1919), Some(1919));
	assert_eq!(axis_max(7, 1919), None, "a block too short to hold the field does not hold it");
	assert_eq!(axis_max(20, 0), None, "a range of zero is not a range");
	assert_eq!(axis_max(20, 0x8000_0000), None, "and one that is negative when signed is not either");
	// The fallback is the relative range, which is what a mouse gets - it reports no axis block.
	assert_eq!(axis_bound(None), REL_RANGE);
	assert_eq!(axis_bound(Some(1919)), 1919);
}

#[test]
// AN ABSOLUTE AXIS SETS AND A RELATIVE ONE NUDGES, and both clamp: a tablet that reports a position
// past its own maximum, and a mouse pushed against the edge, both stop at the edge rather than
// wrapping into the opposite corner.
fn absolute_and_relative_motion_fold_the_way_each_is_defined() {
	let mut state = Pointer::default();
	let mut wheel = 0;
	assert!(!fold(&mut state, &mut wheel, EV_ABS, AXIS_X, 100, 1919, 1079));
	assert!(!fold(&mut state, &mut wheel, EV_ABS, AXIS_Y, 50, 1919, 1079));
	assert_eq!((state.x, state.y), (100, 50));
	// Past the maximum is the maximum, and below zero is zero.
	fold(&mut state, &mut wheel, EV_ABS, AXIS_X, 5000, 1919, 1079);
	assert_eq!(state.x, 1919);
	fold(&mut state, &mut wheel, EV_ABS, AXIS_X, -5, 1919, 1079);
	assert_eq!(state.x, 0);
	// A relative nudge adds to what is there, and saturates rather than wrapping: two billion to the
	// right is the right edge, not the left one.
	fold(&mut state, &mut wheel, EV_REL, AXIS_X, 10, 1919, 1079);
	assert_eq!(state.x, 10);
	fold(&mut state, &mut wheel, EV_REL, AXIS_X, i32::MAX, 1919, 1079);
	assert_eq!(state.x, 1919);
	fold(&mut state, &mut wheel, EV_REL, AXIS_Y, i32::MIN, 1919, 1079);
	assert_eq!(state.y, 0);
}

#[test]
// A BUTTON IS A BIT, AND A WHEEL IS A MOMENTARY DELTA that is not part of the held state - which is
// why it is accumulated separately and reset by the sender rather than living in the pointer.
fn buttons_are_bits_and_the_wheel_is_a_delta() {
	let mut state = Pointer::default();
	let mut wheel = 0;
	fold(&mut state, &mut wheel, EV_KEY, BTN_LEFT, 1, 100, 100);
	fold(&mut state, &mut wheel, EV_KEY, BTN_RIGHT, 1, 100, 100);
	assert_eq!(state.buttons, 3);
	fold(&mut state, &mut wheel, EV_KEY, BTN_LEFT, 0, 100, 100);
	assert_eq!(state.buttons, 2, "releasing one button leaves the other held");
	fold(&mut state, &mut wheel, EV_KEY, BTN_MIDDLE, 1, 100, 100);
	assert_eq!(state.buttons, 6);
	// An unknown key code changes nothing - a keyboard sharing an event queue must not move the
	// pointer's buttons.
	fold(&mut state, &mut wheel, EV_KEY, 0x1e, 1, 100, 100);
	assert_eq!(state.buttons, 6);

	fold(&mut state, &mut wheel, EV_REL, REL_WHEEL, 2, 100, 100);
	fold(&mut state, &mut wheel, EV_REL, REL_WHEEL, -1, 100, 100);
	assert_eq!(wheel, 1);
	fold(&mut state, &mut wheel, EV_REL, REL_WHEEL, i32::MAX, 100, 100);
	assert_eq!(wheel, i32::MAX, "a storm of ticks saturates rather than wrapping into a scroll the other way");

	// AND `EV_SYN` IS THE ONLY EVENT THAT ENDS A FRAME.
	assert!(fold(&mut state, &mut wheel, EV_SYN, 0, 0, 100, 100));
	assert!(!fold(&mut state, &mut wheel, 99, 0, 0, 100, 100), "an event type this driver does not read changes nothing");
}

#[test]
// THE NORMALISATION IS A DIVISION BY THE DEVICE'S OWN NUMBER, so the range that is not a range has to
// answer rather than divide.
fn a_position_normalises_onto_the_range_a_consumer_receives() {
	assert_eq!(normalize(0, 1919), 0);
	assert_eq!(normalize(1919, 1919), NORM_MAX);
	assert_eq!(normalize(960, 1919), (960u64 * NORM_MAX as u64 / 1919) as u16);
	assert_eq!(normalize(5000, 1919), NORM_MAX, "past the maximum is the maximum");
	assert_eq!(normalize(-5, 1919), 0);
	assert_eq!(normalize(100, 0), 0, "a range of zero answers zero rather than dividing by it");
	assert_eq!(normalize(100, -1), 0);
}

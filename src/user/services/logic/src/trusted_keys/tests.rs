use super::*;

#[test]
fn the_chord_is_ctrl_alt_f12_and_nothing_else() {
	let mut watch = Watch::new();
	assert_eq!(watch.record(F12, true), Seen::Key { usage: F12, down: true }, "F12 alone");
	watch.record(F12, false);
	watch.record(LEFT_CTRL, true);
	assert_eq!(watch.record(F12, true), Seen::Key { usage: F12, down: true }, "Ctrl+F12");
	watch.record(F12, false);
	watch.record(RIGHT_ALT, true);
	assert_eq!(watch.record(F12, true), Seen::Attention, "Ctrl+Alt+F12, either side");
	assert_eq!(watch.record(F12, true), Seen::Nothing, "a repeat is not a second chord");
	let mut other = Watch::new();
	other.record(LEFT_CTRL, true);
	other.record(LEFT_ALT, true);
	assert_ne!(other.record(ESCAPE, true), Seen::Attention, "Ctrl+Alt+Escape is not secure attention");
	assert_eq!(other.record(ENTER, false), Seen::Nothing, "a release with no press behind it");
}

#[test]
fn a_session_arms_only_once_every_key_is_released() {
	let mut watch = Watch::new();
	watch.record(LEFT_CTRL, true);
	watch.record(LEFT_ALT, true);
	watch.record(F12, true);
	let mut arming = Arming::arm(7, &watch);
	assert_eq!(arming.armed(), None, "the chord is still held");
	for key in [F12, LEFT_ALT] {
		watch.record(key, false);
		assert_eq!(arming.settle(&watch), None);
	}
	watch.record(LEFT_CTRL, false);
	assert_eq!(arming.settle(&watch), Some(7));
	assert_eq!(arming.armed(), Some(7));
	assert_eq!(Arming::arm(8, &Watch::new()).armed(), Some(8), "nothing held: armed at once");
}

#[test]
fn only_a_fresh_enter_under_the_confirmed_epoch_approves() {
	let mut decision = Decision::new(5);
	assert_eq!(decision.key(5, ENTER, true), None);
	assert_eq!(decision.key(5, ENTER, false), None, "neither the display nor the keyboard confirmed the epoch");
	decision.armed(5);
	assert_eq!(decision.key(5, ENTER, false), None, "the display has not confirmed it");
	decision.presented(4);
	assert!(!decision.ready(), "another epoch's presentation");
	decision.presented(5);
	assert_eq!(decision.key(5, ENTER, false), None, "a release with no press after arming");
	assert_eq!(decision.key(4, ENTER, true), None, "a stale epoch");
	assert_eq!(decision.key(4, ENTER, false), None);
	decision.key(5, ENTER, true);
	decision.key(5, 0x04, true);
	assert_eq!(decision.key(5, ENTER, false), None, "a key pressed in between spoils the Enter");
	decision.key(5, ENTER, true);
	assert_eq!(decision.key(5, ENTER, false), Some(Verdict::Approve));
	let mut refusing = Decision::new(9);
	refusing.armed(9);
	refusing.presented(9);
	assert_eq!(refusing.key(9, ESCAPE, true), Some(Verdict::Decline));
}

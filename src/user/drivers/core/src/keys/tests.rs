// The console key fixtures. The modifier state is what the whole cooked path reads, so its rules are
// held here rather than being inferred from what a terminal happened to print.

use super::*;

#[test]
// EACH SIDE OF A MODIFIER IS TRACKED SEPARATELY. A keyboard has two Shift keys, and folding them
// into one boolean means releasing the right one cancels a left one that is still held - which is
// the defect that drops the capital letter out of the middle of a word typed with both hands, and
// which nobody reproduces on purpose because it needs two keys in a particular order.
fn releasing_one_shift_does_not_cancel_the_other() {
	let mut mods = Mods::default();
	feed_key(KEY_LEFTSHIFT, 1, &mut mods);
	assert!(mods.shift);
	feed_key(KEY_RIGHTSHIFT, 1, &mut mods);
	assert!(mods.shift, "both held");
	feed_key(KEY_RIGHTSHIFT, 0, &mut mods);
	assert!(mods.shift, "the left one is still down");
	feed_key(KEY_LEFTSHIFT, 0, &mut mods);
	assert!(!mods.shift, "and now neither is");

	// The same for every other modifier that has two of them.
	for (left, right) in [(KEY_LEFTCTRL, KEY_RIGHTCTRL), (KEY_LEFTALT, KEY_RIGHTALT), (KEY_LEFTMETA, KEY_RIGHTMETA)] {
		let mut mods = Mods::default();
		feed_key(left, 1, &mut mods);
		feed_key(right, 1, &mut mods);
		feed_key(left, 0, &mut mods);
		let held = match left {
			KEY_LEFTCTRL => mods.ctrl,
			KEY_LEFTALT => mods.alt,
			_ => mods.meta,
		};
		assert!(held, "releasing the left one cancelled the right");
		feed_key(right, 0, &mut mods);
		let held = match left {
			KEY_LEFTCTRL => mods.ctrl,
			KEY_LEFTALT => mods.alt,
			_ => mods.meta,
		};
		assert!(!held);
	}
}

#[test]
// AN UNPLUGGED DEVICE LEAVES NOTHING HELD. A key that is down when its device goes away stays down
// for ever otherwise, and the next thing typed arrives shifted.
fn releasing_everything_clears_every_side() {
	let mut mods = Mods::default();
	feed_key(KEY_LEFTSHIFT, 1, &mut mods);
	feed_key(KEY_RIGHTCTRL, 1, &mut mods);
	feed_key(KEY_LEFTALT, 1, &mut mods);
	assert!(mods.shift && mods.ctrl && mods.alt);
	assert_ne!(mods.held_sides(), 0);
	mods.release_all();
	assert!(!mods.shift && !mods.ctrl && !mods.alt && !mods.meta);
	assert_eq!(mods.held_sides(), 0);
	// The LOCK keys are not modifiers and are not cleared: Caps Lock survives a device going away,
	// which is what a person expects from a lock.
	feed_key(KEY_CAPSLOCK, 1, &mut mods);
	let locked = mods.caps;
	mods.release_all();
	assert_eq!(mods.caps, locked);
}

#[test]
// A LOCK KEY TOGGLES ON PRESS AND IGNORES ITS RELEASE, which is what makes it a lock rather than a
// modifier - and an autorepeat must not toggle it again, or holding it flickers.
fn a_lock_key_toggles_once_per_press() {
	let mut mods = Mods::default();
	let before = mods.caps;
	feed_key(KEY_CAPSLOCK, 1, &mut mods);
	assert_ne!(mods.caps, before, "the press toggled it");
	feed_key(KEY_CAPSLOCK, 0, &mut mods);
	assert_ne!(mods.caps, before, "the release did not");
	feed_key(KEY_CAPSLOCK, 1, &mut mods);
	assert_eq!(mods.caps, before, "and the next press toggled it back");
}

// Shared keyboard-input logic for the userspace input drivers.
//
// Both keyboard drivers - virtio-input and the xHCI USB HID
// keyboard (HID usages translated to the same keycodes) - feed the interactive
// console through this one module: it tracks the modifier state across press /
// release events, applies the US layout (Shift / Caps / Ctrl / Alt), turns the
// navigation keys into their ANSI escape sequences, handles the Ctrl+Alt+Delete
// reboot chord, and injects the resulting bytes with `console_feed`. Keeping it
// shared means a key behaves identically no matter which keyboard produced it.

#![allow(dead_code)]

use rt::*;

// Keycodes for the navigation keys (all above the 64-entry ASCII KEYMAP), which
// the driver turns into ANSI escape sequences rather than glyphs.
pub const KEY_HOME: u16 = 102;
pub const KEY_UP: u16 = 103;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_END: u16 = 107;
pub const KEY_DOWN: u16 = 108;
pub const KEY_DELETE: u16 = 111;
pub const KEY_PAGEUP: u16 = 104;
pub const KEY_PAGEDOWN: u16 = 109;

// Keycodes for the modifier keys (tracked across press/release, not emitted).
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_CAPSLOCK: u16 = 58;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_RIGHTALT: u16 = 100;

// Input keycode -> ASCII for the unshifted main block: the letter keys
// (lowercase), the digit row, and the few control keys a line shell needs. 0 means
// "no character" (modifiers, function keys, unmapped). Indices are KEY_* codes, e.g.
// KEY_A = 30 -> 'a', KEY_ENTER = 28 -> '\n', KEY_BACKSPACE = 14 -> 0x08.
#[rustfmt::skip]
const KEYMAP: [u8; 64] = [
	0,    0,    b'1', b'2',  b'3',  b'4', b'5', b'6', b'7', b'8',
	b'9', b'0', b'-', b'=',  0x08,  0x09, b'q', b'w', b'e', b'r',
	b't', b'y', b'u', b'i',  b'o',  b'p', b'[', b']', b'\n', 0,
	b'a', b's', b'd', b'f',  b'g',  b'h', b'j', b'k', b'l', b';',
	b'\'', b'`', 0,   b'\\', b'z',  b'x', b'c', b'v', b'b', b'n',
	b'm', b',', b'.', b'/',  0,     b'*', 0,    b' ', 0,    0,
	0,    0,    0,    0,
];

// The shifted layout (US): the same indices as KEYMAP, with Shift applied - capitals
// for letters and the shifted symbols for the digit row and punctuation. Caps Lock
// flips only the letters (handled in `layout`).
#[rustfmt::skip]
const KEYMAP_SHIFT: [u8; 64] = [
	0,    0,    b'!', b'@',  b'#',  b'$', b'%', b'^', b'&', b'*',
	b'(', b')', b'_', b'+',  0x08,  0x09, b'Q', b'W', b'E', b'R',
	b'T', b'Y', b'U', b'I',  b'O',  b'P', b'{', b'}', b'\n', 0,
	b'A', b'S', b'D', b'F',  b'G',  b'H', b'J', b'K', b'L', b':',
	b'"', b'~', 0,   b'|',  b'Z',  b'X', b'C', b'V', b'B', b'N',
	b'M', b'<', b'>', b'?',  0,     b'*', 0,    b' ', 0,    0,
	0,    0,    0,    0,
];

// The live modifier state, tracked across key press / release events.
#[derive(Default)]
pub struct Mods {
	pub shift: bool,
	pub ctrl: bool,
	pub alt: bool,
	pub caps: bool,
}

// Feed one key event (a keycode and its value: 1 = press, 2 = autorepeat,
// 0 = release) into the console. Modifier keys update `mods` (tracked across press
// and release); an ordinary key press / autorepeat is turned into a character
// through the layout (Shift / Caps / Ctrl / Alt applied) and fed to the console,
// and a navigation key into its ANSI escape sequence.
pub unsafe fn feed_key(code: u16, value: u32, mods: &mut Mods) {
	unsafe {
		// Modifier keys track press (1) / release (0); Caps Lock toggles on press. They
		// emit no character, so handle them before the press-only gate below.
		match code {
			KEY_LEFTSHIFT | KEY_RIGHTSHIFT => {
				mods.shift = value != 0;
				return;
			}
			KEY_LEFTCTRL | KEY_RIGHTCTRL => {
				mods.ctrl = value != 0;
				return;
			}
			KEY_LEFTALT | KEY_RIGHTALT => {
				mods.alt = value != 0;
				return;
			}
			KEY_CAPSLOCK => {
				if value == 1 {
					mods.caps = !mods.caps;
				}
				return;
			}
			_ => {}
		}
		// 1 = press, 2 = autorepeat (both emit); 0 = release is ignored.
		if value != 1 && value != 2 {
			return;
		}
		// Ctrl+Alt+Delete is the reboot chord: a Delete press while both Ctrl and Alt are
		// held reboots the machine. The keyboard is interrupt-driven, so it fires and
		// interrupts whatever userspace is doing, even if the shell is wedged.
		if code == KEY_DELETE && value == 1 && mods.ctrl && mods.alt {
			system_power(POWER_REBOOT);
		}
		// PageUp / PageDown: Shift pages the console's own scrollback (a private control
		// byte the console intercepts); unshifted sends the standard ANSI sequence to the
		// client. Collapsing the chord here means the console needs no input escape parser.
		if code == KEY_PAGEUP || code == KEY_PAGEDOWN {
			if mods.shift {
				console_feed(if code == KEY_PAGEUP { 0x1e } else { 0x1f });
			} else {
				let seq: &[u8] = if code == KEY_PAGEUP { b"\x1b[5~" } else { b"\x1b[6~" };
				for &b in seq {
					console_feed(b);
				}
			}
			return;
		}
		// Navigation keys (arrows / Home / End / Delete) carry no ASCII glyph; emit the
		// ANSI escape sequence a serial terminal sends for them, so the shell's line
		// editor decodes the framebuffer keyboard and a serial terminal identically.
		if let Some(seq) = nav_sequence(code) {
			for &b in seq {
				console_feed(b);
			}
			return;
		}
		if code >= 64 {
			return;
		}
		let ch: u8 = layout(code, mods);
		if ch != 0 {
			// Alt makes the key a "meta" key: prefix the byte with ESC, the convention a
			// serial terminal uses (Alt+x -> ESC x).
			if mods.alt {
				console_feed(0x1b);
			}
			console_feed(ch);
		}
	}
}

// Resolve a main-block keycode to a character given the modifier state: Ctrl maps a
// letter (and `[ \ ]`) to its control code; otherwise Shift (and, for letters, Caps
// Lock) selects the shifted layout. Returns 0 for an unmapped key.
fn layout(code: u16, mods: &Mods) -> u8 {
	let base: u8 = KEYMAP[code as usize];
	if base == 0 {
		return 0;
	}
	if mods.ctrl {
		// Ctrl + letter -> 0x01..0x1a; Ctrl + [ \ ] -> 0x1b..0x1d; other combos ignored.
		let upper: u8 = base.to_ascii_uppercase();
		if upper.is_ascii_uppercase() {
			return upper - b'A' + 1;
		}
		return match base {
			b'[' => 0x1b,
			b'\\' => 0x1c,
			b']' => 0x1d,
			_ => 0,
		};
	}
	// Letters flip with Shift XOR Caps Lock; symbols only with Shift.
	let shifted: bool = if base.is_ascii_lowercase() { mods.shift ^ mods.caps } else { mods.shift };
	if shifted { KEYMAP_SHIFT[code as usize] } else { base }
}

// The ANSI escape sequence a navigation keycode maps to, or None for an ordinary key.
// These are the standard xterm sequences (ESC [ A/B/C/D for the arrows, ESC [ H / F
// for Home / End, ESC [ 3 ~ for Delete) the shell's editor already understands.
fn nav_sequence(code: u16) -> Option<&'static [u8]> {
	match code {
		KEY_UP => Some(b"\x1b[A"),
		KEY_DOWN => Some(b"\x1b[B"),
		KEY_RIGHT => Some(b"\x1b[C"),
		KEY_LEFT => Some(b"\x1b[D"),
		KEY_HOME => Some(b"\x1b[H"),
		KEY_END => Some(b"\x1b[F"),
		KEY_DELETE => Some(b"\x1b[3~"),
		_ => None,
	}
}

// HID keyboard-page usage -> keycode, for the boot-protocol report a USB
// keyboard sends: the letters (0x04..), the digit row, the control and punctuation
// keys, and the navigation block, all onto the same keycodes the virtio-input
// KEYMAP above indexes. 0 = unmapped (function keys, keypad).
#[rustfmt::skip]
const HID_KEYCODES: [u16; 0x53] = [
	0,   0,   0,   0,   30,  48,  46,  32,  18,  33,  // 00: -, -, -, -, a, b, c, d, e, f
	34,  35,  23,  36,  37,  38,  50,  49,  24,  25,  // 0a: g, h, i, j, k, l, m, n, o, p
	16,  19,  31,  20,  22,  47,  17,  45,  21,  44,  // 14: q, r, s, t, u, v, w, x, y, z
	2,   3,   4,   5,   6,   7,   8,   9,   10,  11,  // 1e: 1, 2, 3, 4, 5, 6, 7, 8, 9, 0
	28,  1,   14,  15,  57,  12,  13,  26,  27,  43,  // 28: enter, esc, bksp, tab, space, -, =, [, ], backslash
	0,   39,  40,  41,  51,  52,  53,  58,  0,   0,   // 32: -, ;, ', `, comma, ., /, caps, f1, f2
	0,   0,   0,   0,   0,   0,   0,   0,   0,   0,   // 3c: f3..f12
	0,   0,   0,   0,   102, 104, 111, 107, 109, 106, // 46: prtsc, scroll, pause, ins, home, pgup, del, end, pgdn, right
	105, 108, 103,                                    // 50: left, down, up
];

// Resolve a HID keyboard-page usage id to its keycode (0 = unmapped).
pub fn hid_keycode(usage: u8) -> u16 {
	if (usage as usize) < HID_KEYCODES.len() { HID_KEYCODES[usage as usize] } else { 0 }
}

// The keycode of each HID boot-report modifier bit (byte 0, bits 0..7):
// LCtrl, LShift, LAlt, LGui, RCtrl, RShift, RAlt, RGui. The GUI keys carry no
// keycode here (0 = ignored).
pub const HID_MODIFIER_KEYCODES: [u16; 8] = [KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_LEFTALT, 0, KEY_RIGHTCTRL, KEY_RIGHTSHIFT, KEY_RIGHTALT, 0];

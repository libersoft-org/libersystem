// Shared keyboard-input logic for the userspace input drivers.
//
// Both keyboard drivers - virtio-input and the xHCI USB HID
// keyboard (HID usages translated to the same keycodes) - feed the interactive
// console through this one module: it tracks the modifier and lock state across
// press / release events (Shift / Ctrl / Alt / the Windows meta keys, Caps / Num /
// Scroll Lock), applies the US layout, turns the navigation, edit and function keys
// into their ANSI escape sequences, runs the keypad in digit or navigation mode by
// NumLock, handles the Ctrl+Alt+Delete reboot chord and the Power key, recognizes
// the system / media / launcher keys (reserved until their subsystem exists), and
// injects the resulting bytes with `console_feed`. Keeping it shared means a key
// behaves identically no matter which keyboard produced it.

#![allow(dead_code)]

use rt::*;

// Keycodes for the navigation and edit keys (all above the 64-entry ASCII KEYMAP),
// which the driver turns into ANSI escape sequences rather than glyphs.
pub const KEY_HOME: u16 = 102;
pub const KEY_UP: u16 = 103;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_END: u16 = 107;
pub const KEY_DOWN: u16 = 108;
pub const KEY_DELETE: u16 = 111;
pub const KEY_PAGEUP: u16 = 104;
pub const KEY_PAGEDOWN: u16 = 109;
pub const KEY_INSERT: u16 = 110;

// Keycodes for the function keys (F11 / F12 sit apart from the F1..F10 run;
// F13..F24 are the extended row professional and legacy UNIX keyboards carry).
pub const KEY_F1: u16 = 59;
pub const KEY_F2: u16 = 60;
pub const KEY_F3: u16 = 61;
pub const KEY_F4: u16 = 62;
pub const KEY_F5: u16 = 63;
pub const KEY_F6: u16 = 64;
pub const KEY_F7: u16 = 65;
pub const KEY_F8: u16 = 66;
pub const KEY_F9: u16 = 67;
pub const KEY_F10: u16 = 68;
pub const KEY_F11: u16 = 87;
pub const KEY_F12: u16 = 88;
pub const KEY_F13: u16 = 183;
pub const KEY_F14: u16 = 184;
pub const KEY_F15: u16 = 185;
pub const KEY_F16: u16 = 186;
pub const KEY_F17: u16 = 187;
pub const KEY_F18: u16 = 188;
pub const KEY_F19: u16 = 189;
pub const KEY_F20: u16 = 190;
pub const KEY_F21: u16 = 191;
pub const KEY_F22: u16 = 192;
pub const KEY_F23: u16 = 193;
pub const KEY_F24: u16 = 194;

// Keycodes for the keypad block: the operator keys, KP Enter, and the digit /
// navigation island KP7..KP0 + KP dot (which NumLock switches between digits and
// navigation). KP* (55) sits inside the main KEYMAP; KP = and KP , appear on
// extended and Brazilian keypads.
pub const KEY_KPASTERISK: u16 = 55;
pub const KEY_KP7: u16 = 71;
pub const KEY_KP8: u16 = 72;
pub const KEY_KP9: u16 = 73;
pub const KEY_KPMINUS: u16 = 74;
pub const KEY_KP4: u16 = 75;
pub const KEY_KP5: u16 = 76;
pub const KEY_KP6: u16 = 77;
pub const KEY_KPPLUS: u16 = 78;
pub const KEY_KP1: u16 = 79;
pub const KEY_KP2: u16 = 80;
pub const KEY_KP3: u16 = 81;
pub const KEY_KP0: u16 = 82;
pub const KEY_KPDOT: u16 = 83;
pub const KEY_KPENTER: u16 = 96;
pub const KEY_KPSLASH: u16 = 98;
pub const KEY_KPEQUAL: u16 = 117;
pub const KEY_KPCOMMA: u16 = 121;
pub const KEY_KPPLUSMINUS: u16 = 118;
pub const KEY_KPJPCOMMA: u16 = 95;

// Keycodes for the modifier and lock keys (tracked across press/release, not
// emitted): Shift / Ctrl / Alt, the Windows (meta) keys, and Caps / Num / Scroll Lock.
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_CAPSLOCK: u16 = 58;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;
pub const KEY_NUMLOCK: u16 = 69;
pub const KEY_SCROLLLOCK: u16 = 70;

// The remaining standard-keyboard keys: the Menu (context) key, Print Screen /
// SysRq and Pause carry no terminal byte and are recognized but inert; the ISO
// 102nd key (next to the left Shift) types as a second backslash on the US layout.
pub const KEY_COMPOSE: u16 = 127;
pub const KEY_SYSRQ: u16 = 99;
pub const KEY_PAUSE: u16 = 119;
pub const KEY_102ND: u16 = 86;
const KEY_BACKSLASH: u16 = 43;

// The system power keys: Power shuts the machine down (wired like the
// Ctrl+Alt+Delete chord); Sleep / Wake are reserved until suspend exists.
pub const KEY_POWER: u16 = 116;
pub const KEY_SLEEP: u16 = 142;
pub const KEY_WAKEUP: u16 = 143;

// The audio keys (volume / mute cluster), reserved until the audio mixer exists.
pub const KEY_MUTE: u16 = 113;
pub const KEY_VOLUMEDOWN: u16 = 114;
pub const KEY_VOLUMEUP: u16 = 115;
pub const KEY_MICMUTE: u16 = 248;

// The media transport keys, reserved until a media session exists.
pub const KEY_PLAYPAUSE: u16 = 164;
pub const KEY_STOPCD: u16 = 166;
pub const KEY_PREVIOUSSONG: u16 = 165;
pub const KEY_NEXTSONG: u16 = 163;
pub const KEY_EJECTCD: u16 = 161;

// The launcher / browser keys of a multimedia keyboard, reserved until there are
// applications to launch or steer.
pub const KEY_CALC: u16 = 140;
pub const KEY_MAIL: u16 = 155;
pub const KEY_COMPUTER: u16 = 157;
pub const KEY_HOMEPAGE: u16 = 172;
pub const KEY_REFRESH: u16 = 173;
pub const KEY_SEARCH: u16 = 217;
pub const KEY_BACK: u16 = 158;
pub const KEY_FORWARD: u16 = 159;

// The display brightness keys, reserved until backlight control exists.
pub const KEY_BRIGHTNESSDOWN: u16 = 224;
pub const KEY_BRIGHTNESSUP: u16 = 225;

// The edit-action cluster (HID usages 0x74..0x7e, the Sun / UNIX keyboard block:
// Stop, Again, Props, Undo, Front, Copy, Open, Paste, Find, Cut, Help, plus the
// labeled Menu key), reserved until there is a clipboard / action target.
pub const KEY_STOP: u16 = 128;
pub const KEY_AGAIN: u16 = 129;
pub const KEY_PROPS: u16 = 130;
pub const KEY_UNDO: u16 = 131;
pub const KEY_FRONT: u16 = 132;
pub const KEY_COPY: u16 = 133;
pub const KEY_OPEN: u16 = 134;
pub const KEY_PASTE: u16 = 135;
pub const KEY_FIND: u16 = 136;
pub const KEY_CUT: u16 = 137;
pub const KEY_HELP: u16 = 138;
pub const KEY_MENU: u16 = 139;

// The Japanese (JIS: Zenkaku/Hankaku, Ro, Katakana, Hiragana, Henkan, Muhenkan,
// Yen) and Korean (Hangul, Hanja) layout keys, reserved for non-US layouts.
pub const KEY_ZENKAKUHANKAKU: u16 = 85;
pub const KEY_RO: u16 = 89;
pub const KEY_KATAKANA: u16 = 90;
pub const KEY_HIRAGANA: u16 = 91;
pub const KEY_HENKAN: u16 = 92;
pub const KEY_KATAKANAHIRAGANA: u16 = 93;
pub const KEY_MUHENKAN: u16 = 94;
pub const KEY_HANGEUL: u16 = 122;
pub const KEY_HANJA: u16 = 123;
pub const KEY_YEN: u16 = 124;

// Every recognized key whose action belongs to a subsystem that does not exist
// yet. They are consumed here (never leaking a byte into the console) and this
// list is the single place their future wiring hooks in: Sleep / Wake to power
// management, the audio cluster to the mixer, the media transport to a media
// session, the launcher / edit / brightness keys to their targets, and the
// layout keys to non-US keyboard layouts. F21..F24 have no standard terminal
// sequence, so they wait here too.
#[rustfmt::skip]
const RESERVED_KEYS: [u16; 49] = [
	KEY_SLEEP, KEY_WAKEUP,
	KEY_MUTE, KEY_VOLUMEDOWN, KEY_VOLUMEUP, KEY_MICMUTE,
	KEY_PLAYPAUSE, KEY_STOPCD, KEY_PREVIOUSSONG, KEY_NEXTSONG, KEY_EJECTCD,
	KEY_CALC, KEY_MAIL, KEY_COMPUTER, KEY_HOMEPAGE, KEY_REFRESH, KEY_SEARCH, KEY_BACK, KEY_FORWARD,
	KEY_BRIGHTNESSDOWN, KEY_BRIGHTNESSUP,
	KEY_STOP, KEY_AGAIN, KEY_PROPS, KEY_UNDO, KEY_FRONT, KEY_COPY, KEY_OPEN, KEY_PASTE, KEY_FIND, KEY_CUT, KEY_HELP, KEY_MENU,
	KEY_F21, KEY_F22, KEY_F23, KEY_F24,
	KEY_KPPLUSMINUS, KEY_KPJPCOMMA,
	KEY_ZENKAKUHANKAKU, KEY_RO, KEY_KATAKANA, KEY_HIRAGANA, KEY_HENKAN, KEY_KATAKANAHIRAGANA, KEY_MUHENKAN, KEY_HANGEUL, KEY_HANJA, KEY_YEN,
];

// Input keycode -> ASCII for the unshifted main block: the letter keys
// (lowercase), the digit row, and the few control keys a line shell needs. 0 means
// "no character" (modifiers, function keys, unmapped). Indices are KEY_* codes, e.g.
// KEY_ESC = 1 -> 0x1b, KEY_A = 30 -> 'a', KEY_ENTER = 28 -> '\n', KEY_BACKSPACE = 14 -> 0x08.
#[rustfmt::skip]
const KEYMAP: [u8; 64] = [
	0,    0x1b, b'1', b'2',  b'3',  b'4', b'5', b'6', b'7', b'8',
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
	0,    0x1b, b'!', b'@',  b'#',  b'$', b'%', b'^', b'&', b'*',
	b'(', b')', b'_', b'+',  0x08,  0x09, b'Q', b'W', b'E', b'R',
	b'T', b'Y', b'U', b'I',  b'O',  b'P', b'{', b'}', b'\n', 0,
	b'A', b'S', b'D', b'F',  b'G',  b'H', b'J', b'K', b'L', b':',
	b'"', b'~', 0,   b'|',  b'Z',  b'X', b'C', b'V', b'B', b'N',
	b'M', b'<', b'>', b'?',  0,     b'*', 0,    b' ', 0,    0,
	0,    0,    0,    0,
];

// The live modifier and lock state, tracked across key press / release events.
pub struct Mods {
	pub shift: bool,
	pub ctrl: bool,
	pub alt: bool,
	// the Windows (meta / GUI) keys; tracked, but a terminal has no byte for them.
	pub meta: bool,
	pub caps: bool,
	// NumLock selects the keypad's digit mode.
	pub numlock: bool,
	// Scroll Lock toggles; nothing consumes it yet (terminals use it for output
	// flow control, which the console does not implement).
	pub scroll: bool,
}

impl Default for Mods {
	// NumLock starts on, so the keypad types digits out of the box (there are no
	// keyboard LEDs to mirror the state anyway).
	fn default() -> Mods {
		Mods { shift: false, ctrl: false, alt: false, meta: false, caps: false, numlock: true, scroll: false }
	}
}

// Feed one key event (a keycode and its value: 1 = press, 2 = autorepeat,
// 0 = release) into the console. Modifier and lock keys update `mods` (tracked
// across press and release); an ordinary key press / autorepeat is turned into a
// character through the layout (Shift / Caps / Ctrl / Alt applied), a navigation,
// edit or function key into its ANSI escape sequence, and a keypad key into a digit
// or a navigation sequence by the NumLock state.
pub unsafe fn feed_key(code: u16, value: u32, mods: &mut Mods) {
	unsafe {
		// Modifier keys track press (1) / release (0); the lock keys (Caps / Num / Scroll)
		// toggle on press. They emit no character, so handle them before the press-only
		// gate below.
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
			KEY_LEFTMETA | KEY_RIGHTMETA => {
				mods.meta = value != 0;
				return;
			}
			KEY_CAPSLOCK => {
				if value == 1 {
					mods.caps = !mods.caps;
				}
				return;
			}
			KEY_NUMLOCK => {
				if value == 1 {
					mods.numlock = !mods.numlock;
				}
				return;
			}
			KEY_SCROLLLOCK => {
				if value == 1 {
					mods.scroll = !mods.scroll;
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
		// The Power key shuts the machine down (interrupt-driven like the reboot chord,
		// so it works even when userspace is wedged).
		if code == KEY_POWER {
			system_power(POWER_OFF);
			return;
		}
		// The recognized keys whose subsystem does not exist yet: consumed, no bytes.
		if RESERVED_KEYS.contains(&code) {
			return;
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
		// Navigation, edit and function keys carry no ASCII glyph; emit the ANSI escape
		// sequence a serial terminal sends for them, so the shell's line editor decodes
		// the framebuffer keyboard and a serial terminal identically.
		if let Some(seq) = escape_sequence(code) {
			for &b in seq {
				console_feed(b);
			}
			return;
		}
		// The keypad: the operator keys and KP Enter always type; the digit block types
		// digits while NumLock is on (a held Shift temporarily reverses it, PC-style) and
		// doubles as the navigation island (arrows / Home / PgUp / Ins / Del) while it is off.
		let kp: u8 = keypad_char(code, mods.numlock ^ mods.shift);
		if kp != 0 {
			console_feed(kp);
			return;
		}
		if let Some(seq) = keypad_sequence(code) {
			for &b in seq {
				console_feed(b);
			}
			return;
		}
		// The ISO 102nd key has no slot in the 64-entry maps: on the US layout it is a
		// second backslash / pipe key, so route it through the backslash keycode.
		let code: u16 = if code == KEY_102ND { KEY_BACKSLASH } else { code };
		// Anything else above the maps - the Menu key, Print Screen / SysRq, Pause - has
		// no byte representation on a terminal: recognized, but inert.
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

// The ANSI escape sequence a navigation, edit or function keycode maps to, or None
// for an ordinary key. The navigation keys send the standard xterm sequences
// (ESC [ A/B/C/D for the arrows, ESC [ H / F for Home / End); the edit and function
// keys send the CSI-number-~ family (Insert 2, Delete 3, F1..F12 11..24, F13..F20
// 25..34 - the legacy form rather than xterm's SS3 F1..F4, so every sequence parses
// the same way and the console's line discipline swallows them uniformly).
fn escape_sequence(code: u16) -> Option<&'static [u8]> {
	match code {
		KEY_UP => Some(b"\x1b[A"),
		KEY_DOWN => Some(b"\x1b[B"),
		KEY_RIGHT => Some(b"\x1b[C"),
		KEY_LEFT => Some(b"\x1b[D"),
		KEY_HOME => Some(b"\x1b[H"),
		KEY_END => Some(b"\x1b[F"),
		KEY_INSERT => Some(b"\x1b[2~"),
		KEY_DELETE => Some(b"\x1b[3~"),
		KEY_F1 => Some(b"\x1b[11~"),
		KEY_F2 => Some(b"\x1b[12~"),
		KEY_F3 => Some(b"\x1b[13~"),
		KEY_F4 => Some(b"\x1b[14~"),
		KEY_F5 => Some(b"\x1b[15~"),
		KEY_F6 => Some(b"\x1b[17~"),
		KEY_F7 => Some(b"\x1b[18~"),
		KEY_F8 => Some(b"\x1b[19~"),
		KEY_F9 => Some(b"\x1b[20~"),
		KEY_F10 => Some(b"\x1b[21~"),
		KEY_F11 => Some(b"\x1b[23~"),
		KEY_F12 => Some(b"\x1b[24~"),
		KEY_F13 => Some(b"\x1b[25~"),
		KEY_F14 => Some(b"\x1b[26~"),
		KEY_F15 => Some(b"\x1b[28~"),
		KEY_F16 => Some(b"\x1b[29~"),
		KEY_F17 => Some(b"\x1b[31~"),
		KEY_F18 => Some(b"\x1b[32~"),
		KEY_F19 => Some(b"\x1b[33~"),
		KEY_F20 => Some(b"\x1b[34~"),
		_ => None,
	}
}

// A keypad key's character, if in the given mode it produces one: the operator keys
// (/ - + = ,) and KP Enter always do (KP* lives in the main KEYMAP), the digit
// block only while it is in digit mode.
fn keypad_char(code: u16, digits: bool) -> u8 {
	match code {
		KEY_KPSLASH => b'/',
		KEY_KPMINUS => b'-',
		KEY_KPPLUS => b'+',
		KEY_KPEQUAL => b'=',
		KEY_KPCOMMA => b',',
		KEY_KPENTER => b'\n',
		KEY_KP7 if digits => b'7',
		KEY_KP8 if digits => b'8',
		KEY_KP9 if digits => b'9',
		KEY_KP4 if digits => b'4',
		KEY_KP5 if digits => b'5',
		KEY_KP6 if digits => b'6',
		KEY_KP1 if digits => b'1',
		KEY_KP2 if digits => b'2',
		KEY_KP3 if digits => b'3',
		KEY_KP0 if digits => b'0',
		KEY_KPDOT if digits => b'.',
		_ => 0,
	}
}

// The navigation meaning of a keypad digit-block key while digit mode is off, as on
// a PC keyboard's navigation island (KP5 has no navigation meaning and stays inert).
fn keypad_sequence(code: u16) -> Option<&'static [u8]> {
	match code {
		KEY_KP7 => Some(b"\x1b[H"),
		KEY_KP8 => Some(b"\x1b[A"),
		KEY_KP9 => Some(b"\x1b[5~"),
		KEY_KP4 => Some(b"\x1b[D"),
		KEY_KP6 => Some(b"\x1b[C"),
		KEY_KP1 => Some(b"\x1b[F"),
		KEY_KP2 => Some(b"\x1b[B"),
		KEY_KP3 => Some(b"\x1b[6~"),
		KEY_KP0 => Some(b"\x1b[2~"),
		KEY_KPDOT => Some(b"\x1b[3~"),
		_ => None,
	}
}

// HID keyboard-page usage -> keycode, for the boot-protocol report a USB
// keyboard sends: the whole keyboard page - the letters (0x04..), the digit row,
// the control and punctuation keys, the function keys F1..F24, the navigation
// block, the keypad including its extras, the system / edit-action / audio keys,
// and the international and language keys - onto the same keycodes the
// virtio-input KEYMAP above indexes. 0 = unmapped (the Locking Caps / Num /
// Scroll usages of legacy terminals; the multimedia keys live on the Consumer
// page, which the boot protocol does not carry).
#[rustfmt::skip]
const HID_KEYCODES: [u16; 0x95] = [
	0,   0,   0,   0,   30,  48,  46,  32,  18,  33,  // 00: -, -, -, -, a, b, c, d, e, f
	34,  35,  23,  36,  37,  38,  50,  49,  24,  25,  // 0a: g, h, i, j, k, l, m, n, o, p
	16,  19,  31,  20,  22,  47,  17,  45,  21,  44,  // 14: q, r, s, t, u, v, w, x, y, z
	2,   3,   4,   5,   6,   7,   8,   9,   10,  11,  // 1e: 1, 2, 3, 4, 5, 6, 7, 8, 9, 0
	28,  1,   14,  15,  57,  12,  13,  26,  27,  43,  // 28: enter, esc, bksp, tab, space, -, =, [, ], backslash
	0,   39,  40,  41,  51,  52,  53,  58,  59,  60,  // 32: -, ;, ', `, comma, ., /, caps, f1, f2
	61,  62,  63,  64,  65,  66,  67,  68,  87,  88,  // 3c: f3, f4, f5, f6, f7, f8, f9, f10, f11, f12
	99,  70,  119, 110, 102, 104, 111, 107, 109, 106, // 46: prtsc, scroll, pause, ins, home, pgup, del, end, pgdn, right
	105, 108, 103, 69,  98,  55,  74,  78,  96,  79,  // 50: left, down, up, numlock, kp/, kp*, kp-, kp+, kpenter, kp1
	80,  81,  75,  76,  77,  71,  72,  73,  82,  83,  // 5a: kp2, kp3, kp4, kp5, kp6, kp7, kp8, kp9, kp0, kp.
	86,  127, 116, 117, 183, 184, 185, 186, 187, 188, // 64: 102nd, menu, power, kp=, f13, f14, f15, f16, f17, f18
	189, 190, 191, 192, 193, 194, 134, 138, 130, 132, // 6e: f19, f20, f21, f22, f23, f24, open, help, props, front
	128, 129, 131, 137, 133, 135, 136, 113, 115, 114, // 78: stop, again, undo, cut, copy, paste, find, mute, vol+, vol-
	0,   0,   0,   121, 0,   89,  93,  124, 92,  94,  // 82: lock caps, lock num, lock scroll, kp comma, kp= (AS/400), ro, katakana/hiragana, yen, henkan, muhenkan
	95,  0,   0,   0,   122, 123, 90,  91,  85,       // 8c: kp jp comma, intl7, intl8, intl9, hangul, hanja, katakana, hiragana, zenkaku/hankaku
];

// Resolve a HID keyboard-page usage id to its keycode (0 = unmapped).
pub fn hid_keycode(usage: u8) -> u16 {
	if (usage as usize) < HID_KEYCODES.len() { HID_KEYCODES[usage as usize] } else { 0 }
}

// The keycode of each HID boot-report modifier bit (byte 0, bits 0..7):
// LCtrl, LShift, LAlt, LGui, RCtrl, RShift, RAlt, RGui. The GUI bits map to the
// Windows (meta) keycodes.
pub const HID_MODIFIER_KEYCODES: [u16; 8] = [KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_LEFTALT, KEY_LEFTMETA, KEY_RIGHTCTRL, KEY_RIGHTSHIFT, KEY_RIGHTALT, KEY_RIGHTMETA];

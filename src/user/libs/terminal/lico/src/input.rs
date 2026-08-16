//! Bounded raw-terminal input decoding.

/// One decoded key action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
	Byte(u8),
	Control(u8),
	Alt(u8),
	Enter,
	Tab,
	Backspace,
	Escape,
	ArrowUp,
	ArrowDown,
	ArrowRight,
	ArrowLeft,
	Home,
	End,
	Insert,
	Delete,
	PageUp,
	PageDown,
	Function(u8),
}

/// A pointer report decoded from SGR mouse input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PointerEvent {
	pub code: u16,
	pub column: u16,
	pub row: u16,
	pub pressed: bool,
}

/// A key with modifiers held, as `CSI 1;<mod><letter>` and `CSI <n>;<mod>~` report them.
///
/// The bitmask is xterm's and is one more than the bits: 1 is nothing, 2 is shift, 5 is control,
/// and so on. Decoded here rather than handed on raw, because a number a caller has to remember to
/// subtract one from is a number somebody will forget to subtract one from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chord {
	pub key: Key,
	pub shift: bool,
	pub alt: bool,
	pub control: bool,
}

/// One raw-terminal input event.
///
/// A MODIFIED KEY IS A DIFFERENT EVENT from the same key alone, deliberately. Folding shift+F10
/// into F10 would make every program that ignores modifiers act on a keystroke its user did not
/// press, and the alternative - a modifier field on every key - would change fifty call sites that
/// have no opinion about modifiers. Nothing regresses by adding this: a modified key decoded to
/// `InvalidSequence` before it existed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputEvent {
	Key(Key),
	Chord(Chord),
	Pointer(PointerEvent),
	InvalidSequence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
	Ground,
	Escape,
	Ss3,
	Csi,
}

/// Incremental parser for the bounded ANSI subset used by LiberCommander.
///
/// It recognizes ordinary bytes, SS3 and CSI navigation keys, common function-key
/// encodings, and SGR pointer reports. CSI parsing stores at most three u16 values, so
/// hostile streams cannot grow parser state or cause unbounded numeric work.
pub struct InputDecoder {
	state: State,
	params: [u16; 3],
	param: usize,
	has_value: bool,
	sgr: bool,
	overflow: bool,
}

impl Default for InputDecoder {
	fn default() -> Self {
		Self::new()
	}
}

impl InputDecoder {
	pub const fn new() -> InputDecoder {
		InputDecoder { state: State::Ground, params: [0; 3], param: 0, has_value: false, sgr: false, overflow: false }
	}

	/// Feed one raw input byte. Escape sequences may span arbitrary channel messages.
	pub fn feed(&mut self, byte: u8) -> Option<InputEvent> {
		match self.state {
			State::Ground => self.feed_ground(byte),
			State::Escape => self.feed_escape(byte),
			State::Ss3 => self.feed_ss3(byte),
			State::Csi => self.feed_csi(byte),
		}
	}

	fn feed_ground(&mut self, byte: u8) -> Option<InputEvent> {
		let key = match byte {
			0x1b => {
				self.state = State::Escape;
				return None;
			}
			b'\r' | b'\n' => Key::Enter,
			b'\t' => Key::Tab,
			0x08 | 0x7f => Key::Backspace,
			0x00..=0x1f => Key::Control(byte),
			_ => Key::Byte(byte),
		};
		Some(InputEvent::Key(key))
	}

	fn feed_escape(&mut self, byte: u8) -> Option<InputEvent> {
		match byte {
			b'[' => {
				self.reset_csi();
				self.state = State::Csi;
				None
			}
			b'O' => {
				self.state = State::Ss3;
				None
			}
			0x1b => Some(InputEvent::Key(Key::Escape)),
			_ => {
				self.state = State::Ground;
				Some(InputEvent::Key(Key::Alt(byte)))
			}
		}
	}

	fn feed_ss3(&mut self, byte: u8) -> Option<InputEvent> {
		self.state = State::Ground;
		let key = match byte {
			b'A' => Key::ArrowUp,
			b'B' => Key::ArrowDown,
			b'C' => Key::ArrowRight,
			b'D' => Key::ArrowLeft,
			b'H' => Key::Home,
			b'F' => Key::End,
			b'P' => Key::Function(1),
			b'Q' => Key::Function(2),
			b'R' => Key::Function(3),
			b'S' => Key::Function(4),
			_ => return Some(InputEvent::InvalidSequence),
		};
		Some(InputEvent::Key(key))
	}

	fn feed_csi(&mut self, byte: u8) -> Option<InputEvent> {
		match byte {
			b'<' if self.param == 0 && !self.has_value && !self.sgr => {
				self.sgr = true;
				None
			}
			b'0'..=b'9' => {
				self.has_value = true;
				let value = self.params[self.param].checked_mul(10).and_then(|value| value.checked_add((byte - b'0') as u16));
				match value {
					Some(value) => self.params[self.param] = value,
					None => self.overflow = true,
				}
				None
			}
			b';' if self.has_value && self.param + 1 < self.params.len() => {
				self.param += 1;
				self.has_value = false;
				None
			}
			0x40..=0x7e => Some(self.finish_csi(byte)),
			_ => {
				self.state = State::Ground;
				Some(InputEvent::InvalidSequence)
			}
		}
	}

	fn finish_csi(&mut self, final_byte: u8) -> InputEvent {
		let count = self.param + usize::from(self.has_value);
		let params = self.params;
		let sgr = self.sgr;
		let overflow = self.overflow;
		self.state = State::Ground;
		self.reset_csi();
		if overflow {
			return InputEvent::InvalidSequence;
		}
		if sgr {
			if matches!(final_byte, b'M' | b'm') && count == 3 && params[1] != 0 && params[2] != 0 {
				return InputEvent::Pointer(PointerEvent { code: params[0], column: params[1], row: params[2], pressed: final_byte == b'M' });
			}
			return InputEvent::InvalidSequence;
		}
		// BACK-TAB has its own final byte rather than a modifier parameter, which is why it is here
		// and not in the modified-key arm below. It is how every terminal reports shift+Tab, and it
		// is what an editor's unindent is bound to.
		if final_byte == b'Z' && count == 0 {
			return InputEvent::Chord(Chord { key: Key::Tab, shift: true, alt: false, control: false });
		}
		if count == 0 {
			let Some(key) = csi_navigation_key(final_byte) else {
				return InputEvent::InvalidSequence;
			};
			return InputEvent::Key(key);
		}
		if final_byte == b'~' && count == 1 {
			return csi_tilde_key(params[0]).map(InputEvent::Key).unwrap_or(InputEvent::InvalidSequence);
		}
		// THE MODIFIED FORMS. `CSI 1;2A` is shift+up and `CSI 3;5~` is control+delete; the first
		// parameter of the letter form is always 1, and a value that is not is a sequence this
		// decoder does not claim to understand rather than one to guess at.
		if count == 2 {
			let key = if final_byte == b'~' {
				csi_tilde_key(params[0])
			} else if params[0] == 1 {
				csi_navigation_key(final_byte)
			} else {
				None
			};
			let Some(key) = key else {
				return InputEvent::InvalidSequence;
			};
			let Some(bits) = params[1].checked_sub(1) else {
				return InputEvent::InvalidSequence;
			};
			// A MODIFIER OF NONE IS THE PLAIN KEY. Some terminals spell `CSI 1;1A` for a bare arrow,
			// and reporting it as a chord with nothing held would make callers handle one key twice.
			if bits == 0 {
				return InputEvent::Key(key);
			}
			return InputEvent::Chord(Chord { key, shift: bits & 1 != 0, alt: bits & 2 != 0, control: bits & 4 != 0 });
		}
		InputEvent::InvalidSequence
	}

	fn reset_csi(&mut self) {
		self.params = [0; 3];
		self.param = 0;
		self.has_value = false;
		self.sgr = false;
		self.overflow = false;
	}
}

// The navigation letters, shared by the bare and the modified forms so the two cannot disagree
// about what `A` means.
fn csi_navigation_key(final_byte: u8) -> Option<Key> {
	match final_byte {
		b'A' => Some(Key::ArrowUp),
		b'B' => Some(Key::ArrowDown),
		b'C' => Some(Key::ArrowRight),
		b'D' => Some(Key::ArrowLeft),
		b'H' => Some(Key::Home),
		b'F' => Some(Key::End),
		_ => None,
	}
}

fn csi_tilde_key(code: u16) -> Option<Key> {
	match code {
		1 | 7 => Some(Key::Home),
		2 => Some(Key::Insert),
		3 => Some(Key::Delete),
		4 | 8 => Some(Key::End),
		5 => Some(Key::PageUp),
		6 => Some(Key::PageDown),
		11 => Some(Key::Function(1)),
		12 => Some(Key::Function(2)),
		13 => Some(Key::Function(3)),
		14 => Some(Key::Function(4)),
		15 => Some(Key::Function(5)),
		17 => Some(Key::Function(6)),
		18 => Some(Key::Function(7)),
		19 => Some(Key::Function(8)),
		20 => Some(Key::Function(9)),
		21 => Some(Key::Function(10)),
		23 => Some(Key::Function(11)),
		24 => Some(Key::Function(12)),
		_ => None,
	}
}

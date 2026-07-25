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

/// One raw-terminal input event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputEvent {
	Key(Key),
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
		if count == 0 {
			let key = match final_byte {
				b'A' => Key::ArrowUp,
				b'B' => Key::ArrowDown,
				b'C' => Key::ArrowRight,
				b'D' => Key::ArrowLeft,
				b'H' => Key::Home,
				b'F' => Key::End,
				_ => return InputEvent::InvalidSequence,
			};
			return InputEvent::Key(key);
		}
		if final_byte == b'~' && count == 1 {
			return csi_tilde_key(params[0]).map(InputEvent::Key).unwrap_or(InputEvent::InvalidSequence);
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

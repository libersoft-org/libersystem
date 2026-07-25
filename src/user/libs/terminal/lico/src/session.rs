//! Terminal mode ownership and restoration.

/// Mouse reports requested from the controlling terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseTracking {
	Off,
	Press,
	Drag,
	AnyMotion,
}

/// The terminal modes one full-screen TUI owns for its lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalOptions {
	pub alternate_screen: bool,
	pub raw_input: bool,
	pub disable_echo: bool,
	pub hide_cursor: bool,
	pub mouse: MouseTracking,
	pub bracketed_paste: bool,
}

impl TerminalOptions {
	/// The standard LiberCommander full-screen terminal contract.
	pub const fn tui() -> TerminalOptions {
		TerminalOptions { alternate_screen: true, raw_input: true, disable_echo: true, hide_cursor: true, mouse: MouseTracking::Drag, bracketed_paste: true }
	}
}

impl Default for TerminalOptions {
	fn default() -> Self {
		Self::tui()
	}
}

/// Output endpoint used to change terminal modes.
pub trait TerminalWriter {
	fn write(&mut self, bytes: &[u8]) -> bool;
}

/// Tracks whether a program currently owns non-default terminal modes.
pub struct TerminalSession {
	options: TerminalOptions,
	active: bool,
}

impl TerminalSession {
	pub const fn new(options: TerminalOptions) -> TerminalSession {
		TerminalSession { options, active: false }
	}

	pub const fn is_active(&self) -> bool {
		self.active
	}

	/// Enter the requested modes. Calling this repeatedly does not duplicate escapes.
	pub fn enter<W: TerminalWriter>(&mut self, writer: &mut W) -> bool {
		if self.active {
			return true;
		}
		self.active = true;
		if self.write_enter(writer) {
			true
		} else {
			self.restore(writer);
			false
		}
	}

	/// Restore the terminal defaults. This is idempotent and attempts every cleanup
	/// sequence even if an earlier write fails.
	pub fn restore<W: TerminalWriter>(&mut self, writer: &mut W) -> bool {
		if !self.active {
			return true;
		}
		self.active = false;
		self.write_restore(writer)
	}

	fn write_enter<W: TerminalWriter>(&self, writer: &mut W) -> bool {
		if self.options.alternate_screen {
			if !writer.write(b"\x1b[?1049h") {
				return false;
			}
		}
		if self.options.hide_cursor {
			if !writer.write(b"\x1b[?25l") {
				return false;
			}
		}
		if self.options.raw_input {
			if !writer.write(b"\x1b[?9001h") {
				return false;
			}
		}
		if self.options.disable_echo {
			if !writer.write(b"\x1b[?9002l") {
				return false;
			}
		}
		match self.options.mouse {
			MouseTracking::Off => {}
			MouseTracking::Press if !writer.write(b"\x1b[?1000h") => return false,
			MouseTracking::Drag if !writer.write(b"\x1b[?1002h") => return false,
			MouseTracking::AnyMotion if !writer.write(b"\x1b[?1003h") => return false,
			_ => {}
		}
		if self.options.mouse != MouseTracking::Off {
			if !writer.write(b"\x1b[?1006h") {
				return false;
			}
		}
		if self.options.bracketed_paste {
			if !writer.write(b"\x1b[?2004h") {
				return false;
			}
		}
		true
	}

	fn write_restore<W: TerminalWriter>(&self, writer: &mut W) -> bool {
		let mut ok = true;
		if self.options.bracketed_paste {
			write(writer, b"\x1b[?2004l", &mut ok);
		}
		if self.options.mouse != MouseTracking::Off {
			write(writer, b"\x1b[?1006l", &mut ok);
			match self.options.mouse {
				MouseTracking::Off => {}
				MouseTracking::Press => write(writer, b"\x1b[?1000l", &mut ok),
				MouseTracking::Drag => write(writer, b"\x1b[?1002l", &mut ok),
				MouseTracking::AnyMotion => write(writer, b"\x1b[?1003l", &mut ok),
			}
		}
		if self.options.raw_input {
			write(writer, b"\x1b[?9001l", &mut ok);
		}
		if self.options.disable_echo {
			write(writer, b"\x1b[?9002h", &mut ok);
		}
		if self.options.hide_cursor {
			write(writer, b"\x1b[?25h", &mut ok);
		}
		if self.options.alternate_screen {
			write(writer, b"\x1b[?1049l", &mut ok);
		}
		ok
	}
}

fn write<W: TerminalWriter>(writer: &mut W, bytes: &[u8], ok: &mut bool) {
	if !writer.write(bytes) {
		*ok = false;
	}
}

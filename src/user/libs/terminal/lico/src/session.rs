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

/// An entered terminal session that restores every owned mode when it leaves scope.
///
/// Applications create this immediately after acquiring their full-duplex terminal. A
/// normal return, caught signal, service-disconnect branch, or early error return drops
/// the guard and restores raw/cooked mode, cursor visibility, mouse reporting, paste
/// handling and the alternate screen in one shared implementation.
pub struct TerminalGuard<'a, W: TerminalWriter> {
	session: TerminalSession,
	writer: &'a mut W,
}

impl<'a, W: TerminalWriter> TerminalGuard<'a, W> {
	/// Enter `options` and return a guard that owns their restoration.
	pub fn enter(writer: &'a mut W, options: TerminalOptions) -> Option<TerminalGuard<'a, W>> {
		let mut session = TerminalSession::new(options);
		if session.enter(writer) { Some(TerminalGuard { session, writer }) } else { None }
	}

	/// The writer used for rendering while the terminal modes are owned.
	pub fn writer(&mut self) -> &mut W {
		self.writer
	}

	pub const fn is_active(&self) -> bool {
		self.session.is_active()
	}

	/// Restore modes before the end of this scope. Drop remains idempotent afterwards.
	pub fn restore(&mut self) -> bool {
		self.session.restore(self.writer)
	}
}

impl<W: TerminalWriter> Drop for TerminalGuard<'_, W> {
	fn drop(&mut self) {
		let _ = self.session.restore(self.writer);
	}
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
		// THE TTY'S MODES ARE NOT THIS LIBRARY'S BUSINESS ANY MORE.
		//
		// `raw_input` and `disable_echo` used to be `ESC[?9001h` / `ESC[?9002l` written into the
		// output stream, where a program's data and a program's request are the same bytes - so
		// `cat` on a file containing them reconfigured the terminal. They are a request on the
		// terminal's control channel now (`rt::tty_set_mode`), made by the PROGRAM, which is what
		// holds that capability. The options are still read here so a caller can see what it asked
		// for; what this library emits is only ever the terminal's own screen state.
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

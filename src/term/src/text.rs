// A non-graphical consumer of the grid model (L2): it serializes the scrollback and the
// live screen to logical text lines, joining soft-wrapped rows into one unbounded line and
// emitting a newline only on a hard break. It reads `Screen` through its row interface and
// touches no pixels, so it proves the model is renderer-independent (the same model a
// framebuffer renderer draws can be dumped to text, piped to ssh/telnet, or diffed in a test).

use crate::screen::Screen;
use alloc::vec::Vec;

pub struct TextSink {
	out: Vec<u8>,
}

impl TextSink {
	pub fn new() -> TextSink {
		TextSink { out: Vec::new() }
	}

	// Serialize `screen` (scrollback rows first, then the live screen) into logical lines:
	// soft-wrapped rows are concatenated into one line, a hard break ends a line, trailing
	// spaces are trimmed, and trailing empty lines are dropped. Lines are joined with '\n'
	// (no trailing newline). Replaces any previously captured text.
	pub fn capture(&mut self, screen: &Screen) {
		self.out.clear();
		let cols = screen.cols();
		let total = screen.total_logical_rows();
		let mut lines: Vec<Vec<u8>> = Vec::new();
		let mut line: Vec<u8> = Vec::new();
		for g in 0..total {
			for col in 0..cols {
				line.push(screen.global_glyph(col, g));
			}
			if !screen.global_wrap(g) {
				// Hard break (or a non-wrapped row): the logical line ends here.
				trim_trailing_spaces(&mut line);
				lines.push(core::mem::take(&mut line));
			}
			// Otherwise the row soft-wraps: keep accumulating into the same logical line.
		}
		// A trailing soft-wrapped partial with no closing hard break still forms a line.
		if !line.is_empty() {
			trim_trailing_spaces(&mut line);
			lines.push(line);
		}
		// Drop trailing empty logical lines (the blank bottom of the screen).
		while matches!(lines.last(), Some(l) if l.is_empty()) {
			lines.pop();
		}
		for (i, l) in lines.iter().enumerate() {
			if i > 0 {
				self.out.push(b'\n');
			}
			self.out.extend_from_slice(l);
		}
	}

	// The serialized text from the last `capture`.
	pub fn as_bytes(&self) -> &[u8] {
		&self.out
	}
}

impl Default for TextSink {
	fn default() -> TextSink {
		TextSink::new()
	}
}

// Drop trailing ASCII spaces from a logical line in place.
fn trim_trailing_spaces(line: &mut Vec<u8>) {
	while line.last() == Some(&b' ') {
		line.pop();
	}
}

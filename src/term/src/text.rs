// A non-graphical consumer of the grid model (L2): it serializes the scrollback and the
// live screen to logical text lines, joining soft-wrapped rows into one unbounded line and
// emitting a newline only on a hard break. It reads `Screen` through its row interface and
// touches no pixels, so it proves the model is renderer-independent (the same model a
// framebuffer renderer draws can be dumped to text, piped to ssh/telnet, or diffed in a test).

use crate::screen::{Screen, push_utf8};
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
	// FALLIBLE, because a ring-3 caller reaches this through `SYS_CONSOLE_READLOG` and every
	// allocation below is sized by the scrollback: the line buffer, the per-line vector, the encoded
	// bytes and the joined output, each growing infallibly, with peak memory of roughly twice the
	// whole scrollback while both the line list and the joined result are live. An infallible
	// allocation that fails does not fail the syscall - it ends the KERNEL.
	//
	// `false` means the capture could not be made and the sink holds nothing, which the caller
	// reports as an absent log rather than as an empty one.
	pub fn capture(&mut self, screen: &Screen) -> bool {
		self.out.clear();
		let cols = screen.cols();
		let total = screen.total_logical_rows();
		let mut lines: Vec<Vec<u8>> = Vec::new();
		let mut line: Vec<u32> = Vec::new();
		for g in 0..total {
			if line.try_reserve(cols).is_err() {
				self.out.clear();
				return false;
			}
			for col in 0..cols {
				line.push(screen.global_glyph(col, g));
			}
			if !screen.global_wrap(g) {
				// Hard break (or a non-wrapped row): the logical line ends here.
				trim_trailing_spaces(&mut line);
				if lines.try_reserve(1).is_err() {
					self.out.clear();
					return false;
				}
				lines.push(encode_line(&line));
				line.clear();
			}
			// Otherwise the row soft-wraps: keep accumulating into the same logical line.
		}
		// A trailing soft-wrapped partial with no closing hard break still forms a line.
		if !line.is_empty() {
			trim_trailing_spaces(&mut line);
			if lines.try_reserve(1).is_err() {
				self.out.clear();
				return false;
			}
			lines.push(encode_line(&line));
		}
		// Drop trailing empty logical lines (the blank bottom of the screen).
		while matches!(lines.last(), Some(l) if l.is_empty()) {
			lines.pop();
		}
		// One reservation for the joined result rather than one per line: the total is known here,
		// and this is the allocation that doubles the peak.
		let joined: usize = lines.iter().map(|l| l.len() + 1).sum();
		if self.out.try_reserve(joined).is_err() {
			self.out.clear();
			return false;
		}
		for (i, l) in lines.iter().enumerate() {
			if i > 0 {
				self.out.push(b'\n');
			}
			self.out.extend_from_slice(l);
		}
		true
	}

	// The serialized text from the last `capture`.
	pub fn as_bytes(&self) -> &[u8] {
		&self.out
	}

	// The serialized text, taken rather than copied. For a caller that wants to own it: the sink
	// already holds exactly this buffer, so `sink.as_bytes().to_vec()` allocated a second copy of a
	// whole scrollback to hand over the one that was already there.
	pub fn into_bytes(self) -> Vec<u8> {
		self.out
	}
}

impl Default for TextSink {
	fn default() -> TextSink {
		TextSink::new()
	}
}

// Drop trailing ASCII spaces from a logical line in place.
fn trim_trailing_spaces(line: &mut Vec<u32>) {
	while line.last() == Some(&(b' ' as u32)) {
		line.pop();
	}
}

// Encode a logical line of codepoints as UTF-8 bytes.
fn encode_line(line: &[u32]) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	for &cp in line {
		push_utf8(&mut out, cp);
	}
	out
}

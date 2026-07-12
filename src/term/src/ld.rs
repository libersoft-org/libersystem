// The tty line discipline (L2.5): cooked-mode line editing between the byte
// stream and the cell grid.
//
// In cooked mode it line-edits and echoes keystrokes on the reading program's
// behalf - a movable cursor, mid-line insert/delete, command history, Tab
// completion over a caller-supplied vocabulary, the editing control keys - and
// delivers a complete line on Enter; in raw mode keystrokes pass straight
// through. It lives here next to the Screen it echoes into, so every console
// host (a display VT, a PTY) gets the same editor; the echo sink renders live
// to an optional `Term` and collects the same bytes for a byte-stream mirror.

use alloc::vec::Vec;

use crate::Term;

// The tty line discipline limits (per VT). LD_HIST_MAX is the history default when
// no configuration answers (the live depth is the `console.history` config key).
const LD_LINE_MAX: usize = 4096;
pub const LD_HIST_MAX: usize = 512;

// A small fixed buffer the line discipline accumulates echo bytes in, mirrored to the
// serial port after a keystroke is processed (the framebuffer is echoed live).
pub struct EchoBuf {
	buf: [u8; 512],
	len: usize,
}

impl EchoBuf {
	pub fn new() -> EchoBuf {
		EchoBuf { buf: [0u8; 512], len: 0 }
	}
	fn push(&mut self, bytes: &[u8]) {
		for &b in bytes {
			if self.len < self.buf.len() {
				self.buf[self.len] = b;
				self.len += 1;
			}
		}
	}
	pub fn as_slice(&self) -> &[u8] {
		&self.buf[..self.len]
	}
}

// The echo sink: line-edit feedback renders live to the VT's cell grid (if any) and is
// collected for the serial mirror.
pub struct Echo<'a> {
	pub term: Option<&'a mut Term>,
	pub ser: EchoBuf,
}

impl Echo<'_> {
	fn put(&mut self, bytes: &[u8]) {
		if let Some(t) = &mut self.term {
			for &b in bytes {
				t.screen.put_byte(b);
			}
		}
		self.ser.push(bytes);
	}
}

// The tty line discipline for one VT: in cooked mode it line-edits + echoes keystrokes
// (a movable cursor, mid-line insert/delete, command history, the editing control keys)
// on the program's behalf and delivers a complete line on Enter; in raw mode keystrokes
// pass straight through. This is the line editor moved out of the shell into the
// terminal, so every program reading this console gets the editor for free.
pub struct Ld {
	pub line: [u8; LD_LINE_MAX],
	pub len: usize,
	cursor: usize,
	history: Vec<Vec<u8>>,
	// The history depth: the operator's policy (the `console.history` config key,
	// read at VT creation), LD_HIST_MAX when no configuration answers.
	hist_max: usize,
	hist_pos: usize,
	esc: u8,
	csi_param: u8,
	// false = raw mode (keystrokes pass through), true = cooked (line-edited). The
	// program toggles it with ESC[?9001h/l in its output stream.
	pub cooked: bool,
	// whether keystrokes are echoed (ESC[?9002h/l).
	pub echo: bool,
	// set when Ctrl+D ends input on an empty line: feed_key delivers a zero-byte read
	// (EOF) to the program instead of a line.
	pub eof: bool,
	// whether the previous keystroke was a Tab, so a second one asks for the listing.
	last_tab: bool,
	// set when a double Tab found several completions: feed_key delivers the unfinished
	// line to the program marked with a leading tab (which a cooked line can never
	// contain), the program prints the matches and re-draws the prompt, and the buffer
	// stays intact so typing continues in place.
	pub relist: bool,
}

impl Ld {
	pub fn new(history_max: usize) -> Ld {
		Ld { line: [0u8; LD_LINE_MAX], len: 0, cursor: 0, history: Vec::new(), hist_max: history_max, hist_pos: 0, esc: 0, csi_param: 0, cooked: true, echo: true, eof: false, last_tab: false, relist: false }
	}

	// Feed one cooked-mode keystroke (`vocab` is the Tab-completion vocabulary). Returns
	// true when the line was submitted (Enter, the
	// Ctrl+C cancel, or Ctrl+D); on a Ctrl+D EOF `self.eof` is set and the line is empty.
	pub fn feed(&mut self, b: u8, vocab: &[Vec<u8>], e: &mut Echo) -> bool {
		let again: bool = self.last_tab;
		self.last_tab = false;
		match self.esc {
			1 => {
				self.esc = if b == b'[' { 2 } else { 0 };
				return false;
			}
			2 => {
				self.csi(b, e);
				return false;
			}
			_ => {}
		}
		match b {
			0x1b => self.esc = 1,
			b'\n' | b'\r' => {
				if self.echo {
					e.put(b"\n");
				}
				return true;
			}
			0x08 | 0x7f => self.backspace(e),
			0x01 => self.home(e),      // Ctrl+A
			0x05 => self.end(e),       // Ctrl+E
			0x15 => self.kill_line(e), // Ctrl+U
			0x17 => self.kill_word(e), // Ctrl+W
			0x04 => {
				// Ctrl+D: EOF on an empty line (feed_key delivers a zero-byte read so the
				// shell logs out), otherwise submit the buffered line like Enter.
				if self.len == 0 {
					self.eof = true;
				} else if self.echo {
					e.put(b"\n");
				}
				return true;
			}
			0x03 => {
				// Ctrl+C at the prompt: cancel the line and reprompt (deliver an empty
				// line). A foreground job is interrupted in raw mode, not here.
				if self.echo {
					e.put(b"^C\n");
				}
				self.len = 0;
				self.cursor = 0;
				return true;
			}
			0x20..=0x7e => self.insert(b, e),
			b'\t' => {
				self.last_tab = true;
				return self.tab(again, vocab, e);
			}
			_ => {}
		}
		false
	}

	// Tab completion over the command word (the line's first token, cursor at its end),
	// against `vocab` - the shell builtins plus the live bin/ listing: a unique match
	// completes fully, several matches extend to their longest common
	// prefix, and a second Tab with nothing left to extend asks the program to list the
	// matches (returns true; `self.relist` marks the delivery). Elsewhere in the line the
	// key is ignored - path and argument completion is future work.
	fn tab(&mut self, again: bool, vocab: &[Vec<u8>], e: &mut Echo) -> bool {
		if self.cursor != self.len || self.line[..self.len].contains(&b' ') {
			return false;
		}
		let matches: Vec<&[u8]> = vocab.iter().map(|v: &Vec<u8>| v.as_slice()).filter(|c: &&[u8]| c.starts_with(&self.line[..self.len])).collect();
		let first: Vec<u8> = match matches.first() {
			Some(&m) => m.to_vec(),
			None => return false,
		};
		if matches.len() == 1 {
			for i in self.len..first.len() {
				self.insert(first[i], e);
			}
			self.insert(b' ', e);
			return false;
		}
		// several matches: extend to the longest common prefix they share.
		let mut common: usize = first.len();
		for m in &matches[1..] {
			let mut i: usize = 0;
			while i < common && i < m.len() && m[i] == first[i] {
				i += 1;
			}
			common = i;
		}
		if common > self.len {
			for i in self.len..common {
				self.insert(first[i], e);
			}
			return false;
		}
		// nothing left to extend: the second Tab lists the matches.
		if again {
			self.relist = true;
			return true;
		}
		false
	}

	fn csi(&mut self, b: u8, e: &mut Echo) {
		match b {
			b'A' => self.history_prev(e),
			b'B' => self.history_next(e),
			b'C' => self.right(e),
			b'D' => self.left(e),
			b'H' => self.home(e),
			b'F' => self.end(e),
			b'0'..=b'9' => {
				self.csi_param = self.csi_param.wrapping_mul(10).wrapping_add(b - b'0');
				return;
			}
			b'~' => match self.csi_param {
				1 | 7 => self.home(e),
				4 | 8 => self.end(e),
				3 => self.delete(e),
				_ => {}
			},
			_ => {}
		}
		self.esc = 0;
		self.csi_param = 0;
	}

	fn insert(&mut self, c: u8, e: &mut Echo) {
		if self.len >= LD_LINE_MAX {
			return;
		}
		let mut i = self.len;
		while i > self.cursor {
			self.line[i] = self.line[i - 1];
			i -= 1;
		}
		self.line[self.cursor] = c;
		self.len += 1;
		if self.echo {
			e.put(&self.line[self.cursor..self.len]);
		}
		self.cursor += 1;
		if self.echo {
			self.move_left(self.len - self.cursor, e);
		}
	}

	fn backspace(&mut self, e: &mut Echo) {
		if self.cursor == 0 {
			return;
		}
		let mut i = self.cursor;
		while i < self.len {
			self.line[i - 1] = self.line[i];
			i += 1;
		}
		self.cursor -= 1;
		self.len -= 1;
		if self.echo {
			e.put(b"\x08");
			e.put(&self.line[self.cursor..self.len]);
			e.put(b" ");
			self.move_left(self.len - self.cursor + 1, e);
		}
	}

	fn delete(&mut self, e: &mut Echo) {
		if self.cursor >= self.len {
			return;
		}
		let mut i = self.cursor + 1;
		while i < self.len {
			self.line[i - 1] = self.line[i];
			i += 1;
		}
		self.len -= 1;
		if self.echo {
			e.put(&self.line[self.cursor..self.len]);
			e.put(b" ");
			self.move_left(self.len - self.cursor + 1, e);
		}
	}

	fn left(&mut self, e: &mut Echo) {
		if self.cursor > 0 {
			if self.echo {
				e.put(b"\x08");
			}
			self.cursor -= 1;
		}
	}

	fn right(&mut self, e: &mut Echo) {
		if self.cursor < self.len {
			if self.echo {
				e.put(&self.line[self.cursor..self.cursor + 1]);
			}
			self.cursor += 1;
		}
	}

	fn home(&mut self, e: &mut Echo) {
		if self.echo {
			self.move_left(self.cursor, e);
		}
		self.cursor = 0;
	}

	fn end(&mut self, e: &mut Echo) {
		if self.echo {
			e.put(&self.line[self.cursor..self.len]);
		}
		self.cursor = self.len;
	}

	fn move_left(&self, n: usize, e: &mut Echo) {
		for _ in 0..n {
			e.put(b"\x08");
		}
	}

	// Ctrl+U: erase the whole line.
	fn kill_line(&mut self, e: &mut Echo) {
		self.replace_line(b"", e);
	}

	// Ctrl+W: erase the word before the cursor (trailing spaces, then the word).
	fn kill_word(&mut self, e: &mut Echo) {
		while self.cursor > 0 && self.line[self.cursor - 1] == b' ' {
			self.backspace(e);
		}
		while self.cursor > 0 && self.line[self.cursor - 1] != b' ' {
			self.backspace(e);
		}
	}

	fn replace_line(&mut self, new: &[u8], e: &mut Echo) {
		if self.echo {
			e.put(&self.line[self.cursor..self.len]);
			for _ in 0..self.len {
				e.put(b"\x08 \x08");
			}
		}
		let n = new.len().min(LD_LINE_MAX);
		self.line[..n].copy_from_slice(&new[..n]);
		self.len = n;
		self.cursor = n;
		if self.echo {
			e.put(&self.line[..n]);
		}
	}

	fn history_prev(&mut self, e: &mut Echo) {
		if self.hist_pos == 0 {
			return;
		}
		self.hist_pos -= 1;
		// clone to the heap: a line is up to LD_LINE_MAX (4 kB), too big for the stack.
		let h: Vec<u8> = self.history[self.hist_pos].clone();
		self.replace_line(&h, e);
	}

	fn history_next(&mut self, e: &mut Echo) {
		if self.hist_pos >= self.history.len() {
			return;
		}
		self.hist_pos += 1;
		if self.hist_pos == self.history.len() {
			self.replace_line(b"", e);
		} else {
			let h: Vec<u8> = self.history[self.hist_pos].clone();
			self.replace_line(&h, e);
		}
	}

	// Record the submitted line in history (skipping empty / duplicate), then reset.
	pub fn commit(&mut self) {
		let trimmed = ld_trim(&self.line[..self.len]);
		if !trimmed.is_empty() && self.history.last().map(|h: &Vec<u8>| h.as_slice()) != Some(trimmed) {
			if self.history.len() >= self.hist_max {
				self.history.remove(0);
			}
			self.history.push(trimmed.to_vec());
		}
		self.len = 0;
		self.cursor = 0;
		self.hist_pos = self.history.len();
		self.esc = 0;
		self.csi_param = 0;
		self.eof = false;
	}
}

// Trim ASCII whitespace from both ends (the line discipline's history dedup).
fn ld_trim(mut s: &[u8]) -> &[u8] {
	while let [first, rest @ ..] = s {
		if first.is_ascii_whitespace() {
			s = rest;
		} else {
			break;
		}
	}
	while let [rest @ .., last] = s {
		if last.is_ascii_whitespace() {
			s = rest;
		} else {
			break;
		}
	}
	s
}

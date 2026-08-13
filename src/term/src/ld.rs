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

// The buffer the line discipline accumulates echo bytes in, mirrored to the serial port or the PTY
// master after a keystroke is processed (the framebuffer is echoed live).
//
// IT GROWS. This was a fixed 512 bytes whose `push` simply stopped, and a line is up to
// `LD_LINE_MAX` = 4096: `replace_line` - Ctrl+U, and every history recall - echoes the tail, then
// three bytes per column to erase it, then the new line, which is past 20 kB for a full line. The
// framebuffer echo is live and stayed correct, so the local display looked right while the serial
// mirror and the PTY master received a truncated ESCAPE STREAM and lost sync - a half-written
// sequence is not a shorter update, it is a terminal in a state nobody chose.
//
// One keystroke's echo, drained immediately by the caller, so the allocation is short-lived and
// bounded by what the editor can emit for one edit.
pub struct EchoBuf {
	buf: Vec<u8>,
}

impl EchoBuf {
	pub fn new() -> EchoBuf {
		EchoBuf { buf: Vec::new() }
	}
	fn push(&mut self, bytes: &[u8]) {
		self.buf.extend_from_slice(bytes);
	}
	pub fn as_slice(&self) -> &[u8] {
		&self.buf
	}
}

// Write `value` as decimal digits at the front of `out`, returning how many bytes it took. The
// caller sizes `out` for the largest value it can pass; a screen coordinate needs five.
fn write_decimal(out: &mut [u8], mut value: usize) -> usize {
	let mut digits: [u8; 20] = [0; 20];
	let mut len: usize = 0;
	loop {
		digits[len] = b'0' + (value % 10) as u8;
		len += 1;
		value /= 10;
		if value == 0 || len == digits.len() {
			break;
		}
	}
	let n: usize = len.min(out.len());
	for i in 0..n {
		out[i] = digits[len - 1 - i];
	}
	n
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

	// Bytes for the LOCAL GRID ONLY - an absolute cursor address, which is meaningful against a
	// screen whose geometry and contents we can read and meaningless on the mirror, where the far
	// end has its own size and its own idea of what is on it.
	fn put_screen(&mut self, bytes: &[u8]) {
		if let Some(t) = &mut self.term {
			for &b in bytes {
				t.screen.put_byte(b);
			}
		}
	}

	// Address a cell on the local grid (one-based CUP, as the wire encodes it).
	fn cup(&mut self, row: usize, col: usize) {
		let mut out: [u8; 16] = [0; 16];
		let mut n: usize = 0;
		out[n] = 0x1b;
		n += 1;
		out[n] = b'[';
		n += 1;
		n += write_decimal(&mut out[n..], row + 1);
		out[n] = b';';
		n += 1;
		n += write_decimal(&mut out[n..], col + 1);
		out[n] = b'H';
		n += 1;
		let seq: [u8; 16] = out;
		self.put_screen(&seq[..n]);
	}

	// Move the caret `n` columns left. Returns false when the local grid could not go that far
	// because the target is above the top of the screen - the caller then repaints instead.
	//
	// THE MIRROR ALWAYS GETS `n` BACKSPACES, which is the encoding it has always got and the only
	// one that makes sense to a terminal we cannot see. The local grid gets an ABSOLUTE address: it
	// is the half where "a backspace always moves" was false. Once the first row of a long command
	// line has scrolled into the scrollback, the reverse-wrap backspace has no row above it to step
	// onto and stops - and the editor, which counts the backspaces it emits, carried on believing
	// the caret had moved. From then on `Ld.cursor` and the caret disagreed and every later edit was
	// drawn in the wrong place.
	// Erase `n` columns to the left of the caret, leaving the caret where they started.
	//
	// THE MIRROR GETS THE CLASSIC `BS SP BS` PER COLUMN and the local grid gets one absolute move
	// plus an erase-to-end-of-display, for the same reason `move_left` splits: walking backwards
	// works only while there is a row above to walk onto. Returns false when the local grid could
	// not walk that far, and the caller repaints.
	fn erase_left(&mut self, n: usize) -> bool {
		for _ in 0..n {
			self.ser.push(b"\x08 \x08");
		}
		if self.term.is_none() {
			return true;
		}
		let moved: bool = self.move_left_screen(n);
		if moved {
			self.put_screen(b"\x1b[J");
		}
		moved
	}

	// The local-grid half of `move_left`, with no mirror bytes.
	fn move_left_screen(&mut self, n: usize) -> bool {
		let Some(t) = &mut self.term else {
			return true;
		};
		let cols: usize = t.screen.cols().max(1);
		let here: usize = t.screen.caret_index();
		if n > here {
			return false;
		}
		let target: usize = here - n;
		self.cup(target / cols, target % cols);
		true
	}

	fn move_left(&mut self, n: usize) -> bool {
		for _ in 0..n {
			self.ser.push(b"\x08");
		}
		// No local grid: nothing to contradict the count, and the far end owns the outcome.
		self.move_left_screen(n)
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
	// WIDER THAN A BYTE. This was a `u8` accumulated with `wrapping_mul`/`wrapping_add`, so
	// `CSI 259~` wrapped to 3 and executed Delete - a key nobody pressed, from a sequence a
	// terminal database or a mangled paste can produce.
	csi_param: u16,
	// false = raw mode (keystrokes pass through), true = cooked (line-edited).
	//
	// SET OUT OF BAND, over the VT's control channel (`SET_MODE`), by the foreground job holding a
	// send-only control capability. It used to be `ESC[?9001h/l` in the program's own output, which
	// made `cat` on a file containing those bytes a way to reconfigure the terminal: on a byte
	// stream a program's data and a program's request are the same thing, and no filter on the
	// output can tell them apart.
	pub cooked: bool,
	// whether keystrokes are echoed. Set the same way, and for the same reason.
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
		// AT LEAST ONE. `Ld::new(0)` is a legal public call, and its first `commit` then ran
		// `history.remove(0)` on an empty vector. The configuration path filters zero today, which
		// is protection by coincidence - the constructor is public and the panic is here.
		Ld { line: [0u8; LD_LINE_MAX], len: 0, cursor: 0, history: Vec::new(), hist_max: history_max.max(1), hist_pos: 0, esc: 0, csi_param: 0, cooked: true, echo: true, eof: false, last_tab: false, relist: false }
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
			// PRINTABLE ASCII AND EVERYTHING ABOVE 0x7F. Cooked mode took `0x20..=0x7e` and dropped
			// the rest, so this terminal rendered Czech perfectly and could not accept one accented
			// character as INPUT - and an unbracketed paste comes through the same path byte by
			// byte, so `příliš žluťoučký kůň` silently lost every non-ASCII byte.
			//
			// The bytes are buffered as they arrive; what has to know about UTF-8 is the EDITOR,
			// which is why `backspace` and `move_left` below count characters rather than bytes.
			0x20..=0x7e => self.insert(b, e),
			0x80..=0xff => self.insert(b, e),
			b'\t' => {
				self.last_tab = true;
				return self.tab(again, vocab, e);
			}
			_ => {}
		}
		false
	}

	// Tab completion of the segment under the cursor (the cursor must sit at the end of
	// the line) against `vocab`: a unique match completes fully, several matches extend to
	// their longest common prefix, and a second Tab with nothing left to extend asks the
	// program to list them (returns true; `self.relist` marks the delivery). The segment
	// is the run of characters back to the previous space OR slash, so this drives both
	// command-word completion (the first token, vocab = the builtins plus the live bin/
	// listing) and path / argument completion (a later token, vocab = the target
	// directory's entries with a trailing '/' on the sub-directories). A vocab entry that
	// ends in '/' is a directory, so a unique match of one is NOT followed by a space -
	// the operator keeps typing the sub-path.
	fn tab(&mut self, again: bool, vocab: &[Vec<u8>], e: &mut Echo) -> bool {
		if self.cursor != self.len {
			return false;
		}
		// The segment starts after the last space or slash, so the prefix we complete is
		// just the final path component (or the whole first token for a bare command word).
		let seg_start: usize = self.line[..self.len].iter().rposition(|&c: &u8| c == b' ' || c == b'/').map_or(0, |p: usize| p + 1);
		let prefix: &[u8] = &self.line[seg_start..self.len];
		let matches: Vec<&[u8]> = vocab.iter().map(|v: &Vec<u8>| v.as_slice()).filter(|c: &&[u8]| c.starts_with(prefix)).collect();
		let first: Vec<u8> = match matches.first() {
			Some(&m) => m.to_vec(),
			None => return false,
		};
		if matches.len() == 1 {
			for i in prefix.len()..first.len() {
				self.insert(first[i], e);
			}
			// A directory (trailing '/') keeps the line open for the sub-path; anything
			// else is a complete word, so close it with a space.
			if !first.ends_with(b"/") {
				self.insert(b' ', e);
			}
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
		// AND BACK OFF TO A CHARACTER BOUNDARY. The comparison above is over bytes, so two names
		// whose first multi-byte character shares a lead byte produced a prefix ending INSIDE a
		// code point - which is a broken glyph on the display and half a character in the buffer.
		while common > 0 && common < first.len() && first[common] & 0xc0 == 0x80 {
			common -= 1;
		}
		if common > prefix.len() {
			for i in prefix.len()..common {
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
				// SATURATING, not wrapping: a parameter this terminal cannot mean is clamped to one
				// it will not match, rather than folded into a small number that it will.
				self.csi_param = self.csi_param.saturating_mul(10).saturating_add((b - b'0') as u16);
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
		self.cursor += 1;
		// A CHARACTER REACHES THE SCREEN, NOT A BYTE.
		//
		// The echo used to happen per byte, so a multi-byte character mid-line put its lead byte on
		// the display, then the whole suffix, and only then its continuation bytes - a valid line
		// in the buffer and a replacement character on the screen. Nothing is echoed until the
		// character under the cursor is complete.
		if self.echo && self.char_is_complete() {
			e.put(&self.line[self.char_start(self.cursor)..self.len]);
			// And the step back is in CELLS, which is what the terminal moves in.
			self.move_left(self.columns(self.cursor, self.len), e);
		}
	}

	// Whether the bytes ending at the cursor form a complete UTF-8 character.
	//
	// A lead byte says how many continuation bytes follow it; until they have all arrived the
	// character is half-typed and nothing should be drawn.
	fn char_is_complete(&self) -> bool {
		let start = self.char_start(self.cursor);
		let lead = self.line[start];
		let want = if lead < 0x80 {
			1
		} else if lead & 0xe0 == 0xc0 {
			2
		} else if lead & 0xf0 == 0xe0 {
			3
		} else if lead & 0xf8 == 0xf0 {
			4
		} else {
			// A stray continuation byte is not the start of anything; treat it as complete so a
			// malformed paste still shows something rather than swallowing the rest of the line.
			1
		};
		self.cursor - start >= want
	}

	fn backspace(&mut self, e: &mut Echo) {
		if self.cursor == 0 {
			return;
		}
		// A WHOLE CHARACTER, not a byte. With non-ASCII input admitted, deleting one byte of a
		// multi-byte sequence leaves a fragment that is not text and that the screen renders as a
		// replacement character - so Backspace over `ř` had to be pressed twice and left rubbish in
		// between.
		let start = self.char_start(self.cursor);
		let removed = self.cursor - start;
		let mut i = self.cursor;
		while i < self.len {
			self.line[i - removed] = self.line[i];
			i += 1;
		}
		self.cursor = start;
		self.len -= removed;
		if self.echo {
			// The echo is in COLUMNS: one character is one cell however many bytes it took.
			e.put(b"\x08");
			e.put(&self.line[self.cursor..self.len]);
			e.put(b" ");
			self.move_left(self.columns(self.cursor, self.len) + 1, e);
		}
	}

	// The byte index where the character ending at `at` begins: `at` itself unless the byte before
	// it is a UTF-8 continuation, in which case walk back over them. A lone continuation byte -
	// which the medium can produce and this buffer can hold - stops after four steps rather than
	// walking off the front.
	fn char_start(&self, at: usize) -> usize {
		let mut start = at;
		let mut steps = 0;
		while start > 0 && steps < 4 {
			start -= 1;
			steps += 1;
			if self.line[start] & 0xc0 != 0x80 {
				break;
			}
		}
		start
	}

	// How many CHARACTERS the bytes in `from..to` occupy, which is how many columns they printed.
	fn columns(&self, from: usize, to: usize) -> usize {
		self.line[from..to].iter().filter(|&&b| b & 0xc0 != 0x80).count()
	}

	fn delete(&mut self, e: &mut Echo) {
		if self.cursor >= self.len {
			return;
		}
		// The whole character under the cursor, for the same reason as `backspace`.
		let mut end = self.cursor + 1;
		while end < self.len && self.line[end] & 0xc0 == 0x80 {
			end += 1;
		}
		let removed = end - self.cursor;
		let mut i = end;
		while i < self.len {
			self.line[i - removed] = self.line[i];
			i += 1;
		}
		self.len -= removed;
		if self.echo {
			e.put(&self.line[self.cursor..self.len]);
			e.put(b" ");
			self.move_left(self.columns(self.cursor, self.len) + 1, e);
		}
	}

	// One CHARACTER left, and one cell of echo - see `backspace`. Stepping one byte left over a
	// multi-byte character put the cursor inside it, so the next insert or delete split it.
	fn left(&mut self, e: &mut Echo) {
		if self.cursor > 0 {
			if self.echo {
				e.put(b"\x08");
			}
			self.cursor = self.char_start(self.cursor);
		}
	}

	fn right(&mut self, e: &mut Echo) {
		if self.cursor < self.len {
			// The whole character: its lead byte and every continuation after it.
			let mut end = self.cursor + 1;
			while end < self.len && self.line[end] & 0xc0 == 0x80 {
				end += 1;
			}
			if self.echo {
				e.put(&self.line[self.cursor..end]);
			}
			self.cursor = end;
		}
	}

	fn home(&mut self, e: &mut Echo) {
		// CELLS, NOT BYTES. `self.cursor` is a byte offset, so `cau` with an accent - four bytes,
		// three cells - moved the cursor four columns and left the caret a column left of where the
		// text starts, inside the prompt.
		let columns: usize = self.columns(0, self.cursor);
		// THE BUFFER MOVES FIRST. `move_left` may end in a repaint, and the repaint prints the line
		// from the cursor - so the cursor has to be where the caret is going before it runs.
		self.cursor = 0;
		if self.echo {
			self.move_left(columns, e);
		}
	}

	fn end(&mut self, e: &mut Echo) {
		if self.echo {
			e.put(&self.line[self.cursor..self.len]);
		}
		self.cursor = self.len;
	}

	// Move the caret `n` columns left, and REPAINT when the terminal cannot go that far.
	//
	// The reverse-wrap backspace steps onto the previous physical row only while there IS one:
	// once a command line is long enough that its first row has scrolled into the scrollback, the
	// caret reaches the top-left and stops. This walked it backwards anyway and set `self.cursor`
	// regardless, so the buffer and the screen diverged by however many backspaces the screen
	// swallowed and every later edit was drawn in the wrong place.
	fn move_left(&self, n: usize, e: &mut Echo) {
		if e.move_left(n) {
			return;
		}
		self.repaint(e);
	}

	// Reprint the line from the cursor at the top-left of the screen.
	//
	// Reached only when a leftward move ran off the top, which means the line is longer than the
	// viewport and has ALREADY filled it - so this overwrites the line's own cells rather than
	// anybody else's. The caret ends where the buffer cursor is, which is the property the walking
	// version could not hold, and the text after it follows down the screen for as far as there is
	// screen. It is the same answer `replace_line` gives for Ctrl+U - reprint rather than seek -
	// applied to the case where seeking is not possible at all.
	fn repaint(&self, e: &mut Echo) {
		let Some(t) = &e.term else {
			// No local grid: the far end owns its own caret and there is nothing here to repaint.
			return;
		};
		let cols: usize = t.screen.cols().max(1);
		let fits: usize = cols * t.screen.rows().max(1);
		// ONE ROW OF CONTEXT BEFORE THE CURSOR when there is any, so the window does not open
		// exactly on the caret and hide the character just typed. The caret then lands at the start
		// of the second row rather than the top-left, which is also where a naturally scrolled line
		// would have put it.
		let back: usize = self.columns(0, self.cursor).min(cols);
		let mut start: usize = self.cursor;
		for _ in 0..back {
			start = self.char_start(start.saturating_sub(1));
		}
		e.cup(0, 0);
		// Clear first, from the top-left, so nothing of the old rendering survives below the
		// reprint - and so the reprint may fill the screen exactly without the erase eating its
		// last cell.
		e.put_screen(b"\x1b[J");
		let mut end: usize = start;
		let mut columns: usize = 0;
		while end < self.len && columns < fits {
			end += 1;
			while end < self.len && self.line[end] & 0xc0 == 0x80 {
				end += 1;
			}
			columns += 1;
		}
		let tail: &[u8] = &self.line[start..end];
		if let Some(t) = &mut e.term {
			for &b in tail {
				t.screen.put_byte(b);
			}
		}
		e.cup(back / cols, back % cols);
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
		let mut repaint: bool = false;
		if self.echo {
			e.put(&self.line[self.cursor..self.len]);
			// CELLS, NOT BYTES. This erased `self.len` times, so Ctrl+U or a history recall over a
			// line containing any multi-byte character erased past the start of the line and into
			// the prompt - one extra backspace-space-backspace per continuation byte.
			//
			// And it walked backwards, which a line longer than the viewport cannot do: the caret
			// stops at the top-left and the erase then chewed through whatever was there instead.
			repaint = !e.erase_left(self.columns(0, self.len));
		}
		let n = new.len().min(LD_LINE_MAX);
		self.line[..n].copy_from_slice(&new[..n]);
		self.len = n;
		self.cursor = 0;
		if self.echo {
			if repaint {
				// The old line ran off the top of the screen, so there is no start of it to erase
				// back to: put the new one at the top-left and start again from there.
				self.repaint(e);
			}
			e.put(&self.line[..n]);
		}
		self.cursor = n;
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

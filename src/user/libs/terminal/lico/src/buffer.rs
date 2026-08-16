//! Bounded editable text storage shared by LiberCommander editor surfaces.

extern crate alloc;

use alloc::vec::Vec;

/// Why a buffer operation could not complete without changing existing text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextBufferError {
	TooLarge,
	OutOfMemory,
}

/// How much text the undo history may hold, across every recorded edit.
///
/// A BUDGET RATHER THAN A COUNT, because what costs memory is the bytes an edit remembers and not
/// how many edits there are: one `replace all` over a large file remembers more than a thousand
/// keystrokes. When the budget is exceeded the OLDEST edits are dropped, so the history is always
/// the most recent work rather than the first work - undoing what you just did is what undo is for.
pub const UNDO_BYTE_BUDGET: usize = 256 * 1024;

/// One reversible change: the bytes that were there and the bytes that replaced them.
///
/// Both sides are kept because a history that stores only what was removed can undo and not redo,
/// and one that stores only a length cannot restore what a deletion took.
struct Edit {
	at: usize,
	removed: Vec<u8>,
	inserted: Vec<u8>,
	// Where the cursor was BEFORE this edit. Restoring the text without restoring the cursor
	// leaves the caret somewhere the reader did not put it, which is what makes undo feel broken
	// even when the text is right.
	cursor_before: usize,
}

impl Edit {
	fn weight(&self) -> usize {
		self.removed.len() + self.inserted.len()
	}
}

/// A bounded byte-preserving text buffer with a cursor between bytes.
///
/// The buffer preserves every original byte, including CRLF sequences and incomplete UTF-8.
/// Renderers decide how to display malformed text; editing never silently normalizes or
/// truncates it. Every insertion reserves before moving bytes, so allocation failure leaves
/// both content and cursor unchanged.
pub struct TextBuffer {
	bytes: Vec<u8>,
	limit: usize,
	cursor: usize,
	revision: u64,
	clean_revision: u64,
	// WHERE A SELECTION STARTED, if one is being made. The other end is always the cursor, so a
	// selection cannot be left pointing at text that has moved: every edit clears it.
	anchor: Option<usize>,
	undo: Vec<Edit>,
	redo: Vec<Edit>,
	undo_bytes: usize,
	// Set while an undo or redo is being applied, so the edit it performs is not itself recorded
	// as new work. Without it, undoing would push a new undo entry and the history would never
	// reach the beginning.
	replaying: bool,
	// Whether the last recorded edit may still absorb the next one. Typing a word is one edit to
	// the person who typed it, and a history with one entry per keystroke makes undo useless for
	// exactly the operation it is used for most.
	coalescing: bool,
}

impl TextBuffer {
	pub fn new(limit: usize) -> TextBuffer {
		TextBuffer { bytes: Vec::new(), limit, cursor: 0, revision: 0, clean_revision: 0, anchor: None, undo: Vec::new(), redo: Vec::new(), undo_bytes: 0, replaying: false, coalescing: false }
	}

	pub fn from_bytes(bytes: &[u8], limit: usize) -> Result<TextBuffer, TextBufferError> {
		if bytes.len() > limit {
			return Err(TextBufferError::TooLarge);
		}
		let mut data = Vec::new();
		data.try_reserve_exact(bytes.len()).map_err(|_| TextBufferError::OutOfMemory)?;
		data.extend_from_slice(bytes);
		Ok(TextBuffer { bytes: data, limit, cursor: 0, revision: 0, clean_revision: 0, anchor: None, undo: Vec::new(), redo: Vec::new(), undo_bytes: 0, replaying: false, coalescing: false })
	}

	pub fn bytes(&self) -> &[u8] {
		&self.bytes
	}

	pub fn cursor(&self) -> usize {
		self.cursor
	}

	/// Put the caret at a byte offset, clamped to the buffer. For a caller that already knows where
	/// it wants to be - a search hit, a restored position - rather than one moving a step at a time.
	/// The selection is left alone, so a search can put the anchor at one end and the caret at the
	/// other and have the match selected.
	pub fn set_cursor(&mut self, offset: usize) {
		self.cursor = offset.min(self.bytes.len());
		self.coalescing = false;
	}

	pub fn is_dirty(&self) -> bool {
		self.revision != self.clean_revision
	}

	pub fn mark_clean(&mut self) {
		self.clean_revision = self.revision;
	}

	pub fn move_left(&mut self) -> bool {
		self.coalescing = false;
		if self.cursor == 0 {
			return false;
		}
		self.cursor -= 1;
		true
	}

	pub fn move_right(&mut self) -> bool {
		self.coalescing = false;
		if self.cursor >= self.bytes.len() {
			return false;
		}
		self.cursor += 1;
		true
	}

	pub fn move_home(&mut self) -> bool {
		self.coalescing = false;
		let next = line_start(&self.bytes, self.cursor);
		if next == self.cursor {
			return false;
		}
		self.cursor = next;
		true
	}

	pub fn move_end(&mut self) -> bool {
		self.coalescing = false;
		let next = line_end(&self.bytes, self.cursor);
		if next == self.cursor {
			return false;
		}
		self.cursor = next;
		true
	}

	pub fn move_up(&mut self) -> bool {
		self.coalescing = false;
		let column = self.cursor - line_start(&self.bytes, self.cursor);
		let current_start = line_start(&self.bytes, self.cursor);
		if current_start == 0 {
			return false;
		}
		let previous_end = current_start.saturating_sub(1);
		let previous_start = line_start(&self.bytes, previous_end);
		self.cursor = (previous_start + column).min(line_end(&self.bytes, previous_start));
		true
	}

	pub fn move_down(&mut self) -> bool {
		self.coalescing = false;
		let column = self.cursor - line_start(&self.bytes, self.cursor);
		let current_end = line_end(&self.bytes, self.cursor);
		if current_end >= self.bytes.len() {
			return false;
		}
		let next_start = current_end + 1;
		self.cursor = (next_start + column).min(line_end(&self.bytes, next_start));
		true
	}

	/// Move to the start of the previous word.
	///
	/// A WORD IS A RUN OF WORD BYTES, and the movement first skips whatever separates the caret
	/// from one - so pressing it in the middle of a run of spaces reaches the word before them
	/// rather than stepping over the spaces one at a time. Word bytes are ASCII alphanumeric plus
	/// `_`, the same rule the search's whole-word option uses, stated in one place so a reader
	/// cannot find that the two disagree about what a word is.
	pub fn move_word_left(&mut self) -> bool {
		self.coalescing = false;
		let mut at = self.cursor;
		while at > 0 && !is_word_byte(self.bytes[at - 1]) {
			at -= 1;
		}
		while at > 0 && is_word_byte(self.bytes[at - 1]) {
			at -= 1;
		}
		if at == self.cursor {
			return false;
		}
		self.cursor = at;
		true
	}

	/// Move past the end of the next word.
	pub fn move_word_right(&mut self) -> bool {
		self.coalescing = false;
		let mut at = self.cursor;
		while at < self.bytes.len() && !is_word_byte(self.bytes[at]) {
			at += 1;
		}
		while at < self.bytes.len() && is_word_byte(self.bytes[at]) {
			at += 1;
		}
		if at == self.cursor {
			return false;
		}
		self.cursor = at;
		true
	}

	pub fn insert(&mut self, byte: u8) -> Result<(), TextBufferError> {
		if let Some((start, end)) = self.selection() {
			return self.splice(start, end - start, &[byte]);
		}
		self.splice(self.cursor, 0, &[byte])
	}

	/// Insert a run of bytes at the cursor, replacing the selection when there is one. The paste
	/// primitive, and one edit rather than a keystroke's worth each - so undo takes it back whole.
	pub fn insert_slice(&mut self, bytes: &[u8]) -> Result<(), TextBufferError> {
		if let Some((start, end)) = self.selection() {
			return self.splice(start, end - start, bytes);
		}
		self.splice(self.cursor, 0, bytes)
	}

	pub fn delete_before(&mut self) -> bool {
		if let Some((start, end)) = self.selection() {
			return self.splice(start, end - start, &[]).is_ok();
		}
		if self.cursor == 0 {
			return false;
		}
		self.splice(self.cursor - 1, 1, &[]).is_ok()
	}

	pub fn delete_at(&mut self) -> bool {
		if let Some((start, end)) = self.selection() {
			return self.splice(start, end - start, &[]).is_ok();
		}
		if self.cursor >= self.bytes.len() {
			return false;
		}
		self.splice(self.cursor, 1, &[]).is_ok()
	}

	// THE ONE PLACE TEXT CHANGES. Every mutator routes through it, so the undo record, the revision,
	// the selection and the cursor cannot be updated by one path and forgotten by another - which is
	// the way an editor comes to have an operation that is not undoable and nobody notices until a
	// reader loses work to it.
	//
	// EVERYTHING THAT CAN FAIL IS BOOKED BEFORE ANYTHING MOVES, so a refusal leaves the text and the
	// cursor exactly as they were. An oversized edit that failed half way is the failure mode this
	// milestone's own item names: "an oversized edit fails without corrupting the buffer".
	fn splice(&mut self, at: usize, remove_len: usize, insert: &[u8]) -> Result<(), TextBufferError> {
		let at = at.min(self.bytes.len());
		let remove_len = remove_len.min(self.bytes.len() - at);
		let new_len = self.bytes.len() - remove_len + insert.len();
		if new_len > self.limit {
			return Err(TextBufferError::TooLarge);
		}
		let mut removed: Vec<u8> = Vec::new();
		let mut inserted: Vec<u8> = Vec::new();
		if !self.replaying {
			removed.try_reserve_exact(remove_len).map_err(|_| TextBufferError::OutOfMemory)?;
			inserted.try_reserve_exact(insert.len()).map_err(|_| TextBufferError::OutOfMemory)?;
			self.undo.try_reserve(1).map_err(|_| TextBufferError::OutOfMemory)?;
			removed.extend_from_slice(&self.bytes[at..at + remove_len]);
			inserted.extend_from_slice(insert);
		}
		if insert.len() > remove_len {
			self.bytes.try_reserve(insert.len() - remove_len).map_err(|_| TextBufferError::OutOfMemory)?;
		}
		let cursor_before = self.cursor;
		drop(self.bytes.splice(at..at + remove_len, insert.iter().copied()));
		self.cursor = at + insert.len();
		self.anchor = None;
		self.revision = self.revision.wrapping_add(1);
		if self.replaying {
			return Ok(());
		}
		// A NEW EDIT ENDS THE FUTURE. Keeping the redo stack across a fresh change would offer to
		// re-apply work against text it was never written for.
		self.redo.clear();
		// TYPING A WORD IS ONE EDIT to the person who typed it. Absorbed only when the new byte
		// continues the previous insertion exactly - same place, nothing removed - so a cursor move
		// or a deletion starts a new entry without anything having to remember to say so.
		if self.coalescing && remove_len == 0 && insert.len() == 1 {
			if let Some(last) = self.undo.last_mut() {
				if last.removed.is_empty() && last.at + last.inserted.len() == at && last.inserted.try_reserve(1).is_ok() {
					last.inserted.push(insert[0]);
					self.undo_bytes += 1;
					return Ok(());
				}
			}
		}
		self.coalescing = remove_len == 0 && insert.len() == 1;
		self.undo_bytes += removed.len() + inserted.len();
		self.undo.push(Edit { at, removed, inserted, cursor_before });
		// OLDEST FIRST. The budget is what bounds this history, and the entries worth keeping when
		// it is reached are the recent ones: undo is used to take back what was just done.
		while self.undo_bytes > UNDO_BYTE_BUDGET && self.undo.len() > 1 {
			let dropped = self.undo.remove(0);
			self.undo_bytes -= dropped.weight();
		}
		Ok(())
	}

	/// Take back the last edit. False when there is nothing to take back, or when restoring it
	/// would need memory that is not there - in which case nothing changes and the entry is kept.
	pub fn undo(&mut self) -> bool {
		let Some(edit) = self.undo.pop() else { return false };
		if self.redo.try_reserve(1).is_err() {
			self.undo.push(edit);
			return false;
		}
		self.undo_bytes -= edit.weight();
		self.replaying = true;
		let applied = self.splice(edit.at, edit.inserted.len(), &edit.removed).is_ok();
		self.replaying = false;
		if !applied {
			self.undo_bytes += edit.weight();
			self.undo.push(edit);
			return false;
		}
		self.cursor = edit.cursor_before.min(self.bytes.len());
		self.coalescing = false;
		self.redo.push(edit);
		true
	}

	/// Put back the last undone edit.
	pub fn redo(&mut self) -> bool {
		let Some(edit) = self.redo.pop() else { return false };
		if self.undo.try_reserve(1).is_err() {
			self.redo.push(edit);
			return false;
		}
		self.replaying = true;
		let applied = self.splice(edit.at, edit.removed.len(), &edit.inserted).is_ok();
		self.replaying = false;
		if !applied {
			self.redo.push(edit);
			return false;
		}
		self.cursor = edit.at + edit.inserted.len();
		self.coalescing = false;
		self.undo_bytes += edit.weight();
		self.undo.push(edit);
		true
	}

	/// Whether there is anything to undo / redo, for a menu that should not offer what it cannot do.
	pub fn can_undo(&self) -> bool {
		!self.undo.is_empty()
	}

	pub fn can_redo(&self) -> bool {
		!self.redo.is_empty()
	}

	/// Begin (or restart) a selection at the cursor.
	pub fn set_anchor(&mut self) {
		self.anchor = Some(self.cursor);
	}

	pub fn clear_anchor(&mut self) {
		self.anchor = None;
	}

	/// The selected range as `(start, end)` with `start <= end`, or None when nothing is selected.
	/// An anchor sitting exactly on the cursor is NOT a selection - it is a caret with a memory of
	/// where shift was first held, and treating it as one would make a shift-arrow that came back to
	/// where it started delete a character.
	pub fn selection(&self) -> Option<(usize, usize)> {
		let anchor = self.anchor?;
		let (start, end) = if anchor <= self.cursor { (anchor, self.cursor) } else { (self.cursor, anchor) };
		if start == end { None } else { Some((start, end)) }
	}

	pub fn selected_bytes(&self) -> &[u8] {
		match self.selection() {
			Some((start, end)) => &self.bytes[start..end],
			None => &[],
		}
	}

	/// Delete the selection, if there is one.
	pub fn delete_selection(&mut self) -> bool {
		let Some((start, end)) = self.selection() else { return false };
		self.splice(start, end - start, &[]).is_ok()
	}

	/// Copy the current line below itself, cursor following the copy.
	pub fn duplicate_line(&mut self) -> Result<(), TextBufferError> {
		let start = line_start(&self.bytes, self.cursor);
		let end = line_end(&self.bytes, self.cursor);
		let mut copy: Vec<u8> = Vec::new();
		copy.try_reserve_exact(end - start + 1).map_err(|_| TextBufferError::OutOfMemory)?;
		copy.push(b'\n');
		copy.extend_from_slice(&self.bytes[start..end]);
		self.cursor = end;
		self.coalescing = false;
		self.splice(end, 0, &copy)
	}

	/// Remove the current line, including its terminator. The last line of a file has none, so its
	/// removal takes the newline BEFORE it instead - otherwise deleting the last line leaves a blank
	/// one behind and the file grows a trailing newline it did not have.
	pub fn delete_line(&mut self) -> bool {
		let start = line_start(&self.bytes, self.cursor);
		let end = line_end(&self.bytes, self.cursor);
		self.coalescing = false;
		if end < self.bytes.len() {
			return self.splice(start, end - start + 1, &[]).is_ok();
		}
		if start == 0 {
			return self.splice(0, end, &[]).is_ok();
		}
		self.splice(start - 1, end - start + 1, &[]).is_ok()
	}

	/// Swap the current line with the one above it, keeping the cursor on the text that moved.
	pub fn move_line_up(&mut self) -> Result<bool, TextBufferError> {
		let start = line_start(&self.bytes, self.cursor);
		if start == 0 {
			return Ok(false);
		}
		let previous = line_start(&self.bytes, start - 1);
		let end = line_end(&self.bytes, self.cursor);
		let column = self.cursor - start;
		let mut reordered: Vec<u8> = Vec::new();
		reordered.try_reserve_exact(end - previous).map_err(|_| TextBufferError::OutOfMemory)?;
		reordered.extend_from_slice(&self.bytes[start..end]);
		reordered.push(b'\n');
		reordered.extend_from_slice(&self.bytes[previous..start - 1]);
		self.coalescing = false;
		self.splice(previous, end - previous, &reordered)?;
		self.cursor = previous + column;
		Ok(true)
	}

	/// Swap the current line with the one below it.
	pub fn move_line_down(&mut self) -> Result<bool, TextBufferError> {
		let start = line_start(&self.bytes, self.cursor);
		let end = line_end(&self.bytes, self.cursor);
		if end >= self.bytes.len() {
			return Ok(false);
		}
		let next_start = end + 1;
		let next_end = line_end(&self.bytes, next_start);
		let column = self.cursor - start;
		let mut reordered: Vec<u8> = Vec::new();
		reordered.try_reserve_exact(next_end - start).map_err(|_| TextBufferError::OutOfMemory)?;
		reordered.extend_from_slice(&self.bytes[next_start..next_end]);
		reordered.push(b'\n');
		reordered.extend_from_slice(&self.bytes[start..end]);
		self.coalescing = false;
		self.splice(start, next_end - start, &reordered)?;
		self.cursor = start + (next_end - next_start) + 1 + column;
		Ok(true)
	}

	/// Put `unit` at the front of every line the selection touches (or the current line when there
	/// is none), as ONE edit - so one undo takes the whole block back out again.
	pub fn indent_block(&mut self, unit: &[u8]) -> Result<usize, TextBufferError> {
		let (first, last) = self.block_bounds();
		let mut rebuilt: Vec<u8> = Vec::new();
		let mut lines = 0;
		let mut offset = first;
		while offset <= last {
			let end = line_end(&self.bytes, offset);
			rebuilt.try_reserve(unit.len() + (end - offset) + 1).map_err(|_| TextBufferError::OutOfMemory)?;
			rebuilt.extend_from_slice(unit);
			rebuilt.extend_from_slice(&self.bytes[offset..end]);
			lines += 1;
			if end >= last {
				break;
			}
			rebuilt.push(b'\n');
			offset = end + 1;
		}
		self.coalescing = false;
		self.splice(first, last - first, &rebuilt)?;
		Ok(lines)
	}

	/// Take one `unit` off the front of every line the selection touches. A line that does not
	/// begin with it is left ALONE rather than having its first characters removed - unindenting a
	/// block whose lines are indented differently must not eat text.
	pub fn unindent_block(&mut self, unit: &[u8]) -> Result<usize, TextBufferError> {
		if unit.is_empty() {
			return Ok(0);
		}
		let (first, last) = self.block_bounds();
		let mut rebuilt: Vec<u8> = Vec::new();
		let mut lines = 0;
		let mut offset = first;
		while offset <= last {
			let end = line_end(&self.bytes, offset);
			let line = &self.bytes[offset..end];
			let trimmed = if line.starts_with(unit) {
				lines += 1;
				&line[unit.len()..]
			} else {
				line
			};
			rebuilt.try_reserve(trimmed.len() + 1).map_err(|_| TextBufferError::OutOfMemory)?;
			rebuilt.extend_from_slice(trimmed);
			if end >= last {
				break;
			}
			rebuilt.push(b'\n');
			offset = end + 1;
		}
		self.coalescing = false;
		self.splice(first, last - first, &rebuilt)?;
		Ok(lines)
	}

	// The first line start and the last line end the current selection touches; the current line
	// when there is no selection. A block operation works on whole lines, so a selection that ends
	// at the first column of a line does NOT include that line - selecting downward to the start of
	// the next line is how a reader selects the lines above it.
	fn block_bounds(&self) -> (usize, usize) {
		let (from, to) = match self.selection() {
			Some((start, end)) => (start, end),
			None => (self.cursor, self.cursor),
		};
		let first = line_start(&self.bytes, from);
		let to = if to > from && line_start(&self.bytes, to) == to { to - 1 } else { to };
		(first, line_end(&self.bytes, to))
	}

	/// The 1-based line the cursor is on.
	pub fn line_number(&self) -> usize {
		1 + self.bytes[..self.cursor].iter().filter(|&&byte| byte == b'\n').count()
	}

	/// How many lines the buffer holds. An empty buffer is one empty line, which is what a reader
	/// sees; a trailing newline does not open another.
	pub fn line_count(&self) -> usize {
		if self.bytes.is_empty() {
			return 1;
		}
		let breaks = self.bytes.iter().filter(|&&byte| byte == b'\n').count();
		if self.bytes.last() == Some(&b'\n') { breaks } else { breaks + 1 }
	}

	/// Put the cursor at the start of 1-based line `line`, clamped to the last line.
	pub fn goto_line(&mut self, line: usize) {
		let target = line.max(1);
		let mut offset = 0;
		let mut current = 1;
		while current < target && offset < self.bytes.len() {
			offset = self.next_line_start(offset);
			if offset == line_end(&self.bytes, offset) && offset >= self.bytes.len() {
				break;
			}
			current += 1;
		}
		self.cursor = offset.min(self.bytes.len());
		self.anchor = None;
		self.coalescing = false;
	}

	/// The next occurrence of `needle` at or after `from` (searching backward: strictly before
	/// `from`). Literal bytes, because a viewer or editor that can be made to loop by opening a file
	/// has a denial of service in it - the same reason the syntax descriptors match literals.
	pub fn find(&self, needle: &[u8], from: usize, backward: bool, ignore_case: bool) -> Option<usize> {
		find_in(&self.bytes, needle, from, backward, ignore_case)
	}

	/// Replace the selection with `replacement` when it is exactly `needle`, then find the next
	/// occurrence and select it. Answers whether anything was replaced.
	pub fn replace_selection(&mut self, needle: &[u8], replacement: &[u8], ignore_case: bool) -> bool {
		let Some((start, end)) = self.selection() else { return false };
		if end - start != needle.len() || !equal_bytes(&self.bytes[start..end], needle, ignore_case) {
			return false;
		}
		self.coalescing = false;
		self.splice(start, needle.len(), replacement).is_ok()
	}

	/// Replace the bytes between `start` and `end` with `bytes`, as one edit.
	///
	/// The general primitive, for a caller that found its own match - a regular expression, say,
	/// whose engine lives where the tools that share it can reach it. The buffer does not need to
	/// know how the range was chosen, only that it is one.
	pub fn replace_range(&mut self, start: usize, end: usize, bytes: &[u8]) -> Result<(), TextBufferError> {
		let start = start.min(self.bytes.len());
		let end = end.clamp(start, self.bytes.len());
		self.coalescing = false;
		self.splice(start, end - start, bytes)
	}

	/// Replace every occurrence, as ONE edit - so a replace-all that turned out to be wrong is one
	/// undo rather than one per occurrence.
	pub fn replace_all(&mut self, needle: &[u8], replacement: &[u8], ignore_case: bool) -> Result<usize, TextBufferError> {
		if needle.is_empty() {
			return Ok(0);
		}
		let mut rebuilt: Vec<u8> = Vec::new();
		let mut count = 0;
		let mut offset = 0;
		while let Some(hit) = find_in(&self.bytes, needle, offset, false, ignore_case) {
			rebuilt.try_reserve(hit - offset + replacement.len()).map_err(|_| TextBufferError::OutOfMemory)?;
			rebuilt.extend_from_slice(&self.bytes[offset..hit]);
			rebuilt.extend_from_slice(replacement);
			offset = hit + needle.len();
			count += 1;
		}
		if count == 0 {
			return Ok(0);
		}
		rebuilt.try_reserve(self.bytes.len() - offset).map_err(|_| TextBufferError::OutOfMemory)?;
		rebuilt.extend_from_slice(&self.bytes[offset..]);
		self.coalescing = false;
		let total = self.bytes.len();
		self.splice(0, total, &rebuilt)?;
		self.cursor = 0;
		Ok(count)
	}

	pub fn line_at(&self, offset: usize) -> &[u8] {
		let start = line_start(&self.bytes, offset.min(self.bytes.len()));
		let end = line_end(&self.bytes, start);
		&self.bytes[start..end]
	}

	pub fn line_start_at(&self, offset: usize) -> usize {
		line_start(&self.bytes, offset.min(self.bytes.len()))
	}

	pub fn next_line_start(&self, offset: usize) -> usize {
		let end = line_end(&self.bytes, offset.min(self.bytes.len()));
		if end < self.bytes.len() { end + 1 } else { end }
	}
}

// Whether two runs of bytes are the same, optionally ignoring ASCII case.
//
// ASCII ONLY, and deliberately: case folding outside ASCII is a locale question, and a search that
// quietly folded some scripts and not others would give an answer whose rule nobody could state.
// What counts as part of a word, for word movement and for the search's whole-word option. ASCII
// alphanumeric plus `_`, stated once so the two cannot disagree about where a word ends.
fn is_word_byte(byte: u8) -> bool {
	byte.is_ascii_alphanumeric() || byte == b'_'
}

fn equal_bytes(left: &[u8], right: &[u8], ignore_case: bool) -> bool {
	if left.len() != right.len() {
		return false;
	}
	if ignore_case { left.iter().zip(right).all(|(a, b)| a.eq_ignore_ascii_case(b)) } else { left == right }
}

// The next occurrence of `needle` in `haystack`, at or after `from` going forward, strictly before
// `from` going backward. An empty needle matches nothing rather than matching everywhere: "find
// nothing" is a question with no useful answer, and returning a position for it makes a replace-all
// with an empty pattern into an infinite loop.
fn find_in(haystack: &[u8], needle: &[u8], from: usize, backward: bool, ignore_case: bool) -> Option<usize> {
	if needle.is_empty() || needle.len() > haystack.len() {
		return None;
	}
	let last = haystack.len() - needle.len();
	if backward {
		let start = from.min(haystack.len()).checked_sub(1)?;
		let mut at = start.min(last);
		loop {
			if equal_bytes(&haystack[at..at + needle.len()], needle, ignore_case) {
				return Some(at);
			}
			at = at.checked_sub(1)?;
		}
	}
	let mut at = from.min(haystack.len());
	while at <= last {
		if equal_bytes(&haystack[at..at + needle.len()], needle, ignore_case) {
			return Some(at);
		}
		at += 1;
	}
	None
}

fn line_start(bytes: &[u8], mut offset: usize) -> usize {
	offset = offset.min(bytes.len());
	while offset > 0 && bytes[offset - 1] != b'\n' {
		offset -= 1;
	}
	offset
}

fn line_end(bytes: &[u8], mut offset: usize) -> usize {
	offset = offset.min(bytes.len());
	while offset < bytes.len() && bytes[offset] != b'\n' {
		offset += 1;
	}
	offset
}

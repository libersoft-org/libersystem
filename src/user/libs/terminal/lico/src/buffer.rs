//! Bounded editable text storage shared by LiberCommander editor surfaces.

extern crate alloc;

use alloc::vec::Vec;

/// Why a buffer operation could not complete without changing existing text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextBufferError {
	TooLarge,
	OutOfMemory,
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
}

impl TextBuffer {
	pub fn new(limit: usize) -> TextBuffer {
		TextBuffer { bytes: Vec::new(), limit, cursor: 0, revision: 0, clean_revision: 0 }
	}

	pub fn from_bytes(bytes: &[u8], limit: usize) -> Result<TextBuffer, TextBufferError> {
		if bytes.len() > limit {
			return Err(TextBufferError::TooLarge);
		}
		let mut data = Vec::new();
		data.try_reserve_exact(bytes.len()).map_err(|_| TextBufferError::OutOfMemory)?;
		data.extend_from_slice(bytes);
		Ok(TextBuffer { bytes: data, limit, cursor: 0, revision: 0, clean_revision: 0 })
	}

	pub fn bytes(&self) -> &[u8] {
		&self.bytes
	}

	pub fn cursor(&self) -> usize {
		self.cursor
	}

	pub fn is_dirty(&self) -> bool {
		self.revision != self.clean_revision
	}

	pub fn mark_clean(&mut self) {
		self.clean_revision = self.revision;
	}

	pub fn move_left(&mut self) -> bool {
		if self.cursor == 0 {
			return false;
		}
		self.cursor -= 1;
		true
	}

	pub fn move_right(&mut self) -> bool {
		if self.cursor >= self.bytes.len() {
			return false;
		}
		self.cursor += 1;
		true
	}

	pub fn move_home(&mut self) -> bool {
		let next = line_start(&self.bytes, self.cursor);
		if next == self.cursor {
			return false;
		}
		self.cursor = next;
		true
	}

	pub fn move_end(&mut self) -> bool {
		let next = line_end(&self.bytes, self.cursor);
		if next == self.cursor {
			return false;
		}
		self.cursor = next;
		true
	}

	pub fn move_up(&mut self) -> bool {
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
		let column = self.cursor - line_start(&self.bytes, self.cursor);
		let current_end = line_end(&self.bytes, self.cursor);
		if current_end >= self.bytes.len() {
			return false;
		}
		let next_start = current_end + 1;
		self.cursor = (next_start + column).min(line_end(&self.bytes, next_start));
		true
	}

	pub fn insert(&mut self, byte: u8) -> Result<(), TextBufferError> {
		if self.bytes.len() == self.limit {
			return Err(TextBufferError::TooLarge);
		}
		self.bytes.try_reserve(1).map_err(|_| TextBufferError::OutOfMemory)?;
		self.bytes.insert(self.cursor, byte);
		self.cursor += 1;
		self.revision = self.revision.wrapping_add(1);
		Ok(())
	}

	pub fn delete_before(&mut self) -> bool {
		if self.cursor == 0 {
			return false;
		}
		self.cursor -= 1;
		self.bytes.remove(self.cursor);
		self.revision = self.revision.wrapping_add(1);
		true
	}

	pub fn delete_at(&mut self) -> bool {
		if self.cursor >= self.bytes.len() {
			return false;
		}
		self.bytes.remove(self.cursor);
		self.revision = self.revision.wrapping_add(1);
		true
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

//! ConsoleService terminal-control messages.

/// Request the current terminal grid size from the controlling terminal.
pub const WINSIZE_REQUEST: &[u8] = b"GET_WINSIZE";
/// Reply carrying the current terminal grid size.
pub const WINSIZE_REPLY: &[u8] = b"WINSIZE";
/// Asynchronous notification that the terminal grid size changed.
pub const RESIZE_EVENT: &[u8] = b"RESIZE";

/// Terminal dimensions measured in character cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
	pub rows: u16,
	pub columns: u16,
}

impl TerminalSize {
	pub const fn new(rows: u16, columns: u16) -> TerminalSize {
		TerminalSize { rows, columns }
	}

	pub const fn is_empty(self) -> bool {
		self.rows == 0 || self.columns == 0
	}
}

/// A terminal-control reply or notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalControl {
	InitialSize(TerminalSize),
	Resized(TerminalSize),
}

/// Decode an exact ConsoleService size message.
///
/// The protocol carries a tag followed by little-endian `rows` and `columns`.
/// Rejecting trailing bytes keeps an unrelated control message from being mistaken for
/// a resize event.
pub fn decode_control(bytes: &[u8]) -> Option<TerminalControl> {
	if let Some(size) = decode_size(bytes, WINSIZE_REPLY) {
		return Some(TerminalControl::InitialSize(size));
	}
	decode_size(bytes, RESIZE_EVENT).map(TerminalControl::Resized)
}

fn decode_size(bytes: &[u8], tag: &[u8]) -> Option<TerminalSize> {
	if bytes.len() != tag.len() + 4 || !bytes.starts_with(tag) {
		return None;
	}
	let offset = tag.len();
	let rows = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
	let columns = u16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]]);
	Some(TerminalSize::new(rows, columns))
}

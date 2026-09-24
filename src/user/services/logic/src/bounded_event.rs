//! ONE READER'S BOUNDED QUEUE OF ORDERED EVENTS, for any source: what is admitted, what number it gets, and
//! how the stream ends when it cannot go on.
//!
//! GENERIC ON PURPOSE. Nothing here knows what an event is - a MIDI chunk, a key, anything with a host
//! receipt time - and nothing here reaches `rt` or IPC; the caller says how many encoded bytes each event
//! costs, and the queue holds it to both of its bounds.
//!
//! TWO BOUNDS, BOTH CHECKED AT ADMISSION: 256 events and 32 kB of encoded storage, integrity records
//! included. An event that does not fit ENDS THE STREAM with `overflow`: everything queued is discarded -
//! a reader must not be shown a sequence that looks continuous across a loss - and the end is kept apart
//! from the queue, so it is readable however full the queue was.
//!
//! NUMBERS IN OBSERVATION ORDER. Every admitted event takes the next sequence number, in the order it was
//! pushed; the queue never sorts. The sequence never wraps: a source that would need `u64::MAX` ends with
//! `exhausted` instead.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub const MAX_EVENTS: usize = 256;
pub const MAX_BYTES: usize = 32 * 1024;

/// Why a stream ended - the same reasons `liber:event@1` names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
	Overflow,
	SourceDiscontinuity,
	Removed,
	Revoked,
	Exhausted,
	Stopped,
	Shutdown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct End {
	pub reason: Reason,
	pub next_sequence: u64,
}

/// What one pull returns: events in order, and the end once nothing is left before it.
#[derive(Debug, PartialEq, Eq)]
pub struct Pulled<T> {
	pub events: Vec<(u64, T)>,
	pub end: Option<End>,
}

pub struct Queue<T> {
	events: VecDeque<(u64, T, usize)>,
	bytes: usize,
	next_sequence: u64,
	end: Option<End>,
}

impl<T> Default for Queue<T> {
	fn default() -> Self {
		Self::new()
	}
}

impl<T> Queue<T> {
	pub fn new() -> Self {
		Self { events: VecDeque::new(), bytes: 0, next_sequence: 0, end: None }
	}

	pub fn len(&self) -> usize {
		self.events.len()
	}

	pub fn is_empty(&self) -> bool {
		self.events.is_empty()
	}

	pub fn bytes(&self) -> usize {
		self.bytes
	}

	pub fn next_sequence(&self) -> u64 {
		self.next_sequence
	}

	pub fn end(&self) -> Option<End> {
		self.end
	}

	/// Admit one event costing `bytes` encoded. Its sequence number, or the end the stream reached -
	/// already, or now, because this event did not fit.
	pub fn push(&mut self, event: T, bytes: usize) -> Result<u64, End> {
		if let Some(end) = self.end {
			return Err(end);
		}
		if self.next_sequence == u64::MAX {
			return Err(self.terminate(Reason::Exhausted));
		}
		if self.events.len() >= MAX_EVENTS || self.bytes + bytes > MAX_BYTES {
			return Err(self.terminate(Reason::Overflow));
		}
		let sequence = self.next_sequence;
		self.next_sequence += 1;
		self.bytes += bytes;
		self.events.push_back((sequence, event, bytes));
		Ok(sequence)
	}

	/// End the stream: every queued event is discarded, and the end is kept for the next read. The first
	/// end stands; a later one does not replace it.
	pub fn terminate(&mut self, reason: Reason) -> End {
		if let Some(end) = self.end {
			return end;
		}
		self.events.clear();
		self.bytes = 0;
		let end = End { reason, next_sequence: self.next_sequence };
		self.end = Some(end);
		end
	}

	/// At most `max` events in order; the end only once nothing is queued before it.
	pub fn pull(&mut self, max: usize) -> Pulled<T> {
		let mut events = Vec::new();
		while events.len() < max {
			let Some((sequence, event, bytes)) = self.events.pop_front() else { break };
			self.bytes -= bytes;
			events.push((sequence, event));
		}
		let end = if self.events.is_empty() { self.end } else { None };
		Pulled { events, end }
	}
}

#[cfg(test)]
mod tests;

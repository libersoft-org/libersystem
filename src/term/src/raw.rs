// A non-graphical consumer of the byte stream (L1): it records the raw bytes a program
// emits to a terminal - text and ANSI control codes alike - exactly as sent, before the L2
// grid model parses them. It is the tap the stream is forked into alongside the model, the
// foundation for forwarding a session (a future ssh/telnet) or capturing it to a file (the
// `script` tool). Parallel to the L2 `TextSink`, one layer lower: `TextSink` reads the
// parsed grid, `RawSink` reads the unparsed stream.

use alloc::vec::Vec;

pub struct RawSink {
	out: Vec<u8>,
	// How much of `out` has already been consumed.
	//
	// `consume` was `drain(..n)`, which memmoves the whole remainder on every call - and the console
	// drains this in transmit-ring-sized pieces every frame, so a backlog was copied once per piece
	// rather than once. A read offset makes a drain O(n) in what it drops; the front is reclaimed
	// only when it is worth reclaiming, so the buffer does not grow without bound either.
	taken: usize,
}

// Reclaim the consumed front once it is both large in absolute terms and most of the buffer, so a
// steady drain compacts about once per buffer-full rather than on every call.
const COMPACT_BYTES: usize = 4096;

impl RawSink {
	pub fn new() -> RawSink {
		RawSink { out: Vec::new(), taken: 0 }
	}

	// Record a chunk of the stream verbatim, exactly as emitted.
	pub fn feed(&mut self, bytes: &[u8]) {
		self.out.extend_from_slice(bytes);
	}

	// The stream captured so far.
	pub fn as_bytes(&self) -> &[u8] {
		&self.out[self.taken..]
	}

	// Drop the oldest `n` captured bytes - a downstream consumer draining the stream
	// in bounded slices (e.g. as much as a transmit ring accepted) removes what it
	// took and leaves the rest for a later pass.
	pub fn consume(&mut self, n: usize) {
		self.taken += n.min(self.out.len() - self.taken);
		if self.taken == self.out.len() {
			// Fully drained, which is the common ending: reset rather than copy.
			self.out.clear();
			self.taken = 0;
		} else if self.taken >= COMPACT_BYTES && self.taken * 2 >= self.out.len() {
			self.out.drain(..self.taken);
			self.taken = 0;
		}
	}

	// True until anything has been fed (or since the last `clear`).
	pub fn is_empty(&self) -> bool {
		self.taken == self.out.len()
	}

	// Drop the captured stream, e.g. after draining it to a downstream consumer.
	pub fn clear(&mut self) {
		self.out.clear();
		self.taken = 0;
	}
}

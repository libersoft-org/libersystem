// A non-graphical consumer of the byte stream (L1): it records the raw bytes a program
// emits to a terminal - text and ANSI control codes alike - exactly as sent, before the L2
// grid model parses them. It is the tap the stream is forked into alongside the model, the
// foundation for forwarding a session (a future ssh/telnet) or capturing it to a file (the
// `script` tool). Parallel to the L2 `TextSink`, one layer lower: `TextSink` reads the
// parsed grid, `RawSink` reads the unparsed stream.

use alloc::vec::Vec;

pub struct RawSink {
	out: Vec<u8>,
}

impl RawSink {
	pub fn new() -> RawSink {
		RawSink { out: Vec::new() }
	}

	// Record a chunk of the stream verbatim, exactly as emitted.
	pub fn feed(&mut self, bytes: &[u8]) {
		self.out.extend_from_slice(bytes);
	}

	// The stream captured so far.
	pub fn as_bytes(&self) -> &[u8] {
		&self.out
	}

	// Drop the oldest `n` captured bytes - a downstream consumer draining the stream
	// in bounded slices (e.g. as much as a transmit ring accepted) removes what it
	// took and leaves the rest for a later pass.
	pub fn consume(&mut self, n: usize) {
		self.out.drain(..n.min(self.out.len()));
	}

	// True until anything has been fed (or since the last `clear`).
	pub fn is_empty(&self) -> bool {
		self.out.is_empty()
	}

	// Drop the captured stream, e.g. after draining it to a downstream consumer.
	pub fn clear(&mut self) {
		self.out.clear();
	}
}

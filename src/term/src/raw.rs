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
	// The most pending bytes this sink will hold. `feed` trims to it BEFORE allocating, so a
	// producer that outruns its drain costs a bounded buffer rather than an unbounded one. Without
	// it the cap lived in the caller and was applied AFTER the growth it was meant to prevent, so a
	// single large chunk allocated past the cap first and was trimmed back afterwards.
	limit: usize,
}

// Reclaim the consumed front once it is both large in absolute terms and most of the buffer, so a
// steady drain compacts about once per buffer-full rather than on every call.
const COMPACT_BYTES: usize = 4096;

// Backlog cap for `new`. A debug mirror wants the newest output, so the bound is generous enough
// that an ordinary burst is never clipped and small enough to stay a rounding error against RAM.
pub const DEFAULT_LIMIT: usize = 32768;

impl RawSink {
	pub fn new() -> RawSink {
		RawSink::with_limit(DEFAULT_LIMIT)
	}

	// A sink holding at most `limit` pending bytes.
	pub fn with_limit(limit: usize) -> RawSink {
		RawSink { out: Vec::new(), taken: 0, limit: limit.max(1) }
	}

	// Record a chunk of the stream verbatim, exactly as emitted.
	//
	// Returns false when the stream is no longer verbatim - because the backlog cap dropped older
	// bytes, or because the allocation failed and this chunk was not recorded at all. A caller
	// mirroring a session marks the gap; it must not silently present a spliced stream as whole.
	#[must_use]
	pub fn feed(&mut self, bytes: &[u8]) -> bool {
		let mut whole = true;
		// A chunk larger than the whole cap keeps its tail: the newest bytes are the ones worth
		// having, and this happens before any allocation rather than after it.
		let bytes = if bytes.len() > self.limit {
			whole = false;
			&bytes[bytes.len() - self.limit..]
		} else {
			bytes
		};
		let pending = self.out.len() - self.taken;
		if pending + bytes.len() > self.limit {
			self.consume(pending + bytes.len() - self.limit);
			whole = false;
			// `consume` reclaims the front only when that is cheap. Here the point is the bound, so
			// force it: otherwise the backing vector keeps growing while `pending` stays capped.
			if self.taken > 0 {
				self.out.drain(..self.taken);
				self.taken = 0;
			}
		}
		if self.out.try_reserve(bytes.len()).is_err() {
			return false;
		}
		self.out.extend_from_slice(bytes);
		whole
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

	// The backing capacity, so a test can assert the bound holds for the BUFFER and not only for
	// the pending count.
	#[cfg(test)]
	pub fn capacity_for_test(&self) -> usize {
		self.out.capacity()
	}

	// Drop the captured stream, e.g. after draining it to a downstream consumer.
	pub fn clear(&mut self) {
		self.out.clear();
		self.taken = 0;
	}
}

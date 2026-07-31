// The byte-stream contract: one discipline for stdio, pipeline edges and storage adapters.
//
// This defines almost no mechanism, and that is the point. A Channel already gives every
// property a pipe needs, and each was built for a different reason before pipelines existed:
//
//   - bounded queue depth: an endpoint's inbox holds CHANNEL_QUEUE_DEFAULT (64) messages and a
//     send past that reports Full, so a producer cannot outrun a consumer without noticing;
//   - bounded queued BYTES: every queued message is charged to the sender's Domain IPC quota
//     until it is taken, so a slow stage cannot make another Domain allocate without bound -
//     which is exactly what this contract is required to guarantee;
//   - peer close is observable at both ends, which is EOF in one direction and broken pipe in
//     the other.
//
// So what is written here is the CONVENTION on top: how large a chunk is, what a message means,
// and how an error that is not an ordinary end-of-stream is expressed. Inventing a second
// queueing mechanism beside the one the kernel already accounts would mean two answers to "how
// much memory can a stalled pipeline hold", and the kernel's is the one Domains enforce.

use crate::{ERR_PEER_CLOSED, Received, channel_peek, close, recv_blocking, send_blocking, try_send};

// The largest payload one chunk carries. Sized to fill a channel message without forcing a
// producer to split ordinary output: a terminal write, a line of text and a filesystem block
// all fit in one, so the common case is one message per write rather than a loop.
//
// A stream is a sequence of chunks, never a stream of bytes with its own framing. The chunk IS
// the frame, because the channel already delivers messages whole and in order - re-framing a
// byte stream on top of an ordered message queue would be work that buys nothing.
pub const MAX_CHUNK: usize = 4096;

// The most bytes that can be in flight on one edge before a writer blocks: the queue depth
// times the chunk size. Stated so a pipeline's worst-case memory is a number somebody can
// reason about rather than a property of whichever code path happens to run.
pub const MAX_IN_FLIGHT: usize = 64 * MAX_CHUNK;

// What a read returned.
#[derive(Debug, PartialEq, Eq)]
pub enum Chunk {
	// Bytes to process. Never empty: a zero-length write is not an end-of-stream marker here,
	// because conflating "nothing to say" with "nothing more ever" is how a stream ends early
	// on an empty line.
	Data(usize),
	// The writer closed. The ordinary end of a stream, and the only one a consumer should treat
	// as success.
	End,
	// The writer is reporting that it could not finish - an adapter that lost its backing
	// store, a stage that faulted mid-output. Distinct from `End` because a consumer that
	// cannot tell them apart will publish a truncated result as a complete one, which is the
	// failure the transactional writer exists to prevent.
	Failed,
}

// A stream error frame. One byte, chosen so it cannot be mistaken for data: an ordinary chunk
// is only ever sent with a non-empty payload, so an empty message is unambiguous, and the tag
// distinguishes a deliberate failure from a peer that merely vanished.
const FAILED_TAG: &[u8] = b"!";

// The write half of one stream edge.
pub struct Writer {
	channel: u64,
	closed: bool,
}

impl Writer {
	pub fn new(channel: u64) -> Self {
		Self { channel, closed: false }
	}

	// Write every byte, splitting into chunks and blocking while the consumer is behind. The
	// block IS the backpressure: the kernel refuses the send once the inbox is full or the
	// Domain's queue quota is spent, and waiting there is what keeps a fast producer from
	// turning into unbounded memory somewhere else.
	//
	// Returns false once the far end is gone - the broken-pipe signal. A caller that keeps
	// writing after that is writing into nothing, so this reports rather than pretends.
	pub unsafe fn write(&mut self, bytes: &[u8]) -> bool {
		if self.closed {
			return false;
		}
		for chunk in bytes.chunks(MAX_CHUNK) {
			if !unsafe { send_blocking(self.channel, chunk, 0) } {
				return false;
			}
		}
		true
	}

	// End the stream reporting failure rather than completion. Idempotent, and it deliberately
	// uses a non-blocking send: a writer that is failing must not block behind a consumer that
	// has stopped reading, or a broken stage would hang instead of reporting.
	pub unsafe fn fail(&mut self) {
		if !self.closed {
			unsafe { try_send(self.channel, FAILED_TAG, 0) };
			self.close();
		}
	}

	// End the stream normally. The close is what the reader sees as `End`; idempotent, because
	// a caller unwinding through several layers should not have to track whether it already
	// closed.
	pub fn close(&mut self) {
		if !self.closed {
			self.closed = true;
			unsafe { close(self.channel) };
		}
	}
}

impl Drop for Writer {
	// Dropping a writer ends its stream. Without this a stage that returned early would leave
	// its consumer waiting for an EOF that never comes, which is a pipeline that hangs instead
	// of finishing.
	fn drop(&mut self) {
		self.close();
	}
}

// The read half of one stream edge.
pub struct Reader {
	channel: u64,
}

impl Reader {
	pub fn new(channel: u64) -> Self {
		Self { channel }
	}

	// Read one chunk into `buf`, blocking until there is one. `buf` should be MAX_CHUNK bytes;
	// a shorter one truncates, which the caller sees as a shorter `Data`.
	pub unsafe fn read(&mut self, buf: &mut [u8]) -> Chunk {
		match unsafe { recv_blocking(self.channel, buf) } {
			Received::Message { len, .. } if len == FAILED_TAG.len() && buf.starts_with(FAILED_TAG) => Chunk::Failed,
			Received::Message { len, .. } if len > 0 => Chunk::Data(len),
			// A zero-length message is not data and not a failure report; treating it as the
			// end would let an empty write truncate a stream, so it is simply skipped.
			Received::Message { .. } => unsafe { self.read(buf) },
			Received::Closed => Chunk::End,
		}
	}

	// Stop reading, which the producer observes as a broken pipe at its next write. This is how
	// an early-exiting consumer - `head` taking ten lines of a huge file - tells the stage
	// upstream to stop rather than letting it produce output nobody will take.
	pub fn close(self) {
		unsafe { close(self.channel) };
	}

	// Whether the far end is already gone AND nothing is left to read, without consuming
	// anything. Both halves matter: a writer that closed after producing output leaves data
	// behind it, and reporting that stream as gone would discard bytes the consumer is owed.
	pub unsafe fn writer_gone(&self) -> bool {
		unsafe { channel_peek(self.channel) == ERR_PEER_CLOSED }
	}
}

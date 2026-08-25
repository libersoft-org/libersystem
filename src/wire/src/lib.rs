//! Transport-independent wire primitives for generated LSIDL codecs.
//!
//! All integers are little-endian and there is no padding or alignment, so the
//! byte layout is exactly as written. Encoding writes into a caller buffer and
//! returns `None` on overflow; decoding returns `None` on a short or malformed
//! buffer.
//!
//! THE FIXED-BUFFER BINARY PATH ALLOCATES NOTHING. That is the property worth having and it is
//! narrower than "everything is heap-free except `string_lp`", which this header used to claim
//! above a `VecWriter`, a JSON renderer returning `String` and a CBOR renderer building a `Vec`.
//! The owned decoding and rendering helpers may allocate; `SliceWriter`, `Reader` over a borrowed
//! buffer and `Handles` do not.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

// A byte sink the generated codecs write into. The default methods build the
// little-endian and length-prefixed encodings on top of `put`, so a concrete sink
// only implements `put`.
pub trait Sink {
	// Append one byte, or return None if the sink is full.
	fn put(&mut self, b: u8) -> Option<()>;

	// Record one out-of-band handle to transfer with this message, refusing past `MAX_HANDLES`.
	//
	// THE KERNEL ALLOWS FOUR. This said "at most one per message, matching the kernel channel's
	// single-handle limit", which was the opposite of true - the syscalls exist, `set_handle` itself
	// accepts four, and a trait comment is the first thing a reader builds their model from.
	fn set_handle(&mut self, h: u64) -> Option<()>;

	// The default is byte at a time; the two concrete sinks below override it with a bulk copy.
	//
	// This is the codec's hot path - a multi-kilobyte string goes through it - and the default was
	// the only implementation. It stays as the DEFAULT because a new sink should work before it is
	// fast, and because `ipc-client` is checked against an exact list of runtime imports: the
	// overrides are where a `memcpy` may appear, and they are the two sinks that already carry one.
	fn raw(&mut self, s: &[u8]) -> Option<()> {
		for &b in s {
			self.put(b)?;
		}
		Some(())
	}

	fn boolean(&mut self, v: bool) -> Option<()> {
		self.put(v as u8)
	}

	fn u8(&mut self, v: u8) -> Option<()> {
		self.put(v)
	}

	fn u16(&mut self, v: u16) -> Option<()> {
		self.raw(&v.to_le_bytes())
	}

	fn u32(&mut self, v: u32) -> Option<()> {
		self.raw(&v.to_le_bytes())
	}

	fn u64(&mut self, v: u64) -> Option<()> {
		self.raw(&v.to_le_bytes())
	}

	fn i8(&mut self, v: i8) -> Option<()> {
		self.raw(&v.to_le_bytes())
	}

	fn i16(&mut self, v: i16) -> Option<()> {
		self.raw(&v.to_le_bytes())
	}

	fn i32(&mut self, v: i32) -> Option<()> {
		self.raw(&v.to_le_bytes())
	}

	fn i64(&mut self, v: i64) -> Option<()> {
		self.raw(&v.to_le_bytes())
	}

	fn f32(&mut self, v: f32) -> Option<()> {
		self.raw(&v.to_le_bytes())
	}

	fn f64(&mut self, v: f64) -> Option<()> {
		self.raw(&v.to_le_bytes())
	}

	// A length-prefixed byte string: `[len u16][bytes]`. Refuses strings longer
	// than `u16::MAX`.
	fn bytes_lp(&mut self, s: &[u8]) -> Option<()> {
		if s.len() > u16::MAX as usize {
			return None;
		}
		self.u16(s.len() as u16)?;
		self.raw(s)
	}
}

// A sink over a fixed caller buffer; `put` fails once the buffer is full. This is
// the heap-free path the kernel and IPC send use.
pub struct SliceWriter<'a> {
	buf: &'a mut [u8],
	pos: usize,
	handles: [u64; MAX_HANDLES],
	handle_count: usize,
}

impl<'a> SliceWriter<'a> {
	pub fn new(buf: &'a mut [u8]) -> SliceWriter<'a> {
		SliceWriter { buf, pos: 0, handles: [0; MAX_HANDLES], handle_count: 0 }
	}

	// The number of bytes written so far.
	pub fn pos(&self) -> usize {
		self.pos
	}

	// The first handle recorded during encoding (0 = none), for the callers that only ever
	// send one.
	pub fn handle(&self) -> u64 {
		self.handles[0]
	}

	// Every handle recorded, in encoding order.
	pub fn handles(&self) -> &[u64] {
		&self.handles[..self.handle_count]
	}

	pub fn has_handle(&self) -> bool {
		self.handle_count > 0
	}

	// The finished message's length, consuming the writer - AND ONLY WHEN THERE IS NOTHING ELSE TO
	// CARRY.
	//
	// The counterpart of `VecWriter::into_inner`, and the same argument (WIRE-001): a caller that
	// takes the LENGTH out of a writer holding a capability has taken half a message and dropped the
	// live half. The generator emitted `if w.has_handle() { return None }` at each `encode` for
	// exactly this, which made it a property of the generator rather than of the codec; it is here
	// now, so a hand-written encoder inherits it too.
	//
	// `pos()` stays and borrows: a dispatch that goes on to hand its handles over separately - the
	// serve loops do - is asking a live writer how far it has got, which is a different question.
	pub fn finish(self) -> Option<usize> {
		if self.handle_count != 0 {
			return None;
		}
		Some(self.pos)
	}

	// Rewind to an empty buffer, dropping anything written and any recorded handle,
	// so a failed encode can be replaced in place - the dispatch overflow fallback.
	pub fn reset(&mut self) {
		self.pos = 0;
		self.handles = [0; MAX_HANDLES];
		self.handle_count = 0;
	}
}

impl<'a> Sink for SliceWriter<'a> {
	fn put(&mut self, b: u8) -> Option<()> {
		*self.buf.get_mut(self.pos)? = b;
		self.pos += 1;
		Some(())
	}

	// One bounds check and one copy, rather than one of each per byte.
	fn raw(&mut self, s: &[u8]) -> Option<()> {
		let end = self.pos.checked_add(s.len())?;
		self.buf.get_mut(self.pos..end)?.copy_from_slice(s);
		self.pos = end;
		Some(())
	}

	// Append a handle to be transferred with this message. Refuses past the bound rather than
	// dropping one, because a silently missing capability is a stage wired to nothing.
	fn set_handle(&mut self, h: u64) -> Option<()> {
		if self.handle_count >= MAX_HANDLES {
			return None;
		}
		self.handles[self.handle_count] = h;
		self.handle_count += 1;
		Some(())
	}
}

// A growable sink, used by the generated clients to build a request without
// sizing a buffer up front.
#[derive(Default)]
pub struct VecWriter {
	buf: Vec<u8>,
	handles: [u64; MAX_HANDLES],
	handle_count: usize,
}

impl VecWriter {
	pub fn new() -> VecWriter {
		VecWriter { buf: Vec::new(), handles: [0; MAX_HANDLES], handle_count: 0 }
	}

	// The first handle recorded during encoding (0 = none).
	pub fn handle(&self) -> u64 {
		self.handles[0]
	}

	// Every handle recorded, in encoding order.
	pub fn handles(&self) -> &[u64] {
		&self.handles[..self.handle_count]
	}

	// The bytes written so far, consuming the writer - AND ONLY WHEN THERE IS NOTHING ELSE TO
	// CARRY.
	//
	// This returned `self.buf` unconditionally, so a writer that had recorded a capability handed
	// over the bytes and dropped the record: the caller got half a message, and the half it lost was
	// the live one. That the generated encoders did not do this was a property of the GENERATOR -
	// each `encode_vec` it emitted carried its own `if !w.handles().is_empty() { return None }` -
	// and a hand-written encoder got no such line. `wire`'s own test for it was named
	// "a vec writer that records a handle cannot be read as bytes alone" and asserted that it could.
	// That is WIRE-001 in one function: ownership stated in prose, enforced by whoever remembered.
	//
	// Now the refusal is here, once, and every caller inherits it. `into_message` is the way to get
	// both halves, and there is no way to get one.
	pub fn into_inner(self) -> Option<Vec<u8>> {
		if self.handle_count != 0 {
			return None;
		}
		Some(self.buf)
	}

	// Both halves of the message, together. Infallible by construction: the writer's own array is
	// `MAX_HANDLES` wide and `set_handle` refuses past it, so the list it accumulated always fits
	// the list it becomes.
	pub fn into_message(self) -> (Vec<u8>, Handles) {
		let mut handles = Handles::new();
		let mut index = 0;
		// Element at a time rather than `try_from_slice`, to stay off `memcpy` - `ipc-client` is
		// checked against an exact list of runtime imports and this is on its path.
		while index < self.handle_count {
			handles.list[index] = self.handles[index];
			index += 1;
		}
		handles.count = self.handle_count;
		(self.buf, handles)
	}
}

impl Sink for VecWriter {
	// FALLIBLE GROWTH. `Vec::push` aborts on an allocation failure, and everywhere else this
	// runtime works to make attacker-sized allocations refusable - `Reader::string_lp` does exactly
	// that. `None` here means the same thing it means for `SliceWriter`: the sink could not take
	// the byte.
	fn put(&mut self, b: u8) -> Option<()> {
		self.buf.try_reserve(1).ok()?;
		self.buf.push(b);
		Some(())
	}

	// One reservation and one extend for the whole slice.
	fn raw(&mut self, s: &[u8]) -> Option<()> {
		self.buf.try_reserve(s.len()).ok()?;
		self.buf.extend_from_slice(s);
		Some(())
	}

	fn set_handle(&mut self, h: u64) -> Option<()> {
		if self.handle_count >= MAX_HANDLES {
			return None;
		}
		self.handles[self.handle_count] = h;
		self.handle_count += 1;
		Some(())
	}
}

// Why a call did not produce a reply.
//
// `call` returned `Option<Vec<u8>>`, so peer closure, a refused send, a timeout, an allocation
// failure and a malformed reply were ONE VALUE - and the decision a client has to make after a
// failed call depends on telling them apart. `PeerClosed` means the service is gone and a retry on
// this channel cannot succeed; `SendRefused` means the request never left, so retrying is safe;
// `TimedOut` means the request MAY have been received and acted on, so retrying is only safe for an
// idempotent operation. Collapsing those three into `None` made every caller guess, and the
// conservative guess - do not retry - is wrong for the one case where retrying is free.
//
// Service-level failures are NOT here: they stay in the generated `result<_, error>` where the
// schema already puts them. This is about the pipe, not about what travelled through it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportError {
	// No channel to the service - it was never resolved, or the handle is zero.
	NoRoute,
	// The request was refused by the kernel and did not leave. Retrying is safe.
	SendRefused,
	// The peer closed the channel. A retry on this channel cannot succeed.
	PeerClosed,
	// The deadline passed with no reply. The request may have been received and acted on.
	TimedOut,
	// The receive itself was refused, but the peer may still be there.
	ReceiveFailed,
	// The reply could not be held.
	NoMemory,
	// The reply arrived but does not obey the framing rules - a wrong correlation id, unexpected
	// capabilities, trailing bytes.
	Malformed,
}

// A request/reply channel the generated clients call over. The userspace impl
// sends on a channel and blocks for the reply; tests use an in-memory loopback.
pub trait Transport {
	// Send a request (bytes plus the capabilities it transfers, in encoding order) and receive
	// the reply the same way. A list rather than one handle because a single op may hand over
	// several - a pipeline stage needs its stdin AND its stdout, and one slot made that
	// impossible however the interface was written.
	// The reply's capabilities are written through `reply_handles` rather than returned beside
	// the bytes: returning a `Handles` inside a tuple is a 40-byte move that aarch64 codegen
	// performs with a `memcpy` call, and `ipc-client` is held to an exact list of runtime
	// imports that should not grow for a calling convention detail.
	//
	// A DEADLINE, because a live peer that accepts a request and never replies used to block the
	// caller forever: the trait could not express "give up". It is absolute rather than a duration
	// so a retry loop cannot extend its own budget by restarting the clock, and `0` means no
	// deadline for the transports - the host loopbacks - that answer immediately by construction.
	fn call(&mut self, request: &[u8], request_handles: &[u64], reply_handles: &mut Handles, deadline: u64) -> Result<Vec<u8>, TransportError>;

	// Release reply capabilities that could not be decoded or were not expected by the schema.
	// Host test transports need no action; the runtime closes them. Takes the whole list: a
	// reply refused after decoding may carry more than one, and closing only the first leaks
	// the rest.
	//
	// NO DEFAULT BODY. It had one - an empty method - so a future transport whose author simply did
	// not implement it compiled silently and leaked every capability from every malformed reply.
	// Resource ownership is not an opt-in; a test transport that genuinely needs no action writes
	// the empty body itself and says so.
	fn discard_handles(&mut self, handles: &[u64]);
}

// A cursor that reads from a borrowed buffer.
// The most transferred handles one message carries. The kernel has always allowed a message a
// list of capabilities - `Message { caps: Vec<Capability> }` - and it was this layer that
// admitted exactly one, which is what stopped a pipeline stage from being handed its stdin and
// stdout together. Four is stdin, stdout, stderr and one spare: bounded, because everything
// here is, and a fixed array keeps this no_std and allocation-free.
//
// THE KERNEL'S NUMBER, not a copy of it. This was `pub const MAX_HANDLES: usize = 4` beside a
// `pub use abi::{PROTOCOL_INFO_OP, TYPED_OP_MAX}` carrying a comment about not copying values -
// the file did the right thing for two constants and the wrong thing for the third. The day
// somebody raises the kernel's limit is the day that matters.
pub use abi::MAX_MESSAGE_CAPS as MAX_HANDLES;

// Re-exported so generated code, which reaches this crate as `crate::codec` but never `abi`,
// names the SAME constant the runtime and the validator use rather than a copy of its value.
pub use abi::{PROTOCOL_INFO_OP, TYPED_OP_MAX};

// A bounded, allocation-free list of transferred handles: what one message carries.
//
// It exists so the signatures that pass handles around stay readable. `dispatch` used to take
// `request_handle: &mut u64` and `reply_handle: &mut u64`, and widening those to a list as a
// pair of parameters each (an array plus a count) would have doubled the parameter count at
// every one of the 22 places that call a generated dispatch. One value carries both.
//
// NON-OWNING TRANSPORT METADATA, and `Copy` for that reason.
//
// Copying the numbers does not duplicate the capabilities, so two copies can each believe they own
// them - which with handle reuse is the shape of an ABA bug. The alternative considered was a
// move-only envelope with a `Drop` that closes; it was not taken, because this type is read by the
// kernel-facing syscall wrappers, by the generated dispatch and by the serve loops, and a `Drop`
// that closes would fire on every one of those borrows-by-value. What owns a capability in this
// system is the code path, stated explicitly at each hand-off; this type carries the numbers
// between them and says so here rather than implying otherwise by being move-only.
#[derive(Clone, Copy)]
pub struct Handles {
	list: [u64; MAX_HANDLES],
	count: usize,
}

impl Default for Handles {
	fn default() -> Self {
		Handles::new()
	}
}

impl Handles {
	#[inline]
	pub const fn new() -> Handles {
		Handles { list: [0; MAX_HANDLES], count: 0 }
	}

	// Every handle given, in the order given - which is encoding order, so a stage's stdin and
	// stdout keep the positions their encoder gave them. `None` past the bound.
	//
	// REFUSES RATHER THAN TRUNCATING. This was `handles.iter().take(MAX_HANDLES)`, and for ordinary
	// data keeping the first four of five is a defensible API. For CAPABILITIES the fifth is not
	// lost information - it is a live kernel object that nothing then closes. `set_handle` and
	// `push` already refuse past the bound; these are the same question with the same answer.
	#[inline]
	pub fn try_from_slice(handles: &[u64]) -> Option<Handles> {
		if handles.len() > MAX_HANDLES {
			return None;
		}
		let mut built = Handles::new();
		// Copied element by element rather than with `copy_from_slice`, and read back through
		// `get` rather than a range index, so this carries no call to `memcpy` and no slice
		// bounds-check panic path. `ipc-client` is checked against an exact list of runtime
		// imports, and a list this small should not add either.
		for handle in handles.iter() {
			built.list[built.count] = *handle;
			built.count += 1;
		}
		Some(built)
	}

	// The first `count` of a raw array, for callers that receive one from a syscall. Takes a
	// reference and copies element by element: passing the array by value is a 32-byte move
	// that aarch64 codegen turns into a `memcpy` call, and `ipc-client` is checked against an
	// exact list of runtime imports that should not have to grow for this.
	//
	// `None` for a count past the array, for the same reason `try_from_slice` refuses: the caller
	// is reporting what a syscall said it received, and clamping it silently would lose a live
	// capability.
	#[inline]
	pub fn try_from_array(list: &[u64; MAX_HANDLES], count: usize) -> Option<Handles> {
		if count > MAX_HANDLES {
			return None;
		}
		let mut built = Handles::new();
		while built.count < count {
			built.list[built.count] = list[built.count];
			built.count += 1;
		}
		Some(built)
	}

	#[inline]
	pub fn as_slice(&self) -> &[u64] {
		match self.list.get(..self.count) {
			Some(handles) => handles,
			None => &[],
		}
	}

	#[inline]
	pub fn len(&self) -> usize {
		self.count
	}

	#[inline]
	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	// The first handle, or 0 when none was sent - for the callers that only ever expect one.
	#[inline]
	pub fn first(&self) -> u64 {
		if self.count == 0 { 0 } else { self.list[0] }
	}

	// Append one, refusing past the bound rather than dropping it: a silently missing
	// capability is a stage wired to nothing.
	#[inline]
	pub fn push(&mut self, handle: u64) -> Option<()> {
		if self.count >= MAX_HANDLES {
			return None;
		}
		self.list[self.count] = handle;
		self.count += 1;
		Some(())
	}

	// Remove and return the first capability, shifting the rest down; 0 when there is none.
	// The bootstrap sequences use this: they read one named capability per message and must
	// leave the list empty afterwards, so the serve loop does not close what was adopted.
	#[inline]
	pub fn take_first(&mut self) -> u64 {
		if self.count == 0 {
			return 0;
		}
		let first = self.list[0];
		let mut index = 1;
		while index < self.count {
			self.list[index - 1] = self.list[index];
			index += 1;
		}
		self.count -= 1;
		self.list[self.count] = 0;
		first
	}

	#[inline]
	pub fn clear(&mut self) {
		self.list = [0; MAX_HANDLES];
		self.count = 0;
	}
}

pub struct Reader<'a> {
	buf: &'a [u8],
	pos: usize,
	handles: [u64; MAX_HANDLES],
	count: usize,
	taken: usize,
}

impl<'a> Reader<'a> {
	pub fn new(buf: &'a [u8]) -> Reader<'a> {
		Reader { buf, pos: 0, handles: [0; MAX_HANDLES], count: 0, taken: 0 }
	}

	// A reader for a message that arrived with one out-of-band transferred handle.
	pub fn with_handle(buf: &'a [u8], handle: u64) -> Reader<'a> {
		let mut handles = [0u64; MAX_HANDLES];
		handles[0] = handle;
		Reader { buf, pos: 0, handles, count: 1, taken: 0 }
	}

	// A reader for a message that arrived with several. They are taken in the order the
	// encoder set them, which is the order the fields appear - so a stage's stdin and stdout
	// cannot be swapped by a decoder reading them in a different order than they were written.
	//
	// TAKES A `Handles`, NOT AN ARBITRARY SLICE. It used to take a slice and clamp with
	// `transferred.len().min(MAX_HANDLES)`, so handing it five capabilities silently dropped one -
	// and a dropped capability is a live kernel object nothing then closes. A `Handles` is already
	// bounded by construction, so the question cannot arise.
	pub fn with_handles(buf: &'a [u8], transferred: &Handles) -> Reader<'a> {
		let mut handles = [0u64; MAX_HANDLES];
		let taken = transferred.as_slice();
		handles[..taken.len()].copy_from_slice(taken);
		Reader { buf, pos: 0, handles, count: taken.len(), taken: 0 }
	}

	// The name the generator emits. Kept as an alias so a regeneration is not needed to rename it.
	pub fn with_handle_list(buf: &'a [u8], transferred: &Handles) -> Reader<'a> {
		Reader::with_handles(buf, transferred)
	}

	// The next transferred handle, in the order they were encoded. None once they are spent,
	// which is what a decoder expecting one that was not sent sees.
	pub fn take_handle(&mut self) -> Option<u64> {
		if self.taken >= self.count {
			return None;
		}
		let handle = self.handles[self.taken];
		self.taken += 1;
		Some(handle)
	}

	// Whether any transferred handle is still unclaimed. A dispatch checks this AFTER decoding
	// to refuse a message carrying more handles than its signature accounts for - a caller that
	// sent a capability nothing reads must not have it silently dropped.
	pub fn has_handle(&self) -> bool {
		self.taken < self.count
	}

	// The number of bytes consumed so far.
	pub fn pos(&self) -> usize {
		self.pos
	}

	fn take(&mut self, n: usize) -> Option<&'a [u8]> {
		let s = self.buf.get(self.pos..self.pos + n)?;
		self.pos += n;
		Some(s)
	}

	// ONE ENCODING PER VALUE. This was `Some(self.u8()? != 0)`, so 2 through 255 all decoded as
	// `true` - one logical value with 255 spellings, against a stated contract that a malformed
	// buffer answers `None`. Malleability with no purpose, and it starts costing the moment a frame
	// is hashed, compared or replayed.
	pub fn boolean(&mut self) -> Option<bool> {
		self.tag()
	}

	// THE DISCRIMINANT BYTE OF AN `option` OR A `result`, and the same rule as `boolean` because it
	// is the same rule.
	//
	// It was fixed for `bool` and left in three other spellings: the generator emitted
	// `if r.u8()? != 0 { Some(..) } else { None }` for `option`, the same shape for `result`, and a
	// third copy beside them. So a reply whose result tag was `0xff` decoded as `Ok` and the same
	// byte in an option decoded as `Some` - after the finding that named this exact malleability had
	// been closed. A rule that lives in four places is a rule that gets fixed in one of them.
	//
	// `false` is the zero tag - `None` / `Err`; `true` is one - `Some` / `Ok`. The names stay at the
	// call site, where they mean something.
	pub fn tag(&mut self) -> Option<bool> {
		match self.u8()? {
			0 => Some(false),
			1 => Some(true),
			_ => None,
		}
	}

	// EVERY BYTE AND EVERY CAPABILITY CONSUMED - called at the FRAMING boundary, not inside a
	// nested `read`.
	//
	// `decode(bytes)` was `T::read(&mut Reader::new(bytes))` and never asked whether the reader had
	// finished, so `01 02 03` and `01 02 03 DE AD BE EF` decoded to the same value. With extensible
	// records deliberately deferred, the current format is fixed and a trailing byte is a message
	// its writer and its reader disagree about.
	//
	// A nested `read` must NOT require the end of the buffer, which is why this is a separate call
	// rather than a check inside `take`.
	pub fn finish(&self) -> Option<()> {
		if self.pos != self.buf.len() || self.taken != self.count {
			return None;
		}
		Some(())
	}

	pub fn u8(&mut self) -> Option<u8> {
		Some(self.take(1)?[0])
	}

	pub fn u16(&mut self) -> Option<u16> {
		Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
	}

	pub fn u32(&mut self) -> Option<u32> {
		Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
	}

	pub fn u64(&mut self) -> Option<u64> {
		Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
	}

	pub fn i8(&mut self) -> Option<i8> {
		Some(i8::from_le_bytes(self.take(1)?.try_into().ok()?))
	}

	pub fn i16(&mut self) -> Option<i16> {
		Some(i16::from_le_bytes(self.take(2)?.try_into().ok()?))
	}

	pub fn i32(&mut self) -> Option<i32> {
		Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
	}

	pub fn i64(&mut self) -> Option<i64> {
		Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
	}

	pub fn f32(&mut self) -> Option<f32> {
		Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
	}

	pub fn f64(&mut self) -> Option<f64> {
		Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
	}

	pub fn bytes_lp(&mut self) -> Option<&'a [u8]> {
		let n = self.u16()? as usize;
		self.take(n)
	}

	// Fallible in its ALLOCATION as well as its parse. `bytes.to_vec()` aborts the process through
	// the allocation error handler when the heap is short, which turned a decoder into a second
	// way for a message to kill its receiver - the one place the transport had just been taught
	// not to. `None` covers both endings, and every caller already treats it as a failed frame.
	pub fn string_lp(&mut self) -> Option<String> {
		let bytes = self.bytes_lp()?;
		let mut owned: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
		owned.try_reserve_exact(bytes.len()).ok()?;
		owned.extend_from_slice(bytes);
		String::from_utf8(owned).ok()
	}
}

// A `buffer`: bulk payload carried zero-copy as a handle to a shared memory object
// (a MemoryObject / SharedBuffer) plus its byte length. The length travels in-stream
// and the handle out-of-band (the message's single transferred handle, like a
// `handle<R>`); the bytes themselves never cross the channel - the producer fills
// the memory object and the consumer maps it. A descriptor only: the create / map /
// read of the actual bytes is done by the application via the runtime syscalls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Buffer {
	pub handle: u64,
	pub len: u64,
}

// Append `s` to `out` as a JSON string literal: wrapped in quotes with the
// mandatory characters escaped. Used by the generated `to_json` renderers.
pub fn json_escape(s: &str, out: &mut String) {
	out.push('"');
	for c in s.chars() {
		match c {
			'"' => out.push_str("\\\""),
			'\\' => out.push_str("\\\\"),
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			'\t' => out.push_str("\\t"),
			c if (c as u32) < 0x20 => {
				let _ = write!(out, "\\u{:04x}", c as u32);
			}
			c => out.push(c),
		}
	}
	out.push('"');
}

// The JSON output modes a tool offers: `json` (the `--json` flag) renders the
// document indented and colored for a human, `json-min` (`--json-min`) prints the
// minified single-line form for a machine. Tools build the minified document
// either way and hand it to `render` last, so the two forms cannot drift.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JsonMode {
	Pretty,
	Min,
}

impl JsonMode {
	// The mode a normalized argument token selects, if any.
	pub fn parse(token: &[u8]) -> Option<JsonMode> {
		match token {
			b"json" => Some(JsonMode::Pretty),
			b"json-min" => Some(JsonMode::Min),
			_ => None,
		}
	}

	// Render a minified JSON document in this mode.
	pub fn render(self, min: String) -> String {
		match self {
			JsonMode::Pretty => json_pretty(&min, true),
			JsonMode::Min => min,
		}
	}
}

// Reformat a minified JSON document as an indented, optionally colored one - the
// shared renderer behind every tool's `--json` form (`--json-min` prints the
// minified document as produced). The tools keep building the compact form as
// their single source of truth; this walks it token by token (quote- and
// escape-aware, so brackets inside strings do not indent) and re-emits it with
// two-space indentation, a space after `:`, and ANSI colors when `color` is set:
// keys cyan, strings green, numbers yellow, `true`/`false`/`null` magenta.
// Malformed input is not diagnosed - the tokens are re-emitted as they come.
pub fn json_pretty(min: &str, color: bool) -> String {
	const KEY: &str = "\x1b[36m";
	const STR: &str = "\x1b[32m";
	const NUM: &str = "\x1b[33m";
	const LIT: &str = "\x1b[35m";
	const RESET: &str = "\x1b[0m";
	// `with_capacity` ABORTS on a short heap, and `min.len() * 2` is unchecked arithmetic over a
	// length that came from a service reply. Reserved fallibly and then grown, so a diagnostic
	// command on a low-memory machine renders less rather than killing the process it runs in.
	//
	// The binary writer beside this one has been fallible since it was written; the presentation
	// paths were not, and generated list rendering amplifies its input substantially through keys,
	// escaping, colour codes and indentation.
	let mut out = String::new();
	if out.try_reserve(min.len().saturating_mul(2)).is_err() {
		// Nothing rendered rather than a process gone. The caller has the minified form already -
		// that is what was passed in - so an empty pretty rendering is a degradation and not a loss.
		return String::new();
	}
	let bytes = min.as_bytes();
	let mut depth: usize = 0;
	let mut i: usize = 0;
	let indent = |out: &mut String, depth: usize| {
		out.push('\n');
		for _ in 0..depth {
			out.push_str("  ");
		}
	};
	while i < bytes.len() {
		match bytes[i] {
			b'{' | b'[' => {
				let close = if bytes[i] == b'{' { b'}' } else { b']' };
				out.push(bytes[i] as char);
				// keep an empty container on one line ("{}", "[]").
				if i + 1 < bytes.len() && bytes[i + 1] == close {
					out.push(close as char);
					i += 2;
					continue;
				}
				depth += 1;
				indent(&mut out, depth);
				i += 1;
			}
			b'}' | b']' => {
				depth = depth.saturating_sub(1);
				indent(&mut out, depth);
				out.push(bytes[i] as char);
				i += 1;
			}
			b',' => {
				out.push(',');
				indent(&mut out, depth);
				i += 1;
			}
			b':' => {
				out.push_str(": ");
				i += 1;
			}
			b'"' => {
				// the whole string literal, escapes included; a string followed by `:`
				// is a key.
				let start = i;
				i += 1;
				while i < bytes.len() {
					match bytes[i] {
						b'\\' => i += 2,
						b'"' => {
							i += 1;
							break;
						}
						_ => i += 1,
					}
				}
				// a truncated escape at end-of-input must not slice past the buffer.
				i = i.min(bytes.len());
				let is_key = i < bytes.len() && bytes[i] == b':';
				if color {
					out.push_str(if is_key { KEY } else { STR });
				}
				out.push_str(&min[start..i]);
				if color {
					out.push_str(RESET);
				}
			}
			c => {
				// a number, true/false/null, or stray whitespace (skipped - the input
				// is expected minified).
				if c == b' ' {
					i += 1;
					continue;
				}
				let start = i;
				while i < bytes.len() && !matches!(bytes[i], b',' | b'}' | b']' | b':' | b'"' | b'{' | b'[') {
					i += 1;
				}
				let token = &min[start..i];
				if color {
					out.push_str(if token.starts_with(|ch: char| ch.is_ascii_digit() || ch == '-') { NUM } else { LIT });
				}
				out.push_str(token);
				if color {
					out.push_str(RESET);
				}
			}
		}
	}
	out
}

// CBOR (RFC 8949) encoding primitives for the generated `to_cbor` renderers. The
// CBOR form is the binary analog of the JSON one: a record is a text-keyed map, an
// enum case is a text string, a `result` is a single-pair map (`ok` / `err`), an
// `option` is the value or `null`, a `list` is an array. Only definite-length
// encodings are emitted, each with the canonical shortest head, so the output is
// deterministic and round-trips with any conformant CBOR decoder.
pub mod cbor {
	use alloc::vec::Vec;

	// Write a major-type head: `(major << 5) | additional`, with `n` as the
	// argument in the shortest encoding (inline < 24, then 1/2/4/8 big-endian bytes).
	fn head(out: &mut Vec<u8>, major: u8, n: u64) {
		let mt = major << 5;
		if n < 24 {
			out.push(mt | n as u8);
		} else if n <= u8::MAX as u64 {
			out.push(mt | 24);
			out.push(n as u8);
		} else if n <= u16::MAX as u64 {
			out.push(mt | 25);
			out.extend_from_slice(&(n as u16).to_be_bytes());
		} else if n <= u32::MAX as u64 {
			out.push(mt | 26);
			out.extend_from_slice(&(n as u32).to_be_bytes());
		} else {
			out.push(mt | 27);
			out.extend_from_slice(&n.to_be_bytes());
		}
	}

	// An unsigned integer (major type 0).
	pub fn uint(out: &mut Vec<u8>, v: u64) {
		head(out, 0, v);
	}

	// A signed integer: a negative `v` is major type 1 over `-1 - v`.
	pub fn int(out: &mut Vec<u8>, v: i64) {
		if v < 0 {
			head(out, 1, (-1 - v) as u64);
		} else {
			head(out, 0, v as u64);
		}
	}

	// A boolean (major type 7 simple value `false` / `true`).
	pub fn boolean(out: &mut Vec<u8>, v: bool) {
		out.push(if v { 0xf5 } else { 0xf4 });
	}

	// The `null` simple value (major type 7).
	pub fn null(out: &mut Vec<u8>) {
		out.push(0xf6);
	}

	// An IEEE-754 single-precision float (major type 7).
	pub fn f32(out: &mut Vec<u8>, v: f32) {
		out.push(0xfa);
		out.extend_from_slice(&v.to_be_bytes());
	}

	// An IEEE-754 double-precision float (major type 7).
	pub fn f64(out: &mut Vec<u8>, v: f64) {
		out.push(0xfb);
		out.extend_from_slice(&v.to_be_bytes());
	}

	// A UTF-8 text string (major type 3).
	pub fn text(out: &mut Vec<u8>, s: &str) {
		head(out, 3, s.len() as u64);
		out.extend_from_slice(s.as_bytes());
	}

	// The head of a definite-length array of `len` items (major type 4); the items
	// follow.
	pub fn array(out: &mut Vec<u8>, len: usize) {
		head(out, 4, len as u64);
	}

	// The head of a definite-length map of `pairs` key/value pairs (major type 5);
	// the pairs follow.
	pub fn map(out: &mut Vec<u8>, pairs: usize) {
		head(out, 5, pairs as u64);
	}
}

// AUTHORITY A RECEIVED HANDLE MUST ALREADY CARRY (IDL-004).
//
// A schema can declare `@rights(...)` on a handle parameter, and until now nothing read it: the
// annotation reached the ABI signature and no generated code. So what a service was entitled to
// assume about a handle it was sent lived in prose, and held for exactly as long as every caller
// remembered - which is the shape of gap that is only discovered by something going wrong.
//
// The generated dispatch calls this before it calls the service. A handle the sender narrowed, or
// one of a type this parameter is not for, is refused with the schema's own error rather than handed
// over, so the declaration is the enforcement instead of a description of it.
//
// The runtime publishes the answer as one packed word because this crate has `abi` and nothing else
// - no syscall wrapper, on purpose, since the codec is linked in places where issuing one would be
// wrong. `u64::MAX` is an unknown handle and fails like any other missing authority.
unsafe extern "C" {
	fn liber_handle_authority(handle: u64) -> u64;
}

pub fn handle_carries(handle: u64, required_rights: u32, required_type: u64) -> bool {
	let authority = unsafe { liber_handle_authority(handle) };
	if authority == u64::MAX {
		return false;
	}
	let rights = authority as u32;
	let object_type = authority >> 32;
	if required_type != NO_REQUIRED_TYPE && object_type != required_type {
		return false;
	}
	rights & required_rights == required_rights
}

// A resource the schema names but the ABI has no type code for: the rights are still checked, the
// type is not, and that is said here rather than left as a zero somebody reads as "Domain".
pub const NO_REQUIRED_TYPE: u64 = u64::MAX;

#[cfg(test)]
mod tests;

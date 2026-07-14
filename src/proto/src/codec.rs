//! Shared wire primitives for the generated codecs.
//!
//! All integers are little-endian and there is no padding or alignment, so the
//! byte layout is exactly as written. Encoding writes into a caller buffer and
//! returns `None` on overflow; decoding returns `None` on a short or malformed
//! buffer. Everything is heap-free except `string_lp`, which allocates the
//! decoded `String`.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

// A byte sink the generated codecs write into. The default methods build the
// little-endian and length-prefixed encodings on top of `put`, so a concrete sink
// only implements `put`.
pub trait Sink {
	// Append one byte, or return None if the sink is full.
	fn put(&mut self, b: u8) -> Option<()>;

	// Record the out-of-band handle to transfer with this message (at most one per
	// message, matching the kernel channel's single-handle limit).
	fn set_handle(&mut self, h: u64) -> Option<()>;

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
	handle: u64,
	handle_occupied: bool,
}

impl<'a> SliceWriter<'a> {
	pub fn new(buf: &'a mut [u8]) -> SliceWriter<'a> {
		SliceWriter { buf, pos: 0, handle: 0, handle_occupied: false }
	}

	// The number of bytes written so far.
	pub fn pos(&self) -> usize {
		self.pos
	}

	// The out-of-band handle recorded during encoding (0 = none).
	pub fn handle(&self) -> u64 {
		self.handle
	}

	pub fn has_handle(&self) -> bool {
		self.handle_occupied
	}

	// Rewind to an empty buffer, dropping anything written and any recorded handle,
	// so a failed encode can be replaced in place - the dispatch overflow fallback.
	pub fn reset(&mut self) {
		self.pos = 0;
		self.handle = 0;
		self.handle_occupied = false;
	}
}

impl<'a> Sink for SliceWriter<'a> {
	fn put(&mut self, b: u8) -> Option<()> {
		*self.buf.get_mut(self.pos)? = b;
		self.pos += 1;
		Some(())
	}

	fn set_handle(&mut self, h: u64) -> Option<()> {
		if self.handle_occupied {
			return None;
		}
		self.handle = h;
		self.handle_occupied = true;
		Some(())
	}
}

// A growable sink, used by the generated clients to build a request without
// sizing a buffer up front.
#[derive(Default)]
pub struct VecWriter {
	buf: Vec<u8>,
	handle: u64,
	handle_occupied: bool,
}

impl VecWriter {
	pub fn new() -> VecWriter {
		VecWriter { buf: Vec::new(), handle: 0, handle_occupied: false }
	}

	// The out-of-band handle recorded during encoding (0 = none).
	pub fn handle(&self) -> u64 {
		self.handle
	}

	// The bytes written so far, consuming the writer.
	pub fn into_inner(self) -> Vec<u8> {
		self.buf
	}
}

impl Sink for VecWriter {
	fn put(&mut self, b: u8) -> Option<()> {
		self.buf.push(b);
		Some(())
	}

	fn set_handle(&mut self, h: u64) -> Option<()> {
		if self.handle_occupied {
			return None;
		}
		self.handle = h;
		self.handle_occupied = true;
		Some(())
	}
}

// A request/reply channel the generated clients call over. The userspace impl
// sends on a channel and blocks for the reply; tests use an in-memory loopback.
pub trait Transport {
	// Send a request (bytes plus an optional transferred handle, 0 = none) and
	// receive the reply (bytes plus an optional transferred handle).
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(Vec<u8>, u64)>;

	// Release a reply handle that could not be decoded or was not expected by the
	// schema. Host test transports need no action; the runtime closes it.
	fn discard_handle(&mut self, _handle: u64) {}
}

// A cursor that reads from a borrowed buffer.
pub struct Reader<'a> {
	buf: &'a [u8],
	pos: usize,
	handle: u64,
	handle_available: bool,
}

impl<'a> Reader<'a> {
	pub fn new(buf: &'a [u8]) -> Reader<'a> {
		Reader { buf, pos: 0, handle: 0, handle_available: false }
	}

	// A reader for a message that arrived with an out-of-band transferred handle.
	pub fn with_handle(buf: &'a [u8], handle: u64) -> Reader<'a> {
		Reader { buf, pos: 0, handle, handle_available: true }
	}

	// The out-of-band handle that arrived with the message (0 = none).
	pub fn take_handle(&mut self) -> Option<u64> {
		if !self.handle_available {
			return None;
		}
		self.handle_available = false;
		Some(self.handle)
	}

	pub fn has_handle(&self) -> bool {
		self.handle_available
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

	pub fn boolean(&mut self) -> Option<bool> {
		Some(self.u8()? != 0)
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

	pub fn string_lp(&mut self) -> Option<String> {
		let bytes = self.bytes_lp()?;
		String::from_utf8(bytes.to_vec()).ok()
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
	let mut out = String::with_capacity(min.len() * 2);
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

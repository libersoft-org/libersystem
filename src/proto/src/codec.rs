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
}

impl<'a> SliceWriter<'a> {
	pub fn new(buf: &'a mut [u8]) -> SliceWriter<'a> {
		SliceWriter { buf, pos: 0 }
	}

	// The number of bytes written so far.
	pub fn pos(&self) -> usize {
		self.pos
	}
}

impl<'a> Sink for SliceWriter<'a> {
	fn put(&mut self, b: u8) -> Option<()> {
		*self.buf.get_mut(self.pos)? = b;
		self.pos += 1;
		Some(())
	}
}

// A growable sink, used by the generated clients to build a request without
// sizing a buffer up front.
#[derive(Default)]
pub struct VecWriter {
	buf: Vec<u8>,
}

impl VecWriter {
	pub fn new() -> VecWriter {
		VecWriter { buf: Vec::new() }
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
}

// A request/reply channel the generated clients call over. The userspace impl
// sends on a channel and blocks for the reply; tests use an in-memory loopback.
pub trait Transport {
	fn call(&mut self, request: &[u8]) -> Option<Vec<u8>>;
}

// A cursor that reads from a borrowed buffer.
pub struct Reader<'a> {
	buf: &'a [u8],
	pos: usize,
}

impl<'a> Reader<'a> {
	pub fn new(buf: &'a [u8]) -> Reader<'a> {
		Reader { buf, pos: 0 }
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

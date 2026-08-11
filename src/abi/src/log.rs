//! The canonical structured-log type shared across the system.
//!
//! Per the System API model, a log entry is a typed object - `LogRecord { ts,
//! severity, source, fields }` - not a line of text (the journald model, not
//! syslog). The record's binary wire form is canonical; the human-text, JSON, and
//! CBOR forms are *representations* derived from it. Emitters (services) encode a
//! record, LogService stores the canonical bytes, and a query renders them into
//! whichever representation the caller asked for. All encoders write into a
//! caller-provided buffer and return the byte count (or `None` on overflow), so
//! this stays `no_std` and heap-free for both the kernel and userspace.
//!
//! `encode` is ALL OR NOTHING: it computes the record's length first and refuses before writing a
//! byte, so `None` means the buffer is untouched. The RENDERERS are not - a rendering that runs out
//! of room has already written a prefix, and `None` means the contents are unspecified rather than
//! unchanged. The distinction is stated because "None on overflow" invites the stronger reading,
//! and only one of the two earns it.
//!
//! Wire layout (all integers little-endian):
//! `[ts u64][severity u8][source_len u16][source][field_count u16]( [key_len u16][key][val_len u16][val] )*`

// LogService request opcodes (the first byte of a message on its serve channel).
// EMIT carries one encoded LogRecord to store; QUERY carries a [format][min
// severity] pair and is answered with the rendered matching records.
pub const OP_EMIT: u8 = 1;
pub const OP_QUERY: u8 = 2;

// Representation a QUERY asks the matching records to be rendered in.
pub const FORMAT_TEXT: u8 = 0;
pub const FORMAT_JSON: u8 = 1;
pub const FORMAT_CBOR: u8 = 2;

// Log severity, ordered from least to most urgent. The numeric value is the
// stable wire encoding (a single byte in the record).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Severity {
	Trace = 0,
	Debug = 1,
	Info = 2,
	Warn = 3,
	Error = 4,
	Fatal = 5,
}

impl Severity {
	// Decode a severity byte, or None if it is out of range.
	pub const fn from_u8(value: u8) -> Option<Severity> {
		match value {
			0 => Some(Severity::Trace),
			1 => Some(Severity::Debug),
			2 => Some(Severity::Info),
			3 => Some(Severity::Warn),
			4 => Some(Severity::Error),
			5 => Some(Severity::Fatal),
			_ => None,
		}
	}

	// The canonical uppercase name, used by the text and JSON representations.
	pub const fn name(self) -> &'static str {
		match self {
			Severity::Trace => "TRACE",
			Severity::Debug => "DEBUG",
			Severity::Info => "INFO",
			Severity::Warn => "WARN",
			Severity::Error => "ERROR",
			Severity::Fatal => "FATAL",
		}
	}
}

// A small append-only writer over a caller buffer. Every write returns None once
// the buffer is full, so the encoders propagate overflow with `?`.
struct Buf<'a> {
	out: &'a mut [u8],
	pos: usize,
}

impl<'a> Buf<'a> {
	fn new(out: &'a mut [u8]) -> Buf<'a> {
		Buf { out, pos: 0 }
	}

	fn byte(&mut self, b: u8) -> Option<()> {
		*self.out.get_mut(self.pos)? = b;
		self.pos += 1;
		Some(())
	}

	// All of `s` or none of it, copied as a range.
	//
	// This wrote byte by byte and returned `None` when it ran out, leaving the output partly
	// overwritten - not a safety problem, and "None on overflow" invites a caller to assume nothing
	// was written. Checking first makes that assumption true, and a range copy is faster than a
	// bounds check per byte.
	fn bytes(&mut self, s: &[u8]) -> Option<()> {
		let end = self.pos.checked_add(s.len())?;
		self.out.get_mut(self.pos..end)?.copy_from_slice(s);
		self.pos = end;
		Some(())
	}

	fn u16_le(&mut self, v: u16) -> Option<()> {
		self.bytes(&v.to_le_bytes())
	}

	fn u64_le(&mut self, v: u64) -> Option<()> {
		self.bytes(&v.to_le_bytes())
	}

	// A length-prefixed byte string ([len u16 LE][bytes]). Refuses strings longer
	// than u16::MAX.
	fn lp(&mut self, s: &[u8]) -> Option<()> {
		if s.len() > u16::MAX as usize {
			return None;
		}
		self.u16_le(s.len() as u16)?;
		self.bytes(s)
	}

	// An unsigned integer in ASCII decimal.
	fn dec(&mut self, mut v: u64) -> Option<()> {
		if v == 0 {
			return self.byte(b'0');
		}
		let mut tmp: [u8; 20] = [0u8; 20];
		let mut i: usize = tmp.len();
		while v > 0 {
			i -= 1;
			tmp[i] = b'0' + (v % 10) as u8;
			v /= 10;
		}
		self.bytes(&tmp[i..])
	}
}

// Encode a log record into `out`, returning the number of bytes written, or None
// if the buffer is too small (or there are more than u16::MAX fields). `fields` is
// a slice of (key, value) byte-string pairs.
pub fn encode(ts: u64, severity: Severity, source: &[u8], fields: &[(&[u8], &[u8])], out: &mut [u8]) -> Option<usize> {
	if fields.len() > u16::MAX as usize {
		return None;
	}
	// SOURCE, KEYS AND VALUES ARE STRINGS, and the schema already said so: `log.lsidl` declares all
	// three as `string` and its generated codec carries `String`. This side took `&[u8]` and checked
	// nothing, so `source = b"\xff"` was valid input that produced invalid JSON (a raw 0xff inside a
	// string literal) and invalid CBOR (major type 3 is defined as UTF-8 text). The two
	// implementations of one wire format disagreed about what the format accepts.
	if core::str::from_utf8(source).is_err() {
		return None;
	}
	for &(key, val) in fields {
		if core::str::from_utf8(key).is_err() || core::str::from_utf8(val).is_err() {
			return None;
		}
	}
	// The whole length before a byte is written, so a refusal leaves `out` untouched.
	let mut needed: usize = 8 + 1 + 2 + source.len() + 2;
	for &(key, val) in fields {
		if key.len() > u16::MAX as usize || val.len() > u16::MAX as usize {
			return None;
		}
		needed = needed.checked_add(4)?.checked_add(key.len())?.checked_add(val.len())?;
	}
	if source.len() > u16::MAX as usize || out.len() < needed {
		return None;
	}
	let mut b: Buf<'_> = Buf::new(out);
	b.u64_le(ts)?;
	b.byte(severity as u8)?;
	b.lp(source)?;
	b.u16_le(fields.len() as u16)?;
	for &(key, val) in fields {
		b.lp(key)?;
		b.lp(val)?;
	}
	Some(b.pos)
}

// Read a little-endian u16 at `at`.
fn rd_u16_le(bytes: &[u8], at: usize) -> Option<u16> {
	Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

// Read a little-endian u64 at `at`.
fn rd_u64_le(bytes: &[u8], at: usize) -> Option<u64> {
	Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

// Read a length-prefixed byte string at `at`, returning the slice and the offset
// just past it.
fn read_lp(bytes: &[u8], at: usize) -> Option<(&[u8], usize)> {
	let len: usize = rd_u16_le(bytes, at)? as usize;
	let start: usize = at + 2;
	let end: usize = start.checked_add(len)?;
	Some((bytes.get(start..end)?, end))
}

// A parsed log record borrowing the canonical wire bytes. `parse` validates the WHOLE record - the
// prefix, every field, and that the buffer is consumed exactly - so `fields()` walks bytes that are
// already known to be well formed and every renderer can rely on the declared count.
pub struct LogRecord<'a> {
	ts: u64,
	severity: Severity,
	source: &'a [u8],
	field_count: u16,
	fields: &'a [u8],
}

impl<'a> LogRecord<'a> {
	// Parse the fixed prefix (timestamp, severity, source, field count) and keep a
	// reference to the trailing field region. Returns None if the prefix is
	// malformed or the severity byte is out of range.
	pub fn parse(bytes: &'a [u8]) -> Option<LogRecord<'a>> {
		let ts: u64 = rd_u64_le(bytes, 0)?;
		let severity: Severity = Severity::from_u8(*bytes.get(8)?)?;
		let (source, after_source): (&[u8], usize) = read_lp(bytes, 9)?;
		if core::str::from_utf8(source).is_err() {
			return None;
		}
		let field_count: u16 = rd_u16_le(bytes, after_source)?;
		let start: usize = after_source + 2;
		let fields: &[u8] = bytes.get(start..)?;
		// THE WHOLE GRAMMAR, not the fixed prefix.
		//
		// This used to take the entire remaining slice as `fields` and leave the walking to a lazy
		// iterator that stops early and reports nothing, so a record declaring `field_count = 1`
		// over an empty field region parsed successfully - and after `Some(LogRecord)` a caller is
		// entitled to assume the wire grammar holds. Every renderer downstream assumed it: CBOR
		// announced a map of `field_count` pairs and then emitted however many the iterator
		// produced, which is not CBOR at all.
		//
		// And EXACTLY to the end. A format that calls itself canonical cannot have a record, that
		// record plus a zero byte, and that record plus four bytes of junk all be the same record.
		let mut at: usize = 0;
		for _ in 0..field_count {
			let (key, after_key): (&[u8], usize) = read_lp(fields, at)?;
			let (val, after_val): (&[u8], usize) = read_lp(fields, after_key)?;
			if core::str::from_utf8(key).is_err() || core::str::from_utf8(val).is_err() {
				return None;
			}
			at = after_val;
		}
		if at != fields.len() {
			return None;
		}
		Some(LogRecord { ts, severity, source, field_count, fields })
	}

	pub fn ts(&self) -> u64 {
		self.ts
	}

	pub fn severity(&self) -> Severity {
		self.severity
	}

	pub fn source(&self) -> &'a [u8] {
		self.source
	}

	pub fn field_count(&self) -> u16 {
		self.field_count
	}

	// Iterate the record's (key, value) fields.
	pub fn fields(&self) -> Fields<'a> {
		Fields { data: self.fields, remaining: self.field_count, pos: 0 }
	}
}

// Iterator over a record's structured fields, which `parse` has already validated: it yields
// exactly `field_count` (key, value) pairs. The `?`s below cannot fire on a record that came from
// `parse`, and are kept because the alternative is an unwrap that says the same thing less well.
pub struct Fields<'a> {
	data: &'a [u8],
	remaining: u16,
	pos: usize,
}

impl<'a> Iterator for Fields<'a> {
	type Item = (&'a [u8], &'a [u8]);

	fn next(&mut self) -> Option<(&'a [u8], &'a [u8])> {
		if self.remaining == 0 {
			return None;
		}
		let (key, after_key): (&[u8], usize) = read_lp(self.data, self.pos)?;
		let (val, after_val): (&[u8], usize) = read_lp(self.data, after_key)?;
		self.pos = after_val;
		self.remaining -= 1;
		Some((key, val))
	}
}

// One hex digit (lowercase) for a nibble.
fn hex(nibble: u8) -> u8 {
	if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 }
}

// Render a record as one line of human-readable text:
// `[<ts>] <SEVERITY> <source>: key=value key2=value2`.
pub fn render_text(rec: &LogRecord, out: &mut [u8]) -> Option<usize> {
	let mut b: Buf<'_> = Buf::new(out);
	b.byte(b'[')?;
	b.dec(rec.ts())?;
	b.bytes(b"] ")?;
	b.bytes(rec.severity().name().as_bytes())?;
	b.byte(b' ')?;
	text_escaped(&mut b, rec.source())?;
	b.byte(b':')?;
	for (key, val) in rec.fields() {
		b.byte(b' ')?;
		text_escaped(&mut b, key)?;
		b.byte(b'=')?;
		text_escaped(&mut b, val)?;
	}
	Some(b.pos)
}

// Write a field of the human line with the bytes that would let it lie escaped.
//
// This copied source, key and value through untouched, so a value containing a newline and a
// plausible `[ts] LEVEL source:` prefix appeared in the log as a SECOND RECORD - a component
// writing its own audit trail - and an ESC sequence went straight to the terminal of whoever was
// reading. The JSON renderer escaped its controls; the human one, which is the one being read by a
// person, did not.
//
// `\` is escaped too, so the escaping is unambiguous rather than merely present.
fn text_escaped(b: &mut Buf, s: &[u8]) -> Option<()> {
	for &c in s {
		match c {
			b'\\' => b.bytes(b"\\\\")?,
			b'\n' => b.bytes(b"\\n")?,
			b'\r' => b.bytes(b"\\r")?,
			b'\t' => b.bytes(b"\\t")?,
			0x00..=0x1f | 0x7f => {
				b.bytes(b"\\x")?;
				b.byte(hex(c >> 4))?;
				b.byte(hex(c & 0x0f))?;
			}
			_ => b.byte(c)?,
		}
	}
	Some(())
}

// Write a JSON string literal, escaping the characters JSON requires.
fn json_str(b: &mut Buf, s: &[u8]) -> Option<()> {
	b.byte(b'"')?;
	for &c in s {
		match c {
			b'"' => b.bytes(b"\\\"")?,
			b'\\' => b.bytes(b"\\\\")?,
			b'\n' => b.bytes(b"\\n")?,
			b'\r' => b.bytes(b"\\r")?,
			b'\t' => b.bytes(b"\\t")?,
			0x00..=0x1f => {
				b.bytes(b"\\u00")?;
				b.byte(hex(c >> 4))?;
				b.byte(hex(c & 0x0f))?;
			}
			_ => b.byte(c)?,
		}
	}
	b.byte(b'"')
}

// Render a record as a JSON object:
// `{"ts":N,"severity":"SEV","source":"...","fields":{"k":"v",...}}`.
pub fn render_json(rec: &LogRecord, out: &mut [u8]) -> Option<usize> {
	let mut b: Buf<'_> = Buf::new(out);
	b.bytes(b"{\"ts\":")?;
	b.dec(rec.ts())?;
	b.bytes(b",\"severity\":")?;
	json_str(&mut b, rec.severity().name().as_bytes())?;
	b.bytes(b",\"source\":")?;
	json_str(&mut b, rec.source())?;
	// A LIST, not an object. The wire format is a list of fields and `log.lsidl` declares
	// `list<field>`, both of which permit a key to repeat; a JSON object does not, so two `error`
	// fields became whatever the consumer does with a duplicate key - usually silently the last.
	// The generated codec has always rendered `[{"key":..,"value":..}]`, so this is also the two
	// implementations of one format agreeing.
	b.bytes(b",\"fields\":[")?;
	let mut emitted: u16 = 0;
	for (key, val) in rec.fields() {
		if emitted > 0 {
			b.byte(b',')?;
		}
		b.bytes(b"{\"key\":")?;
		json_str(&mut b, key)?;
		b.bytes(b",\"value\":")?;
		json_str(&mut b, val)?;
		b.byte(b'}')?;
		emitted += 1;
	}
	if emitted != rec.field_count() {
		return None;
	}
	b.bytes(b"]}")?;
	Some(b.pos)
}

// Write a CBOR head byte for `major` (type, 0..7) carrying `val` (length or
// value), with the minimal-width additional-information encoding.
fn cbor_head(b: &mut Buf, major: u8, val: u64) -> Option<()> {
	let m: u8 = major << 5;
	if val < 24 {
		b.byte(m | val as u8)
	} else if val <= u8::MAX as u64 {
		b.byte(m | 24)?;
		b.byte(val as u8)
	} else if val <= u16::MAX as u64 {
		b.byte(m | 25)?;
		b.bytes(&(val as u16).to_be_bytes())
	} else if val <= u32::MAX as u64 {
		b.byte(m | 26)?;
		b.bytes(&(val as u32).to_be_bytes())
	} else {
		b.byte(m | 27)?;
		b.bytes(&val.to_be_bytes())
	}
}

// A CBOR text string (major type 3): a length head then the UTF-8 bytes.
fn cbor_tstr(b: &mut Buf, s: &[u8]) -> Option<()> {
	cbor_head(b, 3, s.len() as u64)?;
	b.bytes(s)
}

// Render a record as CBOR: a 4-entry map { "ts": uint, "severity": uint,
// "source": tstr, "fields": array of { "key": tstr, "value": tstr } }.
// Definite-length, big-endian per RFC 8949.
pub fn render_cbor(rec: &LogRecord, out: &mut [u8]) -> Option<usize> {
	let mut b: Buf<'_> = Buf::new(out);
	cbor_head(&mut b, 5, 4)?;
	cbor_tstr(&mut b, b"ts")?;
	cbor_head(&mut b, 0, rec.ts())?;
	cbor_tstr(&mut b, b"severity")?;
	cbor_head(&mut b, 0, rec.severity() as u64)?;
	cbor_tstr(&mut b, b"source")?;
	cbor_tstr(&mut b, rec.source())?;
	cbor_tstr(&mut b, b"fields")?;
	// An ARRAY of two-entry maps, for the reason JSON is a list: the wire format and the schema
	// both say `list<field>`, and the generated codec renders exactly this.
	cbor_head(&mut b, 4, rec.field_count() as u64)?;
	let mut emitted: u16 = 0;
	for (key, val) in rec.fields() {
		cbor_head(&mut b, 5, 2)?;
		cbor_tstr(&mut b, b"key")?;
		cbor_tstr(&mut b, key)?;
		cbor_tstr(&mut b, b"value")?;
		cbor_tstr(&mut b, val)?;
		emitted += 1;
	}
	// A definite-length CBOR head is a PROMISE about what follows. Announcing `field_count` items
	// and emitting fewer produced bytes that are not CBOR, and the function returned the length
	// anyway. `parse` makes the mismatch impossible; this makes it unrepresentable.
	if emitted != rec.field_count() {
		return None;
	}
	Some(b.pos)
}

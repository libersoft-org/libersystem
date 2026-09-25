// USB VIDEO CLASS FORMAT AND FRAME DESCRIPTORS, NORMALIZED BEFORE ANYTHING IS OFFERED (UVC 1.5, the
// video streaming interface's class-specific descriptors), and the one negotiation rule over them.
//
// WHAT THIS IS FOR. The in-guest camera fixture keeps synthetic descriptors and normalizes them here,
// and the USB Video class module will feed the descriptors it fetches through the same code; CameraService
// parses none of this - it sees bounded typed formats. So the bounds live here, once.
//
// WHAT IS OFFERED. Two frame types: packed YUY2 (`VS_FORMAT_UNCOMPRESSED` with the YUY2 GUID) and
// Motion-JPEG (`VS_FORMAT_MJPEG`), each with its frame descriptors and the colour format that follows
// them. Any other format is SKIPPED - counted, never reinterpreted. At most eight formats, 32 frame
// sizes per format and 32 intervals per size; a continuous interval range stays a range.
//
// WHAT IS REFUSED, AND REFUSES THE WHOLE GRAPH: a descriptor whose length disagrees with its type or runs
// past the graph, a frame outside a format, a zero dimension, a YUY2 frame whose stride or size
// overflows or does not fit its declared buffer, a zero or disordered interval, a duplicate format or
// frame index, a frame count that disagrees with the format's, and a graph past its budget.

use alloc::vec::Vec;

pub const CS_INTERFACE: u8 = 0x24;
pub const VS_FORMAT_UNCOMPRESSED: u8 = 0x04;
pub const VS_FRAME_UNCOMPRESSED: u8 = 0x05;
pub const VS_FORMAT_MJPEG: u8 = 0x06;
pub const VS_FRAME_MJPEG: u8 = 0x07;
pub const VS_COLORFORMAT: u8 = 0x0d;
/// The YUY2 format GUID, as it appears on the wire.
pub const YUY2_GUID: [u8; 16] = [0x59, 0x55, 0x59, 0x32, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];

pub const MAX_FORMATS: usize = 8;
pub const MAX_SIZES: usize = 32;
pub const MAX_INTERVALS: usize = 32;
/// The largest buffer one frame may need: what a single client buffer may hold.
pub const MAX_FRAME_BYTES: u32 = 8 * 1024 * 1024;
/// A descriptor graph's own budget, before any of it is parsed.
pub const MAX_GRAPH_BYTES: usize = 64 * 1024;
/// Intervals are in UVC's 100-nanosecond units.
pub const INTERVAL_UNITS_PER_SECOND: u32 = 10_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
	/// A length that disagrees with the descriptor's type, or runs past the graph.
	Malformed,
	/// A frame with no format before it, a count that disagrees, or a duplicate index.
	Structure,
	/// A zero dimension, a stride or size that overflows or does not fit, or a bad interval.
	Invalid,
	/// Past the graph's, a format's or a size's budget.
	Budget,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Yuy2,
	Mjpeg,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Range {
	Unknown,
	Limited,
	Full,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Matrix {
	Unknown,
	Bt601,
	Bt709,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Intervals {
	/// In 100 ns units, each non-zero.
	Discrete(Vec<u32>),
	/// Checked: `0 < minimum <= maximum`, and a non-zero step unless the range is one value.
	Stepwise { minimum: u32, maximum: u32, step: u32 },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Size {
	pub index: u8,
	pub width: u16,
	pub height: u16,
	pub max_bytes: u32,
	pub intervals: Intervals,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Format {
	pub index: u8,
	pub kind: Kind,
	pub sizes: Vec<Size>,
	pub range: Range,
	pub matrix: Matrix,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Normalized {
	pub formats: Vec<Format>,
	/// Formats of a type this system does not carry, skipped.
	pub skipped: usize,
}

fn le16(bytes: &[u8], at: usize) -> u16 {
	u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// YUY2's row stride and frame size, checked: two bytes a pixel.
pub fn yuy2_layout(width: u16, height: u16) -> Option<(u32, u32)> {
	let stride = u32::from(width).checked_mul(2)?;
	let bytes = stride.checked_mul(u32::from(height))?;
	Some((stride, bytes))
}

fn intervals(descriptor: &[u8], at: usize, kind: u8) -> Result<Intervals, Refused> {
	if kind == 0 {
		if descriptor.len() < at + 12 {
			return Err(Refused::Malformed);
		}
		let (minimum, maximum, step) = (le32(descriptor, at), le32(descriptor, at + 4), le32(descriptor, at + 8));
		if minimum == 0 || maximum < minimum || (step == 0 && maximum != minimum) || (step != 0 && (maximum - minimum) % step != 0) {
			return Err(Refused::Invalid);
		}
		return Ok(Intervals::Stepwise { minimum, maximum, step });
	}
	let count = usize::from(kind);
	if count > MAX_INTERVALS {
		return Err(Refused::Budget);
	}
	if descriptor.len() < at + 4 * count {
		return Err(Refused::Malformed);
	}
	let mut values: Vec<u32> = Vec::with_capacity(count);
	for n in 0..count {
		let value = le32(descriptor, at + 4 * n);
		if value == 0 || values.contains(&value) {
			return Err(Refused::Invalid);
		}
		values.push(value);
	}
	Ok(Intervals::Discrete(values))
}

/// Normalize a video streaming interface's class-specific descriptors, in the order they came.
pub fn normalize(graph: &[u8]) -> Result<Normalized, Refused> {
	if graph.len() > MAX_GRAPH_BYTES {
		return Err(Refused::Budget);
	}
	let mut out = Normalized::default();
	// The format the frames being read belong to, how many it declared, and whether it is skipped.
	let mut open: Option<(Format, usize, bool)> = None;
	let mut at = 0;
	let close = |open: &mut Option<(Format, usize, bool)>, out: &mut Normalized| -> Result<(), Refused> {
		if let Some((format, declared, skipped)) = open.take() {
			if skipped {
				return Ok(());
			}
			if format.sizes.len() != declared {
				return Err(Refused::Structure);
			}
			if out.formats.len() >= MAX_FORMATS {
				return Err(Refused::Budget);
			}
			out.formats.push(format);
		}
		Ok(())
	};
	while at < graph.len() {
		let length = usize::from(graph[at]);
		if length < 3 || at + length > graph.len() {
			return Err(Refused::Malformed);
		}
		let descriptor = &graph[at..at + length];
		at += length;
		if descriptor[1] != CS_INTERFACE {
			continue;
		}
		match descriptor[2] {
			VS_FORMAT_UNCOMPRESSED | VS_FORMAT_MJPEG => {
				close(&mut open, &mut out)?;
				let uncompressed = descriptor[2] == VS_FORMAT_UNCOMPRESSED;
				if length != if uncompressed { 27 } else { 11 } {
					return Err(Refused::Malformed);
				}
				let index = descriptor[3];
				let declared = usize::from(descriptor[4]);
				if index == 0 || declared == 0 {
					return Err(Refused::Structure);
				}
				if declared > MAX_SIZES {
					return Err(Refused::Budget);
				}
				if out.formats.iter().any(|format| format.index == index) {
					return Err(Refused::Structure);
				}
				let skipped = uncompressed && descriptor[5..21] != YUY2_GUID;
				if skipped {
					out.skipped += 1;
				}
				let kind = if uncompressed { Kind::Yuy2 } else { Kind::Mjpeg };
				open = Some((Format { index, kind, sizes: Vec::new(), range: Range::Unknown, matrix: Matrix::Unknown }, declared, skipped));
			}
			VS_FRAME_UNCOMPRESSED | VS_FRAME_MJPEG => {
				let Some((format, _, skipped)) = open.as_mut() else { return Err(Refused::Structure) };
				// A FRAME OF THE OTHER TYPE than its format is a graph that does not say what it is.
				if (descriptor[2] == VS_FRAME_UNCOMPRESSED) != (format.kind == Kind::Yuy2) {
					return Err(Refused::Structure);
				}
				if length < 26 {
					return Err(Refused::Malformed);
				}
				let index = descriptor[3];
				let width = le16(descriptor, 5);
				let height = le16(descriptor, 7);
				let max_bytes = le32(descriptor, 17);
				let kinds = descriptor[25];
				let expected = if kinds == 0 { 38 } else { 26 + 4 * usize::from(kinds) };
				if length != expected {
					return Err(Refused::Malformed);
				}
				if *skipped {
					continue;
				}
				if index == 0 || format.sizes.iter().any(|size| size.index == index) {
					return Err(Refused::Structure);
				}
				if format.sizes.len() >= MAX_SIZES {
					return Err(Refused::Budget);
				}
				if width == 0 || height == 0 || max_bytes == 0 || max_bytes > MAX_FRAME_BYTES {
					return Err(Refused::Invalid);
				}
				if format.kind == Kind::Yuy2 {
					let Some((_, bytes)) = yuy2_layout(width, height) else { return Err(Refused::Invalid) };
					if bytes > max_bytes {
						return Err(Refused::Invalid);
					}
				}
				let intervals = intervals(descriptor, 26, kinds)?;
				format.sizes.push(Size { index, width, height, max_bytes, intervals });
			}
			VS_COLORFORMAT => {
				if length != 6 {
					return Err(Refused::Malformed);
				}
				// It follows the frames it describes; with no format open it describes nothing.
				let Some((format, _, _)) = open.as_mut() else { return Err(Refused::Structure) };
				// UVC's OWN CODE POINTS, which are not H.273's: 1 BT.709, 2 FCC, 3 BT.470-2 System B,G, 4 SMPTE
				// 170M (the default), 5 SMPTE 240M, 6 and above reserved. B,G and 170M share BT.601's
				// coefficients; FCC's (0.30, 0.11) and 240M's are neither of the two this knows, and a
				// reserved value is nothing - so each of those is unknown rather than a guess.
				format.matrix = match descriptor[5] {
					1 => Matrix::Bt709,
					3 | 4 => Matrix::Bt601,
					_ => Matrix::Unknown,
				};
				// UVC does not state a range; Motion-JPEG is full range by its own definition.
				format.range = if format.kind == Kind::Mjpeg { Range::Full } else { Range::Limited };
			}
			_ => {}
		}
	}
	close(&mut open, &mut out)?;
	Ok(out)
}

/// Whether `interval` is one this size offers.
pub fn offers(intervals: &Intervals, interval: u32) -> bool {
	match intervals {
		Intervals::Discrete(values) => values.contains(&interval),
		Intervals::Stepwise { minimum, maximum, step } => interval >= *minimum && interval <= *maximum && (*step == 0 || (interval - minimum) % step == 0),
	}
}

/// What a provider selects for a request: exactly the format, size and interval asked for, or nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
	pub format: u8,
	pub size: u8,
	pub kind: Kind,
	pub width: u16,
	pub height: u16,
	pub interval: u32,
	pub max_bytes: u32,
	pub stride: u32,
}

pub fn select(normalized: &Normalized, format: u8, size: u8, interval: u32) -> Option<Selection> {
	let found = normalized.formats.iter().find(|candidate| candidate.index == format)?;
	let frame = found.sizes.iter().find(|candidate| candidate.index == size)?;
	if !offers(&frame.intervals, interval) {
		return None;
	}
	let stride = match found.kind {
		Kind::Yuy2 => yuy2_layout(frame.width, frame.height)?.0,
		Kind::Mjpeg => 0,
	};
	Some(Selection { format, size, kind: found.kind, width: frame.width, height: frame.height, interval, max_bytes: frame.max_bytes, stride })
}

// ------------------------------------------------------------------ the transport's half
//
// WHICH INTERFACES, AND BULK ONLY. A video function is a control interface (subclass 1) and a streaming one
// (subclass 2). A streaming interface whose alternate zero carries an endpoint streams over BULK, from alternate
// zero; one whose endpoints are on later alternates streams ISOCHRONOUSLY, which this transport does not drive -
// refused by name, not bound and left silent.
//
// PROBE AND COMMIT. A stream is negotiated by writing the probe control with the format, frame and interval
// wanted, reading back what the device will do, and committing that. The structure is 26 bytes in UVC 1.0 and 34
// from 1.1; the device's own header says which.
//
// A PAYLOAD HEADER IS BELIEVED ONLY AS FAR AS ITS LENGTH: at least its two bytes, and no longer than the transfer
// that carried it. Its bits say which frame the data belongs to (FID), whether the frame ends here (EOF) and
// whether the device knows the data is bad (ERR).

use crate::usb_function::{Configuration, Endpoint, Refused as ConfigRefused};

pub const CLASS_VIDEO: u8 = 0x0e;
pub const SUBCLASS_CONTROL: u8 = 0x01;
pub const SUBCLASS_STREAMING: u8 = 0x02;
pub const VC_HEADER: u8 = 0x01;
pub const SET_CUR: u8 = 0x01;
pub const GET_CUR: u8 = 0x81;
pub const RT_CLASS_INTERFACE_OUT: u8 = 0x21;
pub const RT_CLASS_INTERFACE_IN: u8 = 0xa1;
pub const VS_PROBE_CONTROL: u8 = 0x01;
pub const VS_COMMIT_CONTROL: u8 = 0x02;

pub const HEADER_FID: u8 = 1 << 0;
pub const HEADER_EOF: u8 = 1 << 1;
pub const HEADER_ERR: u8 = 1 << 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotBindable {
	NoVideoInterface,
	/// Its streaming interface streams isochronously: endpoints only on later alternates.
	Isochronous,
	Malformed(ConfigRefused),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
	pub config_value: u8,
	pub control_interface: u8,
	pub streaming_interface: u8,
	pub bulk_in: Endpoint,
	/// UVC's version, from the control interface's header: the probe structure's length follows it.
	pub version: u16,
	/// The streaming interface's class-specific records: what `normalize` reads.
	pub graph: Vec<u8>,
}

impl Binding {
	pub fn probe_length(&self) -> u16 {
		if self.version >= 0x0110 { 34 } else { 26 }
	}
}

/// The video function of one configuration, when it streams over bulk.
pub fn bind(config: &[u8]) -> Result<Binding, NotBindable> {
	let parsed = Configuration::parse(config).map_err(NotBindable::Malformed)?;
	let control = parsed.settings.iter().find(|setting| setting.is(CLASS_VIDEO, SUBCLASS_CONTROL)).ok_or(NotBindable::NoVideoInterface)?;
	let version = parsed.functional(control).find(|record| record.kind == CS_INTERFACE && record.field(2) == Ok(VC_HEADER)).and_then(|record| record.field16(3).ok()).unwrap_or(0x0100);
	let streaming = parsed.settings.iter().find(|setting| setting.is(CLASS_VIDEO, SUBCLASS_STREAMING) && setting.alternate == 0).ok_or(NotBindable::NoVideoInterface)?;
	let Some(bulk_in) = streaming.first(Endpoint::is_bulk_in) else {
		return Err(NotBindable::Isochronous);
	};
	Ok(Binding { config_value: parsed.value, control_interface: control.interface, streaming_interface: streaming.interface, bulk_in, version, graph: parsed.functional_bytes(streaming).to_vec() })
}

/// The probe (or commit) control for this selection, at the device's structure length.
pub fn probe(format: u8, frame: u8, interval: u32, length: u16) -> Vec<u8> {
	let mut out = alloc::vec![0u8; length as usize];
	// bmHint: the frame interval is fixed.
	out[0] = 1;
	out[2] = format;
	out[3] = frame;
	out[4..8].copy_from_slice(&interval.to_le_bytes());
	out
}

/// What the device answered a probe with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probed {
	pub format: u8,
	pub frame: u8,
	pub interval: u32,
	pub max_frame: u32,
	pub max_payload: u32,
}

pub fn probed(bytes: &[u8]) -> Option<Probed> {
	if bytes.len() < 26 {
		return None;
	}
	Some(Probed { format: bytes[2], frame: bytes[3], interval: le32(bytes, 4), max_frame: le32(bytes, 18), max_payload: le32(bytes, 22) })
}

/// One payload's header, read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Payload {
	pub fid: bool,
	pub eof: bool,
	pub err: bool,
	/// Where the data starts in the transfer.
	pub data: usize,
}

pub fn payload(transfer: &[u8]) -> Option<Payload> {
	let length = *transfer.first()? as usize;
	if length < 2 || length > transfer.len() {
		return None;
	}
	let info = transfer[1];
	Some(Payload { fid: info & HEADER_FID != 0, eof: info & HEADER_EOF != 0, err: info & HEADER_ERR != 0, data: length })
}

/// Where an assembled frame's bytes go - the consumer's leased buffer in the class module, a vector in a test.
pub trait FrameSink {
	/// A buffer for a frame that is starting, and how many bytes it holds - or `None` when nothing is queued.
	fn open(&mut self) -> Option<u64>;
	/// `data` at `offset` into the open frame's buffer. Only ever inside the capacity `open` returned.
	fn write(&mut self, offset: u32, data: &[u8]);
	/// The frame is over, as the assembler saw it.
	fn finish(&mut self, frame: Assembled);
}

/// A frame at its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Assembled {
	/// Whether a buffer was open for it.
	pub buffered: bool,
	pub written: u32,
	/// Lost on the way: the device's error bit, a header that was not one, or data past the buffer or past the
	/// most a frame may be.
	pub bad: bool,
	/// Whether an EOF ended it, rather than the next frame's FID.
	pub eof: bool,
}

impl Assembled {
	/// A WHOLE FRAME: nothing lost and something written - and, where the format fixes a frame's length, exactly
	/// that length. A compressed frame has no length to be held against, so it is whole only if its own EOF
	/// ended it: one that ended because the next frame began may have lost its last payload, and nothing here
	/// can tell a short JPEG from a whole one.
	pub fn whole(&self, expected: Option<u32>) -> bool {
		self.buffered
			&& !self.bad
			&& self.written > 0
			&& match expected {
				Some(expected) => self.written == expected,
				None => self.eof,
			}
	}
}

#[derive(Clone, Copy)]
struct Open {
	fid: bool,
	capacity: Option<u64>,
	written: u32,
	bad: bool,
}

impl Open {
	fn ended(self, eof: bool) -> Assembled {
		Assembled { buffered: self.capacity.is_some(), written: self.written, bad: self.bad, eof }
	}
}

/// FRAMES OUT OF PAYLOADS, one bulk transfer at a time. The payload header says which frame its data belongs
/// to (FID), whether the frame ends there (EOF) and whether the device knows it is bad (ERR), and those three
/// bits are all this believes:
///   - a FID that differs from the open frame's ENDS that frame - the device either does not set EOF or lost
///     the payload that carried it - and the payload starts the next;
///   - after a frame has ended, a payload still carrying ITS FID belongs to nothing: it is dropped until the FID
///     toggles, because the device is out of step with its own frames and whatever it is sending is not the
///     start of the next one;
///   - ERR, or a header that is not one, loses the open frame, and nothing more of it is written;
///   - data past the buffer, or past the most a frame may be, loses the frame and is not written;
///   - a frame that found no buffer is still assembled to its end, so that its end is seen, and ends unbuffered.
#[derive(Default)]
pub struct Assembler {
	frame: Option<Open>,
	/// The FID of the frame that ended last.
	ended: Option<bool>,
}

impl Assembler {
	pub const fn new() -> Self {
		Assembler { frame: None, ended: None }
	}

	/// One payload into the frame it belongs to. `limit` is the most a frame may be: the committed frame size.
	pub fn feed(&mut self, transfer: &[u8], limit: u32, sink: &mut impl FrameSink) {
		let Some(header) = payload(transfer) else {
			if let Some(frame) = self.frame.as_mut() {
				frame.bad = true;
			}
			return;
		};
		if let Some(interrupted) = self.frame.take_if(|frame| frame.fid != header.fid) {
			self.ended = Some(interrupted.fid);
			sink.finish(interrupted.ended(false));
		}
		if self.frame.is_none() && self.ended == Some(header.fid) {
			return;
		}
		let frame = self.frame.get_or_insert_with(|| Open { fid: header.fid, capacity: sink.open(), written: 0, bad: false });
		frame.bad |= header.err;
		let data = &transfer[header.data..];
		if let Some(capacity) = frame.capacity
			&& !frame.bad
			&& !data.is_empty()
		{
			let end = u64::from(frame.written) + data.len() as u64;
			if end > capacity || end > u64::from(limit) {
				frame.bad = true;
			} else {
				sink.write(frame.written, data);
				frame.written += data.len() as u32;
			}
		}
		if header.eof
			&& let Some(done) = self.frame.take()
		{
			self.ended = Some(done.fid);
			sink.finish(done.ended(true));
		}
	}

	/// Whether a frame is open.
	pub fn assembling(&self) -> bool {
		self.frame.is_some()
	}
}

#[cfg(test)]
mod tests;

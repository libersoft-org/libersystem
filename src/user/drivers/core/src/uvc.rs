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

#[cfg(test)]
mod tests;

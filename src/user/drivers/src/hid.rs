// HID report-descriptor parsing and input-report decoding.
//
// A HID device describes the shape of its reports in a report descriptor: a
// stream of short items that set global state (usage page, logical range, report
// size / count / id) and local state (the usages the next fields carry), closed
// by main items that declare the fields themselves. This module parses that
// stream into a `Layout` - the list of input-field segments the driver cares
// about - and decodes incoming reports against it: keyboard and Consumer-page
// fields diff into key press / release events, and Generic-Desktop pointer
// fields (X / Y / Wheel with the Button page) fold into a normalized pointer
// state. Everything else (vendor pages, LEDs, feature reports) parses for its
// bit width only, so the offsets that follow it stay correct.

use alloc::vec::Vec;

// The usage pages decoded into events; every other page only advances the bit
// cursor. Usages are handled page-extended (page << 16 | usage) throughout.
const PAGE_GENERIC_DESKTOP: u16 = 0x01;
const PAGE_KEYBOARD: u16 = 0x07;
const PAGE_BUTTON: u16 = 0x09;
const PAGE_CONSUMER: u16 = 0x0c;

// The Generic-Desktop usages of a pointer's axes.
const USAGE_X: u16 = 0x30;
const USAGE_Y: u16 = 0x31;
const USAGE_WHEEL: u16 = 0x38;

// The normalized pointer coordinate range (matching the virtio pointer driver):
// absolute axes scale their logical range into it, relative ones accumulate
// clamped to it.
pub const NORM_MAX: i32 = 0xffff;

// The Input main item's data bits: bit 0 constant (padding), bit 1 variable
// (else array), bit 2 relative (else absolute).
const INPUT_CONSTANT: u32 = 1 << 0;
const INPUT_VARIABLE: u32 = 1 << 1;
const INPUT_RELATIVE: u32 = 1 << 2;

// A report no wider than this decodes; a descriptor asking for more marks its
// report id oversized and its segments are dropped (the previous-report state
// the diff runs against is a 64-byte buffer).
const MAX_REPORT_BYTES: u32 = 64;

// The most usages one array field reports at once (a boot keyboard rolls over
// at 6; NKRO keyboards use variable bitmaps instead of wider arrays).
const ARRAY_MAX: usize = 16;

// One decoded input-field run: `count` fields of `size` bits at `bit_offset`
// within the body of report `report_id` (0 when the device uses no report ids).
// The usages are page-extended; an explicit list serves variable fields, the
// min/max range serves bitmaps and arrays.
struct Segment {
	report_id: u8,
	bit_offset: u32,
	size: u32,
	count: u32,
	variable: bool,
	relative: bool,
	page: u16,
	usages: Vec<u32>,
	usage_min: u32,
	usage_max: u32,
	logical_min: i32,
	logical_max: i32,
}

impl Segment {
	// The page-extended usage of variable field `i`: the explicit list first (its
	// last entry repeating past the end), else the usage range.
	fn usage_for(&self, i: u32) -> u32 {
		if (i as usize) < self.usages.len() {
			return self.usages[i as usize];
		}
		if let Some(&last) = self.usages.last() {
			return last;
		}
		if self.usage_min != 0 || self.usage_max != 0 {
			return self.usage_min.saturating_add(i).min(self.usage_max);
		}
		0
	}
}

// A parsed report descriptor: the decodable input segments, whether reports are
// led by a report-id byte, and the widest input report in bytes (including that
// byte) - the transfer length the driver posts.
pub struct Layout {
	segs: Vec<Segment>,
	uses_ids: bool,
	max_bytes: u32,
}

// The parser's global item state, saved and restored by Push / Pop. The logical
// maximum is kept both sign-extended and raw: it parses signed, but under a
// non-negative minimum a negative reading was meant unsigned (the value's own
// high bit), so the segment picks at declaration time.
#[derive(Clone, Copy, Default)]
struct Globals {
	page: u16,
	logical_min: i32,
	logical_max: i32,
	logical_max_raw: u32,
	size: u32,
	count: u32,
	id: u8,
}

impl Layout {
	// The empty layout: nothing decodes (the placeholder before a fallback).
	pub fn empty() -> Layout {
		Layout { segs: Vec::new(), uses_ids: false, max_bytes: 0 }
	}

	pub fn uses_ids(&self) -> bool {
		self.uses_ids
	}

	// The transfer length to post for input reports.
	pub fn report_bytes(&self) -> u32 {
		self.max_bytes
	}

	// Whether the device reports keyboard-page keys.
	pub fn has_keyboard(&self) -> bool {
		self.segs.iter().any(|s| s.page == PAGE_KEYBOARD)
	}

	// Whether the device reports a pointer (a Generic-Desktop X axis).
	pub fn has_pointer(&self) -> bool {
		self.segs.iter().any(|s| s.page == PAGE_GENERIC_DESKTOP && (0..s.count).any(|i| s.usage_for(i) & 0xffff == USAGE_X as u32))
	}

	// Whether the device reports Consumer-page controls.
	pub fn has_consumer(&self) -> bool {
		self.segs.iter().any(|s| s.page == PAGE_CONSUMER)
	}

	// Whether any segment decodes into an event the system consumes.
	pub fn is_useful(&self) -> bool {
		self.has_keyboard() || self.has_pointer() || self.has_consumer()
	}

	// Diff the keyboard- and Consumer-page fields of report `id` between two
	// report bodies, emitting (page-extended usage, pressed) for every change.
	// Variable fields diff bit-for-bit; array fields diff as usage sets, exactly
	// like the boot keyboard's six-key array.
	pub fn keys_diff(&self, id: u8, prev: &[u8], cur: &[u8], emit: &mut dyn FnMut(u32, bool)) {
		for seg in self.segs.iter().filter(|s| s.report_id == id && (s.page == PAGE_KEYBOARD || s.page == PAGE_CONSUMER)) {
			if seg.variable {
				for i in 0..seg.count {
					let bit: u32 = seg.bit_offset + i * seg.size;
					let was: u32 = field(prev, bit, seg.size);
					let now: u32 = field(cur, bit, seg.size);
					if was != now {
						let usage: u32 = seg.usage_for(i);
						if usage != 0 {
							emit(usage, now != 0);
						}
					}
				}
			} else {
				let was: ([u32; ARRAY_MAX], usize) = array_usages(seg, prev);
				let now: ([u32; ARRAY_MAX], usize) = array_usages(seg, cur);
				for &usage in &was.0[..was.1] {
					if !now.0[..now.1].contains(&usage) {
						emit(usage, false);
					}
				}
				for &usage in &now.0[..now.1] {
					if !was.0[..was.1].contains(&usage) {
						emit(usage, true);
					}
				}
			}
		}
	}

	// Fold the pointer fields of report `id` into the running pointer state: an
	// absolute axis scales its logical range into 0..=NORM_MAX, a relative one
	// accumulates clamped to it, wheel ticks accumulate into `wheel`, and the
	// Button page sets / clears button bits. Returns whether the report carried
	// any pointer field at all.
	pub fn pointer_fold(&self, id: u8, body: &[u8], x: &mut i32, y: &mut i32, buttons: &mut u8, wheel: &mut i32) -> bool {
		let mut matched: bool = false;
		for seg in self.segs.iter().filter(|s| s.report_id == id && s.variable) {
			if seg.page == PAGE_BUTTON {
				matched = true;
				for i in 0..seg.count.min(8) {
					let bit: u8 = 1 << i;
					if field(body, seg.bit_offset + i * seg.size, seg.size) != 0 {
						*buttons |= bit;
					} else {
						*buttons &= !bit;
					}
				}
			}
			if seg.page == PAGE_GENERIC_DESKTOP {
				for i in 0..seg.count {
					let bit: u32 = seg.bit_offset + i * seg.size;
					let axis: &mut i32 = match (seg.usage_for(i) & 0xffff) as u16 {
						USAGE_X => x,
						USAGE_Y => y,
						USAGE_WHEEL => {
							matched = true;
							*wheel += signed_field(body, bit, seg.size);
							continue;
						}
						_ => continue,
					};
					matched = true;
					if seg.relative {
						*axis = (*axis + signed_field(body, bit, seg.size)).clamp(0, NORM_MAX);
					} else {
						*axis = scale(signed_field(body, bit, seg.size), seg.logical_min, seg.logical_max);
					}
				}
			}
		}
		matched
	}
}

// Parse a report descriptor into its decodable input layout. Unknown items and
// pages cost only their declared bit width; a malformed stream parses as far as
// it stays well-formed.
pub fn parse(desc: &[u8]) -> Layout {
	let mut segs: Vec<Segment> = Vec::new();
	let mut g: Globals = Globals::default();
	let mut stack: Vec<Globals> = Vec::new();
	let mut usages: Vec<u32> = Vec::new();
	let mut usage_min: u32 = 0;
	let mut usage_max: u32 = 0;
	// the input bit cursor of each report id, and the ids that overflowed.
	let mut cursors: Vec<(u8, u32)> = Vec::new();
	let mut uses_ids: bool = false;
	let mut i: usize = 0;
	while i < desc.len() {
		let prefix: u8 = desc[i];
		if prefix == 0xfe {
			// a long item: skip its declared payload.
			let dlen: usize = *desc.get(i + 1).unwrap_or(&0) as usize;
			i += 3 + dlen;
			continue;
		}
		let dlen: usize = match prefix & 3 {
			3 => 4,
			n => n as usize,
		};
		if i + 1 + dlen > desc.len() {
			break;
		}
		let mut data: u32 = 0;
		for (n, &b) in desc[i + 1..i + 1 + dlen].iter().enumerate() {
			data |= (b as u32) << (n * 8);
		}
		// the sign-extended reading, for the logical bounds and relative deltas.
		let sdata: i32 = match dlen {
			1 => data as u8 as i8 as i32,
			2 => data as u16 as i16 as i32,
			_ => data as i32,
		};
		let tag: u8 = prefix >> 4;
		match prefix >> 2 & 3 {
			// main items: Input records a segment (and always advances the bit
			// cursor); every main item resets the local state.
			0 => {
				if tag == 8 {
					let cursor: &mut u32 = cursor_for(&mut cursors, g.id);
					let bits: u32 = g.size.saturating_mul(g.count);
					let interesting: bool = matches!(g.page, PAGE_GENERIC_DESKTOP | PAGE_KEYBOARD | PAGE_BUTTON | PAGE_CONSUMER);
					if data & INPUT_CONSTANT == 0 && interesting && g.size >= 1 && g.size <= 32 && *cursor + bits <= MAX_REPORT_BYTES * 8 {
						let logical_max: i32 = if g.logical_min >= 0 && g.logical_max < g.logical_min { g.logical_max_raw as i32 } else { g.logical_max };
						segs.push(Segment { report_id: g.id, bit_offset: *cursor, size: g.size, count: g.count, variable: data & INPUT_VARIABLE != 0, relative: data & INPUT_RELATIVE != 0, page: g.page, usages: core::mem::take(&mut usages), usage_min, usage_max, logical_min: g.logical_min, logical_max });
					}
					*cursor = cursor.saturating_add(bits);
				}
				usages.clear();
				usage_min = 0;
				usage_max = 0;
			}
			// global items.
			1 => match tag {
				0 => g.page = data as u16,
				1 => g.logical_min = sdata,
				2 => {
					g.logical_max = sdata;
					g.logical_max_raw = data;
				}
				7 => g.size = data,
				8 => {
					g.id = data as u8;
					uses_ids = true;
				}
				9 => g.count = data,
				10 => stack.push(g),
				11 => {
					if let Some(saved) = stack.pop() {
						g = saved;
					}
				}
				_ => {}
			},
			// local items: usages, page-extended when the item carries 4 bytes.
			2 => match tag {
				0 => usages.push(extended(g.page, data, dlen)),
				1 => usage_min = extended(g.page, data, dlen),
				2 => usage_max = extended(g.page, data, dlen),
				_ => {}
			},
			_ => {}
		}
		i += 1 + dlen;
	}
	let mut max_bits: u32 = 0;
	for &(_, bits) in cursors.iter() {
		max_bits = max_bits.max(bits.min(MAX_REPORT_BYTES * 8));
	}
	let mut max_bytes: u32 = max_bits.div_ceil(8);
	if uses_ids && max_bytes != 0 {
		max_bytes += 1;
	}
	Layout { segs, uses_ids, max_bytes }
}

// The layout of the fixed HID boot-keyboard report (modifier bitmap, one pad
// byte, six-key array), built through the parser itself - the fallback for a
// boot-subclass keyboard whose report descriptor cannot be read.
pub fn boot_keyboard() -> Layout {
	#[rustfmt::skip]
	const DESC: [u8; 40] = [
		0x05, 0x01,             // usage page (generic desktop)
		0x09, 0x06,             // usage (keyboard)
		0xa1, 0x01,             // collection (application)
		0x05, 0x07,             //   usage page (keyboard)
		0x19, 0xe0, 0x29, 0xe7, //   usage min / max (the modifiers)
		0x15, 0x00, 0x25, 0x01, //   logical 0..1
		0x75, 0x01, 0x95, 0x08, //   8 bits
		0x81, 0x02,             //   input (data, variable)
		0x95, 0x01, 0x75, 0x08, //   one pad byte
		0x81, 0x01,             //   input (constant)
		0x95, 0x06, 0x75, 0x08, //   six bytes
		0x26, 0xff, 0x00,       //   logical max 255
		0x19, 0x00,             //   usage min 0
		0x81, 0x00,             //   input (data, array)
		0xc0,                   // end collection
	];
	parse(&DESC)
}

// The bit cursor of report id `id`, created at zero on first use.
fn cursor_for(cursors: &mut Vec<(u8, u32)>, id: u8) -> &mut u32 {
	if let Some(i) = cursors.iter().position(|&(cid, _)| cid == id) {
		return &mut cursors[i].1;
	}
	cursors.push((id, 0));
	&mut cursors.last_mut().unwrap().1
}

// A local usage item as a page-extended usage: a 4-byte item carries its own
// page in the high half, shorter ones take the current global page.
fn extended(page: u16, data: u32, dlen: usize) -> u32 {
	if dlen == 4 { data } else { (page as u32) << 16 | data }
}

// Extract `size` bits at `bit` from a report body, little-endian within and
// across bytes (the HID field packing). Bits past the body read as zero.
fn field(body: &[u8], bit: u32, size: u32) -> u32 {
	let mut v: u64 = 0;
	let mut got: u32 = 0;
	let mut byte: usize = (bit / 8) as usize;
	let mut shift: u32 = bit % 8;
	while got < size && byte < body.len() {
		v |= ((body[byte] >> shift) as u64) << got;
		got += 8 - shift;
		shift = 0;
		byte += 1;
	}
	(v & ((1u64 << size) - 1)) as u32
}

// The sign-extended reading of a field (for relative deltas and signed axes).
fn signed_field(body: &[u8], bit: u32, size: u32) -> i32 {
	let v: u32 = field(body, bit, size);
	if size < 32 && v >> (size - 1) & 1 != 0 { (v | !((1u32 << size) - 1)) as i32 } else { v as i32 }
}

// Scale an absolute axis value from its logical range into 0..=NORM_MAX.
fn scale(v: i32, min: i32, max: i32) -> i32 {
	let span: i64 = max as i64 - min as i64;
	if span <= 0 {
		return 0;
	}
	let v: i64 = (v as i64 - min as i64).clamp(0, span);
	(v * NORM_MAX as i64 / span) as i32
}

// The usage set an array field reports: each field value indexes the usage
// range (usage = usage_min + value - logical_min); zero is "no event" and the
// keyboard page's 1..=3 are its rollover / error codes, both skipped.
fn array_usages(seg: &Segment, body: &[u8]) -> ([u32; ARRAY_MAX], usize) {
	let mut out: [u32; ARRAY_MAX] = [0; ARRAY_MAX];
	let mut n: usize = 0;
	for i in 0..seg.count {
		if n == ARRAY_MAX {
			break;
		}
		let v: i32 = field(body, seg.bit_offset + i * seg.size, seg.size) as i32;
		if v == 0 || v < seg.logical_min {
			continue;
		}
		let usage: u32 = seg.usage_min.saturating_add((v - seg.logical_min) as u32);
		if seg.page == PAGE_KEYBOARD && usage & 0xffff <= 3 {
			continue;
		}
		if usage != 0 {
			out[n] = usage;
			n += 1;
		}
	}
	(out, n)
}

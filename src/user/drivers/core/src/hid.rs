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
/// Joysticks and flight controls report their axes here rather than on the desktop page.
pub const PAGE_SIMULATION: u16 = 0x02;
/// A gamepad's own page.
pub const PAGE_GAME: u16 = 0x05;
const PAGE_KEYBOARD: u16 = 0x07;
const PAGE_BUTTON: u16 = 0x09;
const PAGE_CONSUMER: u16 = 0x0c;
/// Tablets and touch surfaces. A contact's identifier, its tip switch and its axes are all here.
pub const PAGE_DIGITIZER: u16 = 0x0d;

/// The digitizer usages that carry contact identity, which is what makes several fingers several
/// contacts rather than one pointer that jumps between them.
pub const USAGE_TIP_SWITCH: u16 = 0x42;
pub const USAGE_CONTACT_IDENTIFIER: u16 = 0x51;
pub const USAGE_CONTACT_COUNT: u16 = 0x54;

/// The most collections a descriptor may nest before this parser refuses the rest of it.
///
/// A DEVICE-SUPPLIED DEPTH IS A LIMIT AND NOT A STYLE. `Collection` and `End Collection` are bytes
/// the DEVICE chose, and a descriptor that opens them and never closes them nested without bound -
/// nothing counted them at all. Eight is deeper than any real descriptor: a multi-touch digitizer is
/// application, then one logical collection per contact, which is two.
pub const MAX_COLLECTION_DEPTH: u8 = 8;

/// The most `Push` items a descriptor may nest. The same unbounded growth from the same input: the
/// global-state stack was a `Vec` a descriptor could fill.
const MAX_PUSH_DEPTH: usize = 8;

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
	/// How deep in the collection tree this field was declared. A multi-touch report expresses
	/// contact identity by COLLECTION - each finger is its own, holding an identifier, a tip switch
	/// and a pair of axes - so a parser that does not carry the depth cannot tell the second
	/// finger's X from the first's.
	depth: u8,
	/// The unit word and its exponent, as the descriptor stated them. Zero is "no unit declared",
	/// which is what most devices say and is different from declaring a dimensionless one.
	unit: u32,
	unit_exponent: i8,
}

impl Segment {
	// Read field `i` of this segment out of `body`, in the signedness the DESCRIPTOR declared.
	//
	// A NON-NEGATIVE LOGICAL MINIMUM MEANS THE FIELD IS UNSIGNED, and reading it signed is a defect
	// that hides behind the devices this tree has: an eight-bit absolute axis over `0..255` reports
	// 0xF0 for a touch near the right-hand edge, and sign-extended that is MINUS SIXTEEN - which
	// `scale` then clamps to zero, so the right-hand half of every such surface reads as the left
	// edge. It stayed invisible because the pointer this harness attaches declares sixteen-bit axes
	// over `0..0x7fff`, whose high bit is never set; the first eight-bit digitizer would have found
	// it, on hardware, as a tablet whose right half does not work.
	//
	// The parser already makes this distinction for the logical MAXIMUM - `logical_max_raw` exists
	// because a maximum that parses negative under a non-negative minimum was meant unsigned - and
	// this is the same rule applied to the value the field actually carries.
	fn reading(&self, body: &[u8], bit: u32) -> i32 {
		if self.logical_min >= 0 { field(body, bit, self.size) as i32 } else { signed_field(body, bit, self.size) }
	}

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
/// One contact on a touch surface: which finger the device says it is, whether it is touching, and
/// where - the axes scaled into the same `0..=NORM_MAX` grid the pointer path uses.
///
/// THE IDENTIFIER IS THE DEVICE'S AND IT PERSISTS. It is what makes a drag a drag rather than two
/// touches at different places, and it is the field a parser that flattens contacts destroys first.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Contact {
	pub id: u8,
	pub tip: bool,
	pub x: i32,
	pub y: i32,
}

/// The most contacts this module decodes out of one report. Ten fingers is the bound every touch
/// specification states, and it is what a caller's array costs.
pub const MAX_CONTACTS: usize = 10;

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
	/// The unit word and its exponent, which nothing read before. A tablet reports absolute
	/// position IN A UNIT, and a consumer that cannot ask publishes counts as if they were pixels.
	unit: u32,
	unit_exponent: i8,
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
	// Whether this descriptor carries anything a driver above it can act on.
	//
	// A DIGITIZER IS ONE OF THEM, and it was not in this list. Most touch surfaces report their axes
	// on the GENERIC DESKTOP page, so `has_pointer` happens to be true of them and they were
	// admitted by accident rather than by this rule; one that reports its axes on the digitizer page
	// - which the specification allows and some devices do - was addressed, configured, had its
	// report descriptor read, and was then refused here with no word at all. Found by reading this
	// function rather than by a device failing, which is why it is written as a gap and not as a
	// measurement.
	pub fn is_useful(&self) -> bool {
		self.has_keyboard() || self.has_pointer() || self.has_consumer() || self.has_digitizer()
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

	// Whether this device reports on the digitizer page at all - a tablet, a touch surface, or a
	// pen. Asked before the contacts are read, so a device with none costs no walk.
	pub fn has_digitizer(&self) -> bool {
		self.segs.iter().any(|s| s.page == PAGE_DIGITIZER)
	}

	// The deepest collection this descriptor declared, which is what the depth bound is about.
	pub fn collection_depth(&self) -> u8 {
		self.segs.iter().map(|s| s.depth).max().unwrap_or(0)
	}

	// The unit and its exponent declared for a field, or `None` when the device declared none -
	// which is what most of them say and is a different answer from declaring a dimensionless one.
	pub fn unit_for(&self, page: u16, usage: u16) -> Option<(u32, i8)> {
		let seg = self.segs.iter().find(|s| s.page == page && (0..s.count).any(|i| s.usage_for(i) & 0xffff == usage as u32))?;
		if seg.unit == 0 { None } else { Some((seg.unit, seg.unit_exponent)) }
	}

	// Decode the contacts of report `id`, answering how many were written.
	//
	// A NEW CONTACT BEGINS AT EACH CONTACT IDENTIFIER, which is how a multi-touch report says where
	// one finger's fields end and the next one's begin: the identifier, its tip switch and its pair
	// of axes are declared together inside one collection, repeated per finger. A reader that took
	// every X on the page would take the LAST finger's for all of them.
	//
	// AND CONTACT COUNT IS WHAT SAYS HOW MANY ARE REAL. A digitizer declares slots for every finger
	// it can ever report and fills the unused ones with whatever was there before - so a reader that
	// takes every declared slot reports phantom contacts, at stale positions, that nothing is
	// touching. The count is a field in the same report and it is the answer.
	pub fn contacts(&self, id: u8, body: &[u8], out: &mut [Contact]) -> usize {
		if out.is_empty() {
			return 0;
		}
		let mut written: usize = 0;
		let mut open: Option<Contact> = None;
		let mut count: Option<usize> = None;
		// IN BIT ORDER AND NOT DECLARATION ORDER, because what groups a contact's fields is where
		// they sit in the report, and a descriptor may declare the pieces in any order it likes.
		let mut ordered: Vec<&Segment> = self.segs.iter().filter(|s| s.report_id == id && s.variable && matches!(s.page, PAGE_DIGITIZER | PAGE_GENERIC_DESKTOP)).collect();
		ordered.sort_by_key(|s| s.bit_offset);
		for seg in ordered {
			for i in 0..seg.count {
				let bit: u32 = seg.bit_offset + i * seg.size;
				let usage: u16 = (seg.usage_for(i) & 0xffff) as u16;
				if seg.page == PAGE_DIGITIZER {
					match usage {
						USAGE_CONTACT_COUNT => {
							count = Some(field(body, bit, seg.size) as usize);
							continue;
						}
						USAGE_CONTACT_IDENTIFIER => {
							if let Some(done) = open.take()
								&& written < out.len()
							{
								out[written] = done;
								written += 1;
							}
							open = Some(Contact { id: field(body, bit, seg.size) as u8, tip: false, x: 0, y: 0 });
							continue;
						}
						USAGE_TIP_SWITCH => {
							if let Some(contact) = open.as_mut() {
								contact.tip = field(body, bit, seg.size) != 0;
							}
							continue;
						}
						_ => continue,
					}
				}
				let Some(contact) = open.as_mut() else { continue };
				let axis: &mut i32 = match usage {
					USAGE_X => &mut contact.x,
					USAGE_Y => &mut contact.y,
					_ => continue,
				};
				*axis = scale(seg.reading(body, bit), seg.logical_min, seg.logical_max);
			}
		}
		if let Some(done) = open.take()
			&& written < out.len()
		{
			out[written] = done;
			written += 1;
		}
		// THE COUNT BOUNDS WHAT WAS DECODED AND DOES NOT EXTEND IT. A device claiming more contacts
		// than its report carries slots for is describing fields that are not there.
		match count {
			Some(count) => written.min(count),
			None => written,
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
						// A RELATIVE AXIS IS ALWAYS SIGNED whatever its logical minimum says, because
						// what it carries is a DELTA and a delta with no sign is a mouse that only
						// moves right.
						*axis = (*axis + signed_field(body, bit, seg.size)).clamp(0, NORM_MAX);
					} else {
						*axis = scale(seg.reading(body, bit), seg.logical_min, seg.logical_max);
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
/// Read a HID unit exponent, which is a FOUR-BIT SIGNED nibble and not a byte.
///
/// 0x0F IS MINUS ONE. The values run 0..7 for zero to seven and 8..15 for minus eight to minus one,
/// so a driver reading the byte as unsigned scales a tablet's reported position by ten to the
/// fifteenth instead of dividing it by ten. The number is small either way, which is what makes the
/// mistake survive a glance.
pub fn nibble_exponent(data: u32) -> i8 {
	let nibble = (data & 0x0F) as i8;
	if nibble > 7 { nibble - 16 } else { nibble }
}

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
	// The collection nesting this walk is inside, and whether it ever went past the bound. A
	// descriptor that nested too deep has its REMAINING fields dropped rather than the whole
	// descriptor refused: what was declared before the runaway is still what the device said.
	let mut depth: u8 = 0;
	let mut over_nested: bool = false;
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
				// COLLECTIONS ARE COUNTED NOW, AND THE DEPTH IS BOUNDED. `Collection` is tag 10 and
				// `End Collection` is tag 12, and this parser passed over both - so the depth was
				// not merely unused, it was unknown, and a descriptor that opens collections without
				// closing them was refused by nothing. Both numbers come from the device.
				if tag == 10 {
					depth = depth.saturating_add(1);
					if depth > MAX_COLLECTION_DEPTH {
						over_nested = true;
					}
				} else if tag == 12 {
					depth = depth.saturating_sub(1);
				}
				if tag == 8 && !over_nested {
					let cursor: &mut u32 = cursor_for(&mut cursors, g.id);
					let bits: u32 = g.size.saturating_mul(g.count);
					// THE PAGES A SEGMENT MAY BE ON, STATED. This was four, and every field on any other page was
					// dropped BEFORE it was decoded - which is what flattening a tablet into a mouse
					// actually is in this tree: not a lossy mapping downstream, but a filter upstream
					// that never let the fields exist.
					let interesting: bool = matches!(g.page, PAGE_GENERIC_DESKTOP | PAGE_SIMULATION | PAGE_GAME | PAGE_KEYBOARD | PAGE_BUTTON | PAGE_CONSUMER | PAGE_DIGITIZER);
					// SATURATING, NOT `+`. The cursor itself saturates as a descriptor walks past the
					// bound, so a plain addition here overflows on the very descriptor the check
					// exists to refuse - which panics in a debug build and, in a release one, wraps
					// to a small number and ADMITS the segment it was meant to reject.
					let end: u32 = cursor.saturating_add(bits);
					if data & INPUT_CONSTANT == 0 && interesting && g.size >= 1 && g.size <= 32 && end <= MAX_REPORT_BYTES * 8 {
						let logical_max: i32 = if g.logical_min >= 0 && g.logical_max < g.logical_min { g.logical_max_raw as i32 } else { g.logical_max };
						segs.push(Segment { report_id: g.id, bit_offset: *cursor, size: g.size, count: g.count, variable: data & INPUT_VARIABLE != 0, relative: data & INPUT_RELATIVE != 0, page: g.page, usages: core::mem::take(&mut usages), usage_min, usage_max, logical_min: g.logical_min, logical_max, depth, unit: g.unit, unit_exponent: g.unit_exponent });
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
				// The unit exponent is a FOUR-BIT SIGNED nibble, not a byte: 0x0F is -1 and not 15,
				// and a driver reading it as unsigned scales a tablet's position by ten to the
				// fifteenth.
				5 => g.unit_exponent = nibble_exponent(data),
				6 => g.unit = data,
				7 => g.size = data,
				8 => {
					g.id = data as u8;
					uses_ids = true;
				}
				9 => g.count = data,
				// BOUNDED, for the same reason the collection depth is: the stack was a `Vec` a
				// descriptor could fill with `Push` items and nothing said how many.
				10 => {
					if stack.len() < MAX_PUSH_DEPTH {
						stack.push(g);
					}
				}
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

// Record a report body as the state the NEXT diff runs against.
//
// THE TAIL IS ZEROED AND NOT LEFT. A report shorter than the last one used to overwrite only the
// bytes it carried, so whatever the previous report held past its end stayed in the state - and a
// key whose usage lives in that tail is never released. It reads as a key that sticks down after a
// truncated report, which is what a device sends when it is unplugged mid-transfer.
pub fn remember(state: &mut [u8; MAX_REPORT_BYTES as usize], body: &[u8]) {
	let len: usize = body.len().min(MAX_REPORT_BYTES as usize);
	state[..len].copy_from_slice(&body[..len]);
	state[len..].fill(0);
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
		// IN `i64`, THEN CLAMPED. Both operands come from a descriptor the device wrote: a logical
		// minimum of `i32::MIN` against a reading of zero overflows an `i32` subtraction, which
		// panics in a debug build and wraps to a plausible-looking usage in a release one.
		let offset: i64 = (v as i64 - seg.logical_min as i64).clamp(0, u32::MAX as i64);
		let usage: u32 = seg.usage_min.saturating_add(offset as u32);
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

#[cfg(test)]
mod tests;

//! THE FRAME ACCOUNT'S RECORD BUFFER: one bounded array of fixed records the kernel appends to while
//! a measurement is armed, and the line form the drain writes them out in afterwards.
//!
//! WHY ONE BUFFER IN THE KERNEL AND NOT ONE PER PROCESS. The processes on the measured frame path are
//! not alike: the application ends, and DisplayService and the display driver do not. A buffer of
//! their own would have no moment to leave - nothing tells a long-lived service that the run it was
//! part of is over - so every process appends here, and the application that armed the buffer asks
//! the kernel to drain it when its measured frames end.
//!
//! WHAT AN APPEND IS, AND WHAT IT IS NOT. One atomic index bump claims a slot, the record's fields go
//! in, and its kind byte is stored last with release ordering, which is what marks the slot complete.
//! An append never blocks, never allocates and never formats. A record past the bound is REFUSED AND
//! COUNTED rather than written over an older one: a ring that silently dropped the first frames of
//! every run - or, wrapping the other way, the last - would be an instrument that reports the frames
//! nobody cared about, and the count is what lets a reader of the drain refuse such a run.
//!
//! WHY IT IS A CRATE OF ITS OWN. The kernel is not host-testable, and the layout, the bound, the
//! refusal count and the line form are exactly the things a collector on the host depends on - so
//! they live here, where a test can drive them with a buffer of six slots, and the kernel takes the
//! answers, as it takes the ACPI tables from `acpi`.

#![cfg_attr(not(test), no_std)]

use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering};

/// One record's size. Fixed, because the bound is stated in bytes and a record that grew would
/// quietly shrink the number of them the buffer holds.
pub const RECORD_BYTES: usize = 32;
/// The buffer's size: 8 MiB, which is 262,144 records.
pub const BUFFER_BYTES: usize = 8 << 20;
/// How many records the kernel's buffer holds.
pub const CAPACITY: usize = BUFFER_BYTES / RECORD_BYTES;

/// What a record is. Zero is never a kind: it is how an unfinished slot looks.
pub const KIND_SITE: u8 = 1;
pub const KIND_SWITCH: u8 = 2;
pub const KIND_WAKE: u8 = 3;
/// One eight-byte chunk of the name of a thread's process, written in a contiguous group of
/// [`NAME_CHUNKS`] the first time a thread is seen in an armed window.
pub const KIND_THREAD: u8 = 4;

/// Why a thread left its core, carried in a switch record's `detail`.
pub const LEFT_BLOCKED: u8 = 1;
pub const LEFT_PREEMPTED: u8 = 2;
pub const LEFT_YIELDED: u8 = 3;
pub const LEFT_EXITED: u8 = 4;

/// What made a thread runnable, carried in a wake record's `detail`. A MESSAGE is a channel send
/// reaching a thread waiting on that channel; a DEADLINE is the tick that passed a wait's deadline;
/// everything else - a close, an event, an interrupt, a signal, a timer object - is OTHER.
pub const WOKEN_BY_MESSAGE: u8 = 1;
pub const WOKEN_BY_DEADLINE: u8 = 2;
pub const WOKEN_BY_OTHER: u8 = 3;

/// How many eight-byte chunks a process name gets. Thirty-two bytes names every process on the
/// measured path; a longer name is cut, not refused.
pub const NAME_CHUNKS: usize = 4;

/// The longest line [`format_record`] and the other formatters write.
pub const LINE_MAX: usize = 128;

/// `SYS_PERF_CONTROL`'s operations.
pub const CONTROL_ARM: u64 = 1;
pub const CONTROL_DISARM: u64 = 2;
pub const CONTROL_DRAIN: u64 = 3;

/// How long a drain waits for one claimed slot to be completed by its writer before it counts the
/// slot as incomplete. Every writer completes its slot with interrupts masked a few stores after
/// claiming it, so this bound is never reached by a working kernel and exists so that a broken one
/// produces a count rather than a hang.
const COMPLETION_SPINS: u32 = 1 << 20;

/// One record: an eight-byte site tag, a cycle count, one value, and who and where.
///
/// For a SITE the tag is the caller's, `cycles` is its own clock reading and `value` its value. For
/// a SWITCH `thread` is the incoming thread (0 is the core's idle context), `value` the outgoing one
/// and `detail` why it left. For a WAKE `thread` is the woken thread, `value` its waker (0 for the
/// tick) and `detail` the cause; `core` is the core it was queued on. For a THREAD chunk `value` is
/// the process's identity, `site` eight bytes of its name and `detail` the chunk's index.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Record {
	pub site: [u8; 8],
	pub cycles: u64,
	pub value: u64,
	pub thread: u32,
	pub core: u16,
	pub kind: u8,
	pub detail: u8,
}

const _: () = assert!(core::mem::size_of::<Record>() == RECORD_BYTES);

/// What happened to one append.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Push {
	Accepted,
	/// Nothing is armed, so nothing was measured: dropped, and not counted, because a window that
	/// is not open cannot lose anything.
	Unarmed,
	/// Armed and full: refused, and counted.
	Refused,
}

/// The buffer. A `const` value with no storage until [`Buffer::attach`] gives it some, so the kernel
/// can hold it in a `static` and attach 8 MiB only on a boot that asked to be measured.
pub struct Buffer {
	slots: AtomicPtr<Record>,
	capacity: AtomicUsize,
	next: AtomicUsize,
	refused: AtomicU64,
	armed: AtomicBool,
	/// The clock reading the window was armed at. A record earlier than it was taken before the
	/// window opened and reached the buffer late - a site's clock is read before its syscall - and
	/// the drain leaves it out and counts it rather than let it sit inside a frame it predates.
	armed_at: AtomicU64,
	/// Bumped at every arm, so a caller can tell "seen in this window" from "seen in an earlier one"
	/// with one comparison - which is how a thread's name is written once per window.
	generation: AtomicU64,
}

impl Default for Buffer {
	fn default() -> Self {
		Buffer::new()
	}
}

impl Buffer {
	pub const fn new() -> Buffer {
		Buffer { slots: AtomicPtr::new(core::ptr::null_mut()), capacity: AtomicUsize::new(0), next: AtomicUsize::new(0), refused: AtomicU64::new(0), armed: AtomicBool::new(false), armed_at: AtomicU64::new(0), generation: AtomicU64::new(0) }
	}

	/// Give the buffer its storage, once, before anything arms it.
	///
	/// # Safety
	///
	/// `slots` must be valid for reads and writes of `capacity` records for as long as the buffer
	/// is used, and nothing else may touch that memory.
	pub unsafe fn attach(&self, slots: *mut Record, capacity: usize) {
		self.capacity.store(0, Ordering::Release);
		self.slots.store(slots, Ordering::Release);
		self.capacity.store(capacity, Ordering::Release);
	}

	/// Whether the buffer has storage at all - false on every boot that did not ask to be measured.
	pub fn attached(&self) -> bool {
		self.capacity.load(Ordering::Acquire) != 0
	}

	/// The one test every hook makes while nothing is measured.
	#[inline(always)]
	pub fn armed(&self) -> bool {
		self.armed.load(Ordering::Relaxed)
	}

	pub fn generation(&self) -> u64 {
		self.generation.load(Ordering::Acquire)
	}

	pub fn capacity(&self) -> usize {
		self.capacity.load(Ordering::Acquire)
	}

	/// Empty the buffer and start accepting. `now` is the clock reading the window opens at.
	/// Returns false when there is no storage to arm.
	pub fn arm(&self, now: u64) -> bool {
		let capacity = self.capacity.load(Ordering::Acquire);
		if capacity == 0 {
			return false;
		}
		self.armed.store(false, Ordering::SeqCst);
		// THE PREVIOUS WINDOW'S SLOTS ARE MARKED UNFINISHED, so a slot this window claims and has not
		// completed yet can never be mistaken for the complete record that sat there before.
		let used = self.next.load(Ordering::Acquire).min(capacity);
		let slots = self.slots.load(Ordering::Acquire);
		for index in 0..used {
			// SAFETY: `index < capacity`, and `attach`'s contract makes the storage ours.
			unsafe { kind_of(slots.add(index)).store(0, Ordering::Relaxed) };
		}
		self.next.store(0, Ordering::SeqCst);
		self.refused.store(0, Ordering::SeqCst);
		self.armed_at.store(now, Ordering::SeqCst);
		self.generation.fetch_add(1, Ordering::SeqCst);
		self.armed.store(true, Ordering::SeqCst);
		true
	}

	/// Stop accepting. What is in the buffer stays until the next arm.
	pub fn disarm(&self) {
		self.armed.store(false, Ordering::SeqCst);
	}

	/// Append one record.
	pub fn push(&self, record: Record) -> Push {
		self.push_group(core::slice::from_ref(&record))
	}

	/// Append records into CONTIGUOUS slots, all of them or none: a thread's name is four chunks,
	/// and a group split across a refusal would be a name with a hole in it.
	pub fn push_group(&self, records: &[Record]) -> Push {
		if !self.armed.load(Ordering::Acquire) || records.is_empty() {
			return Push::Unarmed;
		}
		let capacity = self.capacity.load(Ordering::Acquire);
		let first = self.next.fetch_add(records.len(), Ordering::AcqRel);
		if first.checked_add(records.len()).is_none_or(|end| end > capacity) {
			self.refused.fetch_add(records.len() as u64, Ordering::Relaxed);
			return Push::Refused;
		}
		let slots = self.slots.load(Ordering::Acquire);
		for (offset, record) in records.iter().enumerate() {
			// SAFETY: the index is below `capacity` (checked above) and was claimed by this call
			// alone, so no other writer touches the slot; the drain reads it only once its kind is
			// published below.
			unsafe {
				let slot = slots.add(first + offset);
				(&raw mut (*slot).site).write(record.site);
				(&raw mut (*slot).cycles).write(record.cycles);
				(&raw mut (*slot).value).write(record.value);
				(&raw mut (*slot).thread).write(record.thread);
				(&raw mut (*slot).core).write(record.core);
				(&raw mut (*slot).detail).write(record.detail);
				kind_of(slot).store(record.kind, Ordering::Release);
			}
		}
		Push::Accepted
	}

	/// Disarm, then read back every claimed slot in the order it was claimed.
	///
	/// A slot whose writer has not completed it within the bound is INCOMPLETE and counted, and a
	/// record earlier than the arm is STALE and counted; neither is handed to `each`.
	pub fn drain(&self, mut each: impl FnMut(&Record)) -> Drained {
		self.disarm();
		let capacity = self.capacity.load(Ordering::Acquire);
		let claimed = self.next.load(Ordering::Acquire);
		let used = claimed.min(capacity);
		let armed_at = self.armed_at.load(Ordering::Acquire);
		let slots = self.slots.load(Ordering::Acquire);
		let mut drained = Drained { records: 0, refused: self.refused.load(Ordering::Acquire), incomplete: 0, stale: 0 };
		for index in 0..used {
			// SAFETY: below `capacity`, and the kind's acquire load orders the field reads after the
			// writer's release store.
			let record = unsafe {
				let slot = slots.add(index);
				let mut spins = 0u32;
				let kind = loop {
					let kind = kind_of(slot).load(Ordering::Acquire);
					if kind != 0 || spins >= COMPLETION_SPINS {
						break kind;
					}
					spins += 1;
					core::hint::spin_loop();
				};
				if kind == 0 {
					drained.incomplete += 1;
					continue;
				}
				Record { site: (&raw const (*slot).site).read(), cycles: (&raw const (*slot).cycles).read(), value: (&raw const (*slot).value).read(), thread: (&raw const (*slot).thread).read(), core: (&raw const (*slot).core).read(), kind, detail: (&raw const (*slot).detail).read() }
			};
			if record.cycles < armed_at {
				drained.stale += 1;
				continue;
			}
			drained.records += 1;
			each(&record);
		}
		drained
	}
}

/// What a drain found: the records it handed out, and every way a record did not make it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drained {
	pub records: u64,
	pub refused: u64,
	pub incomplete: u64,
	pub stale: u64,
}

/// The kind byte of a slot, as the atomic both sides agree on.
///
/// # Safety
///
/// `slot` must point into attached storage.
unsafe fn kind_of<'a>(slot: *mut Record) -> &'a AtomicU8 {
	// SAFETY: the field is a `u8` in memory this buffer owns, and every access to it goes through
	// this view, so it is only ever accessed atomically.
	unsafe { AtomicU8::from_ptr(&raw mut (*slot).kind) }
}

/// A site tag as it is written into a record: the caller's bytes, and anything a line could not
/// carry - a space, a control byte, a byte past ASCII - made a `?`. A tag is never allowed to split
/// a line or add a field to it.
pub fn site_tag(raw: u64) -> [u8; 8] {
	let mut tag = raw.to_le_bytes();
	for byte in tag.iter_mut() {
		if *byte != 0 && !(0x21..=0x7e).contains(byte) {
			*byte = b'?';
		}
	}
	tag
}

/// The records naming a thread's process, as the contiguous group `push_group` takes.
pub fn name_records(thread: u32, process: u64, name: &[u8], cycles: u64, core: u16) -> [Record; NAME_CHUNKS] {
	let mut records = [Record { site: [0; 8], cycles, value: process, thread, core, kind: KIND_THREAD, detail: 0 }; NAME_CHUNKS];
	for (index, record) in records.iter_mut().enumerate() {
		record.detail = index as u8;
		let start = index * 8;
		for (offset, byte) in record.site.iter_mut().enumerate() {
			let raw = name.get(start + offset).copied().unwrap_or(0);
			*byte = if raw == 0 || (0x21..=0x7e).contains(&raw) { raw } else { b'?' };
		}
	}
	records
}

/// Why a thread left, as the drain spells it.
pub fn left_name(detail: u8) -> &'static [u8] {
	match detail {
		LEFT_BLOCKED => b"blocked",
		LEFT_PREEMPTED => b"preempted",
		LEFT_YIELDED => b"yielded",
		LEFT_EXITED => b"exited",
		_ => b"unknown",
	}
}

/// What woke a thread, as the drain spells it.
pub fn woken_name(detail: u8) -> &'static [u8] {
	match detail {
		WOKEN_BY_MESSAGE => b"message",
		WOKEN_BY_DEADLINE => b"deadline",
		WOKEN_BY_OTHER => b"other",
		_ => b"unknown",
	}
}

/// A line under construction, which cannot overrun.
struct Line<'a> {
	out: &'a mut [u8; LINE_MAX],
	len: usize,
}

impl Line<'_> {
	fn bytes(&mut self, bytes: &[u8]) {
		for &byte in bytes {
			if self.len < LINE_MAX - 1 {
				self.out[self.len] = byte;
				self.len += 1;
			}
		}
	}

	fn number(&mut self, value: u64) {
		let mut digits = [0u8; 20];
		let mut count = 0usize;
		let mut value = value;
		loop {
			digits[count] = b'0' + (value % 10) as u8;
			count += 1;
			value /= 10;
			if value == 0 {
				break;
			}
		}
		while count > 0 {
			count -= 1;
			self.bytes(&[digits[count]]);
		}
	}

	fn field(&mut self, value: u64) {
		self.bytes(b" ");
		self.number(value);
	}

	fn end(mut self) -> usize {
		self.bytes(b"\n");
		self.len
	}
}

/// One record as its drain line - the `\x1ePERF <label> <cycles> <value>` form the console tracer
/// already parses, extended by three fields:
///
/// ```text
/// \x1ePERF <site> <cycles> <value> <thread> <core> site
/// \x1ePERF switch <cycles> <outgoing> <incoming> <core> <why it left>
/// \x1ePERF wake <cycles> <waker> <woken> <core> <what woke it>
/// ```
///
/// A THREAD chunk has no line of its own: it belongs to the table [`drain_lines`] writes last.
/// Returns the line's length, or zero for a record that has none.
pub fn format_record(record: &Record, out: &mut [u8; LINE_MAX]) -> usize {
	let mut line = Line { out, len: 0 };
	line.bytes(b"\x1ePERF ");
	match record.kind {
		KIND_SITE => {
			let tag = site_tag(u64::from_le_bytes(record.site));
			let end = tag.iter().position(|byte| *byte == 0).unwrap_or(tag.len());
			line.bytes(if end == 0 { b"?" } else { &tag[..end] });
			line.field(record.cycles);
			line.field(record.value);
			line.field(record.thread as u64);
			line.field(record.core as u64);
			line.bytes(b" site");
		}
		KIND_SWITCH => {
			line.bytes(b"switch");
			line.field(record.cycles);
			line.field(record.value);
			line.field(record.thread as u64);
			line.field(record.core as u64);
			line.bytes(b" ");
			line.bytes(left_name(record.detail));
		}
		KIND_WAKE => {
			line.bytes(b"wake");
			line.field(record.cycles);
			line.field(record.value);
			line.field(record.thread as u64);
			line.field(record.core as u64);
			line.bytes(b" ");
			line.bytes(woken_name(record.detail));
		}
		_ => return 0,
	}
	line.end()
}

/// Drain `buffer` as lines: every record in claim order, then the table naming each thread's process
/// (`\x1ePERF-THREAD <thread> <process> <name>`), then one line with the counts
/// (`\x1ePERF-END <records> <refused> <incomplete> <stale>`). The table is written from a second
/// read of the buffer, so it names every thread that was named in the window whatever order the
/// records came in.
pub fn drain_lines(buffer: &Buffer, emit: &mut dyn FnMut(&[u8])) -> Drained {
	let mut line = [0u8; LINE_MAX];
	let drained = buffer.drain(|record| {
		let len = format_record(record, &mut line);
		if len != 0 {
			emit(&line[..len]);
		}
	});
	// THE TABLE, from the name groups. A group is contiguous by construction, so its chunks are the
	// records that follow its first one; a group cut short by a drain that found a chunk incomplete
	// names what it has.
	let mut pending: Option<(u32, u64, [u8; NAME_CHUNKS * 8], usize)> = None;
	let flush = |pending: &mut Option<(u32, u64, [u8; NAME_CHUNKS * 8], usize)>, emit: &mut dyn FnMut(&[u8])| {
		if let Some((thread, process, name, len)) = pending.take() {
			let mut out = [0u8; LINE_MAX];
			let mut line = Line { out: &mut out, len: 0 };
			line.bytes(b"\x1ePERF-THREAD");
			line.field(thread as u64);
			line.field(process);
			line.bytes(b" ");
			let end = name[..len].iter().position(|byte| *byte == 0).unwrap_or(len);
			line.bytes(if end == 0 { b"?" } else { &name[..end] });
			let n = line.end();
			emit(&out[..n]);
		}
	};
	// The second read is of the same, now disarmed, buffer: nothing can have been claimed since.
	let capacity = buffer.capacity.load(Ordering::Acquire);
	let used = buffer.next.load(Ordering::Acquire).min(capacity);
	let slots = buffer.slots.load(Ordering::Acquire);
	for index in 0..used {
		// SAFETY: below `capacity`; the drain above already waited for every slot to complete.
		let (kind, thread, process, detail, site) = unsafe {
			let slot = slots.add(index);
			(kind_of(slot).load(Ordering::Acquire), (&raw const (*slot).thread).read(), (&raw const (*slot).value).read(), (&raw const (*slot).detail).read(), (&raw const (*slot).site).read())
		};
		if kind != KIND_THREAD {
			continue;
		}
		if detail == 0 {
			flush(&mut pending, emit);
			pending = Some((thread, process, [0; NAME_CHUNKS * 8], 0));
		}
		if let Some((pending_thread, _, name, len)) = pending.as_mut()
			&& *pending_thread == thread
			&& (detail as usize) < NAME_CHUNKS
		{
			let start = detail as usize * 8;
			name[start..start + 8].copy_from_slice(&site);
			*len = (*len).max(start + 8);
		}
	}
	flush(&mut pending, emit);
	let mut out = [0u8; LINE_MAX];
	let mut end = Line { out: &mut out, len: 0 };
	end.bytes(b"\x1ePERF-END");
	end.field(drained.records);
	end.field(drained.refused);
	end.field(drained.incomplete);
	end.field(drained.stale);
	let n = end.end();
	emit(&out[..n]);
	drained
}

#[cfg(test)]
mod tests;

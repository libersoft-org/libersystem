//! THE DECISION JOURNAL'S GEOMETRY: segments, rotation, reservations, pins, the index a reader pages through,
//! and the emergency ring that holds what storage would not take.
//!
//! A SEGMENT IS ONE FILE of framed records behind a header, appended through the volume's transactional
//! writer - one commit per record. At most 1024 records and 8 MB, header included, whichever comes first;
//! at most four segments; and only a COMPLETE segment is ever retired, oldest first. A complete encoded record
//! is at most 8192 bytes, framing included, which keeps a segment far inside the writer's own 64 MB cap.
//!
//! SPACE IS RESERVED AT ADMISSION. A request is admitted only if the journal can take every record it may
//! produce; and a segment that still holds a record of a request in flight is PINNED - a rotation that would
//! retire it is refused, and so is the request that would need it.
//!
//! RECORDS GO OUT IN ORDER, ONE AT A TIME. A record storage would not take stays at the head of the queue and
//! is tried again; everything behind it waits its turn, so nothing is written out of order. The queue is the
//! emergency ring: 256 records, the oldest dropped - and counted - when it is full. It is memory: a crash, or
//! an eviction, loses what it held, and nothing it holds is ever described as durable.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub const SEGMENTS: usize = 4;
pub const RECORDS: u32 = 1024;
pub const SEGMENT_BYTES: u64 = 8 * 1024 * 1024;
pub const RECORD_BYTES: usize = 8192;
pub const FRAME: usize = 4;
pub const HEADER: usize = 32;
pub const RING: usize = 256;
pub const MAGIC: [u8; 8] = *b"LSADMJ01";
/// Records a request may produce: requested, granted or declined, consumed, and an outcome or its expiry.
pub const PER_REQUEST: u32 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Segment {
	pub number: u64,
	pub first: u64,
	pub records: u32,
	pub bytes: u64,
}

impl Segment {
	fn complete(&self) -> bool {
		self.records >= RECORDS
	}

	fn fits(&self, length: usize) -> bool {
		self.records < RECORDS && self.bytes + length as u64 <= SEGMENT_BYTES
	}
}

/// Where one record goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Placement {
	pub segment: u64,
	pub offset: u64,
	/// The segment is new: its header goes first, at offset zero.
	pub new_segment: bool,
	/// A complete segment to retire once this record is committed.
	pub retire: Option<u64>,
}

/// One retained record, for a reader.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Located {
	pub sequence: u64,
	pub segment: u64,
	pub offset: u64,
	pub length: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Past the record bound.
	Oversized,
	/// The rotation it needs would retire a segment a request in flight still has a record in.
	Pinned,
}

/// A record waiting to be written.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
	pub sequence: u64,
	/// The request it belongs to, for its pin.
	pub request: u64,
	/// Framed bytes.
	pub bytes: Vec<u8>,
	/// Its first attempt is still to be answered to whoever is waiting on it.
	pub first_attempt: bool,
}

pub struct Journal {
	segments: Vec<Segment>,
	index: Vec<Located>,
	next_sequence: u64,
	next_segment: u64,
	/// What each request in flight may still write: its reservation, less what it has written.
	reservations: Vec<(u64, u32)>,
	pins: Vec<(u64, u64)>,
	queue: VecDeque<Entry>,
	/// Records the ring had to drop.
	pub evicted: u64,
	/// The last write failed and nothing since has succeeded.
	pub failing: bool,
}

impl Default for Journal {
	fn default() -> Self {
		Self::new()
	}
}

/// The frame around one record: its length, then its bytes.
pub fn frame(record: &[u8]) -> Result<Vec<u8>, Refusal> {
	if record.len() + FRAME > RECORD_BYTES {
		return Err(Refusal::Oversized);
	}
	let mut framed = Vec::with_capacity(record.len() + FRAME);
	framed.extend_from_slice(&(record.len() as u32).to_le_bytes());
	framed.extend_from_slice(record);
	Ok(framed)
}

/// A segment header: the magic, the segment's number and its first sequence.
pub fn header(segment: u64, first: u64) -> [u8; HEADER] {
	let mut bytes = [0u8; HEADER];
	bytes[..8].copy_from_slice(&MAGIC);
	bytes[8..16].copy_from_slice(&segment.to_le_bytes());
	bytes[16..24].copy_from_slice(&first.to_le_bytes());
	bytes
}

/// Read a segment back: its header and every whole frame, with where each is. A torn tail - a frame whose
/// length runs past the file, or past the bound - ends the scan; nothing after it is believed.
pub fn scan(bytes: &[u8]) -> Option<(u64, u64, Vec<(u64, &[u8])>)> {
	if bytes.len() < HEADER || bytes[..8] != MAGIC {
		return None;
	}
	let number = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
	let first = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
	let mut frames = Vec::new();
	let mut at = HEADER;
	while at + FRAME <= bytes.len() {
		let length = u32::from_le_bytes(bytes[at..at + FRAME].try_into().ok()?) as usize;
		if length + FRAME > RECORD_BYTES || at + FRAME + length > bytes.len() {
			break;
		}
		frames.push((at as u64, &bytes[at + FRAME..at + FRAME + length]));
		at += FRAME + length;
	}
	Some((number, first, frames))
}

impl Journal {
	pub fn new() -> Journal {
		Journal { segments: Vec::new(), index: Vec::new(), next_sequence: 1, next_segment: 1, reservations: Vec::new(), pins: Vec::new(), queue: VecDeque::with_capacity(RING), evicted: 0, failing: false }
	}

	/// What a restart found on disk: the segments, oldest first, and every record's place in them. Read for
	/// investigation and for numbering; nothing in it is authority.
	pub fn recover(&mut self, segments: Vec<Segment>, index: Vec<Located>) {
		self.next_segment = segments.iter().map(|segment| segment.number + 1).max().unwrap_or(1);
		self.next_sequence = index.iter().map(|located| located.sequence + 1).max().unwrap_or(1).max(segments.iter().map(|segment| segment.first).max().unwrap_or(1));
		self.segments = segments;
		self.index = index;
	}

	pub fn sequence(&mut self) -> u64 {
		let sequence = self.next_sequence;
		self.next_sequence += 1;
		sequence
	}

	/// How many more records the journal can promise to take: what the active segment has left, whole new
	/// segments for the free slots, and whole segments that could be retired because nothing pins them.
	fn capacity(&self) -> u64 {
		let active = self.segments.last().map_or(0, |segment| u64::from(RECORDS.saturating_sub(segment.records)));
		let free = (SEGMENTS - self.segments.len().min(SEGMENTS)) as u64;
		let retirable = self.segments.iter().take(self.segments.len().saturating_sub(1)).take_while(|segment| segment.complete() && !self.pinned(segment.number)).count() as u64;
		active + (free + retirable) * u64::from(RECORDS)
	}

	fn pinned(&self, segment: u64) -> bool {
		self.pins.iter().any(|(_, pinned)| *pinned == segment)
	}

	/// RESERVE AT ADMISSION: room for every record one more request may produce, or a refusal.
	pub fn reserve(&mut self, request: u64) -> Result<(), Refusal> {
		let reserved: u64 = self.reservations.iter().map(|(_, remaining)| u64::from(*remaining)).sum();
		if reserved + u64::from(PER_REQUEST) > self.capacity() {
			return Err(Refusal::Pinned);
		}
		self.reservations.push((request, PER_REQUEST));
		Ok(())
	}

	/// A request is over: what it did not write is free again.
	pub fn release(&mut self, request: u64) {
		self.reservations.retain(|(reserved, _)| *reserved != request);
	}

	/// Where a record of `length` framed bytes goes: the active segment, or a new one - retiring the oldest
	/// complete one when four exist, unless something still pins it.
	pub fn place(&self, length: usize) -> Result<Placement, Refusal> {
		if length > RECORD_BYTES {
			return Err(Refusal::Oversized);
		}
		if let Some(active) = self.segments.last()
			&& active.fits(length)
		{
			return Ok(Placement { segment: active.number, offset: active.bytes, new_segment: false, retire: None });
		}
		let retire = if self.segments.len() >= SEGMENTS {
			let oldest = self.segments[0];
			if !oldest.complete() || self.pinned(oldest.number) {
				return Err(Refusal::Pinned);
			}
			Some(oldest.number)
		} else {
			None
		};
		Ok(Placement { segment: self.next_segment, offset: 0, new_segment: true, retire })
	}

	/// A record was committed where it was placed. `offset` and `file_length` are what the writer reported,
	/// not what was expected: a commit whose answer was lost may have landed all the same, and the file, not
	/// this bookkeeping, says where the next record went.
	pub fn committed(&mut self, placement: Placement, sequence: u64, request: u64, length: usize, offset: u64, file_length: u64) {
		if placement.new_segment {
			self.segments.push(Segment { number: placement.segment, first: sequence, records: 0, bytes: HEADER as u64 });
			self.next_segment = placement.segment + 1;
		}
		if let Some(retired) = placement.retire {
			self.segments.retain(|segment| segment.number != retired);
			self.index.retain(|located| located.segment != retired);
		}
		if let Some(active) = self.segments.iter_mut().find(|segment| segment.number == placement.segment) {
			active.records += 1;
			active.bytes = file_length;
		}
		self.index.push(Located { sequence, segment: placement.segment, offset, length: length as u32 });
		if request != 0 && !self.pins.contains(&(request, placement.segment)) {
			self.pins.push((request, placement.segment));
		}
		if let Some((_, remaining)) = self.reservations.iter_mut().find(|(reserved, _)| *reserved == request) {
			*remaining = remaining.saturating_sub(1);
		}
	}

	/// A request is over: its records no longer pin anything, and its reservation is given back.
	pub fn unpin(&mut self, request: u64) {
		self.pins.retain(|(pinned, _)| *pinned != request);
		self.release(request);
	}

	// ------------------------------------------------------------------ the queue and the ring

	/// A record to write, in order, behind whatever is waiting. The ring holds 256; the oldest waiting record
	/// is dropped, and counted, to make room - unless it is the one being written.
	pub fn push(&mut self, entry: Entry) {
		if self.queue.len() >= RING {
			let _ = self.queue.remove(usize::from(self.queue.len() > 1));
			self.evicted += 1;
		}
		self.queue.push_back(entry);
	}

	/// The record to write next.
	pub fn head(&self) -> Option<&Entry> {
		self.queue.front()
	}

	/// The head's write answered. A success removes it; a failure keeps it at the head for another attempt,
	/// and its first attempt is no longer owed to anybody.
	pub fn written(&mut self, ok: bool) -> Option<Entry> {
		self.failing = !ok;
		if ok {
			return self.queue.pop_front();
		}
		if let Some(head) = self.queue.front_mut() {
			head.first_attempt = false;
		}
		None
	}

	/// THE HEAD IS DROPPED UNWRITTEN - a commit whose answer never came, which may have landed and is not
	/// written a second time. Nothing about storage is learned from it.
	pub fn discard(&mut self) -> Option<Entry> {
		self.queue.pop_front()
	}

	/// Records waiting, the failed head included.
	pub fn waiting(&self) -> usize {
		self.queue.len()
	}

	/// Whether a record is still waiting to be written.
	pub fn holds(&self, sequence: u64) -> bool {
		self.queue.iter().any(|entry| entry.sequence == sequence)
	}

	// ------------------------------------------------------------------ the reader

	pub fn oldest(&self) -> u64 {
		self.index.first().map_or(self.next_sequence, |located| located.sequence)
	}

	/// Up to `most` retained records from `from` on.
	pub fn find(&self, from: u64, most: usize) -> Vec<Located> {
		self.index.iter().filter(|located| located.sequence >= from).take(most).copied().collect()
	}

	pub fn segments(&self) -> &[Segment] {
		&self.segments
	}
}

#[cfg(test)]
mod tests;

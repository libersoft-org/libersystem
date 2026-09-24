//! CAMERASERVICE'S DECISIONS: whether what a provider advertises and selects may be offered, which
//! buffer is whose, and what a frame's sequence and device time may be said to mean.
//!
//! WHAT A PROVIDER SAYS IS CHECKED, NOT BELIEVED. Its formats are held to the same bounds its normalizer
//! promised - eight formats, 32 sizes, 32 intervals, no zero or overflowing dimension, no disordered
//! interval, no duplicate identifier - and what it selects must be exactly what was asked for, inside what
//! it advertised, with a layout that fits the buffers a client may register.
//!
//! A BUFFER IS IN EXACTLY ONE STATE. Registered and the CLIENT's; handed to the PRODUCER under a lease
//! (queued, or being written - the service cannot tell the two apart and does not need to); or completed
//! and LEASED to the client under that lease. Only `start` and an exact `release` move a buffer to the
//! producer, and only a completion naming the producer's lease moves it back. At most four buffers, 8 MB
//! each, 32 MB a stream and 128 MB in all; the same backing once per stream.
//!
//! STOPPING ENDS IN ONE OF TWO WAYS. The producer confirms every buffer is back, and the stream is retired;
//! or the two-second deadline passes first, and the camera is QUARANTINED - its slots held, nothing reused -
//! until the producer confirms or is gone.
//!
//! THE SEQUENCE IS THE PRODUCER'S OBSERVATION, CHECKED. Every frame it delivered or definitely dropped
//! advanced it; a completion or a drop must continue it exactly, a gap the producer cannot count is an
//! unknown discontinuity, and a jump nobody explained is one too - never a count of frames invented.
//!
//! Time is in the system's 100 Hz ticks.

use alloc::vec::Vec;

pub const MAX_CAMERAS: usize = 4;
pub const MAX_CLIENTS: usize = 32;
pub const MAX_FORMATS: usize = 8;
pub const MAX_SIZES: usize = 32;
pub const MAX_INTERVALS: usize = 32;
pub const MAX_BUFFERS: usize = 4;
pub const MAX_BUFFER_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_STREAM_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
/// A page of frame sizes.
pub const PAGE_SIZES: usize = 8;
/// How long a stop may take before the camera is quarantined.
pub const STOP_TICKS: u64 = 200;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Yuy2,
	Mjpeg,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Intervals {
	Discrete(Vec<(u32, u32)>),
	Stepwise { minimum: (u32, u32), maximum: (u32, u32), step: (u32, u32) },
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
}

/// What a request or a selection names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Request {
	pub format: u8,
	pub size: u8,
	pub interval: (u32, u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selected {
	pub format: u8,
	pub kind: Kind,
	pub width: u16,
	pub height: u16,
	pub interval: (u32, u32),
	pub max_bytes: u32,
	pub stride: u32,
	pub plane_offset: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Not something this system carries.
	Unsupported,
	/// Past a bound: a size, a count, a budget.
	TooLarge,
	/// Somebody else's stream is running, or this one is in the wrong phase for the request.
	Busy,
	/// Names a generation, a lease or a buffer that is not current.
	Stale,
	/// Malformed, inconsistent or duplicated.
	Invalid,
	/// No such format, size, buffer or page.
	NotFound,
	/// The camera is quarantined.
	Unavailable,
}

// Rationals compared exactly: a/b against c/d without division.
fn cmp(a: (u32, u32), b: (u32, u32)) -> core::cmp::Ordering {
	(u64::from(a.0) * u64::from(b.1)).cmp(&(u64::from(b.0) * u64::from(a.1)))
}

fn positive(interval: (u32, u32)) -> bool {
	interval.0 != 0 && interval.1 != 0
}

/// Whether `interval` is one this size offers: a member of the list, or inside the range on a step.
pub fn offers(intervals: &Intervals, interval: (u32, u32)) -> bool {
	if !positive(interval) {
		return false;
	}
	match intervals {
		Intervals::Discrete(values) => values.iter().any(|value| cmp(*value, interval).is_eq()),
		Intervals::Stepwise { minimum, maximum, step } => {
			if cmp(interval, *minimum).is_lt() || cmp(interval, *maximum).is_gt() {
				return false;
			}
			if cmp(*minimum, *maximum).is_eq() {
				return true;
			}
			// (interval - minimum) / step is a whole number, in exact arithmetic over a common denominator.
			let denominator = u128::from(interval.1) * u128::from(minimum.1) * u128::from(step.1);
			let offset = u128::from(interval.0) * u128::from(minimum.1) * u128::from(step.1) - u128::from(minimum.0) * u128::from(interval.1) * u128::from(step.1);
			let unit = u128::from(step.0) * u128::from(interval.1) * u128::from(minimum.1);
			denominator != 0 && unit != 0 && offset % unit == 0
		}
	}
}

/// YUY2's checked layout: two bytes a pixel, rows packed.
pub fn yuy2_layout(width: u16, height: u16) -> Option<(u32, u32)> {
	let stride = u32::from(width).checked_mul(2)?;
	Some((stride, stride.checked_mul(u32::from(height))?))
}

/// A provider's advertisement, held to the normalizer's own bounds before anything is offered.
pub fn validate(formats: &[Format]) -> Result<(), Refusal> {
	if formats.len() > MAX_FORMATS {
		return Err(Refusal::TooLarge);
	}
	for (at, format) in formats.iter().enumerate() {
		if format.index == 0 || formats[..at].iter().any(|earlier| earlier.index == format.index) {
			return Err(Refusal::Invalid);
		}
		if format.sizes.is_empty() {
			return Err(Refusal::Invalid);
		}
		if format.sizes.len() > MAX_SIZES {
			return Err(Refusal::TooLarge);
		}
		for (n, size) in format.sizes.iter().enumerate() {
			if size.index == 0 || format.sizes[..n].iter().any(|earlier| earlier.index == size.index) {
				return Err(Refusal::Invalid);
			}
			if size.width == 0 || size.height == 0 || size.max_bytes == 0 {
				return Err(Refusal::Invalid);
			}
			if u64::from(size.max_bytes) > MAX_BUFFER_BYTES {
				return Err(Refusal::TooLarge);
			}
			if format.kind == Kind::Yuy2 {
				match yuy2_layout(size.width, size.height) {
					Some((_, bytes)) if bytes <= size.max_bytes => {}
					_ => return Err(Refusal::Invalid),
				}
			}
			match &size.intervals {
				Intervals::Discrete(values) => {
					if values.is_empty() || !values.iter().all(|value| positive(*value)) {
						return Err(Refusal::Invalid);
					}
					if values.len() > MAX_INTERVALS {
						return Err(Refusal::TooLarge);
					}
				}
				Intervals::Stepwise { minimum, maximum, step } => {
					if !positive(*minimum) || !positive(*maximum) || cmp(*minimum, *maximum).is_gt() || (step.0 == 0 && cmp(*minimum, *maximum).is_ne()) || step.1 == 0 {
						return Err(Refusal::Invalid);
					}
					if cmp(*minimum, *maximum).is_ne() && !offers(&size.intervals, *maximum) {
						return Err(Refusal::Invalid);
					}
				}
			}
		}
	}
	Ok(())
}

/// A page of a format's sizes: at most eight, from `offset`.
pub fn page(formats: &[Format], format: u8, offset: u8) -> Result<(&Format, &[Size]), Refusal> {
	let found = formats.iter().find(|candidate| candidate.index == format).ok_or(Refusal::NotFound)?;
	let start = usize::from(offset);
	if start > found.sizes.len() || (start == found.sizes.len() && start != 0) {
		return Err(Refusal::NotFound);
	}
	let end = (start + PAGE_SIZES).min(found.sizes.len());
	Ok((found, &found.sizes[start..end]))
}

/// What the request asks for, as the advertisement describes it - or why it cannot be asked for.
pub fn expect(formats: &[Format], request: Request) -> Result<Selected, Refusal> {
	let format = formats.iter().find(|candidate| candidate.index == request.format).ok_or(Refusal::Unsupported)?;
	let size = format.sizes.iter().find(|candidate| candidate.index == request.size).ok_or(Refusal::Unsupported)?;
	if !offers(&size.intervals, request.interval) {
		return Err(Refusal::Unsupported);
	}
	if u64::from(size.max_bytes) > MAX_BUFFER_BYTES {
		return Err(Refusal::TooLarge);
	}
	let stride = match format.kind {
		Kind::Yuy2 => yuy2_layout(size.width, size.height).ok_or(Refusal::Invalid)?.0,
		Kind::Mjpeg => 0,
	};
	Ok(Selected { format: format.index, kind: format.kind, width: size.width, height: size.height, interval: request.interval, max_bytes: size.max_bytes, stride, plane_offset: 0 })
}

/// A provider's selection, accepted only when it is EXACTLY what was expected - never a silent change of
/// format, size, interval or layout.
pub fn verify(expected: &Selected, selected: &Selected) -> Result<(), Refusal> {
	let same_interval = cmp(expected.interval, selected.interval).is_eq();
	if expected.format != selected.format || expected.kind != selected.kind || expected.width != selected.width || expected.height != selected.height || !same_interval || expected.stride != selected.stride || expected.plane_offset != selected.plane_offset || selected.max_bytes > expected.max_bytes {
		return Err(Refusal::Invalid);
	}
	Ok(())
}

// ------------------------------------------------------------------ buffers and leases

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Holder {
	/// Registered and not handed over: the client's.
	Client,
	/// Queued for, or being written by, the producer, under this lease.
	Producer(u64),
	/// Completed and leased to the client, under this lease.
	Leased(u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Buffer {
	pub id: u8,
	/// The kernel identity of the backing object: one backing, once per stream.
	pub koid: u64,
	pub bytes: u64,
	pub holder: Holder,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
	/// Negotiated; buffers may be registered.
	Ready,
	Streaming,
	/// Waiting for the producer to give every buffer back.
	Stopping {
		deadline: u64,
	},
	/// Every buffer back: the stream is over, and a new one starts from nothing.
	Retired,
	/// The stop was never confirmed: the camera's slots are held until the producer confirms or is gone.
	Quarantined,
}

/// A completion the client may be told about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Delivered {
	pub buffer: u8,
	pub lease: u64,
	pub sequence: u64,
	pub valid_bytes: u32,
	/// The unknown discontinuities recorded before this frame, including one for a jump nobody explained.
	pub discontinuities: u64,
}

pub struct Stream {
	pub generation: u64,
	pub selected: Selected,
	buffers: Vec<Buffer>,
	next_lease: u64,
	phase: Phase,
	next_sequence: u64,
	pub dropped_no_buffer: u64,
	pub dropped_device: u64,
	pub unknown_discontinuities: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropReason {
	NoBuffer,
	Device,
}

impl Stream {
	pub fn new(generation: u64, selected: Selected) -> Self {
		Self { generation, selected, buffers: Vec::new(), next_lease: 1, phase: Phase::Ready, next_sequence: 0, dropped_no_buffer: 0, dropped_device: 0, unknown_discontinuities: 0 }
	}

	pub fn phase(&self) -> Phase {
		self.phase
	}

	pub fn buffers(&self) -> &[Buffer] {
		&self.buffers
	}

	pub fn next_sequence(&self) -> u64 {
		self.next_sequence
	}

	/// Bytes registered to this stream.
	pub fn registered(&self) -> u64 {
		self.buffers.iter().map(|buffer| buffer.bytes).sum()
	}

	fn lease(&mut self) -> u64 {
		let lease = self.next_lease;
		self.next_lease += 1;
		lease
	}

	/// A client's buffer: its own memory, checked before anything is handed on. `elsewhere` is what every
	/// OTHER stream has registered, for the global bound.
	pub fn register(&mut self, koid: u64, bytes: u64, elsewhere: u64) -> Result<u8, Refusal> {
		if self.phase != Phase::Ready {
			return Err(Refusal::Busy);
		}
		if self.buffers.len() >= MAX_BUFFERS {
			return Err(Refusal::TooLarge);
		}
		if self.buffers.iter().any(|buffer| buffer.koid == koid) {
			return Err(Refusal::Invalid);
		}
		// IT MUST HOLD THE FRAME IT WILL BE ASKED TO HOLD, and no more than one buffer may be.
		if bytes < u64::from(self.selected.max_bytes) || bytes > MAX_BUFFER_BYTES {
			return Err(Refusal::TooLarge);
		}
		let stream = self.registered() + bytes;
		if stream > MAX_STREAM_BYTES || elsewhere + stream > MAX_TOTAL_BYTES {
			return Err(Refusal::TooLarge);
		}
		let id = (0..MAX_BUFFERS as u8).find(|id| !self.buffers.iter().any(|buffer| buffer.id == *id)).ok_or(Refusal::TooLarge)?;
		self.buffers.push(Buffer { id, koid, bytes, holder: Holder::Client });
		Ok(id)
	}

	/// A registration the producer refused is undone: the buffer was never handed on.
	pub fn forget(&mut self, id: u8) {
		if self.phase == Phase::Ready {
			self.buffers.retain(|buffer| !(buffer.id == id && buffer.holder == Holder::Client));
		}
	}

	/// Start: every registered buffer goes to the producer, each under a fresh lease.
	pub fn start(&mut self) -> Result<Vec<(u8, u64)>, Refusal> {
		if self.phase != Phase::Ready {
			return Err(Refusal::Busy);
		}
		if self.buffers.is_empty() {
			return Err(Refusal::NotFound);
		}
		let mut queued = Vec::new();
		for at in 0..self.buffers.len() {
			let lease = self.lease();
			self.buffers[at].holder = Holder::Producer(lease);
			queued.push((self.buffers[at].id, lease));
		}
		self.phase = Phase::Streaming;
		Ok(queued)
	}

	/// A completion from the producer: accepted only for a buffer it holds under exactly that lease, with
	/// a length that fits, and a sequence that continues this stream's.
	pub fn completed(&mut self, buffer: u8, lease: u64, sequence: u64, valid_bytes: u32) -> Result<Delivered, Refusal> {
		if !matches!(self.phase, Phase::Streaming | Phase::Stopping { .. }) {
			return Err(Refusal::Stale);
		}
		let at = self.buffers.iter().position(|held| held.id == buffer).ok_or(Refusal::Stale)?;
		if self.buffers[at].holder != Holder::Producer(lease) {
			return Err(Refusal::Stale);
		}
		if u64::from(valid_bytes) > self.buffers[at].bytes || valid_bytes > self.selected.max_bytes || valid_bytes == 0 {
			return Err(Refusal::Invalid);
		}
		if sequence < self.next_sequence {
			return Err(Refusal::Stale);
		}
		// A JUMP NOBODY EXPLAINED is a loss nobody can count: one unknown discontinuity, not a number of
		// frames guessed from the difference.
		if sequence > self.next_sequence {
			self.unknown_discontinuities += 1;
		}
		self.next_sequence = sequence + 1;
		self.buffers[at].holder = Holder::Leased(lease);
		Ok(Delivered { buffer, lease, sequence, valid_bytes, discontinuities: self.unknown_discontinuities })
	}

	/// Frames the producer definitely observed and did not deliver. They must continue the sequence.
	pub fn dropped(&mut self, first: u64, count: u32, reason: DropReason) -> Result<(), Refusal> {
		if first < self.next_sequence || count == 0 {
			return Err(Refusal::Stale);
		}
		if first > self.next_sequence {
			self.unknown_discontinuities += 1;
		}
		self.next_sequence = first + u64::from(count);
		match reason {
			DropReason::NoBuffer => self.dropped_no_buffer += u64::from(count),
			DropReason::Device => self.dropped_device += u64::from(count),
		}
		Ok(())
	}

	/// A loss the producer cannot count: the sequence continues from `next`, and one discontinuity is known.
	pub fn gap(&mut self, next: u64) -> Result<(), Refusal> {
		if next < self.next_sequence {
			return Err(Refusal::Stale);
		}
		self.next_sequence = next;
		self.unknown_discontinuities += 1;
		Ok(())
	}

	/// The client gives back EXACTLY the lease it was handed. While streaming the buffer goes back to the
	/// producer under a new lease; otherwise it is the client's again.
	pub fn release(&mut self, buffer: u8, lease: u64) -> Result<Option<(u8, u64)>, Refusal> {
		let at = self.buffers.iter().position(|held| held.id == buffer).ok_or(Refusal::NotFound)?;
		if self.buffers[at].holder != Holder::Leased(lease) {
			return Err(Refusal::Stale);
		}
		if self.phase == Phase::Streaming {
			let lease = self.lease();
			self.buffers[at].holder = Holder::Producer(lease);
			return Ok(Some((buffer, lease)));
		}
		self.buffers[at].holder = Holder::Client;
		Ok(None)
	}

	/// Stop: no buffer goes to the producer again, and the stream waits for every one it holds.
	pub fn stop(&mut self, now: u64) -> Result<(), Refusal> {
		match self.phase {
			Phase::Streaming => {
				self.phase = Phase::Stopping { deadline: now + STOP_TICKS };
				Ok(())
			}
			// Never started: nothing to wait for.
			Phase::Ready => {
				self.phase = Phase::Retired;
				Ok(())
			}
			Phase::Stopping { .. } => Ok(()),
			Phase::Retired | Phase::Quarantined => Err(Refusal::Stale),
		}
	}

	/// The producer confirmed every buffer is back: whatever it held is the client's again, and the
	/// stream is retired.
	pub fn stopped(&mut self) {
		for buffer in &mut self.buffers {
			if matches!(buffer.holder, Holder::Producer(_)) {
				buffer.holder = Holder::Client;
			}
		}
		self.phase = Phase::Retired;
	}

	/// The stop deadline: past it with the producer still silent, the camera is quarantined.
	pub fn tick(&mut self, now: u64) -> bool {
		if let Phase::Stopping { deadline } = self.phase
			&& now >= deadline
		{
			self.phase = Phase::Quarantined;
			return true;
		}
		false
	}

	pub fn deadline(&self) -> Option<u64> {
		match self.phase {
			Phase::Stopping { deadline } => Some(deadline),
			_ => None,
		}
	}
}

// ------------------------------------------------------------------ device time

/// A device timestamp as the provider gave it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceTime {
	pub ticks: u64,
	pub width_bits: u8,
	pub frequency_hz: Option<u64>,
	pub clock_domain: u32,
	pub reset_generation: u32,
}

/// The one reading of a device clock this service can defend: elapsed ticks between two readings of the
/// SAME timeline, modulo its width - and nothing across a reset, another domain or a discontinuity.
#[derive(Default)]
pub struct DeviceClock {
	last: Option<DeviceTime>,
	generation: u32,
	ambiguous: bool,
}

impl DeviceClock {
	/// Whether a device time is well formed: a width of 1..=64 bits, and ticks inside it.
	pub fn valid(time: &DeviceTime) -> bool {
		time.width_bits >= 1 && time.width_bits <= 64 && (time.width_bits == 64 || time.ticks >> time.width_bits == 0) && time.frequency_hz != Some(0)
	}

	/// An unknown discontinuity happened: the next reading starts a new timeline.
	pub fn discontinuity(&mut self) {
		self.ambiguous = true;
	}

	/// A frame's device time, and the timeline generation it is reported under: advanced on a device reset,
	/// a change of domain or width, and after an unknown discontinuity.
	pub fn observe(&mut self, time: DeviceTime) -> u32 {
		let same = self.last.is_some_and(|last| last.clock_domain == time.clock_domain && last.reset_generation == time.reset_generation && last.width_bits == time.width_bits);
		if !same || self.ambiguous {
			self.generation = self.generation.wrapping_add(1);
			self.ambiguous = false;
		}
		self.last = Some(time);
		self.generation
	}

	/// Elapsed ticks from `earlier` to `later` on one timeline, modulo the width; `None` across anything
	/// that makes the difference meaningless.
	pub fn elapsed(earlier: &DeviceTime, later: &DeviceTime) -> Option<u64> {
		if earlier.clock_domain != later.clock_domain || earlier.reset_generation != later.reset_generation || earlier.width_bits != later.width_bits || !Self::valid(earlier) || !Self::valid(later) {
			return None;
		}
		let difference = later.ticks.wrapping_sub(earlier.ticks);
		Some(if later.width_bits == 64 { difference } else { difference & ((1u64 << later.width_bits) - 1) })
	}
}

#[cfg(test)]
mod tests;

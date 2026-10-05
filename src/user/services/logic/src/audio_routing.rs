//! THE AUDIO DEVICE MODEL'S ARITHMETIC: which device is the default for each direction, where a stream plays, how a
//! level becomes a gain, and the clocks a device whose clock is not the host's is driven by.
//!
//! THE ROUTING RULE, as the owner's playback choice of 2026-09-25 applies it to every device: one default output, one
//! default input and one default voice device. A device that arrives becomes the default for what it does -
//! headphones connected take the music - and when it leaves, the default it displaced returns; the operator may choose
//! instead, which puts the chosen device where an arrival would. A stream follows the default unless it named a
//! device, and NEVER ENDS because its device left: it moves to the default, and with no device at all it keeps
//! accepting and the gap is silence, counted by whoever holds the stream.
//!
//! THE CLOCKS. A sound card's period acknowledgment is its clock. A device with no clock the host can see - an A2DP
//! sink, a stream with no device at all - is paced by the host's own timer at the rate it plays (`TimerPacer`), counted
//! in frames from its start so no rounding accumulates; a voice link is paced one out for one in (`OneForOne`); and a
//! phone's stream, which arrives on the phone's clock and leaves on a device's, passes a jitter buffer bounded in time
//! whose overflow drops the oldest period and whose underrun is silence, both counted (`Jitter`).

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// How many devices the inventory holds: sound cards, USB functions and Bluetooth endpoints together.
pub const MAX_DEVICES: usize = 16;

/// A direction a device serves, and so a default it can be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
	Output,
	Input,
	/// A headset's or earbuds' call audio: both ways, mono, at a voice rate.
	Voice,
}

/// ONE DIRECTION'S ORDER: the devices that serve it, oldest first, in a fixed array - this crate is linked into a
/// shared library, and a growing vector of a primitive would import its growth routine from whichever library happens
/// to export that instance.
#[derive(Clone, Copy, Debug, Default)]
struct Order {
	ids: [u32; MAX_DEVICES],
	len: usize,
}

impl Order {
	fn as_slice(&self) -> &[u32] {
		&self.ids[..self.len]
	}

	fn remove(&mut self, id: u32) {
		if let Some(at) = self.as_slice().iter().position(|held| *held == id) {
			self.ids.copy_within(at + 1..self.len, at);
			self.len -= 1;
		}
	}

	// To the end - the newest - forgetting the oldest when full.
	fn push(&mut self, id: u32) {
		self.remove(id);
		if self.len == MAX_DEVICES {
			self.ids.copy_within(1..MAX_DEVICES, 0);
			self.len -= 1;
		}
		self.ids[self.len] = id;
		self.len += 1;
	}
}

/// THE DEFAULTS: for each direction, the devices that serve it in the order they became its default - the last is the
/// default now, and the one before it is what returns when it leaves.
#[derive(Clone, Copy, Default, Debug)]
pub struct Routing {
	output: Order,
	input: Order,
	voice: Order,
}

impl Routing {
	pub const fn new() -> Routing {
		const EMPTY: Order = Order { ids: [0; MAX_DEVICES], len: 0 };
		Routing { output: EMPTY, input: EMPTY, voice: EMPTY }
	}

	fn order(&self, direction: Direction) -> &Order {
		match direction {
			Direction::Output => &self.output,
			Direction::Input => &self.input,
			Direction::Voice => &self.voice,
		}
	}

	fn order_mut(&mut self, direction: Direction) -> &mut Order {
		match direction {
			Direction::Output => &mut self.output,
			Direction::Input => &mut self.input,
			Direction::Voice => &mut self.voice,
		}
	}

	/// A DEVICE ARRIVED: the default for every direction it serves, until something newer arrives or it leaves. An id
	/// already held is moved, not doubled; past `MAX_DEVICES` in one direction the oldest is forgotten.
	pub fn arrive(&mut self, id: u32, directions: &[Direction]) {
		for &direction in directions {
			self.order_mut(direction).push(id);
		}
	}

	/// A DEVICE LEFT: it is the default for nothing, and what it displaced returns.
	pub fn leave(&mut self, id: u32) {
		for direction in [Direction::Output, Direction::Input, Direction::Voice] {
			self.order_mut(direction).remove(id);
		}
	}

	/// THE OPERATOR CHOSE: the device becomes the default for the direction, as an arrival would make it. Refused - false
	/// - for a device that does not serve the direction.
	pub fn choose(&mut self, id: u32, direction: Direction) -> bool {
		if !self.serves(id, direction) {
			return false;
		}
		self.order_mut(direction).push(id);
		true
	}

	/// The default for a direction now, if any device serves it.
	pub fn default(&self, direction: Direction) -> Option<u32> {
		self.order(direction).as_slice().last().copied()
	}

	/// Whether a device serves a direction.
	pub fn serves(&self, id: u32, direction: Direction) -> bool {
		self.order(direction).as_slice().contains(&id)
	}
}

/// WHERE A STREAM PLAYS: the device it named while that device serves the direction, the default otherwise - or
/// nowhere, which is silence counted and never the stream's end.
pub fn place(routing: &Routing, named: Option<u32>, direction: Direction) -> Option<u32> {
	match named {
		Some(id) if routing.serves(id, direction) => Some(id),
		_ => routing.default(direction),
	}
}

/// WHERE A VOICE SESSION GOES: the default voice device both ways, or otherwise the default output and the default
/// input - which may each be none.
pub fn place_voice(routing: &Routing) -> (Option<u32>, Option<u32>) {
	match routing.default(Direction::Voice) {
		Some(voice) => (Some(voice), Some(voice)),
		None => (routing.default(Direction::Output), routing.default(Direction::Input)),
	}
}

// ------------------------------------------------------------------ volume

/// The highest level.
pub const MAX_LEVEL: u8 = 100;

/// A LEVEL AS A GAIN, Q15: the square of the level's fraction, so equal steps of the level sound like roughly equal
/// steps of loudness rather than crowding the audible change into the bottom of the range. Level 100 is unity and 0 is
/// silence; anything above 100 is 100.
pub fn gain_q15(level: u8) -> u32 {
	let level = u32::from(level.min(MAX_LEVEL));
	(level * level * 32_768) / (u32::from(MAX_LEVEL) * u32::from(MAX_LEVEL))
}

/// One sample at a gain: rounded toward zero, never past the sample's own range.
pub fn scale(sample: i16, gain_q15: u32) -> i16 {
	((i32::from(sample) * gain_q15 as i32) >> 15) as i16
}

// ------------------------------------------------------------------ clocks

/// A CLOCK THAT IS THE HOST'S TIMER: how many frames at `rate` have come due since it started, counted from the start
/// in nanoseconds so the rounding of each step does not add up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerPacer {
	rate: u32,
	start_ns: u64,
	taken: u64,
}

impl TimerPacer {
	pub fn new(rate: u32, now_ns: u64) -> TimerPacer {
		TimerPacer { rate: rate.max(1), start_ns: now_ns, taken: 0 }
	}

	/// The frames due by `now_ns` and not yet taken.
	pub fn due(&self, now_ns: u64) -> u64 {
		let elapsed = now_ns.saturating_sub(self.start_ns) as u128;
		let total = (elapsed * u128::from(self.rate) / 1_000_000_000) as u64;
		total.saturating_sub(self.taken)
	}

	/// The frames were played - or dropped as silence.
	pub fn take(&mut self, frames: u64) {
		self.taken = self.taken.saturating_add(frames);
	}

	/// WHEN `frames` MORE ARE DUE, in nanoseconds since boot: what a service waits for.
	pub fn when(&self, frames: u64) -> u64 {
		let target = u128::from(self.taken.saturating_add(frames));
		let offset = (target * 1_000_000_000).div_ceil(u128::from(self.rate));
		self.start_ns.saturating_add(offset.min(u128::from(u64::MAX)) as u64)
	}

	/// The frames taken so far.
	pub fn taken(&self) -> u64 {
		self.taken
	}
}

/// A VOICE LINK'S CLOCK: the controller delivers a packet each interval and one is sent for each, so what may go out is
/// what came in, less what went - never more, which is what keeps the two directions in step with the radio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OneForOne {
	credit: u32,
	/// Packets that arrived with nothing ready to send back: silence went in their place.
	pub silent: u64,
}

impl OneForOne {
	/// A packet arrived: one may go.
	pub fn arrived(&mut self) {
		self.credit = self.credit.saturating_add(1).min(8);
	}

	/// Whether a packet may go now; taking the credit if so.
	pub fn send(&mut self) -> bool {
		if self.credit == 0 {
			return false;
		}
		self.credit -= 1;
		true
	}

	/// The arrival was answered with silence: counted, and its credit used.
	pub fn silence(&mut self) {
		if self.send() {
			self.silent = self.silent.saturating_add(1);
		}
	}
}

/// A JITTER BUFFER BOUNDED IN TIME: periods arrive on a clock that is not the one they leave on, and the buffer holds at
/// most `bound_frames` of them. One that would pass the bound drops the OLDEST period - late audio is worth less than
/// current audio - and a period asked for when none is held is silence; both are counted.
#[derive(Clone, Debug, Default)]
pub struct Jitter {
	periods: VecDeque<Vec<i16>>,
	held_frames: usize,
	channels: usize,
	bound_frames: usize,
	/// Periods dropped because the buffer was full, and periods of silence given because it was empty.
	pub overflows: u64,
	pub underruns: u64,
	/// Whether the buffer has filled to half its bound once: until then an empty buffer is filling, not running dry.
	primed: bool,
}

impl Jitter {
	/// A buffer of `bound_ms` at `rate`, for `channels`-sample frames.
	pub fn new(rate: u32, channels: u8, bound_ms: u32) -> Jitter {
		let bound_frames = (rate as usize * bound_ms as usize) / 1000;
		Jitter { periods: VecDeque::new(), held_frames: 0, channels: usize::from(channels.max(1)), bound_frames, overflows: 0, underruns: 0, primed: false }
	}

	/// The frames held.
	pub fn held(&self) -> usize {
		self.held_frames
	}

	/// One period arrived, interleaved samples.
	pub fn push(&mut self, period: Vec<i16>) {
		let frames = period.len() / self.channels;
		if frames == 0 {
			return;
		}
		while self.held_frames + frames > self.bound_frames
			&& let Some(oldest) = self.periods.pop_front()
		{
			self.held_frames -= oldest.len() / self.channels;
			self.overflows = self.overflows.saturating_add(1);
		}
		if frames > self.bound_frames {
			self.overflows = self.overflows.saturating_add(1);
			return;
		}
		self.held_frames += frames;
		self.periods.push_back(period);
		if self.held_frames * 2 >= self.bound_frames {
			self.primed = true;
		}
	}

	/// The next period to play, or `None` - silence - while it fills or when it has run dry, which is counted once it
	/// had begun.
	pub fn pop(&mut self) -> Option<Vec<i16>> {
		if !self.primed {
			return None;
		}
		match self.periods.pop_front() {
			Some(period) => {
				self.held_frames -= period.len() / self.channels;
				Some(period)
			}
			None => {
				self.underruns = self.underruns.saturating_add(1);
				// RUN DRY: it fills to half again before it plays, so one late period is not a stutter of many.
				self.primed = false;
				None
			}
		}
	}
}

#[cfg(test)]
mod tests;

//! THE GAMEPAD WIRE a `gamepad` provider serves, and THE RULES ITS PUBLISHER FOLLOWS.
//!
//! HERE AND NOWHERE ELSE, for the reason the block and audio wires are here: the xHCI driver, the
//! development fixture and InputService all speak it, and three hand-written copies of one layout are
//! three chances for two of them to disagree.
//!
//! ONE FRAME PER MESSAGE, LITTLE-ENDIAN, and three shapes:
//!
//!   - ARRIVAL `[1][handle u32][buttons u8][hats u8][axes u8][label length u8][label]` and then, per axis,
//!     `[usage u32][minimum i32][maximum i32]`. The handle is the publisher's own number for this
//!     attachment, never reused while it runs; at most 32 buttons, 2 hats, 8 axes and 32 label bytes of
//!     printable ASCII. An axis's usage carries its page in the high sixteen bits.
//!   - STATE `[2][handle u32][buttons u32][hat 0 u8][hat 1 u8]` and one `i32` per axis: bit n-1 is button
//!     n, a hat is 0..7 for north and then clockwise in eighths or `CENTRED`, an unused hat slot is
//!     `CENTRED`, and an axis is the device's own logical value, never rescaled.
//!   - DEPARTURE `[3][handle u32]`.
//!
//! AN ARRIVAL IMPLIES A STATE: no buttons, every hat centred and every axis at the midpoint of its range
//! (`Shape::initial`). A publisher holds exactly that until a report changes it, and a consumer holds it
//! until a STATE says otherwise - so the first state a consumer shows never reads a zeroed hat as north.
//!
//! A FRAME WHOSE LENGTH OR COUNTS DISAGREE IS REFUSED by `decode`, and so is a hat above `CENTRED`, a
//! count past its bound and a label that is not printable ASCII. What `decode` cannot know alone - that
//! a STATE is for a handle the consumer holds, has that gamepad's number of axes, uses only its hats and
//! presses only its buttons - `Shape::state` refuses. Either way a refused frame changes nothing.
//!
//! THE CONSUMER'S HALF IS `#[inline]`: this crate is linked statically into the drivers and is no shared
//! library, and InputService - a dynamic executable - reads the wire too. An inline function is compiled into
//! the program that calls it, so the service carries its own copy of the one decoder rather than importing a
//! symbol no library it links exports.

/// The frame tags.
pub const ARRIVAL: u8 = 1;
pub const STATE: u8 = 2;
pub const DEPARTURE: u8 = 3;

/// The bounds a frame is held to.
pub const MAX_BUTTONS: u8 = 32;
pub const MAX_HATS: u8 = 2;
pub const MAX_AXES: usize = 8;
pub const MAX_LABEL: usize = 32;

/// A hat that points nowhere - the value of a released hat, and of a hat slot a gamepad does not have.
pub const CENTRED: u8 = 8;

/// The longest frame: an ARRIVAL with every count at its most.
pub const MAX_FRAME: usize = 9 + MAX_LABEL + MAX_AXES * 12;

/// One axis of a gamepad: its usage (page in the high sixteen bits) and its logical range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Axis {
	pub usage: u32,
	pub minimum: i32,
	pub maximum: i32,
}

impl Axis {
	const NONE: Axis = Axis { usage: 0, minimum: 0, maximum: 0 };

	/// THE MIDPOINT OF THE RANGE: the minimum plus half the span, rounded toward the minimum, in 64 bits
	/// so a range across the whole of `i32` cannot overflow.
	#[inline]
	pub fn midpoint(&self) -> i32 {
		let span = self.maximum as i64 - self.minimum as i64;
		(self.minimum as i64 + span / 2) as i32
	}
}

/// What a gamepad IS: its label, its buttons, its hats and its axes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shape {
	label: [u8; MAX_LABEL],
	label_len: u8,
	buttons: u8,
	hats: u8,
	axes: u8,
	axis: [Axis; MAX_AXES],
}

/// Where a gamepad IS: its buttons, its hats and its axes' values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct State {
	pub buttons: u32,
	pub hats: [u8; MAX_HATS as usize],
	pub axes: [i32; MAX_AXES],
}

impl Shape {
	/// A shape, or `None` for a count past its bound or a label that is not printable ASCII.
	#[inline]
	pub fn new(label: &[u8], buttons: u8, hats: u8, axes: &[Axis]) -> Option<Shape> {
		if label.len() > MAX_LABEL || !label.iter().all(|byte| (0x20..0x7f).contains(byte)) || buttons > MAX_BUTTONS || hats > MAX_HATS || axes.len() > MAX_AXES {
			return None;
		}
		let mut shape = Shape { label: [0; MAX_LABEL], label_len: label.len() as u8, buttons, hats, axes: axes.len() as u8, axis: [Axis::NONE; MAX_AXES] };
		shape.label[..label.len()].copy_from_slice(label);
		shape.axis[..axes.len()].copy_from_slice(axes);
		Some(shape)
	}

	#[inline]
	pub fn label(&self) -> &[u8] {
		&self.label[..self.label_len as usize]
	}

	#[inline]
	pub fn buttons(&self) -> u8 {
		self.buttons
	}

	#[inline]
	pub fn hats(&self) -> u8 {
		self.hats
	}

	#[inline]
	pub fn axes(&self) -> &[Axis] {
		&self.axis[..self.axes as usize]
	}

	/// THE STATE BEFORE THE FIRST REPORT: no buttons, every hat centred, every axis at its midpoint.
	#[inline]
	pub fn initial(&self) -> State {
		let mut axes = [0i32; MAX_AXES];
		for (value, axis) in axes.iter_mut().zip(self.axes()) {
			*value = axis.midpoint();
		}
		State { buttons: 0, hats: [CENTRED; MAX_HATS as usize], axes }
	}

	/// The mask of the buttons this gamepad has.
	#[inline]
	fn button_mask(&self) -> u32 {
		if self.buttons >= 32 { u32::MAX } else { (1u32 << self.buttons) - 1 }
	}

	/// A decoded STATE as this gamepad's state, or `None` when it is not one of this gamepad's: another
	/// number of axes, a button it does not have, a hat it does not have that is not centred.
	#[inline]
	pub fn state(&self, frame: &StateFrame) -> Option<State> {
		if frame.axes != self.axes || frame.buttons & !self.button_mask() != 0 {
			return None;
		}
		for (index, hat) in frame.hats.iter().enumerate() {
			if index >= self.hats as usize && *hat != CENTRED {
				return None;
			}
		}
		let mut axes = [0i32; MAX_AXES];
		axes[..self.axes as usize].copy_from_slice(&frame.values[..self.axes as usize]);
		Some(State { buttons: frame.buttons, hats: frame.hats, axes })
	}

	/// Whether a state is one this gamepad could be in - what a publisher checks before it sends one.
	pub fn admits(&self, state: &State) -> bool {
		state.buttons & !self.button_mask() == 0 && state.hats.iter().enumerate().all(|(index, hat)| if index < self.hats as usize { *hat <= CENTRED } else { *hat == CENTRED }) && state.axes[self.axes as usize..].iter().all(|value| *value == 0)
	}
}

/// A STATE frame as it arrived, before it is matched against the gamepad it names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StateFrame {
	pub handle: u32,
	pub buttons: u32,
	pub hats: [u8; MAX_HATS as usize],
	/// How many axis values the frame carried.
	pub axes: u8,
	pub values: [i32; MAX_AXES],
}

/// One decoded frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Frame {
	Arrival { handle: u32, shape: Shape },
	State(StateFrame),
	Departure { handle: u32 },
}

#[inline]
fn u32_at(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Read one frame, or `None` for one this wire does not have.
#[inline]
pub fn decode(bytes: &[u8]) -> Option<Frame> {
	let (&tag, rest) = bytes.split_first()?;
	match tag {
		ARRIVAL => {
			if rest.len() < 8 {
				return None;
			}
			let handle = u32_at(rest, 0);
			let (buttons, hats, axes, label_len) = (rest[4], rest[5], rest[6] as usize, rest[7] as usize);
			if label_len > MAX_LABEL || axes > MAX_AXES || rest.len() != 8 + label_len + axes * 12 {
				return None;
			}
			let label = &rest[8..8 + label_len];
			let mut axis = [Axis::NONE; MAX_AXES];
			for (index, slot) in axis.iter_mut().enumerate().take(axes) {
				let at = 8 + label_len + index * 12;
				*slot = Axis { usage: u32_at(rest, at), minimum: u32_at(rest, at + 4) as i32, maximum: u32_at(rest, at + 8) as i32 };
			}
			Some(Frame::Arrival { handle, shape: Shape::new(label, buttons, hats, &axis[..axes])? })
		}
		STATE => {
			if rest.len() < 10 || (rest.len() - 10) % 4 != 0 || (rest.len() - 10) / 4 > MAX_AXES {
				return None;
			}
			let hats = [rest[8], rest[9]];
			if hats.iter().any(|hat| *hat > CENTRED) {
				return None;
			}
			let axes = (rest.len() - 10) / 4;
			let mut values = [0i32; MAX_AXES];
			for (index, value) in values.iter_mut().enumerate().take(axes) {
				*value = u32_at(rest, 10 + index * 4) as i32;
			}
			Some(Frame::State(StateFrame { handle: u32_at(rest, 0), buttons: u32_at(rest, 4), hats, axes: axes as u8, values }))
		}
		DEPARTURE if rest.len() == 4 => Some(Frame::Departure { handle: u32_at(rest, 0) }),
		_ => None,
	}
}

/// An ARRIVAL into `out`; its length.
pub fn encode_arrival(handle: u32, shape: &Shape, out: &mut [u8; MAX_FRAME]) -> usize {
	out[0] = ARRIVAL;
	out[1..5].copy_from_slice(&handle.to_le_bytes());
	out[5] = shape.buttons;
	out[6] = shape.hats;
	out[7] = shape.axes;
	out[8] = shape.label_len;
	let mut at = 9;
	out[at..at + shape.label().len()].copy_from_slice(shape.label());
	at += shape.label().len();
	for axis in shape.axes() {
		out[at..at + 4].copy_from_slice(&axis.usage.to_le_bytes());
		out[at + 4..at + 8].copy_from_slice(&axis.minimum.to_le_bytes());
		out[at + 8..at + 12].copy_from_slice(&axis.maximum.to_le_bytes());
		at += 12;
	}
	at
}

/// A STATE of a gamepad of `shape` into `out`; its length.
pub fn encode_state(handle: u32, shape: &Shape, state: &State, out: &mut [u8; MAX_FRAME]) -> usize {
	out[0] = STATE;
	out[1..5].copy_from_slice(&handle.to_le_bytes());
	out[5..9].copy_from_slice(&state.buttons.to_le_bytes());
	out[9] = state.hats[0];
	out[10] = state.hats[1];
	let mut at = 11;
	for value in &state.axes[..shape.axes as usize] {
		out[at..at + 4].copy_from_slice(&value.to_le_bytes());
		at += 4;
	}
	at
}

/// A DEPARTURE into `out`; its length.
pub fn encode_departure(handle: u32, out: &mut [u8; MAX_FRAME]) -> usize {
	out[0] = DEPARTURE;
	out[1..5].copy_from_slice(&handle.to_le_bytes());
	5
}

/// Where a publisher's frames go: its consumer connection, which takes a frame or says it cannot now.
///
/// NEVER A CLOSE. A full connection is not a gone one, and DeviceManager counts a consumer back only when
/// it applies the driver's `DISCONNECT` on a later pass of its loop - so a publisher that closed a full
/// connection and reopened it could be refused, and for good when the closed one was the offered one.
pub trait Sender {
	/// Send one frame; `false` when the connection cannot take it now.
	fn send(&mut self, frame: &[u8]) -> bool;
}

/// What one gamepad owes its consumer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Owed {
	Arrival,
	State,
	Departure,
}

#[derive(Clone, Copy)]
struct Pad {
	handle: u32,
	shape: Shape,
	/// What the gamepad is doing now - what an owed STATE carries when it is sent.
	current: State,
	/// A STATE is owed: the consumer has not been sent `current`.
	unsent: bool,
	/// Its ARRIVAL has gone out on this connection, so the consumer knows the handle.
	known: bool,
	/// Detached: it stays until its DEPARTURE is sent.
	departed: bool,
}

/// THE PUBLISHER TABLE: the rules every gamepad publisher follows, written once. `N` gamepads at most,
/// departed ones still owing their DEPARTURE included.
///
/// NOTHING BLOCKS AND NOTHING IS LOST. A STATE is sent only when the mapped state changed, and one the
/// connection cannot take marks its gamepad UNSENT rather than being dropped: a gamepad at rest sends no
/// next report, so a lost release would stay held. An ARRIVAL or a DEPARTURE the connection cannot take is
/// OWED in the same way. Owed frames leave IN THE ORDER THEY BECAME OWED - a gamepad's STATE never before its
/// ARRIVAL, its DEPARTURE never before its last STATE - and an owed STATE carries the gamepad's CURRENT
/// state when it goes, so a change meanwhile only updates what will be sent. While anything is owed the
/// driver retries on a one-tick housekeeping deadline, and at every other wake of its loop.
pub struct Publisher<const N: usize> {
	pads: [Option<Pad>; N],
	/// The owed frames, oldest first: a gamepad's slot and what it owes. At most three per gamepad.
	queue: [(u8, Owed); 48],
	queued: usize,
	next_handle: u32,
	connected: bool,
}

impl<const N: usize> Default for Publisher<N> {
	fn default() -> Self {
		Self::new()
	}
}

impl<const N: usize> Publisher<N> {
	pub const fn new() -> Self {
		assert!(N <= 16, "the owed queue holds three frames for each of at most sixteen gamepads");
		Self { pads: [const { None }; N], queue: [(0, Owed::Arrival); 48], queued: 0, next_handle: 1, connected: false }
	}

	/// A new gamepad, holding its initial state; its handle, or `None` when the table is full or the
	/// shape is not a gamepad's. HANDLES ARE NEVER REUSED, so a gamepad unplugged and plugged back is a new
	/// one to its consumer.
	pub fn attach(&mut self, shape: Shape) -> Option<u32> {
		let slot = self.pads.iter().position(Option::is_none)?;
		let handle = self.next_handle;
		self.next_handle = self.next_handle.wrapping_add(1).max(1);
		self.pads[slot] = Some(Pad { handle, shape, current: shape.initial(), unsent: false, known: false, departed: false });
		if self.connected {
			self.owe(slot, Owed::Arrival);
		}
		Some(handle)
	}

	/// A gamepad's newly mapped state. Nothing is owed when it did not change; otherwise the consumer is
	/// owed a STATE, once, carrying whatever the state is when it is sent. `false` for a handle this table
	/// does not hold live or a state its shape does not admit.
	pub fn report(&mut self, handle: u32, state: State) -> bool {
		let Some(slot) = self.live(handle) else { return false };
		let Some(pad) = self.pads[slot].as_mut() else { return false };
		if !pad.shape.admits(&state) {
			return false;
		}
		if pad.current == state {
			return true;
		}
		pad.current = state;
		if self.connected && !pad.unsent {
			pad.unsent = true;
			self.owe(slot, Owed::State);
		}
		true
	}

	/// A gamepad gone. The consumer is owed its DEPARTURE, after anything it already owes - unless its
	/// ARRIVAL has not gone out, in which case the consumer never heard of it and is owed neither.
	pub fn detach(&mut self, handle: u32) -> bool {
		let Some(slot) = self.live(handle) else { return false };
		let Some(pad) = self.pads[slot] else { return false };
		let arrival_owed = self.owes_slot(slot, Owed::Arrival);
		if !self.connected || arrival_owed || !pad.known {
			self.forget(slot);
			return true;
		}
		// A DEPARTURE CLEARS THE UNSENT MARK: the consumer releases a departed gamepad whole.
		self.unqueue(slot, Owed::State);
		if let Some(pad) = self.pads[slot].as_mut() {
			pad.unsent = false;
			pad.departed = true;
		}
		self.owe(slot, Owed::Departure);
		true
	}

	/// A CONSUMER CONNECTED - the first, or a later one after the last went away. It is owed an ARRIVAL
	/// and a STATE for every gamepad held, in slot order; a departed gamepad still owing a DEPARTURE to the
	/// old connection is simply gone, since this consumer never heard of it.
	pub fn connected(&mut self) {
		self.queued = 0;
		self.connected = true;
		for slot in 0..N {
			match self.pads[slot] {
				Some(pad) if pad.departed => self.pads[slot] = None,
				Some(_) => {
					if let Some(pad) = self.pads[slot].as_mut() {
						pad.known = false;
						pad.unsent = true;
					}
					self.owe(slot, Owed::Arrival);
					self.owe(slot, Owed::State);
				}
				None => {}
			}
		}
	}

	/// The consumer's connection is gone: nothing is owed to anybody, and a departed gamepad is gone.
	pub fn disconnected(&mut self) {
		self.queued = 0;
		self.connected = false;
		for pad in self.pads.iter_mut() {
			match pad {
				Some(held) if held.departed => *pad = None,
				Some(held) => {
					held.known = false;
					held.unsent = false;
				}
				None => {}
			}
		}
	}

	/// Send what is owed, oldest first, until the sender refuses one; whether anything is still owed.
	pub fn flush(&mut self, sender: &mut dyn Sender) -> bool {
		let mut frame = [0u8; MAX_FRAME];
		while self.queued > 0 {
			let (slot, owed) = self.queue[0];
			let slot = slot as usize;
			let Some(pad) = self.pads[slot] else {
				self.pop();
				continue;
			};
			let length = match owed {
				Owed::Arrival => encode_arrival(pad.handle, &pad.shape, &mut frame),
				Owed::State => encode_state(pad.handle, &pad.shape, &pad.current, &mut frame),
				Owed::Departure => encode_departure(pad.handle, &mut frame),
			};
			if !sender.send(&frame[..length]) {
				return true;
			}
			self.pop();
			match owed {
				Owed::Arrival => {
					if let Some(pad) = self.pads[slot].as_mut() {
						pad.known = true;
					}
				}
				Owed::State => {
					if let Some(pad) = self.pads[slot].as_mut() {
						pad.unsent = false;
					}
				}
				Owed::Departure => self.pads[slot] = None,
			}
		}
		false
	}

	/// Whether anything is owed - what the driver's retry deadline is armed on.
	pub fn owes(&self) -> bool {
		self.queued > 0
	}

	/// How many live (not departed) gamepads the table holds.
	pub fn len(&self) -> usize {
		self.pads.iter().flatten().filter(|pad| !pad.departed).count()
	}

	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	/// A live gamepad's current state.
	pub fn current(&self, handle: u32) -> Option<State> {
		self.live(handle).and_then(|slot| self.pads[slot]).map(|pad| pad.current)
	}

	fn live(&self, handle: u32) -> Option<usize> {
		self.pads.iter().position(|pad| pad.is_some_and(|pad| pad.handle == handle && !pad.departed))
	}

	fn owes_slot(&self, slot: usize, owed: Owed) -> bool {
		self.queue[..self.queued].contains(&(slot as u8, owed))
	}

	fn owe(&mut self, slot: usize, owed: Owed) {
		if self.owes_slot(slot, owed) {
			return;
		}
		// THREE PER GAMEPAD AT MOST - an ARRIVAL, a STATE and a DEPARTURE - so the queue cannot overflow;
		// the check keeps a defect from writing past it.
		if self.queued < self.queue.len() {
			self.queue[self.queued] = (slot as u8, owed);
			self.queued += 1;
		}
	}

	fn pop(&mut self) {
		self.queue.copy_within(1..self.queued, 0);
		self.queued -= 1;
	}

	fn unqueue(&mut self, slot: usize, owed: Owed) {
		let mut kept = 0;
		for index in 0..self.queued {
			if self.queue[index] != (slot as u8, owed) {
				self.queue[kept] = self.queue[index];
				kept += 1;
			}
		}
		self.queued = kept;
	}

	fn forget(&mut self, slot: usize) {
		let mut kept = 0;
		for index in 0..self.queued {
			if self.queue[index].0 != slot as u8 {
				self.queue[kept] = self.queue[index];
				kept += 1;
			}
		}
		self.queued = kept;
		self.pads[slot] = None;
	}
}

#[cfg(test)]
mod tests;

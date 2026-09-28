// VIRTIO-GPIO LINES (virtio 1.2, device 41), as the line contract serves them: which line each connection
// holds, and where each interrupt line is in its one-event cycle.
//
// INPUT LINES ONLY. A connection scoped for LEVEL READS sets its line to input and reads it; one scoped for
// an INTERRUPT also arms it with a trigger and gives the device ONE event-queue buffer for it. The device
// completes that buffer when the line fires and has nothing to complete the next event into until the buffer
// comes back - which is the device's own mask. So an event is delivered ONCE, and the buffer is queued again
// only when the consumer acknowledges: a level line that stays asserted is reported again after the
// acknowledgement and not before, and cannot storm.
//
// A line given back is disarmed (interrupt type none, which completes its queued buffer as invalid) and
// deactivated (direction none). Each line is held by one connection at a time.

use alloc::vec::Vec;

// The request types this driver sends.
pub const MSG_GET_NAMES: u16 = 0x0001;
pub const MSG_SET_DIRECTION: u16 = 0x0003;
pub const MSG_GET_VALUE: u16 = 0x0004;
pub const MSG_SET_IRQ_TYPE: u16 = 0x0006;

// Directions.
pub const DIRECTION_NONE: u32 = 0x00;
pub const DIRECTION_IN: u32 = 0x02;

// The event queue exists only with this feature.
pub const FEATURE_IRQ: u64 = 1 << 0;

// A request's status, and an event buffer's.
pub const STATUS_OK: u8 = 0x0;
pub const EVENT_VALID: u8 = 0x1;

// HOW AN INTERRUPT LINE FIRES, in the device's own interrupt-type numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
	Rising = 0x01,
	Falling = 0x02,
	Both = 0x03,
	High = 0x04,
	Low = 0x08,
}

pub const IRQ_NONE: u32 = 0x00;

impl Trigger {
	pub fn from_u8(value: u8) -> Option<Self> {
		match value {
			0x01 => Some(Trigger::Rising),
			0x02 => Some(Trigger::Falling),
			0x03 => Some(Trigger::Both),
			0x04 => Some(Trigger::High),
			0x08 => Some(Trigger::Low),
			_ => None,
		}
	}
}

// What a connection is scoped to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
	// The level, read on demand, and no event.
	Level,
	// Events, on this trigger.
	Interrupt(Trigger),
}

// ONE REQUEST the driver sends on the request queue: `(type, line, value)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Request {
	pub kind: u16,
	pub line: u16,
	pub value: u32,
}

impl Request {
	pub fn encode(&self) -> [u8; 8] {
		let mut out = [0u8; 8];
		out[0..2].copy_from_slice(&self.kind.to_le_bytes());
		out[2..4].copy_from_slice(&self.line.to_le_bytes());
		out[4..8].copy_from_slice(&self.value.to_le_bytes());
		out
	}
}

// What the driver must do next for a line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
	Send(Request),
	// Queue the line's event buffer on the event queue.
	QueueEvent(u16),
}

// Where one held line is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Held {
	Level,
	// Armed, its buffer with the device.
	Armed(Trigger),
	// An event delivered and not yet acknowledged: the buffer is here, and the device can report nothing.
	Delivered(Trigger),
}

// Why a line was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	// The device has no such line.
	NoSuchLine,
	// Another connection holds it.
	Held,
	// An interrupt scope on a device that offers no event queue.
	NoEvents,
}

// What an event completion means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
	// Deliver it to the line's connection - once.
	Deliver(u16),
	// The buffer came back because the line was disarmed, or for a line nobody holds: nothing to deliver.
	Nothing,
}

// EVERY LINE A CONNECTION HOLDS.
pub struct Lines {
	count: u16,
	events: bool,
	held: Vec<(u16, Held)>,
}

impl Lines {
	// A device with `count` lines, which does or does not offer the event queue.
	pub fn new(count: u16, events: bool) -> Self {
		Self { count, events, held: Vec::new() }
	}

	fn find(&self, line: u16) -> Option<usize> {
		self.held.iter().position(|(held, _)| *held == line)
	}

	// TAKE `line` FOR A CONNECTION with `scope`, and the steps that set it up, in order: input direction, then
	// for an interrupt the trigger and only then the event buffer - a device returns a buffer queued for a line
	// whose interrupt type is still none at once, as invalid, and the line would then have no buffer to report
	// on. A level trigger already met completes the buffer as soon as it is queued.
	pub fn take(&mut self, line: u16, scope: Scope, steps: &mut Vec<Step>) -> Result<(), Refusal> {
		if line >= self.count {
			return Err(Refusal::NoSuchLine);
		}
		if self.find(line).is_some() {
			return Err(Refusal::Held);
		}
		if matches!(scope, Scope::Interrupt(_)) && !self.events {
			return Err(Refusal::NoEvents);
		}
		steps.push(Step::Send(Request { kind: MSG_SET_DIRECTION, line, value: DIRECTION_IN }));
		let held = match scope {
			Scope::Level => Held::Level,
			Scope::Interrupt(trigger) => {
				steps.push(Step::Send(Request { kind: MSG_SET_IRQ_TYPE, line, value: trigger as u32 }));
				steps.push(Step::QueueEvent(line));
				Held::Armed(trigger)
			}
		};
		self.held.push((line, held));
		Ok(())
	}

	// AN EVENT BUFFER CAME BACK for `line` with `status`.
	pub fn event(&mut self, line: u16, status: u8) -> Event {
		let Some(at) = self.find(line) else { return Event::Nothing };
		match self.held[at].1 {
			Held::Armed(trigger) if status == EVENT_VALID => {
				self.held[at].1 = Held::Delivered(trigger);
				Event::Deliver(line)
			}
			_ => Event::Nothing,
		}
	}

	// THE CONSUMER ACKNOWLEDGED: the buffer goes back to the device, which reports the line again - a level
	// line still asserted at once. Answers the step, or `None` when nothing was delivered.
	pub fn acknowledge(&mut self, line: u16) -> Option<Step> {
		let at = self.find(line)?;
		match self.held[at].1 {
			Held::Delivered(trigger) => {
				self.held[at].1 = Held::Armed(trigger);
				Some(Step::QueueEvent(line))
			}
			_ => None,
		}
	}

	// GIVE `line` BACK: disarmed and deactivated.
	pub fn give_back(&mut self, line: u16, steps: &mut Vec<Step>) {
		let Some(at) = self.find(line) else { return };
		let (_, held) = self.held.swap_remove(at);
		if !matches!(held, Held::Level) {
			steps.push(Step::Send(Request { kind: MSG_SET_IRQ_TYPE, line, value: IRQ_NONE }));
		}
		steps.push(Step::Send(Request { kind: MSG_SET_DIRECTION, line, value: DIRECTION_NONE }));
	}

	// Whether `line` is held for events - which is what may acknowledge.
	pub fn is_interrupt(&self, line: u16) -> bool {
		self.find(line).is_some_and(|at| !matches!(self.held[at].1, Held::Level))
	}
}

// THE LINE NAMES `GET_NAMES` ANSWERS: one NUL-terminated name per line, in line order; a line with no name is
// an empty string. The name of `line`, or `None` past the list.
pub fn line_name(names: &[u8], line: u16) -> Option<&[u8]> {
	names.split(|&byte| byte == 0).nth(line as usize)
}

#[cfg(test)]
mod tests;

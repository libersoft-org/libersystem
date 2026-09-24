//! THE TRUSTED KEYBOARD'S TWO JOBS: noticing the secure-attention chord, and turning physical keys into a
//! decision nobody else can make.
//!
//! SECURE ATTENTION IS CTRL+ALT+F12, from the trusted keyboard only, and it is noticed before anything is
//! delivered anywhere else. Nothing a requester sends can produce it.
//!
//! A SESSION IS ARMED ONLY WHEN NO KEY IS HELD. Keys already down when the protected screen appears - the
//! chord itself, or a key a program asked the person to hold - are not decisions; the session waits for every
//! one of them to be released, then arms a fresh epoch. After that, and only once the display has confirmed
//! it is showing the same epoch, a physical Enter pressed AND released approves, and Escape declines. A key
//! pressed between Enter's down and up spoils that Enter; a release with no press behind it, a repeated press,
//! a key from another epoch, a pointer click and anything a program sends approve nothing.

use alloc::vec::Vec;

pub const LEFT_CTRL: u16 = 0xe0;
pub const LEFT_ALT: u16 = 0xe2;
pub const RIGHT_CTRL: u16 = 0xe4;
pub const RIGHT_ALT: u16 = 0xe6;
pub const F12: u16 = 0x45;
pub const ENTER: u16 = 0x28;
pub const KEYPAD_ENTER: u16 = 0x58;
pub const ESCAPE: u16 = 0x29;

/// What the trusted keyboard is holding, and whether a transition completed the chord.
#[derive(Default, Debug)]
pub struct Watch {
	held: Vec<u16>,
}

/// One transition, as the trusted keyboard sent it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Seen {
	/// Nothing this module acts on - a repeat, a release of a key not held.
	Nothing,
	/// A key transition to pass on, if a session is armed.
	Key { usage: u16, down: bool },
	/// The secure-attention chord: F12 went down with a Ctrl and an Alt held.
	Attention,
}

impl Watch {
	pub fn new() -> Watch {
		Watch { held: Vec::new() }
	}

	pub fn idle(&self) -> bool {
		self.held.is_empty()
	}

	pub fn record(&mut self, usage: u16, down: bool) -> Seen {
		let at = self.held.iter().position(|held| *held == usage);
		if down {
			if at.is_some() {
				return Seen::Nothing;
			}
			let chord = usage == F12 && self.held.iter().any(|held| matches!(*held, LEFT_CTRL | RIGHT_CTRL)) && self.held.iter().any(|held| matches!(*held, LEFT_ALT | RIGHT_ALT));
			self.held.push(usage);
			if chord {
				return Seen::Attention;
			}
		} else {
			let Some(at) = at else { return Seen::Nothing };
			self.held.swap_remove(at);
		}
		Seen::Key { usage, down }
	}

	/// Everything is released: the device went away, or its stream was reset.
	pub fn clear(&mut self) {
		self.held.clear();
	}
}

/// InputService's side of a protected session: armed once the keyboard is idle, and then passing keys on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arming {
	Off,
	/// Waiting for every key to be released.
	Waiting(u64),
	Armed(u64),
}

impl Arming {
	/// Enter a session. It arms at once when nothing is held.
	pub fn arm(epoch: u64, watch: &Watch) -> Arming {
		if watch.idle() { Arming::Armed(epoch) } else { Arming::Waiting(epoch) }
	}

	/// After a transition was recorded: the epoch that has just armed, if one did.
	pub fn settle(&mut self, watch: &Watch) -> Option<u64> {
		if let Arming::Waiting(epoch) = *self
			&& watch.idle()
		{
			*self = Arming::Armed(epoch);
			return Some(epoch);
		}
		None
	}

	pub fn armed(&self) -> Option<u64> {
		match *self {
			Arming::Armed(epoch) => Some(epoch),
			_ => None,
		}
	}
}

/// What a person's keys decided.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
	Approve,
	Decline,
}

/// AdminService's side: a decision is taken only under the epoch both the display and the keyboard confirmed.
#[derive(Clone, Copy, Debug)]
pub struct Decision {
	pub epoch: u64,
	armed: bool,
	presented: bool,
	enter: bool,
}

impl Decision {
	pub fn new(epoch: u64) -> Decision {
		Decision { epoch, armed: false, presented: false, enter: false }
	}

	pub fn armed(&mut self, epoch: u64) {
		if epoch == self.epoch {
			self.armed = true;
		}
	}

	/// The display confirmed the protected screen is showing, freshly presented, under this epoch.
	pub fn presented(&mut self, epoch: u64) {
		if epoch == self.epoch {
			self.presented = true;
		}
	}

	pub fn ready(&self) -> bool {
		self.armed && self.presented
	}

	pub fn key(&mut self, epoch: u64, usage: u16, down: bool) -> Option<Verdict> {
		if epoch != self.epoch || !self.ready() {
			return None;
		}
		match (usage, down) {
			(ENTER | KEYPAD_ENTER, true) => {
				self.enter = true;
				None
			}
			(ENTER | KEYPAD_ENTER, false) if self.enter => {
				self.enter = false;
				Some(Verdict::Approve)
			}
			(ESCAPE, true) => Some(Verdict::Decline),
			// ANY OTHER KEY PRESSED spoils an Enter in progress.
			(_, true) => {
				self.enter = false;
				None
			}
			_ => None,
		}
	}
}

#[cfg(test)]
mod tests;

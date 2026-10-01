//! THE ACTIVITY SIGNAL'S EDGES - `liber:input@1/input-activity`, as InputService keeps it for each watcher.
//!
//! A watcher asked for `idle` once there has been no input for its `idle-after`, and `active` at the first input after
//! that - edges, never levels, and nothing about what the input was. One clock of input for every watcher: the last
//! input InputService saw from any source, the protected session's included.

/// One edge of the signal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
	Idle,
	Active,
}

/// The shortest and the longest `idle-after` a watcher may ask for, in ticks of a hundredth of a second: a second, a
/// day. Outside them the request is refused rather than clamped - a watcher that asked for a millisecond and got a
/// second would be deciding on an edge it did not ask for.
pub const MIN_IDLE_AFTER_TICKS: u64 = 100;
pub const MAX_IDLE_AFTER_TICKS: u64 = 100 * 86_400;

/// One watcher's side of the signal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Watch {
	idle_after: u64,
	idle: bool,
}

impl Watch {
	/// A watch for `idle_after` ticks, or `None` outside the bounds.
	pub fn new(idle_after: u64) -> Option<Watch> {
		(MIN_IDLE_AFTER_TICKS..=MAX_IDLE_AFTER_TICKS).contains(&idle_after).then_some(Watch { idle_after, idle: false })
	}

	/// Input seen: `active`, once, if this watch had said `idle`.
	pub fn input(&mut self) -> Option<Edge> {
		core::mem::replace(&mut self.idle, false).then_some(Edge::Active)
	}

	/// Time passed with the last input at `last`: `idle`, once, when there has been none for `idle_after`.
	pub fn tick(&mut self, last: u64, now: u64) -> Option<Edge> {
		if self.idle || now.saturating_sub(last) < self.idle_after {
			return None;
		}
		self.idle = true;
		Some(Edge::Idle)
	}

	/// When `tick` next answers, while it can: the wait's deadline.
	pub fn due(&self, last: u64) -> Option<u64> {
		(!self.idle).then(|| last.saturating_add(self.idle_after))
	}
}

#[cfg(test)]
mod tests;

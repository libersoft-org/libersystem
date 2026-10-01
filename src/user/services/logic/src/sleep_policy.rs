//! THE SLEEP POLICY - what the power-state service does with a lid, idleness and a critical battery. Its IO is the
//! service's; every decision is here, a function of the edges it was told and nothing else.
//!
//! THE DEFAULTS, as the plan states them and the owner confirms: closing the lid suspends, unless an external display is
//! in use; the sleep buttons suspend and the power buttons keep powering off - their drivers ask for that themselves, and
//! this policy is not in their path; an idle timeout suspends on battery; a critical battery hibernates where hibernation
//! is set up and otherwise - wherever it is not set up, and whenever a hibernation is refused - powers off in order.
//!
//! EDGES, NEVER LEVELS: a closed lid suspends once per closing, an idle machine on battery once per idle edge (or once
//! when it goes on battery already idle), and a critical battery once until it is no longer critical. A lid still closed
//! at a resume the timed wake ended does not put the machine back to sleep by itself.

/// Why the policy asks for a suspend - `system-sleep`'s `sleep-reason`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
	Lid,
	Idle,
}

/// What the policy asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
	Nothing,
	Suspend(Why),
	/// A critical battery, where hibernation may be set up: `hibernate`, and `refused` if it is not.
	Hibernate,
	/// The orderly power-off: the kernel's forced deadline armed first, then ServiceManager's sequence.
	PowerOff,
}

/// What the policy is told.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
	/// The lid closed (`true`) or opened.
	Lid(bool),
	/// Whether an external display is in use now.
	ExternalDisplay(bool),
	/// The activity signal's edges.
	Idle,
	Active,
	/// What the power sources say now: running on battery, and a battery at critical capacity.
	Power {
		on_battery: bool,
		critical: bool,
	},
}

/// The switches the owner confirms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
	pub lid_suspends: bool,
	pub idle_suspends_on_battery: bool,
	/// The idle timeout, in seconds.
	pub idle_after_seconds: u32,
}

impl Default for Settings {
	fn default() -> Settings {
		Settings { lid_suspends: true, idle_suspends_on_battery: true, idle_after_seconds: 15 * 60 }
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Policy {
	pub settings: Settings,
	lid_closed: bool,
	external: bool,
	idle: bool,
	on_battery: bool,
	critical_acted: bool,
}

impl Policy {
	pub fn new(settings: Settings) -> Policy {
		Policy { settings, lid_closed: false, external: false, idle: false, on_battery: false, critical_acted: false }
	}

	pub fn event(&mut self, event: Event) -> Action {
		match event {
			Event::Lid(closed) => {
				let closing = closed && !self.lid_closed;
				self.lid_closed = closed;
				if closing && self.settings.lid_suspends && !self.external { Action::Suspend(Why::Lid) } else { Action::Nothing }
			}
			Event::ExternalDisplay(external) => {
				self.external = external;
				Action::Nothing
			}
			Event::Idle => {
				let edge = !self.idle;
				self.idle = true;
				if edge && self.on_battery && self.settings.idle_suspends_on_battery { Action::Suspend(Why::Idle) } else { Action::Nothing }
			}
			Event::Active => {
				self.idle = false;
				Action::Nothing
			}
			Event::Power { on_battery, critical } => {
				let unplugged = on_battery && !self.on_battery;
				self.on_battery = on_battery;
				if !critical {
					self.critical_acted = false;
				} else if !self.critical_acted {
					self.critical_acted = true;
					return Action::Hibernate;
				}
				if unplugged && self.idle && self.settings.idle_suspends_on_battery { Action::Suspend(Why::Idle) } else { Action::Nothing }
			}
		}
	}

	/// THE HIBERNATION REFUSED - not set up, or refused for any other reason: the orderly power-off instead.
	pub fn hibernation_refused(&mut self) -> Action {
		Action::PowerOff
	}
}

#[cfg(test)]
mod tests;

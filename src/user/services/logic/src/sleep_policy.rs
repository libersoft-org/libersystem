//! THE SLEEP POLICY - what the power-state service does with a lid, idleness and a critical battery. Its IO is the
//! service's; every decision is here, a function of the edges it was told and nothing else.
//!
//! THE DEFAULTS, as the owner decided them (2026-10-02): closing the lid TURNS THE SCREEN OFF and suspends nothing, and
//! opening it turns the screen on again - nothing at all while an external display is in use; idleness suspends
//! nothing, on battery either; a critical battery neither suspends nor hibernates - it powers the machine off in order,
//! so the filesystems are closed before the battery gives out. The sleep buttons suspend and the power buttons keep
//! powering off - their drivers ask for that themselves, and this policy is not in their path.
//!
//! EACH IS A SETTING, read from ConfigService's tree when the service starts (`Settings::from_keys`): `power.lid`
//! (`screen-off`, `suspend` or `nothing`), `power.idle-suspend` (`on` or `off`: whether idleness on battery suspends),
//! `power.idle-after-s` (the idle timeout, in seconds) and `power.critical` (`power-off`, `hibernate` - where that is
//! set up, and the orderly power-off whenever a hibernation is refused - or `nothing`). A key that is absent keeps its
//! default; a value that is not one of these is refused by name and the default stands.
//!
//! EDGES, NEVER LEVELS: a closed lid acts once per closing, an idle machine on battery once per idle edge (or once when
//! it goes on battery already idle), and a critical battery once until it is no longer critical. A lid still closed at
//! a resume the timed wake ended does not put the machine back to sleep by itself. THE LID'S FIRST STATE counts as an
//! edge - a relaunched service is told the lid as it is - so an open lid turns on a screen an earlier instance turned off.

use alloc::vec::Vec;

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
	/// DisplayService's screen off - black, and every present discarded - or on again.
	ScreenOff,
	ScreenOn,
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

/// What closing the lid does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LidAction {
	ScreenOff,
	Suspend,
	Nothing,
}

/// What a critical battery does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CriticalAction {
	PowerOff,
	Hibernate,
	Nothing,
}

/// The settings - see the head of this file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
	pub lid: LidAction,
	pub idle_suspends_on_battery: bool,
	/// The idle timeout, in seconds.
	pub idle_after_seconds: u32,
	pub critical: CriticalAction,
}

impl Default for Settings {
	fn default() -> Settings {
		Settings { lid: LidAction::ScreenOff, idle_suspends_on_battery: false, idle_after_seconds: 15 * 60, critical: CriticalAction::PowerOff }
	}
}

/// The configuration keys, in the order `Settings::from_keys` takes their values.
pub const KEYS: [&str; 4] = ["power.lid", "power.idle-suspend", "power.idle-after-s", "power.critical"];

impl Settings {
	/// THE SETTINGS FROM THE KEYS' VALUES, each `None` where the key is absent - and the keys whose values were refused,
	/// whose defaults stand.
	pub fn from_keys(lid: Option<&str>, idle_suspend: Option<&str>, idle_after_s: Option<&str>, critical: Option<&str>) -> (Settings, Vec<&'static str>) {
		let mut settings = Settings::default();
		let mut refused = Vec::new();
		if let Some(value) = lid {
			match value {
				"screen-off" => settings.lid = LidAction::ScreenOff,
				"suspend" => settings.lid = LidAction::Suspend,
				"nothing" => settings.lid = LidAction::Nothing,
				_ => refused.push(KEYS[0]),
			}
		}
		if let Some(value) = idle_suspend {
			match value {
				"on" | "true" | "1" => settings.idle_suspends_on_battery = true,
				"off" | "false" | "0" => settings.idle_suspends_on_battery = false,
				_ => refused.push(KEYS[1]),
			}
		}
		if let Some(value) = idle_after_s {
			match value.parse::<u32>() {
				// A ZERO TIMEOUT would be an idle edge at every pause between keys.
				Ok(seconds) if seconds > 0 => settings.idle_after_seconds = seconds,
				_ => refused.push(KEYS[2]),
			}
		}
		if let Some(value) = critical {
			match value {
				"power-off" => settings.critical = CriticalAction::PowerOff,
				"hibernate" => settings.critical = CriticalAction::Hibernate,
				"nothing" => settings.critical = CriticalAction::Nothing,
				_ => refused.push(KEYS[3]),
			}
		}
		(settings, refused)
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Policy {
	pub settings: Settings,
	// The lid as last told, `None` before its first state.
	lid_closed: Option<bool>,
	external: bool,
	idle: bool,
	on_battery: bool,
	critical_acted: bool,
}

impl Policy {
	pub fn new(settings: Settings) -> Policy {
		Policy { settings, lid_closed: None, external: false, idle: false, on_battery: false, critical_acted: false }
	}

	pub fn event(&mut self, event: Event) -> Action {
		match event {
			Event::Lid(closed) => {
				let edge = self.lid_closed != Some(closed);
				self.lid_closed = Some(closed);
				if !edge {
					return Action::Nothing;
				}
				match (self.settings.lid, closed) {
					(LidAction::ScreenOff, false) => Action::ScreenOn,
					(LidAction::ScreenOff, true) if !self.external => Action::ScreenOff,
					(LidAction::Suspend, true) if !self.external => Action::Suspend(Why::Lid),
					_ => Action::Nothing,
				}
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
					match self.settings.critical {
						CriticalAction::PowerOff => return Action::PowerOff,
						CriticalAction::Hibernate => return Action::Hibernate,
						CriticalAction::Nothing => {}
					}
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

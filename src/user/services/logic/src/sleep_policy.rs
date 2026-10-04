//! THE SLEEP POLICY - what the power-state service does with a lid, idleness, a critical battery and the power and sleep
//! buttons. Its IO is the service's; every decision is here, a function of the edges it was told and nothing else.
//!
//! THE DEFAULTS, as the owner decided them (2026-10-02): closing the lid TURNS THE SCREEN OFF and suspends nothing, and
//! opening it turns the screen on again - nothing at all while an external display is in use; idleness suspends
//! nothing, on battery either; and a critical battery does nothing at all - it neither suspends, hibernates nor powers
//! the machine off (the owner, 2026-10-03), which says so on the console. A power button powers the machine off IN
//! ORDER - every service stopped and the logs flushed first - and a sleep button suspends, fixed or control-method alike.
//! A button's driver carries its press out itself only while no policy watches it.
//!
//! EACH IS A SETTING, read from ConfigService's tree when the service starts (`Settings::from_keys`): `power.lid`
//! (`screen-off`, `suspend` or `nothing`), `power.idle-suspend` (`on` or `off`: whether idleness on battery suspends),
//! `power.idle-after-s` (the idle timeout, in seconds), `power.critical` (`power-off`, `hibernate` - where that is set
//! up, and the orderly power-off whenever a hibernation is refused - or `nothing`), `power.button` (`power-off`,
//! `suspend`, `hibernate` - the orderly power-off whenever it is refused - or `nothing`) and `power.sleep-button`
//! (`suspend`, `hibernate` - nothing more when it is refused - or `nothing`). A key that is absent keeps its default; a
//! value that is not one of these is refused by name and the default stands.
//!
//! EDGES, NEVER LEVELS: a closed lid acts once per closing, an idle machine on battery once per idle edge (or once when
//! it goes on battery already idle), a critical battery once until it is no longer critical, and a button once per
//! press. A lid still closed at a resume the timed wake ended does not put the machine back to sleep by itself. THE LID'S
//! FIRST STATE counts as an edge - a relaunched service is told the lid as it is - so an open lid turns on a screen an
//! earlier instance turned off.

use alloc::vec::Vec;

/// Why the policy asks for a sleep - `system-sleep`'s `sleep-reason`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
	Lid,
	Idle,
	Critical,
	PowerButton,
	SleepButton,
}

/// What the policy asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
	Nothing,
	Suspend(Why),
	/// DisplayService's screen off - black, and every present discarded - or on again.
	ScreenOff,
	ScreenOn,
	/// A hibernation, where it may be set up - `hibernation_refused` says what follows where it is not.
	Hibernate(Why),
	/// The orderly power-off: the kernel's forced deadline armed first, then ServiceManager's sequence.
	PowerOff,
}

/// Which button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
	Power,
	Sleep,
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
	/// A button pressed - once per press.
	Pressed(Button),
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

/// What a button's press does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonAction {
	PowerOff,
	Suspend,
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
	pub power_button: ButtonAction,
	/// Never `PowerOff`: a sleep button that stopped the machine would be a second power button.
	pub sleep_button: ButtonAction,
}

impl Default for Settings {
	fn default() -> Settings {
		Settings { lid: LidAction::ScreenOff, idle_suspends_on_battery: false, idle_after_seconds: 15 * 60, critical: CriticalAction::Nothing, power_button: ButtonAction::PowerOff, sleep_button: ButtonAction::Suspend }
	}
}

/// The configuration keys, in the order `Settings::from_keys` takes their values.
pub const KEYS: [&str; 6] = ["power.lid", "power.idle-suspend", "power.idle-after-s", "power.critical", "power.button", "power.sleep-button"];

impl Settings {
	/// THE SETTINGS FROM THE KEYS' VALUES, in `KEYS`' order, each `None` where the key is absent - and the keys whose
	/// values were refused, whose defaults stand.
	pub fn from_keys(values: [Option<&str>; KEYS.len()]) -> (Settings, Vec<&'static str>) {
		let [lid, idle_suspend, idle_after_s, critical, button, sleep_button] = values;
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
		if let Some(value) = button {
			match value {
				"power-off" => settings.power_button = ButtonAction::PowerOff,
				"suspend" => settings.power_button = ButtonAction::Suspend,
				"hibernate" => settings.power_button = ButtonAction::Hibernate,
				"nothing" => settings.power_button = ButtonAction::Nothing,
				_ => refused.push(KEYS[4]),
			}
		}
		if let Some(value) = sleep_button {
			match value {
				"suspend" => settings.sleep_button = ButtonAction::Suspend,
				"hibernate" => settings.sleep_button = ButtonAction::Hibernate,
				"nothing" => settings.sleep_button = ButtonAction::Nothing,
				_ => refused.push(KEYS[5]),
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
						CriticalAction::Hibernate => return Action::Hibernate(Why::Critical),
						CriticalAction::Nothing => {}
					}
				}
				if unplugged && self.idle && self.settings.idle_suspends_on_battery { Action::Suspend(Why::Idle) } else { Action::Nothing }
			}
			Event::Pressed(button) => {
				let (action, why) = match button {
					Button::Power => (self.settings.power_button, Why::PowerButton),
					Button::Sleep => (self.settings.sleep_button, Why::SleepButton),
				};
				match action {
					ButtonAction::PowerOff => Action::PowerOff,
					ButtonAction::Suspend => Action::Suspend(why),
					ButtonAction::Hibernate => Action::Hibernate(why),
					ButtonAction::Nothing => Action::Nothing,
				}
			}
		}
	}

	/// What a press of `button` does, as set.
	pub fn button(&self, button: Button) -> ButtonAction {
		match button {
			Button::Power => self.settings.power_button,
			Button::Sleep => self.settings.sleep_button,
		}
	}

	/// Whether a critical battery has been told since the last reading that was not critical - `event` acts on its first
	/// one only, whatever the setting.
	pub fn critical_told(&self) -> bool {
		self.critical_acted
	}

	/// THE HIBERNATION REFUSED - not set up, or refused for any other reason: the orderly power-off instead for a critical
	/// battery and a power button, whose point is a machine that stops running; nothing more for a sleep button.
	pub fn hibernation_refused(&mut self, why: Why) -> Action {
		match why {
			Why::Critical | Why::PowerButton => Action::PowerOff,
			Why::Lid | Why::Idle | Why::SleepButton => Action::Nothing,
		}
	}
}

#[cfg(test)]
mod tests;

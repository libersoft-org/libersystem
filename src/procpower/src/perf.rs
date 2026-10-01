//! PERFORMANCE: a core's table, its window and the governor that picks its level inside it.
//!
//! LEVELS, FASTEST FIRST. A table's performance states (`_PSS`, or CPPC's range from highest to lowest) are levels
//! 0..n, level 0 the fastest; its throttling states past the first (`_TSS`, T1 onwards - T0 is no throttling) follow as
//! further levels, each applied at the slowest performance state. So one WINDOW - the fastest level the governor may
//! choose (the CAP) and the slowest (the FLOOR) - is where `_PPC`, the thermal limits and the profile meet: a cap pushed
//! past the performance states throttles.
//!
//! THE GOVERNOR follows utilisation, measured at the scheduler's own points - a busy core's tick, entry to and exit
//! from idle - over the period since its last decision: busy most of it, the window's fastest level; idle most of it,
//! one level slower; AT REST - next to nothing done - the window's slowest at once, since an idle core without a tick
//! decides only when it wakes, and one level a wake would leave it fast for minutes; in between, where it is. It changes the level no faster than the table's transition latency
//! allows, and never more often than `MIN_INTERVAL_US`; a window that moves is obeyed at once.

use alloc::vec::Vec;

use crate::{Arch, MAX_PERF_STATES, Refusal, Register, check_register};

/// The governor's shortest interval between two changes of level, in microseconds.
pub const MIN_INTERVAL_US: u64 = 10_000;
/// Busy at least this share of the period, in thousandths: the fastest level the window allows.
pub const UP_PERMILLE: u32 = 800;
/// Busy at most this share: one level slower.
pub const DOWN_PERMILLE: u32 = 300;
/// Busy at most this share - at rest: the slowest level the window allows.
pub const REST_PERMILLE: u32 = 50;

/// ONE `_PSS` STATE.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PerfState {
	pub core_mhz: u32,
	pub power_mw: u32,
	pub latency_us: u32,
	/// The value written to `_PCT`'s control register for it.
	pub control: u32,
	/// The value `_PCT`'s status register reads once it is in force.
	pub status: u32,
}

/// HOW A CORE'S PERFORMANCE IS CONTROLLED.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Control {
	/// `_PSS` states, fastest first, written through `_PCT`'s control register.
	States { control: Register, status: Register, states: Vec<PerfState> },
	/// CPPC: `_CPC`'s desired-performance register, with its minimum and maximum registers where it has them, and the
	/// levels it states.
	Cppc { desired: Register, minimum: Option<Register>, maximum: Option<Register>, preference: Option<Register>, highest: u32, nominal: u32, lowest: u32 },
}

/// `_PSD`'s coordination across the cores of one domain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Coordination {
	/// 0xFC: the operating system sets every core of the domain alike.
	SoftwareAll,
	/// 0xFD: any core of the domain may set it for all.
	SoftwareAny,
	/// 0xFE: the hardware coordinates.
	HardwareAll,
}

impl Coordination {
	pub fn from_type(value: u32) -> Option<Coordination> {
		match value {
			0xFC => Some(Coordination::SoftwareAll),
			0xFD => Some(Coordination::SoftwareAny),
			0xFE => Some(Coordination::HardwareAll),
			_ => None,
		}
	}
}

/// The domain a core's performance is coordinated in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Domain {
	pub domain: u32,
	pub coordination: Coordination,
	pub processors: u32,
}

/// ONE `_TSS` STATE: the share of full speed it runs at, in percent, and the value written to `_PTC`'s control register.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ThrottleState {
	pub percent: u32,
	pub latency_us: u32,
	pub control: u32,
}

/// `_PTC`'s control register and its `_TSS` states, T0 (no throttling) first.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Throttle {
	pub control: Register,
	pub states: Vec<ThrottleState>,
}

/// A CORE'S PERFORMANCE TABLE.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Table {
	pub control: Control,
	pub domain: Option<Domain>,
	pub throttle: Option<Throttle>,
}

/// The most CPPC levels a table counts - a wider range is stepped across in this many levels.
pub const MAX_CPPC_LEVELS: u32 = 32;

impl Table {
	/// How many performance levels it has, before the throttling ones.
	pub fn performance_levels(&self) -> u32 {
		match &self.control {
			Control::States { states, .. } => states.len() as u32,
			Control::Cppc { highest, lowest, .. } => (highest - lowest + 1).min(MAX_CPPC_LEVELS),
		}
	}

	/// How many levels it has in all: the performance levels, then each throttling state past T0.
	pub fn levels(&self) -> u32 {
		self.performance_levels() + self.throttle.as_ref().map_or(0, |throttle| throttle.states.len().saturating_sub(1) as u32)
	}

	/// What level `level` writes: the performance state's control value (or CPPC's desired performance), and the
	/// throttling state's control value where the level is past the performance levels.
	pub fn setting(&self, level: u32) -> Setting {
		let performance = self.performance_levels();
		let (performance_level, throttle) = if level < performance { (level, None) } else { (performance - 1, Some(level - performance + 1)) };
		let value = match &self.control {
			Control::States { states, .. } => states[performance_level as usize].control,
			Control::Cppc { highest, lowest, .. } => {
				// Spread across the range: level 0 the highest, the last level the lowest.
				let span = highest - lowest;
				let steps = performance.saturating_sub(1).max(1);
				highest - (span * performance_level) / steps
			}
		};
		let throttle = throttle.and_then(|at| self.throttle.as_ref().map(|throttle| throttle.states[at as usize].control));
		Setting { performance: value, throttle }
	}

	/// The longest transition the table states, in microseconds - the governor waits at least this between changes.
	pub fn transition_us(&self) -> u32 {
		let states = match &self.control {
			Control::States { states, .. } => states.iter().map(|state| state.latency_us).max().unwrap_or(0),
			Control::Cppc { .. } => 0,
		};
		let throttle = self.throttle.as_ref().map_or(0, |throttle| throttle.states.iter().map(|state| state.latency_us).max().unwrap_or(0));
		states.max(throttle)
	}

	/// Every register it names, each once - None where the heap cannot hold the five at most.
	pub fn registers(&self) -> Option<Vec<Register>> {
		let mut out: Vec<Register> = Vec::new();
		out.try_reserve_exact(5).ok()?;
		match &self.control {
			Control::States { control, status, .. } => {
				out.push(*control);
				out.push(*status);
			}
			Control::Cppc { desired, minimum, maximum, preference, .. } => {
				out.push(*desired);
				out.extend(minimum.iter().copied());
				out.extend(maximum.iter().copied());
				out.extend(preference.iter().copied());
			}
		}
		if let Some(throttle) = &self.throttle {
			out.push(throttle.control);
		}
		out.sort_unstable();
		out.dedup();
		Some(out)
	}
}

/// WHAT A LEVEL WRITES.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Setting {
	/// The `_PSS` control value, or CPPC's desired performance.
	pub performance: u32,
	/// The `_TSS` control value, where the level throttles.
	pub throttle: Option<u32>,
}

/// A CORE'S PERFORMANCE TABLE, checked whole: one to thirty-two states fastest first, every register one this kernel
/// writes - no model-specific register, no platform channel - a coordination it knows, and throttling states with T0
/// first.
pub fn check_table(table: &Table, arch: Arch) -> Result<(), Refusal> {
	match &table.control {
		Control::States { control, status, states } => {
			if states.is_empty() {
				return Err(Refusal::Empty);
			}
			if states.len() > MAX_PERF_STATES {
				return Err(Refusal::TooManyStates);
			}
			check_register(control, arch)?;
			check_register(status, arch)?;
			if states.windows(2).any(|pair| pair[1].core_mhz > pair[0].core_mhz) {
				return Err(Refusal::OutOfOrder);
			}
		}
		Control::Cppc { desired, minimum, maximum, preference, highest, nominal, lowest } => {
			check_register(desired, arch)?;
			for register in minimum.iter().chain(maximum.iter()).chain(preference.iter()) {
				check_register(register, arch)?;
			}
			if *lowest == 0 || lowest > nominal || nominal > highest {
				return Err(Refusal::OutOfOrder);
			}
		}
	}
	if let Some(throttle) = &table.throttle {
		if throttle.states.is_empty() {
			return Err(Refusal::Empty);
		}
		if throttle.states.len() > MAX_PERF_STATES {
			return Err(Refusal::TooManyStates);
		}
		check_register(&throttle.control, arch)?;
		if throttle.states[0].percent != 100 || throttle.states.windows(2).any(|pair| pair[1].percent >= pair[0].percent) {
			return Err(Refusal::OutOfOrder);
		}
	}
	Ok(())
}

/// THE WINDOW the governor never leaves: the fastest level it may choose and the slowest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Window {
	pub cap: u32,
	pub floor: u32,
}

impl Window {
	/// The whole table.
	pub fn all(levels: u32) -> Window {
		Window { cap: 0, floor: levels.saturating_sub(1) }
	}

	/// A window over a table of `levels` levels: the cap no slower than the floor, both inside the table.
	pub fn check(&self, levels: u32) -> Result<(), Refusal> {
		if self.cap > self.floor || self.floor >= levels {
			return Err(Refusal::BadWindow);
		}
		Ok(())
	}

	pub fn clamp(&self, level: u32) -> u32 {
		level.clamp(self.cap, self.floor)
	}
}

/// THE NEXT LEVEL: `current` moved for `busy_permille` of the period just ended, no sooner than the table's transition
/// latency and `MIN_INTERVAL_US` after `since_change_us` - and always into `window`, which a change of the window
/// enforces at once.
pub fn next_level(current: u32, busy_permille: u32, window: Window, since_change_us: u64, transition_us: u32) -> u32 {
	let current = window.clamp(current);
	if since_change_us < MIN_INTERVAL_US.max(u64::from(transition_us)) {
		return current;
	}
	let wanted = if busy_permille >= UP_PERMILLE {
		window.cap
	} else if busy_permille <= REST_PERMILLE {
		window.floor
	} else if busy_permille <= DOWN_PERMILLE {
		current.saturating_add(1)
	} else {
		current
	};
	window.clamp(wanted)
}

#[cfg(test)]
mod tests;

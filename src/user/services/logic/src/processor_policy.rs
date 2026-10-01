//! THE PROCESSOR POWER POLICY - what ProcessorPowerService asks of the kernel's governors and of the fans. Its IO is the
//! service's; every decision is here, a function of what it was told and nothing else.
//!
//! THE WINDOW. Each core's governor is held to a window of levels, fastest first - the table's performance states, then
//! its throttling states past T0 (`procpower::perf`). Where it lies is where three things meet: the PROFILE a person
//! chose (or the power source's default), the firmware's `_PPC` limit, and the THERMAL cap passive cooling holds the
//! core to. The profile sets the floor and a cap; `_PPC` and the thermal cap are obeyed above any profile - the cap is
//! the slowest of the three, and the floor never faster than the cap.
//!
//! PASSIVE COOLING is ACPI's equation (6.5, 11.1.5.1): at every `_TSP` sample while the zone is past `_PSV`,
//! `ΔP[%] = _TC1 * (Tn - Tn-1) + _TC2 * (Tn - Tt)`, with `Tt` the zone's `_PSV`, and the performance the zone's
//! processors may have becomes `Pn = Pn-1 - ΔP`, held between 0 and 100 %. Temperatures are ACPI's tenths of a kelvin,
//! so the equation's ΔP comes out in tenths of a percent - kept here as thousandths of full performance (permille), so
//! nothing is rounded. Cooling ends when the zone is back under `_PSV` and the limit has climbed back to 100 %. The
//! limit becomes a level - the fastest level whose capacity it allows - and past the slowest level, idle the scheduler
//! injects, at most a half.
//!
//! ACTIVE COOLING: a fan listed in `_ALx` runs while the zone is at or past `_ACx`; a fan listed under several trips runs
//! faster with each trip passed - all of its range past the hottest. Where the platform leaves the fan to the operating
//! system (`_FIF`'s fine-grain control), the owner's curve may ask for more; never less than the trips.
//!
//! CRITICAL: past `_CRT`, the orderly power-off within the forced bound; past `_HOT`, hibernation, and the `_CRT`
//! sequence where it is refused. Once per crossing, and nothing here - no profile, curve or setting - turns either off.

use alloc::vec::Vec;

/// The profiles a person chooses between.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Profile {
	Performance,
	Balanced,
	PowerSaving,
}

/// THE DEFAULT PROFILE for the power source: balanced on line power - or where it is not known - and power saving on
/// battery.
pub fn default_profile(on_battery: bool) -> Profile {
	if on_battery { Profile::PowerSaving } else { Profile::Balanced }
}

/// A WINDOW: the fastest level the governor may choose (`cap`) and the slowest (`floor`), levels fastest first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Window {
	pub cap: u32,
	pub floor: u32,
}

/// What the profile alone allows, over a table of `performance_levels` performance states: PERFORMANCE the fastest
/// state and nothing slower; BALANCED every performance state; POWER SAVING the slower half of them. No profile reaches
/// a throttling level - only the thermal cap does.
pub fn profile_window(profile: Profile, performance_levels: u32) -> Window {
	let slowest = performance_levels.saturating_sub(1);
	match profile {
		Profile::Performance => Window { cap: 0, floor: 0 },
		Profile::Balanced => Window { cap: 0, floor: slowest },
		Profile::PowerSaving => Window { cap: slowest / 2, floor: slowest },
	}
}

/// THE WINDOW a core is held to, over a table of `levels` levels of which the first `performance_levels` are performance
/// states: the profile's, with `_PPC`'s limit - an index into the performance states - and the thermal cap obeyed above
/// it. The cap is the slowest of the three; the floor is the profile's, never faster than the cap.
pub fn window(profile: Profile, levels: u32, performance_levels: u32, ppc: u32, thermal_cap: u32) -> Window {
	let last = levels.saturating_sub(1);
	let profile = profile_window(profile, performance_levels);
	let ppc = ppc.min(performance_levels.saturating_sub(1));
	let cap = profile.cap.max(ppc).max(thermal_cap).min(last);
	let floor = profile.floor.max(cap).min(last);
	Window { cap, floor }
}

/// FULL PERFORMANCE, in thousandths.
pub const FULL: u32 = 1000;

/// THE MOST IDLE THE SCHEDULER INJECTS, in thousandths of a core's time - the kernel's own bound.
pub const MAX_INJECT_PERMILLE: u32 = 500;

/// A ZONE'S PASSIVE-COOLING CONSTANTS, as its `_TC1`, `_TC2` and `_PSV` state them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Passive {
	pub tc1: u32,
	pub tc2: u32,
	/// `_PSV`, the target, in tenths of a kelvin.
	pub target: u32,
}

/// A ZONE'S PASSIVE COOLING: whether it is engaged, the limit it holds its processors to (thousandths of full
/// performance) and the temperature of the last sample, `Tn-1`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cooling {
	pub engaged: bool,
	pub limit: u32,
	pub last: u32,
}

impl Default for Cooling {
	fn default() -> Self {
		Cooling { engaged: false, limit: FULL, last: 0 }
	}
}

impl Cooling {
	/// ONE SAMPLE `temperature` (tenths of a kelvin): engaged at or past `_PSV`, the limit moved by the equation while
	/// engaged, and disengaged once the zone is under `_PSV` with the limit back at full.
	pub fn sample(&mut self, passive: Passive, temperature: u32) -> Cooling {
		if !self.engaged {
			if temperature < passive.target {
				self.last = temperature;
				return *self;
			}
			// ENGAGED: the first sample's trend is measured from itself, as no earlier one belongs to the episode.
			self.engaged = true;
			self.limit = FULL;
			self.last = temperature;
		}
		let trend = i64::from(passive.tc1) * (i64::from(temperature) - i64::from(self.last));
		let distance = i64::from(passive.tc2) * (i64::from(temperature) - i64::from(passive.target));
		let delta = trend + distance;
		self.limit = (i64::from(self.limit) - delta).clamp(0, i64::from(FULL)) as u32;
		self.last = temperature;
		if temperature < passive.target && self.limit == FULL {
			self.engaged = false;
		}
		*self
	}
}

/// WHAT A LIMIT MEANS FOR A CORE: the fastest level whose capacity it allows - `capacities` is each level's share of the
/// fastest, in thousandths, fastest first - and, past the slowest level, the idle the scheduler injects for the rest,
/// at most a half.
pub fn limit_level(limit: u32, capacities: &[u32]) -> (u32, u32) {
	let Some(&slowest) = capacities.last() else {
		return (0, inject_for(limit, FULL));
	};
	match capacities.iter().position(|&capacity| capacity <= limit) {
		Some(at) => (at as u32, 0),
		None => ((capacities.len() - 1) as u32, inject_for(limit, slowest)),
	}
}

// The share of time injected as idle so that a core running at `capacity` does `limit`'s work.
fn inject_for(limit: u32, capacity: u32) -> u32 {
	if capacity == 0 || limit >= capacity {
		return 0;
	}
	(FULL - limit * FULL / capacity).min(MAX_INJECT_PERMILLE)
}

/// THE SLOWEST CAP AND THE MOST INJECTION among the limits of every zone listing a core - the strictest wins.
pub fn strictest(limits: impl Iterator<Item = u32>) -> u32 {
	limits.fold(FULL, u32::min)
}

/// A FAN'S SHARE OF ITS RANGE from the active trips (percent): the trips listing it, `trips` their temperatures in
/// tenths of a kelvin in any order, each passed at or above it - all of the range past every one, a share for each.
pub fn active_percent(temperature: u32, trips: &[u32]) -> u8 {
	if trips.is_empty() {
		return 0;
	}
	let passed = trips.iter().filter(|&&trip| temperature >= trip).count();
	(passed * 100 / trips.len()) as u8
}

/// ONE POINT OF A FAN CURVE: at this temperature (tenths of a kelvin) and above, this share of the fan's range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CurvePoint {
	pub temperature: u32,
	pub percent: u8,
}

/// THE DEFAULT CURVE where the platform leaves its fan to the operating system: off below 40 °C, then a straight line to
/// all of its range at 80 °C, in steps of ten degrees.
pub fn default_curve() -> Vec<CurvePoint> {
	[(3132, 0), (3232, 25), (3332, 50), (3432, 75), (3532, 100)].iter().map(|&(temperature, percent)| CurvePoint { temperature, percent }).collect()
}

/// WHY A CURVE IS REFUSED.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CurveRefusal {
	/// No points, or more than eight.
	Size,
	/// Temperatures not rising, or a share past 100 %.
	Order,
	/// A share that falls as the temperature rises: a fan that slows as the zone heats.
	Falling,
}

/// A CURVE AN OPERATOR SETS, checked: one to eight points, temperatures rising, shares at most 100 % and never falling.
pub fn check_curve(curve: &[CurvePoint]) -> Result<(), CurveRefusal> {
	if curve.is_empty() || curve.len() > 8 {
		return Err(CurveRefusal::Size);
	}
	if curve.iter().any(|point| point.percent > 100) || curve.windows(2).any(|pair| pair[1].temperature <= pair[0].temperature) {
		return Err(CurveRefusal::Order);
	}
	if curve.windows(2).any(|pair| pair[1].percent < pair[0].percent) {
		return Err(CurveRefusal::Falling);
	}
	Ok(())
}

/// THE CURVE'S SHARE at `temperature`: the share of the hottest point at or below it, none below the first.
pub fn curve_percent(curve: &[CurvePoint], temperature: u32) -> u8 {
	curve.iter().rev().find(|point| temperature >= point.temperature).map_or(0, |point| point.percent)
}

/// A FAN'S LEVELS, as its driver described them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FanControl {
	/// ACPI 1.0: on (D0) or off (D3hot) by its device power state.
	PowerState,
	/// `_FIF`'s fine-grain control: `_FSL` takes a percentage, in multiples of the step size.
	FineGrain { step: u32 },
	/// `_FPS`'s levels: `_FSL` takes a level's control value. Each level's control value and speed.
	Levels(Vec<(u32, u32)>),
}

/// THE VALUE `set-level` TAKES for `percent` of the fan's range: 1 or 0 for a power-state fan; the percentage rounded up
/// to the step for fine-grain control; for levels, the slowest whose place in the range - by speed, off first - reaches
/// the share asked for, so a fan is never run slower than asked.
pub fn fan_control(control: &FanControl, percent: u8) -> u32 {
	let percent = u32::from(percent.min(100));
	match control {
		FanControl::PowerState => u32::from(percent > 0),
		FanControl::FineGrain { step } => {
			let step = (*step).clamp(1, 100);
			(percent.div_ceil(step) * step).min(100)
		}
		FanControl::Levels(levels) => {
			if levels.is_empty() {
				return 0;
			}
			let mut by_speed: Vec<(u32, u32)> = levels.clone();
			by_speed.sort_by_key(|&(_, speed)| speed);
			let last = (by_speed.len() - 1) as u32;
			let at = (percent * last).div_ceil(100).min(last);
			by_speed[at as usize].0
		}
	}
}

/// What a zone past a critical trip asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Critical {
	Nothing,
	/// Past `_HOT`: hibernation, and the power-off where it is refused.
	Hibernate,
	/// Past `_CRT`: the forced bound armed, then the orderly power-off.
	PowerOff,
}

/// A ZONE'S CRITICAL TRIPS, acted on once per crossing.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Trips {
	hot_acted: bool,
	critical_acted: bool,
}

impl Trips {
	/// One reading against `_CRT` and `_HOT`: `_CRT` first, as it is the graver; each once until the zone is under it
	/// again.
	pub fn reading(&mut self, temperature: u32, critical: Option<u32>, hot: Option<u32>) -> Critical {
		let past_critical = critical.is_some_and(|trip| temperature >= trip);
		let past_hot = hot.is_some_and(|trip| temperature >= trip);
		if !past_critical {
			self.critical_acted = false;
		}
		if !past_hot {
			self.hot_acted = false;
		}
		if past_critical && !self.critical_acted {
			self.critical_acted = true;
			self.hot_acted = true;
			return Critical::PowerOff;
		}
		if past_hot && !self.hot_acted {
			self.hot_acted = true;
			return Critical::Hibernate;
		}
		Critical::Nothing
	}
}

/// CPPC'S ENERGY-PERFORMANCE PREFERENCE for the profile, where `_CPC` names its register: 0 all performance,
/// 128 balanced, 192 leaning to energy - never 255, which some platforms read as "lowest performance always".
pub fn energy_preference(profile: Profile) -> u8 {
	match profile {
		Profile::Performance => 0,
		Profile::Balanced => 128,
		Profile::PowerSaving => 192,
	}
}

/// `_SCP`'s mode for the profile: passive cooling preferred while saving power - the fans quiet, the processors slowed
/// first - and active otherwise.
pub fn cooling_mode(profile: Profile) -> u8 {
	u8::from(profile == Profile::PowerSaving)
}

#[cfg(test)]
mod tests;

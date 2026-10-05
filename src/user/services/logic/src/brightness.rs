//! DISPLAY BRIGHTNESS'S ARITHMETIC AND ITS JOIN - `liber:display@1`'s brightness, as DisplayService keeps it.
//!
//! Two halves, both pure. The SCALE: what a backlight's levels are, the floor no ordinary set goes below, a step, a
//! percent, and an absolute level checked against the scale. And the JOIN: which backlight belongs to which output and
//! why, and which one of those joined to an output is the one that acts - the others shadowed, refused a set and never
//! stepped, so two writers never fight over one panel.
//!
//! And THE POLICY, the brightness policy service's decisions: what a backlight appearing is set to, which changes are
//! stored and when, the idle dim and its undoing, and automatic brightness. The service reads, asks and carries out;
//! everything it decides is here.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

/// A backlight's levels: a discrete list, ascending and without duplicates, or an inclusive range.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Scale {
	Levels(Vec<u32>),
	Range { minimum: u32, maximum: u32 },
}

/// THE FLOOR, IN PERCENT OF THE RANGE: the lowest level at or above it is where every key, firmware hotkey, step and
/// ordinary set stops, because a panel at level zero can be black and a black screen cannot be read to undo it. The
/// owner confirms the number.
pub const FLOOR_PERCENT: u32 = 5;

/// A range steps by this fraction of its span, at least one unit.
pub const RANGE_STEPS: u32 = 16;

impl Scale {
	/// A scale from a driver's description, or `None` for one that cannot be driven: no levels, or a range whose
	/// minimum is past its maximum.
	pub fn new(levels: Vec<u32>) -> Option<Scale> {
		let mut levels = levels;
		levels.sort_unstable();
		levels.dedup();
		(!levels.is_empty()).then_some(Scale::Levels(levels))
	}

	pub fn range(minimum: u32, maximum: u32) -> Option<Scale> {
		(minimum <= maximum).then_some(Scale::Range { minimum, maximum })
	}

	pub fn lowest(&self) -> u32 {
		match self {
			Scale::Levels(levels) => levels[0],
			Scale::Range { minimum, .. } => *minimum,
		}
	}

	pub fn highest(&self) -> u32 {
		match self {
			Scale::Levels(levels) => levels[levels.len() - 1],
			Scale::Range { maximum, .. } => *maximum,
		}
	}

	/// Whether `level` is one this backlight takes.
	pub fn contains(&self, level: u32) -> bool {
		match self {
			Scale::Levels(levels) => levels.binary_search(&level).is_ok(),
			Scale::Range { minimum, maximum } => (*minimum..=*maximum).contains(&level),
		}
	}

	/// The level of the scale nearest to `level`; the lower one on a tie.
	pub fn nearest(&self, level: u32) -> u32 {
		match self {
			Scale::Levels(levels) => {
				let mut best = levels[0];
				for &candidate in levels.iter() {
					if candidate.abs_diff(level) < best.abs_diff(level) {
						best = candidate;
					}
				}
				best
			}
			Scale::Range { minimum, maximum } => level.clamp(*minimum, *maximum),
		}
	}

	/// THE FLOOR: the lowest level at or above `FLOOR_PERCENT` of the span above the lowest level. A one-level scale's
	/// floor is that level.
	pub fn floor(&self) -> u32 {
		let (low, high) = (self.lowest(), self.highest());
		let threshold = low.saturating_add(((u64::from(high - low) * u64::from(FLOOR_PERCENT)).div_ceil(100)) as u32);
		match self {
			Scale::Levels(levels) => levels.iter().copied().find(|&level| level >= threshold).unwrap_or(high),
			Scale::Range { .. } => threshold.min(high),
		}
	}

	/// `steps` steps from `from` - the next listed level each, or a sixteenth of a range's span (at least one unit) -
	/// stopping at either end. A level `from` that is not on the scale starts from the nearest one.
	pub fn step(&self, from: u32, steps: i32) -> u32 {
		let from = self.nearest(from);
		match self {
			Scale::Levels(levels) => {
				let at = levels.binary_search(&from).unwrap_or(0) as i64;
				let to = (at + i64::from(steps)).clamp(0, levels.len() as i64 - 1);
				levels[to as usize]
			}
			Scale::Range { minimum, maximum } => {
				let unit = ((maximum - minimum) / RANGE_STEPS).max(1) as i64;
				(i64::from(from) + unit * i64::from(steps)).clamp(i64::from(*minimum), i64::from(*maximum)) as u32
			}
		}
	}

	/// `percent` of the span, mapped to the nearest level.
	pub fn percent(&self, percent: u32) -> u32 {
		let (low, high) = (self.lowest(), self.highest());
		let raw = u64::from(low) + (u64::from(high - low) * u64::from(percent.min(100)) + 50) / 100;
		self.nearest(raw as u32)
	}
}

/// What a set asks for: an absolute level, a percent of the span, or a signed number of steps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
	Level(u32),
	Percent(u32),
	Steps(i32),
}

/// Why a set is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// An absolute level that is not on the scale, or a percent past a hundred.
	Invalid,
	/// A step from a level nobody knows: the backlight cannot say its level and nothing has set one.
	Unknown,
}

/// THE LEVEL A SET LANDS ON, the floor applied: nothing below it unless `allow_zero`, and then only an explicit ask
/// reaches below - a step or a percent still stops at the floor, because zero is reached on purpose or not at all.
pub fn resolve(scale: &Scale, current: Option<u32>, target: Target, allow_zero: bool) -> Result<u32, Refusal> {
	let floor = scale.floor();
	let level = match target {
		Target::Level(level) => {
			if !scale.contains(level) {
				return Err(Refusal::Invalid);
			}
			if allow_zero {
				return Ok(level);
			}
			level
		}
		Target::Percent(percent) => {
			if percent > 100 {
				return Err(Refusal::Invalid);
			}
			let level = scale.percent(percent);
			if allow_zero && percent == 0 {
				return Ok(scale.lowest());
			}
			level
		}
		Target::Steps(steps) => scale.step(current.ok_or(Refusal::Unknown)?, steps),
	};
	Ok(level.max(floor))
}

/// A firmware hotkey, applied the way a key is: up and down one step, cycle one step up and from the top back to the
/// floor, zero TO THE FLOOR - a key press is not the explicit request zero needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hotkey {
	Up,
	Down,
	Cycle,
	Zero,
}

pub fn hotkey(scale: &Scale, current: Option<u32>, key: Hotkey) -> Option<u32> {
	let floor = scale.floor();
	Some(match key {
		Hotkey::Zero => floor,
		Hotkey::Up => scale.step(current?, 1).max(floor),
		Hotkey::Down => scale.step(current?, -1).max(floor),
		Hotkey::Cycle => {
			let current = current?;
			if scale.nearest(current) >= scale.highest() { floor } else { scale.step(current, 1).max(floor) }
		}
	})
}

/// ONE PRESS, ONE STEP: a brightness key and a firmware hotkey for the same physical press - many laptops send both -
/// arrive within this many ticks of each other in the same direction, and the second is dropped.
pub const SAME_PRESS_TICKS: u64 = 10;

/// Where a step came from, for `SAME_PRESS_TICKS`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
	Key,
	Hotkey,
}

/// The last step taken and when, so a second source's copy of the same press is told apart from a second press.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LastStep {
	at: u64,
	up: bool,
	source: Option<Source>,
}

impl LastStep {
	/// Whether a step `up` from `source` at `now` is a second copy of the last press, and takes it if not.
	pub fn duplicate(&mut self, source: Source, up: bool, now: u64) -> bool {
		let copy = self.source.is_some_and(|last| last != source) && self.up == up && now.saturating_sub(self.at) <= SAME_PRESS_TICKS;
		if !copy {
			*self = LastStep { at: now, up, source: Some(source) };
		}
		copy
	}
}

// ------------------------------------------------------------------------------------------------------------- join

/// A PCI function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Function {
	pub bus: u32,
	pub dev: u32,
	pub func: u32,
}

/// A monitor's EDID identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Monitor {
	pub manufacturer: u16,
	pub product: u16,
	pub serial: u32,
}

/// Where the output's pixels go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputSource {
	/// A `display` provider, by the function of the binding that publishes it.
	Provider(Function),
	/// The boot framebuffer, with the function that decodes it where the kernel found one.
	BootFramebuffer(Option<Function>),
}

impl OutputSource {
	/// The function whose firmware video device can drive this output's panel.
	fn adapter(&self) -> Option<Function> {
		match self {
			OutputSource::Provider(function) => Some(*function),
			OutputSource::BootFramebuffer(decoder) => *decoder,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Output {
	pub source: OutputSource,
	pub edid: Option<Monitor>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Firmware,
	UsbMonitor,
	Native,
}

/// What a backlight belongs to, as its driver read it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BacklightTarget {
	Function(Function),
	Monitor(Monitor),
	None,
}

/// One backlight as the join sees it: its kind, its target, and the binding that publishes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Candidate {
	pub kind: Kind,
	pub target: BacklightTarget,
	pub publisher: Function,
	/// Left out of the join: its provider stopped answering.
	pub failed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
	Native,
	FirmwareAdapter,
	Edid,
	SingleOutput,
}

impl Reason {
	/// Precedence among the backlights joined to one output: the lower acts.
	fn rank(self) -> u8 {
		match self {
			Reason::Native => 0,
			Reason::FirmwareAdapter => 1,
			Reason::Edid => 2,
			Reason::SingleOutput => 3,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standing {
	Active,
	/// Joined to the output another backlight acts on: the index of that one.
	Shadowed(usize),
	Unjoined,
}

/// One candidate's place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Place {
	pub reason: Option<Reason>,
	pub standing: Standing,
}

/// THE JOIN for output 0, run whenever a backlight appears or leaves or the output's source changes: each candidate's
/// reason, if it joins, and its standing. Precedence native, then firmware by adapter, then USB by EDID, then single
/// output; a tie goes to the earlier candidate, which is arrival order. A USB backlight joins by `single-output` only
/// when nothing joined by the other rules and it is the only USB backlight left unjoined.
pub fn join(output: &Output, candidates: &[Candidate]) -> Vec<Place> {
	let mut reasons: Vec<Option<Reason>> = candidates
		.iter()
		.map(|candidate| {
			if candidate.failed {
				return None;
			}
			match (candidate.kind, candidate.target) {
				(Kind::Native, _) if matches!(output.source, OutputSource::Provider(function) if function == candidate.publisher) => Some(Reason::Native),
				(Kind::Firmware, BacklightTarget::Function(function)) if output.source.adapter() == Some(function) => Some(Reason::FirmwareAdapter),
				(Kind::UsbMonitor, BacklightTarget::Monitor(monitor)) if output.edid == Some(monitor) => Some(Reason::Edid),
				_ => None,
			}
		})
		.collect();
	if reasons.iter().all(Option::is_none) {
		let mut usb = candidates.iter().enumerate().filter(|(at, candidate)| !candidate.failed && candidate.kind == Kind::UsbMonitor && reasons[*at].is_none());
		if let (Some((only, _)), None) = (usb.next(), usb.next()) {
			reasons[only] = Some(Reason::SingleOutput);
		}
	}
	let winner = reasons.iter().enumerate().filter_map(|(at, reason)| reason.map(|reason| (reason.rank(), at))).min().map(|(_, at)| at);
	reasons
		.iter()
		.enumerate()
		.map(|(at, reason)| Place {
			reason: *reason,
			standing: match (reason, winner) {
				(None, _) => Standing::Unjoined,
				(Some(_), Some(winner)) if winner == at => Standing::Active,
				(Some(_), Some(winner)) => Standing::Shadowed(winner),
				(Some(_), None) => Standing::Unjoined,
			},
		})
		.collect()
}

// ----------------------------------------------------------------------------------------------------------- policy

/// HOW LONG A LEVEL MUST HOLD BEFORE IT IS STORED, in seconds: ConfigService writes through on every set, and a held
/// key would otherwise write each step.
pub const SETTLE_SECONDS: u64 = 2;

/// IDLE DIMMING'S FRACTION of the level in force, in percent - never below the floor. The owner confirms the number.
pub const DIM_PERCENT: u32 = 30;

/// The idle timeout a machine starts with, in seconds, until someone sets another. The owner confirms the number.
pub const DEFAULT_IDLE_SECONDS: u32 = 300;

/// The level idle dimming takes the backlight to from `level`: the nearest level at `DIM_PERCENT` of it, never below the
/// floor and never above where it was.
pub fn dimmed(scale: &Scale, level: u32) -> u32 {
	let target = (u64::from(level) * u64::from(DIM_PERCENT) / 100) as u32;
	scale.nearest(target).max(scale.floor()).min(level.max(scale.floor()))
}

/// One point of a light response curve: the brightness in percent of the range at an illuminance in lux.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CurvePoint {
	pub percent: u32,
	pub lux: u32,
}

/// The curve automatic brightness follows for a sensor that carries none of its own - a HID sensor: dim rooms dim,
/// daylight full. The owner confirms it.
pub const DEFAULT_CURVE: [CurvePoint; 4] = [CurvePoint { percent: 10, lux: 0 }, CurvePoint { percent: 40, lux: 100 }, CurvePoint { percent: 80, lux: 1000 }, CurvePoint { percent: 100, lux: 10000 }];

/// The brightness, in percent, the curve gives at `milli_lux`: interpolated between the two points around it, the end
/// points held beyond them. An empty curve is the default one.
pub fn curve_percent(curve: &[CurvePoint], milli_lux: u64) -> u32 {
	let curve = if curve.is_empty() { &DEFAULT_CURVE[..] } else { curve };
	let lux = milli_lux / 1000;
	let first = curve[0];
	if lux <= u64::from(first.lux) {
		return first.percent.min(100);
	}
	for pair in curve.windows(2) {
		let (low, high) = (pair[0], pair[1]);
		if lux <= u64::from(high.lux) {
			let span = u64::from(high.lux - low.lux).max(1);
			let into = lux - u64::from(low.lux);
			let percent = i64::from(low.percent) + (i64::from(high.percent) - i64::from(low.percent)) * into as i64 / span as i64;
			return (percent.clamp(0, 100)) as u32;
		}
	}
	curve[curve.len() - 1].percent.min(100)
}

/// AUTOMATIC BRIGHTNESS MOVES ONLY PAST THIS MANY POINTS, so a reading hovering around a curve's step does not flicker
/// the panel between two levels.
pub const HYSTERESIS_PERCENT: u32 = 5;

/// Whether automatic brightness should move from `current` percent to `target`.
pub fn worth_moving(current: Option<u32>, target: u32) -> bool {
	current.is_none_or(|current| current.abs_diff(target) >= HYSTERESIS_PERCENT)
}

// ----------------------------------------------------------------------------------------------------------- settings

/// Where the policy's settings and stored levels live in ConfigService's tree.
pub const AUTO_KEY: &str = "display.brightness.auto";
pub const IDLE_KEY: &str = "display.brightness.idle";
pub const LEVEL_PREFIX: &str = "display.brightness.level.";

/// The policy's two settings: automatic brightness, and the idle timeout after which the active backlight is dimmed -
/// `None` for never.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
	pub automatic: bool,
	pub idle_seconds: Option<u32>,
}

/// AUTOMATIC BRIGHTNESS STARTS OFF, and idle dimming on at `DEFAULT_IDLE_SECONDS`. The owner confirms both.
impl Default for Settings {
	fn default() -> Settings {
		Settings { automatic: false, idle_seconds: Some(DEFAULT_IDLE_SECONDS) }
	}
}

/// The stored text of `display.brightness.auto`: `on` or `off`.
pub fn parse_automatic(text: &str) -> Option<bool> {
	match text.trim() {
		"on" | "true" | "1" => Some(true),
		"off" | "false" | "0" => Some(false),
		_ => None,
	}
}

/// The stored text of `display.brightness.idle`: a number of seconds, at least one, or `off`.
pub fn parse_idle(text: &str) -> Option<Option<u32>> {
	match text.trim() {
		"off" | "never" => Some(None),
		number => number.parse::<u32>().ok().filter(|&seconds| seconds > 0).map(Some),
	}
}

pub fn automatic_text(on: bool) -> &'static str {
	if on { "on" } else { "off" }
}

pub fn idle_text(seconds: Option<u32>) -> String {
	match seconds {
		Some(seconds) => format!("{seconds}"),
		None => String::from("off"),
	}
}

/// A stored level's key: the backlight's stable key under `LEVEL_PREFIX`.
pub fn level_key(backlight: &str) -> String {
	format!("{LEVEL_PREFIX}{backlight}")
}

pub fn parse_level(text: &str) -> Option<u32> {
	text.trim().parse::<u32>().ok()
}

// ----------------------------------------------------------------------------------------------------------- appearing

/// A BACKLIGHT AS THE POLICY SEES IT when it appears: its standing, its scale, whether anything has moved its level
/// since it appeared, the firmware's defaults, and the level ConfigService holds for its key.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Appearing {
	pub standing: Standing,
	pub scale: Scale,
	pub level: Option<u32>,
	pub touched: bool,
	pub ac_default: Option<u32>,
	pub battery_default: Option<u32>,
	pub stored: Option<u32>,
}

/// THE LEVEL A BACKLIGHT APPEARING IS SET TO, or none. Nothing for one anything has moved since it appeared, and
/// nothing for a shadowed one. Its STORED level - an active or an unjoined backlight alike - snapped to the nearest level
/// it has now and raised to the floor. Where nothing is stored, and only for the active backlight, the firmware's battery
/// default on battery and its AC default otherwise, the same way; without either, the level it came up with stands.
pub fn first_level(backlight: &Appearing, on_battery: bool) -> Option<u32> {
	if backlight.touched || matches!(backlight.standing, Standing::Shadowed(_)) {
		return None;
	}
	let wanted = match backlight.stored {
		Some(stored) => stored,
		None if backlight.standing == Standing::Active => {
			let default = if on_battery { backlight.battery_default.or(backlight.ac_default) } else { backlight.ac_default.or(backlight.battery_default) };
			default?
		}
		None => return None,
	};
	let level = backlight.scale.nearest(wanted).max(backlight.scale.floor());
	(backlight.level != Some(level)).then_some(level)
}

// ----------------------------------------------------------------------------------------------------------- the policy

/// What the policy asks for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
	/// Set this backlight to this level; the serial the set answers is the policy's own.
	Set { key: String, level: u32 },
	/// Set this backlight to this percent of its range, the same way.
	Percent { key: String, percent: u32 },
	/// Write this level to the tree under the backlight's key.
	Store { key: String, level: u32 },
}

/// THE ACTIVE BACKLIGHT as an idle edge or a reading finds it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ActiveBacklight<'a> {
	pub key: &'a str,
	pub scale: &'a Scale,
	pub level: Option<u32>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Pending {
	key: String,
	level: u32,
	due: u64,
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Dim {
	key: String,
	before: u32,
	to: u32,
}

/// How many of its own serials the policy remembers: a set's change is read within a few frames of its answer.
const OWN_SERIALS: usize = 32;

/// THE POLICY'S STATE: its settings, the serials of the sets it made, the levels waiting to settle, the dim in force, and
/// automatic brightness's pause and last level.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Policy {
	pub settings: Settings,
	own: Vec<u64>,
	pending: Vec<Pending>,
	dim: Option<Dim>,
	paused: bool,
	automatic_percent: Option<u32>,
}

impl Policy {
	pub fn new(settings: Settings) -> Policy {
		Policy { settings, ..Policy::default() }
	}

	/// A SET THE POLICY MADE answered this serial: its change is the policy's own, and neither stored nor taken as
	/// someone's hand on the level.
	pub fn answered(&mut self, serial: u64) {
		if self.own.len() == OWN_SERIALS {
			self.own.remove(0);
		}
		self.own.push(serial);
	}

	/// A CHANGE OF A LEVEL, read from DisplayService's subscription. The policy's own is forgotten. Anyone else's - a
	/// key, a hotkey, the device, a set through the policy's control - waits `settle_ticks` to be stored, cancels a dim
	/// of that backlight, and pauses automatic brightness until the next idle.
	pub fn changed(&mut self, key: &str, level: Option<u32>, serial: u64, now: u64, settle_ticks: u64) {
		if let Some(at) = self.own.iter().position(|&own| own == serial) {
			self.own.remove(at);
			return;
		}
		if self.dim.as_ref().is_some_and(|dim| dim.key == key) {
			self.dim = None;
		}
		self.paused = true;
		self.automatic_percent = None;
		self.pending.retain(|pending| pending.key != key);
		if let Some(level) = level {
			self.pending.push(Pending { key: String::from(key), level, due: now.saturating_add(settle_ticks) });
		}
	}

	/// A backlight left: nothing of it waits or is dimmed.
	pub fn left(&mut self, key: &str) {
		self.pending.retain(|pending| pending.key != key);
		if self.dim.as_ref().is_some_and(|dim| dim.key == key) {
			self.dim = None;
		}
	}

	/// When the next level settles, if one waits.
	pub fn next_due(&self) -> Option<u64> {
		self.pending.iter().map(|pending| pending.due).min()
	}

	/// THE LEVELS STILL FOR LONG ENOUGH, to be stored.
	pub fn settled(&mut self, now: u64) -> Vec<Action> {
		let mut out = Vec::new();
		self.pending.retain(|pending| {
			if pending.due <= now {
				out.push(Action::Store { key: pending.key.clone(), level: pending.level });
				false
			} else {
				true
			}
		});
		out
	}

	/// IDLE: the active backlight dimmed to `DIM_PERCENT` of its level, never below the floor - nothing when that is no
	/// lower - and automatic brightness's pause ends.
	pub fn idle(&mut self, active: Option<ActiveBacklight<'_>>) -> Option<Action> {
		self.paused = false;
		if self.settings.idle_seconds.is_none() || self.dim.is_some() {
			return None;
		}
		let active = active?;
		let level = active.level?;
		let to = dimmed(active.scale, level);
		if to >= level {
			return None;
		}
		self.dim = Some(Dim { key: String::from(active.key), before: level, to });
		Some(Action::Set { key: String::from(active.key), level: to })
	}

	/// ACTIVE: the dim undone - unless someone moved the level meanwhile, which cancelled it.
	pub fn active(&mut self) -> Option<Action> {
		let dim = self.dim.take()?;
		Some(Action::Set { key: dim.key, level: dim.before })
	}

	/// Whether a dim is in force.
	pub fn dimmed(&self) -> bool {
		self.dim.is_some()
	}

	/// A READING: with automatic brightness on, not paused and nothing dimmed, the active backlight follows the curve -
	/// moved only past the hysteresis.
	pub fn illuminance(&mut self, active: Option<ActiveBacklight<'_>>, milli_lux: u64, curve: &[CurvePoint]) -> Option<Action> {
		if !self.settings.automatic || self.paused || self.dim.is_some() {
			return None;
		}
		let active = active?;
		let percent = curve_percent(curve, milli_lux);
		if !worth_moving(self.automatic_percent, percent) {
			return None;
		}
		self.automatic_percent = Some(percent);
		Some(Action::Percent { key: String::from(active.key), percent })
	}

	/// AUTOMATIC BRIGHTNESS TURNED ON OR OFF: on starts unpaused, at the next reading.
	pub fn set_automatic(&mut self, on: bool) {
		self.settings.automatic = on;
		self.paused = false;
		self.automatic_percent = None;
	}

	/// THE IDLE TIMEOUT CHANGED: off undoes a dim in force.
	pub fn set_idle(&mut self, seconds: Option<u32>) -> Option<Action> {
		self.settings.idle_seconds = seconds;
		if seconds.is_none() { self.active() } else { None }
	}
}

/// `_ALR`'s CURVE AS THE POLICY FOLLOWS IT. ACPI gives each point's adjustment in percent of normal, and a firmware may
/// list more than a hundred for daylight; the policy takes it as percent of the backlight's range, the brightest at the
/// top of it.
pub fn firmware_curve(points: impl Iterator<Item = (u32, u32)>) -> Vec<CurvePoint> {
	let mut curve: Vec<CurvePoint> = points.map(|(adjustment, lux)| CurvePoint { percent: adjustment.min(100), lux }).collect();
	curve.sort_by_key(|point| point.lux);
	curve.dedup_by_key(|point| point.lux);
	curve
}

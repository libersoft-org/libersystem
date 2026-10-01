// THE ACPI FAN - the parts of the `acpi_fan` driver a host can test: ACPI 4.0's `_FIF` (whether the platform leaves the
// fan's speed to the operating system in fine steps), `_FPS` (its levels) and `_FST` (its state), read off the node
// channel's values as the specification lays them out (ACPI 6.5, 11.3.1). An ACPI 1.0 fan has none of them and is
// switched by its device power state. A result of the wrong shape is refused by name.

use alloc::vec::Vec;
use aml::wire::Value;

/// Why a result is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Not a package, or a package too short for what it is.
	Shape(&'static str),
	/// An element that should be an integer is not one.
	NotInteger(&'static str),
}

/// The most `_FPS` levels a fan is described with.
pub const MAX_LEVELS: usize = 16;

fn package<'a>(value: &'a Value, what: &'static str, least: usize) -> Result<&'a [Value], Refusal> {
	match value {
		Value::Package(elements) if elements.len() >= least => Ok(elements),
		_ => Err(Refusal::Shape(what)),
	}
}

// A 32-bit field; `0xFFFFFFFF` is the specification's "not known", kept as it is.
fn dword(value: &Value, what: &'static str) -> Result<u32, Refusal> {
	match value {
		Value::Integer(raw) => Ok(u32::try_from(*raw).unwrap_or(u32::MAX)),
		_ => Err(Refusal::NotInteger(what)),
	}
}

/// `_FIF`: `{ Revision, FineGrainControl, StepSize, LowSpeedNotificationSupport }`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Interface {
	/// `_FSL` takes a percentage, not a level's control value.
	pub fine_grain: bool,
	/// The percentage steps fine-grain control moves in, 1 to 9.
	pub step: u32,
}

pub fn fif(value: &Value) -> Result<Interface, Refusal> {
	let elements = package(value, "_FIF", 3)?;
	Ok(Interface { fine_grain: dword(&elements[1], "_FIF's fine-grain control")? != 0, step: dword(&elements[2], "_FIF's step size")?.clamp(1, 9) })
}

/// ONE `_FPS` LEVEL: `{ Control, TripPoint, Speed, NoiseLevel, Power }`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Level {
	pub control: u32,
	pub speed_rpm: u32,
	pub noise: u32,
	pub power_mw: u32,
}

/// `_FPS`: `{ Revision, Level, ... }`, at most `MAX_LEVELS` read.
pub fn fps(value: &Value) -> Result<Vec<Level>, Refusal> {
	let elements = package(value, "_FPS", 2)?;
	let mut out = Vec::new();
	for level in elements[1..].iter().take(MAX_LEVELS) {
		let fields = package(level, "an _FPS level", 5)?;
		let known = |raw: u32| if raw == u32::MAX { 0 } else { raw };
		out.push(Level { control: dword(&fields[0], "an _FPS level's control")?, speed_rpm: known(dword(&fields[2], "an _FPS level's speed")?), noise: known(dword(&fields[3], "an _FPS level's noise")?), power_mw: known(dword(&fields[4], "an _FPS level's power")?) });
	}
	Ok(out)
}

/// `_FST`: `{ Revision, Control, Speed }` - the speed zero where the fan does not say.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct State {
	pub control: u32,
	pub speed_rpm: u32,
}

pub fn fst(value: &Value) -> Result<State, Refusal> {
	let elements = package(value, "_FST", 3)?;
	let speed = dword(&elements[2], "_FST's speed")?;
	Ok(State { control: dword(&elements[1], "_FST's control")?, speed_rpm: if speed == u32::MAX { 0 } else { speed } })
}

#[cfg(test)]
mod tests;

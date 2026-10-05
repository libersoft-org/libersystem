// ACPI'S VIDEO EXTENSION AND ITS LIGHT SENSOR - the parts of the `acpi_backlight` and `acpi_als` drivers a host can test:
// `_BCL` normalised into the firmware's two defaults and its levels, `_BQC`'s answer snapped to them, the output node's
// notification map, the firmware that steps a level itself as well as notifying, and `ACPI0008`'s `_ALI` and `_ALR`.
// Read off the node channel's values as the specification lays them out (ACPI 6.5, B.6 and 9.2); a result of the wrong
// shape is refused by name.

use alloc::vec::Vec;
use aml::wire::Value;

/// Why a result is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Not a package, or a package too short for what it is.
	Shape(&'static str),
	/// An element that should be an integer is not one.
	NotInteger(&'static str),
	/// `_BCL` names fewer than two levels a backlight can be set to: a firmware defect, and nothing is published.
	TooFewLevels,
}

/// THE VALUE `_DOS` IS EVALUATED WITH at bind and at every resume: bit 2 set, so the firmware changes no brightness on a
/// hotkey and only notifies; bits 1:0 clear, so an output switch is notified rather than performed behind the system.
pub const DOS_NOTIFY_ONLY: u64 = 0x04;

/// The most `_BCL` levels read.
pub const MAX_LEVELS: usize = 101;

fn integer(value: &Value, what: &'static str) -> Result<u64, Refusal> {
	match value {
		Value::Integer(raw) => Ok(*raw),
		_ => Err(Refusal::NotInteger(what)),
	}
}

/// `_BCL`, NORMALISED: the AC and battery defaults - the first two entries, kept where they are a percentage - and the
/// levels, the rest sorted and deduplicated, every value above 100 dropped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Levels {
	pub ac_default: Option<u32>,
	pub battery_default: Option<u32>,
	pub levels: Vec<u32>,
}

pub fn bcl(value: &Value) -> Result<Levels, Refusal> {
	let Value::Package(elements) = value else { return Err(Refusal::Shape("_BCL")) };
	if elements.len() < 2 {
		return Err(Refusal::Shape("_BCL"));
	}
	let percent = |raw: u64| (raw <= 100).then_some(raw as u32);
	let ac_default = percent(integer(&elements[0], "_BCL's AC default")?);
	let battery_default = percent(integer(&elements[1], "_BCL's battery default")?);
	let mut levels: Vec<u32> = Vec::new();
	for element in elements[2..].iter().take(MAX_LEVELS) {
		if let Some(level) = percent(integer(element, "a _BCL level")?) {
			levels.push(level);
		}
	}
	levels.sort_unstable();
	levels.dedup();
	if levels.len() < 2 {
		return Err(Refusal::TooFewLevels);
	}
	Ok(Levels { ac_default, battery_default, levels })
}

/// `_BQC`'S ANSWER AS A LEVEL: itself where it is listed, the nearest listed level otherwise - and whether it had to be
/// snapped, which the driver logs once.
pub fn bqc(levels: &[u32], value: u64) -> (u32, bool) {
	let value = value.min(u64::from(u32::MAX)) as u32;
	if levels.contains(&value) {
		return (value, false);
	}
	let mut best = levels.first().copied().unwrap_or(0);
	for &level in levels {
		if level.abs_diff(value) < best.abs_diff(value) {
			best = level;
		}
	}
	(best, true)
}

/// A FIRMWARE HOTKEY, as the output node notified it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hotkey {
	Cycle,
	Up,
	Down,
	Zero,
}

/// What one `Notify` on the output node means: the whole map, 0x85 to 0x89, and anything else.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Notification {
	Hotkey(Hotkey),
	/// 0x89: display off - no level changes; display power is not the backlight's.
	DisplayOff,
	Other(u64),
}

pub fn notification(value: u64) -> Notification {
	match value {
		0x85 => Notification::Hotkey(Hotkey::Cycle),
		0x86 => Notification::Hotkey(Hotkey::Up),
		0x87 => Notification::Hotkey(Hotkey::Down),
		0x88 => Notification::Hotkey(Hotkey::Zero),
		0x89 => Notification::DisplayOff,
		other => Notification::Other(other),
	}
}

/// FIRMWARE THAT STEPS ANYWAY: before a hotkey is forwarded, `_BQC` is read, and a level that already moved in the key's
/// direction since the last one set is the firmware's own change - one press, one step - and reported as one. Cycle goes
/// up, or from the top to the bottom; zero is never the firmware's doing.
pub fn already_moved(last: Option<u32>, now: Option<u32>, key: Hotkey) -> bool {
	let (Some(last), Some(now)) = (last, now) else { return false };
	match key {
		Hotkey::Up => now > last,
		Hotkey::Down => now < last,
		Hotkey::Cycle => now != last,
		Hotkey::Zero => false,
	}
}

/// `_ALI`: the illuminance in lux, as thousandths. `0xFFFFFFFF` is the specification's "not known".
pub fn ali(value: &Value) -> Result<Option<u64>, Refusal> {
	let raw = integer(value, "_ALI")?;
	Ok((raw != u64::from(u32::MAX)).then(|| raw.saturating_mul(1000)))
}

/// ONE `_ALR` POINT: the display adjustment in percent of normal, at an illuminance in lux.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Response {
	pub adjustment: u32,
	pub illuminance: u32,
}

/// The most `_ALR` points read.
pub const MAX_RESPONSES: usize = 32;

/// `_ALR`: `{ { Adjustment, Illuminance }, ... }`, ascending by illuminance as the specification requires - sorted here
/// rather than trusted.
pub fn alr(value: &Value) -> Result<Vec<Response>, Refusal> {
	let Value::Package(points) = value else { return Err(Refusal::Shape("_ALR")) };
	let mut out = Vec::new();
	for point in points.iter().take(MAX_RESPONSES) {
		let Value::Package(pair) = point else { return Err(Refusal::Shape("an _ALR point")) };
		if pair.len() < 2 {
			return Err(Refusal::Shape("an _ALR point"));
		}
		let clamp = |raw: u64| raw.min(u64::from(u32::MAX)) as u32;
		out.push(Response { adjustment: clamp(integer(&pair[0], "an _ALR adjustment")?), illuminance: clamp(integer(&pair[1], "an _ALR illuminance")?) });
	}
	out.sort_by_key(|point| point.illuminance);
	Ok(out)
}

/// THE POLLING INTERVAL `_ALP` names, in tenths of a second, as ticks: none for zero, which means the firmware notifies.
pub fn alp_ticks(value: &Value, ticks_per_second: u64) -> Result<Option<u64>, Refusal> {
	let tenths = integer(value, "_ALP")?;
	Ok((tenths != 0).then(|| (tenths.saturating_mul(ticks_per_second) / 10).max(1)))
}

/// What one `Notify` on the light sensor means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SensorNotification {
	/// 0x80: the illuminance changed.
	Illuminance,
	/// 0x82: the response curve changed.
	Response,
	Other(u64),
}

pub fn sensor_notification(value: u64) -> SensorNotification {
	match value {
		0x80 => SensorNotification::Illuminance,
		0x82 => SensorNotification::Response,
		other => SensorNotification::Other(other),
	}
}

#[cfg(test)]
mod tests;

//! IPMI SENSORS, FROM THEIR SDR AND THEIR READINGS: the linear conversion a full sensor record declares, applied to a
//! reading - for every threshold sensor, so there is one conversion - and a temperature sensor as a thermal zone.
//!
//! THE CONVERSION (IPMI 2.0, 36.3): y = (M x + B 10^Bexp) 10^Rexp, in the record's unit, where x is the raw reading
//! read as the record's analog data format says - unsigned, one's complement or two's complement - and M and B are
//! ten-bit signed factors. It is computed in wide checked integers, scaled by a power of ten the caller names, and
//! truncated toward zero once at the end. A NON-LINEAR RECORD (a linearization formula, or 0x70 and above) and a
//! sensor with no analog reading are UNSUPPORTED here: the formulas are the BMC's, and a value invented from the wrong
//! one is worse than none. "Reading unavailable" and "scanning disabled" are UNKNOWN.
//!
//! A TEMPERATURE: degrees C, F or K (base units 1, 2 and 3) to millidegrees Celsius; the upper non-critical, critical
//! and non-recoverable thresholds as the zone's passive, hot and critical trip points; `over-temperature` REPORTED from
//! the BMC's own comparison of the reading with its upper critical threshold, or derived from the converted values when
//! the BMC did not report one. Trip points are observations: the BMC enforces its own thresholds.

use alloc::vec::Vec;

use crate::canon::{at_or_above, derived, reported, temperature, trip, unmeasured};
use crate::convert::Tagged;
use crate::schema::{AlarmKind, InvalidReason, SourceKind, SourceState, TemperatureReference, TripKind, Tristate};

/// How a record says its raw readings are signed - the top two bits of Sensor Units 1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
	Unsigned,
	OnesComplement,
	TwosComplement,
	/// The sensor has no analog reading.
	None,
}

impl Format {
	pub const fn from_units(units1: u8) -> Format {
		match units1 >> 6 {
			0 => Format::Unsigned,
			1 => Format::OnesComplement,
			2 => Format::TwosComplement,
			_ => Format::None,
		}
	}
}

/// A full sensor record's conversion factors, as the record stores them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Linear {
	/// Ten-bit signed.
	pub m: i16,
	/// Ten-bit signed.
	pub b: i16,
	/// Four-bit signed.
	pub b_exp: i8,
	/// Four-bit signed.
	pub r_exp: i8,
	/// The record's linearization byte: 0 is linear.
	pub linearization: u8,
	pub format: Format,
}

/// Sign-extend a `bits`-wide two's-complement field.
pub const fn signed(value: u16, bits: u32) -> i16 {
	let shift = 16 - bits;
	((value << shift) as i16) >> shift
}

impl Linear {
	/// The factors from a full sensor record's bytes 24 through 30 (1-based: linearization, M, M and tolerance, B, B and
	/// accuracy, accuracy, the exponents), and its Sensor Units 1 byte.
	pub fn from_record(units1: u8, factors: &[u8; 7]) -> Linear {
		let m = signed(u16::from(factors[1]) | (u16::from(factors[2] >> 6) << 8), 10);
		let b = signed(u16::from(factors[3]) | (u16::from(factors[4] >> 6) << 8), 10);
		let r_exp = signed(u16::from(factors[6] >> 4), 4) as i8;
		let b_exp = signed(u16::from(factors[6] & 0x0F), 4) as i8;
		Linear { m, b, b_exp, r_exp, linearization: factors[0] & 0x7F, format: Format::from_units(units1) }
	}

	/// The raw reading as a signed integer, by the record's format.
	pub fn raw(&self, raw: u8) -> Option<i64> {
		match self.format {
			Format::Unsigned => Some(i64::from(raw)),
			// One's complement: a set top bit is the negation of the bits' complement.
			Format::OnesComplement => Some(if raw & 0x80 != 0 { -i64::from(!raw) } else { i64::from(raw) }),
			Format::TwosComplement => Some(i64::from(raw as i8)),
			Format::None => None,
		}
	}

	/// THE CONVERSION, scaled by 10^`scale` - 3 for milli-units: y 10^scale, truncated toward zero once.
	pub fn value(&self, raw: u8, scale: i32) -> Tagged<i64> {
		if self.linearization != 0 {
			return Tagged::Unsupported;
		}
		let Some(x) = self.raw(raw) else { return Tagged::Unsupported };
		// (M x + B 10^Bexp) 10^(Rexp + scale). With Bexp negative both terms are multiplied by 10^-Bexp and the
		// exponent reduced by as much, so every intermediate is an integer.
		let low = i32::from(self.b_exp).min(0);
		let term_m = pow10(-low).and_then(|factor| i128::from(self.m).checked_mul(i128::from(x))?.checked_mul(factor));
		let term_b = pow10(i32::from(self.b_exp) - low).and_then(|factor| i128::from(self.b).checked_mul(factor));
		let Some(sum) = term_m.zip(term_b).and_then(|(m, b)| m.checked_add(b)) else { return Tagged::Invalid(InvalidReason::Overflow) };
		let exponent = i32::from(self.r_exp) + scale + low;
		let scaled = if exponent >= 0 { pow10(exponent).and_then(|factor| sum.checked_mul(factor)) } else { pow10(-exponent).map(|divisor| sum / divisor) };
		match scaled.and_then(|value| i64::try_from(value).ok()) {
			Some(value) => Tagged::Known(value),
			None => Tagged::Invalid(InvalidReason::Overflow),
		}
	}
}

fn pow10(exponent: i32) -> Option<i128> {
	if !(0..=30).contains(&exponent) {
		return None;
	}
	Some(10i128.pow(exponent as u32))
}

/// The temperature units IPMI names: base units 1, 2 and 3.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TemperatureUnit {
	Celsius,
	Fahrenheit,
	Kelvin,
}

pub const fn temperature_unit(base_unit: u8) -> Option<TemperatureUnit> {
	match base_unit {
		1 => Some(TemperatureUnit::Celsius),
		2 => Some(TemperatureUnit::Fahrenheit),
		3 => Some(TemperatureUnit::Kelvin),
		_ => None,
	}
}

/// A reading in the record's temperature unit, to millidegrees Celsius.
pub fn millidegrees(linear: &Linear, unit: TemperatureUnit, raw: u8) -> Tagged<i64> {
	match unit {
		TemperatureUnit::Celsius => linear.value(raw, 3),
		// (F - 32) 5 / 9, from the value in millidegrees Fahrenheit.
		TemperatureUnit::Fahrenheit => match linear.value(raw, 3) {
			Tagged::Known(milli) => match milli.checked_sub(32_000).and_then(|above| above.checked_mul(5)) {
				Some(scaled) => Tagged::Known(scaled / 9),
				None => Tagged::Invalid(InvalidReason::Overflow),
			},
			other => other,
		},
		TemperatureUnit::Kelvin => match linear.value(raw, 3) {
			Tagged::Known(milli) => milli.checked_sub(273_150).map_or(Tagged::Invalid(InvalidReason::Overflow), Tagged::Known),
			other => other,
		},
	}
}

/// A sensor's reading, from Get Sensor Reading: the raw value, whether the BMC says it is not a reading at all, and
/// its threshold comparison when it sent one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reading {
	pub raw: u8,
	pub unavailable: bool,
	pub scanning_disabled: bool,
	/// The comparison byte: bit 4 at or above upper critical, bit 5 at or above upper non-recoverable.
	pub comparison: Option<u8>,
}

/// Decode Get Sensor Reading's data (after the completion code).
pub fn reading(data: &[u8]) -> Option<Reading> {
	if data.len() < 2 {
		return None;
	}
	// BIT 6 CLEAR IS SCANNING DISABLED and bit 5 set is a reading or state unavailable.
	Some(Reading { raw: data[0], unavailable: data[1] & 0x20 != 0, scanning_disabled: data[1] & 0x40 == 0, comparison: data.get(2).copied() })
}

impl Reading {
	/// The reading as a value, or `Unknown` when the BMC says it is not one.
	pub fn value(&self, convert: impl FnOnce(u8) -> Tagged<i64>) -> Tagged<i64> {
		if self.unavailable || self.scanning_disabled {
			return Tagged::Unknown;
		}
		convert(self.raw)
	}
}

/// A temperature sensor as the SDR describes it and as it last read: its conversion, its unit, its readable upper
/// thresholds as raw values, and the reading.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TemperatureSensor {
	pub linear: Linear,
	/// The record's base unit.
	pub base_unit: u8,
	pub upper_non_critical: Option<u8>,
	pub upper_critical: Option<u8>,
	pub upper_non_recoverable: Option<u8>,
	/// None before the first reading.
	pub reading: Option<Reading>,
}

/// THE THERMAL ZONE a temperature sensor is.
pub fn thermal(sensor: &TemperatureSensor) -> SourceState {
	let mut state = unmeasured(SourceKind::ThermalZone);
	state.present = Tristate::Yes;
	let Some(unit) = temperature_unit(sensor.base_unit) else {
		state.temperature = temperature(TemperatureReference::Absolute, Tagged::Invalid(InvalidReason::Unit));
		return state;
	};
	let convert = |raw: u8| millidegrees(&sensor.linear, unit, raw);
	let value = match sensor.reading {
		Some(reading) => reading.value(convert),
		None => Tagged::Unknown,
	};
	state.temperature = temperature(TemperatureReference::Absolute, value);
	let mut trips = Vec::new();
	for (kind, raw) in [(TripKind::Passive, sensor.upper_non_critical), (TripKind::Hot, sensor.upper_critical), (TripKind::Critical, sensor.upper_non_recoverable)] {
		if let Some(raw) = raw {
			trips.push(trip(kind, 0, temperature(TemperatureReference::Absolute, convert(raw))));
		}
	}
	// OVER-TEMPERATURE, AS THE BMC COMPARED IT when it said, and derived from the converted values when it did not.
	let comparison = sensor.reading.filter(|reading| !reading.unavailable && !reading.scanning_disabled).and_then(|reading| reading.comparison);
	match (comparison, trips.iter().find(|candidate| candidate.kind == TripKind::Hot)) {
		(Some(bits), _) if sensor.upper_critical.is_some() => state.alarms.push(reported(AlarmKind::OverTemperature, bits & 0x30 != 0)),
		(_, Some(hot)) => state.alarms.push(derived(AlarmKind::OverTemperature, at_or_above(&state.temperature, &hot.temperature))),
		_ => {}
	}
	state.trips = trips;
	state
}

#[cfg(test)]
mod tests;

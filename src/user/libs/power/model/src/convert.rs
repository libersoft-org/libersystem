//! UNIT ARITHMETIC: a source's number, in the source's unit, to a tagged canonical value.
//!
//! Two families. ACPI's power-source fields are milli-units in a declared range with one sentinel for
//! "unknown", and its temperatures are tenths of a kelvin, absolute or relative to the zone's critical
//! trip. HID's are logical values in a field of some width and signedness, whose Unit item names an
//! SI-linear dimension and whose Unit Exponent a power of ten. Nothing here parses a descriptor or a
//! package: the producer has already found the number and says what it is.

use crate::schema::{ChargeState, InvalidReason, Quantity};

/// A tagged measurement. Every canonical value is one of these before it is a wire record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tagged<T> {
	Known(T),
	Unknown,
	Unsupported,
	Invalid(InvalidReason),
}

impl<T> Tagged<T> {
	pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Tagged<U> {
		match self {
			Tagged::Known(value) => Tagged::Known(f(value)),
			Tagged::Unknown => Tagged::Unknown,
			Tagged::Unsupported => Tagged::Unsupported,
			Tagged::Invalid(reason) => Tagged::Invalid(reason),
		}
	}

	pub fn known(self) -> Option<T> {
		match self {
			Tagged::Known(value) => Some(value),
			_ => None,
		}
	}
}

// ------------------------------------------------------------------ ACPI

/// ACPI's "unknown" in a 32-bit power-source field, honoured before any arithmetic so it can never
/// become 4294967295000 of anything.
pub const ACPI_UNKNOWN: u32 = 0xffff_ffff;

/// The largest value ACPI defines for a capacity, a rate or a voltage. Above it and below the sentinel
/// is not a measurement.
pub const ACPI_MAX: u32 = 0x7fff_ffff;

/// The quantity `_BIF`/`_BIX` declares with its power unit: 0 is milliwatts and milliwatt-hours, 1 is
/// milliamps and milliamp-hours. Anything else declares no unit, and nothing measured in it converts.
pub const fn acpi_quantity(power_unit: u32) -> Option<Quantity> {
	match power_unit {
		0 => Some(Quantity::Energy),
		1 => Some(Quantity::Charge),
		_ => None,
	}
}

/// A capacity, a rate magnitude or a voltage, from its milli-unit to its micro-unit.
pub fn acpi_milli(raw: u32) -> Tagged<u64> {
	if raw == ACPI_UNKNOWN {
		return Tagged::Unknown;
	}
	if raw > ACPI_MAX {
		return Tagged::Invalid(InvalidReason::Range);
	}
	match (raw as u64).checked_mul(1000) {
		Some(micro) => Tagged::Known(micro),
		None => Tagged::Invalid(InvalidReason::Overflow),
	}
}

/// The charge state in `_BST`'s state word: bit 0 discharging, bit 1 charging. BOTH AT ONCE IS A
/// CONTRADICTION and not a state - the specification says the two are mutually exclusive, and picking
/// either would be inventing which.
pub const fn acpi_charge_state(state: u32) -> ChargeState {
	match state & 0b11 {
		0b00 => ChargeState::Idle,
		0b01 => ChargeState::Discharging,
		0b10 => ChargeState::Charging,
		_ => ChargeState::Invalid,
	}
}

/// `_BST`'s present rate, SIGNED BY THE CHARGE STATE: positive into storage, negative out. It is power
/// or current according to the battery's power unit, which the caller decides - the magnitude
/// arithmetic is the same. A nonzero rate on an idle battery has no usable direction and stays
/// unknown in signed form; it does not become a positive number.
pub fn acpi_rate(raw: u32, state: u32) -> Tagged<i64> {
	if raw == ACPI_UNKNOWN {
		return Tagged::Unknown;
	}
	let magnitude = match acpi_milli(raw) {
		Tagged::Known(micro) => micro as i64,
		other => return other.map(|_| 0),
	};
	match acpi_charge_state(state) {
		ChargeState::Charging => Tagged::Known(magnitude),
		ChargeState::Discharging => Tagged::Known(-magnitude),
		ChargeState::Invalid => Tagged::Invalid(InvalidReason::Contradiction),
		ChargeState::Idle | ChargeState::Unknown => {
			if magnitude == 0 {
				Tagged::Known(0)
			} else {
				Tagged::Unknown
			}
		}
	}
}

/// A thermal reading in tenths of a kelvin, and whether the zone's `_RTV` made it relative.
///
/// ABSOLUTE: millidegrees Celsius are `raw * 100 - 273150`, in checked signed arithmetic. The unknown
/// sentinel is honoured first, as it is for every other ACPI field.
///
/// RELATIVE: the value is a signed offset from the zone's critical trip, in the same tenths, scaled by
/// a hundred into millidegrees - and NO absolute Celsius is invented for it. Every bit pattern is an
/// offset here, the all-ones one included: it is minus a tenth.
pub fn acpi_temperature(raw: u32, relative: bool) -> Tagged<i64> {
	if relative {
		return match (raw as i32 as i64).checked_mul(100) {
			Some(offset) => Tagged::Known(offset),
			None => Tagged::Invalid(InvalidReason::Overflow),
		};
	}
	if raw == ACPI_UNKNOWN {
		return Tagged::Unknown;
	}
	match (raw as i64).checked_mul(100).and_then(|centi| centi.checked_sub(273_150)) {
		Some(millidegrees) => Tagged::Known(millidegrees),
		None => Tagged::Invalid(InvalidReason::Overflow),
	}
}

// ------------------------------------------------------------------ HID

/// One decoded HID field: how wide it is, the logical range its descriptor declared, whether it has a
/// null state, and its Unit and Unit Exponent. The exponent is the SIGNED power of ten the descriptor
/// means - a driver that read the 4-bit form has already sign-extended it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Field {
	pub bits: u8,
	pub logical_min: i32,
	pub logical_max: i32,
	pub null_state: bool,
	pub unit: u32,
	pub exponent: i8,
}

/// The bits of one field, as a logical value.
///
/// SIGNEDNESS IS THE LOGICAL MINIMUM'S: a field whose minimum is negative holds a two's-complement
/// number of its own width. A value outside the logical range is NO DATA when the field declares a
/// null state and INVALID when it does not - it cannot be a measurement either way. A value wider than
/// its field was not extracted from it.
pub fn hid_logical(raw: u32, field: &Field) -> Tagged<i64> {
	if field.bits == 0 || field.bits > 32 {
		return Tagged::Invalid(InvalidReason::Range);
	}
	if field.logical_min > field.logical_max {
		return Tagged::Invalid(InvalidReason::Contradiction);
	}
	if field.bits < 32 && raw >> field.bits != 0 {
		return Tagged::Invalid(InvalidReason::Range);
	}
	let value: i64 = if field.logical_min < 0 {
		let shift = 64 - u32::from(field.bits);
		((raw as u64) << shift) as i64 >> shift
	} else {
		raw as i64
	};
	if value < i64::from(field.logical_min) || value > i64::from(field.logical_max) {
		return if field.null_state { Tagged::Unknown } else { Tagged::Invalid(InvalidReason::Range) };
	}
	Tagged::Known(value)
}

/// The canonical unit a HID value is converted into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Canonical {
	Microvolt,
	Microamp,
	Microwatt,
	/// Energy, from HID's joule.
	MicrowattHour,
	/// Charge, from HID's ampere-second.
	MicroampHour,
	MillidegreeCelsius,
	Second,
}

// A unit's exponents in HID's SI-linear system: centimetre, gram, second, kelvin, ampere.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Dimensions {
	length: i8,
	mass: i8,
	time: i8,
	temperature: i8,
	current: i8,
}

// One signed nibble of a Unit item.
fn nibble(unit: u32, at: u32) -> i8 {
	let raw = ((unit >> (at * 4)) & 0xf) as i8;
	if raw >= 8 { raw - 16 } else { raw }
}

// The dimensions of an SI-linear unit, or none for anything else: another system, a unit using
// luminous intensity (no quantity of power does), or the reserved nibble set.
fn dimensions(unit: u32) -> Option<Dimensions> {
	if unit & 0xf != 1 || nibble(unit, 6) != 0 || unit >> 28 != 0 {
		return None;
	}
	Some(Dimensions { length: nibble(unit, 1), mass: nibble(unit, 2), time: nibble(unit, 3), temperature: nibble(unit, 4), current: nibble(unit, 5) })
}

const fn expected(target: Canonical) -> Dimensions {
	match target {
		// g cm^2 s^-3 A^-1: the volt, in HID's centimetre and gram.
		Canonical::Microvolt => Dimensions { length: 2, mass: 1, time: -3, temperature: 0, current: -1 },
		Canonical::Microamp => Dimensions { length: 0, mass: 0, time: 0, temperature: 0, current: 1 },
		Canonical::Microwatt => Dimensions { length: 2, mass: 1, time: -3, temperature: 0, current: 0 },
		Canonical::MicrowattHour => Dimensions { length: 2, mass: 1, time: -2, temperature: 0, current: 0 },
		Canonical::MicroampHour => Dimensions { length: 0, mass: 0, time: 1, temperature: 0, current: 1 },
		Canonical::MillidegreeCelsius => Dimensions { length: 0, mass: 0, time: 0, temperature: 1, current: 0 },
		Canonical::Second => Dimensions { length: 0, mass: 0, time: 1, temperature: 0, current: 0 },
	}
}

/// Whether a HID Unit item measures what `target` measures.
pub fn hid_unit_is(unit: u32, target: Canonical) -> bool {
	dimensions(unit) == Some(expected(target))
}

/// A logical value in `unit` scaled by ten to `exponent`, in the canonical `target` unit.
///
/// THE WHOLE CONVERSION IS ONE RATIONAL: the value, times ten to the exponent, times the corrections
/// from HID's gram and centimetre to the kilogram and metre, times the canonical scale, over 3600 for
/// the hour-based units - and for a temperature, less 273.15 kelvin expressed in the SAME fraction -
/// held exactly in 128 bits and truncated toward zero ONCE, at the canonical unit.
///
/// A UNIT OF A DIFFERENT QUANTITY DOES NOT CONVERT. Charge is never turned into energy with an
/// instantaneous voltage, and a field in a unit this leaf cannot read is invalid rather than
/// reinterpreted: an unknown unit encoding cannot become a known measurement.
pub fn hid_convert(value: i64, unit: u32, exponent: i8, target: Canonical) -> Tagged<i64> {
	let Some(have) = dimensions(unit) else { return Tagged::Invalid(InvalidReason::Unit) };
	if have != expected(target) {
		return Tagged::Invalid(InvalidReason::Unit);
	}
	let (scale, hours): (i32, i128) = match target {
		Canonical::Microvolt | Canonical::Microamp | Canonical::Microwatt => (6, 1),
		Canonical::MicrowattHour | Canonical::MicroampHour => (6, 3600),
		Canonical::MillidegreeCelsius => (3, 1),
		Canonical::Second => (0, 1),
	};
	let power: i32 = i32::from(exponent) - 3 * i32::from(have.mass) - 2 * i32::from(have.length) + scale;
	if power.unsigned_abs() > 30 {
		return Tagged::Invalid(InvalidReason::Overflow);
	}
	let ten = 10i128.pow(power.unsigned_abs());
	let (mut numerator, denominator) = if power >= 0 {
		match (value as i128).checked_mul(ten) {
			Some(scaled) => (scaled, hours),
			None => return Tagged::Invalid(InvalidReason::Overflow),
		}
	} else {
		(value as i128, ten * hours)
	};
	if target == Canonical::MillidegreeCelsius {
		numerator -= 273_150 * denominator;
	}
	// Rust's integer division truncates toward zero, which is the one rounding this leaf does.
	match i64::try_from(numerator / denominator) {
		Ok(converted) => Tagged::Known(converted),
		Err(_) => Tagged::Invalid(InvalidReason::Overflow),
	}
}

/// A unitless percentage, scaled by its exponent, in basis points. Negative is out of range.
pub fn hid_basis_points(value: i64, unit: u32, exponent: i8) -> Tagged<u64> {
	if unit != 0 {
		return Tagged::Invalid(InvalidReason::Unit);
	}
	if value < 0 {
		return Tagged::Invalid(InvalidReason::Range);
	}
	let power = i32::from(exponent) + 2;
	if power.unsigned_abs() > 30 {
		return Tagged::Invalid(InvalidReason::Overflow);
	}
	let ten = 10i128.pow(power.unsigned_abs());
	let converted = if power >= 0 { (value as i128).checked_mul(ten) } else { Some(value as i128 / ten) };
	match converted.and_then(|basis| u64::try_from(basis).ok()) {
		Some(basis) => Tagged::Known(basis),
		None => Tagged::Invalid(InvalidReason::Overflow),
	}
}

/// What a HID capacity field turned out to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HidCapacity {
	/// Energy or charge, in its micro-unit.
	Quantity(Quantity, Tagged<u64>),
	/// A percentage, in basis points: CapacityMode said so.
	Percent(Tagged<u64>),
	/// Neither: CapacityMode declared boolean-only support, or the value is invalid before a unit
	/// could be told.
	None(Tagged<u64>),
}

/// A HID capacity value, by its Unit when the descriptor gave one and by the Battery System page's
/// CapacityMode when it did not - 0 milliamp-hours, 1 milliwatt-hours, 2 percent, 3 boolean-only.
/// A Unit and a CapacityMode that name different quantities contradict each other; a unitless field
/// with no CapacityMode declares nothing, and nothing measured in it converts.
pub fn hid_capacity(value: i64, unit: u32, exponent: i8, capacity_mode: Option<u32>) -> HidCapacity {
	if value < 0 {
		return HidCapacity::None(Tagged::Invalid(InvalidReason::Range));
	}
	let unsigned = |tagged: Tagged<i64>| -> Tagged<u64> {
		match tagged {
			Tagged::Known(v) if v >= 0 => Tagged::Known(v as u64),
			Tagged::Known(_) => Tagged::Invalid(InvalidReason::Range),
			other => other.map(|_| 0),
		}
	};
	if unit != 0 {
		let (quantity, target) = if hid_unit_is(unit, Canonical::MicroampHour) {
			(Quantity::Charge, Canonical::MicroampHour)
		} else if hid_unit_is(unit, Canonical::MicrowattHour) {
			(Quantity::Energy, Canonical::MicrowattHour)
		} else {
			return HidCapacity::None(Tagged::Invalid(InvalidReason::Unit));
		};
		let declared = match capacity_mode {
			Some(0) => Some(Quantity::Charge),
			Some(1) => Some(Quantity::Energy),
			_ => None,
		};
		if declared.is_some_and(|declared| declared != quantity) {
			return HidCapacity::Quantity(quantity, Tagged::Invalid(InvalidReason::Contradiction));
		}
		return HidCapacity::Quantity(quantity, unsigned(hid_convert(value, unit, exponent, target)));
	}
	// Unitless: CapacityMode is the unit. Milli-units to micro-units is three more powers of ten.
	let milli = |quantity: Quantity| -> HidCapacity {
		let power = i32::from(exponent) + 3;
		if power.unsigned_abs() > 30 {
			return HidCapacity::Quantity(quantity, Tagged::Invalid(InvalidReason::Overflow));
		}
		let ten = 10i128.pow(power.unsigned_abs());
		let converted = if power >= 0 { (value as i128).checked_mul(ten) } else { Some(value as i128 / ten) };
		match converted.and_then(|micro| u64::try_from(micro).ok()) {
			Some(micro) => HidCapacity::Quantity(quantity, Tagged::Known(micro)),
			None => HidCapacity::Quantity(quantity, Tagged::Invalid(InvalidReason::Overflow)),
		}
	};
	match capacity_mode {
		Some(0) => milli(Quantity::Charge),
		Some(1) => milli(Quantity::Energy),
		Some(2) => HidCapacity::Percent(hid_basis_points(value, 0, exponent)),
		Some(3) => HidCapacity::None(Tagged::Unsupported),
		_ => HidCapacity::None(Tagged::Invalid(InvalidReason::Unit)),
	}
}

#[cfg(test)]
mod tests;

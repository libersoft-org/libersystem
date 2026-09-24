//! ACPI POWER SOURCES, FROM THEIR DECODED METHOD RESULTS: a control-method battery (`_STA`, `_BIF` or
//! `_BIX`, `_BST`), an AC adapter (`_STA`, `_PSR`) and a thermal zone (`_TMP`, `_RTV`, `_CRT`, `_HOT`,
//! `_PSV`, `_ACx`).
//!
//! The caller evaluated the methods and read the integers out of their packages; this module decides
//! what those integers mean. It evaluates no AML and knows no namespace.

use alloc::vec::Vec;

use crate::canon::{at_or_above, below_threshold, capacity, derived, measure_i64, measure_u64, reported, state_of_charge, temperature, trip, unmeasured};
use crate::convert::{Tagged, acpi_charge_state, acpi_milli, acpi_quantity, acpi_rate, acpi_temperature};
use crate::schema::{AlarmKind, InvalidReason, Quantity, SourceKind, SourceState, TemperatureReference, TripKind, Tristate};

// `_STA` bit 4 on a battery device: a battery is in the slot.
const STA_BATTERY_PRESENT: u32 = 1 << 4;
// `_STA` bit 0 on any device: the device is present.
const STA_PRESENT: u32 = 1 << 0;
// `_BST` state bit 2: the battery is in its critical energy state.
const BST_CRITICAL: u32 = 1 << 2;

/// A control-method battery's decoded results. `status` is `_STA` when the device has one; the static
/// fields are `_BIF`'s or `_BIX`'s, which agree on these; the rest is `_BST`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Battery {
	pub status: Option<u32>,
	/// 0 declares milliwatts and milliwatt-hours, 1 milliamps and milliamp-hours.
	pub power_unit: u32,
	pub design_capacity: u32,
	pub last_full_capacity: u32,
	/// "Design Capacity of Warning": the level at which the platform says capacity is LOW.
	pub design_warning: u32,
	/// "Design Capacity of Low": the level at which it says capacity is CRITICAL.
	pub design_low: u32,
	pub state: u32,
	pub rate: u32,
	pub remaining: u32,
	pub voltage: u32,
}

/// The battery, normalised.
///
/// THE RATE IS POWER OR CURRENT BY THE POWER UNIT, never always current, and the other is unsupported
/// rather than manufactured from it. A POWER UNIT THAT NAMES NEITHER makes every capacity and the rate
/// invalid - there is no unit to read them in. A SLOT WITH NO BATTERY is present-no and measures
/// nothing: `_BST` is undefined then, and none of its numbers is published.
pub fn battery(b: &Battery) -> SourceState {
	let mut state = unmeasured(SourceKind::Battery);
	state.present = match b.status {
		Some(status) if status & STA_BATTERY_PRESENT != 0 => Tristate::Yes,
		Some(_) => Tristate::No,
		None => Tristate::Unknown,
	};
	let quantity = acpi_quantity(b.power_unit);
	let in_unit = |raw: u32| -> Tagged<u64> { if quantity.is_some() { acpi_milli(raw) } else { Tagged::Invalid(InvalidReason::Unit) } };
	let q = quantity.unwrap_or(Quantity::Energy);
	state.design = capacity(q, in_unit(b.design_capacity));
	state.full = capacity(q, in_unit(b.last_full_capacity));
	if state.present == Tristate::No {
		// Nothing is measured, and the static description goes too: `_BIF` describes a battery,
		// and there is none in the slot.
		state.design = capacity(q, Tagged::Unknown);
		state.full = capacity(q, Tagged::Unknown);
		state.remaining = capacity(q, Tagged::Unknown);
		state.state_of_charge = measure_u64(Tagged::Unknown);
		state.voltage = measure_u64(Tagged::Unknown);
		return state;
	}
	state.charge = acpi_charge_state(b.state);
	state.remaining = capacity(q, in_unit(b.remaining));
	state.state_of_charge = measure_u64(state_of_charge(None, &state.remaining, &state.full));
	state.voltage = measure_u64(acpi_milli(b.voltage));
	let rate = if quantity.is_some() { acpi_rate(b.rate, b.state) } else { Tagged::Invalid(InvalidReason::Unit) };
	match quantity {
		Some(Quantity::Energy) => state.power = measure_i64(rate),
		Some(Quantity::Charge) => state.current = measure_i64(rate),
		None => {
			state.power = measure_i64(rate);
			state.current = measure_i64(rate);
		}
	}
	let warning = capacity(q, in_unit(b.design_warning));
	let low = capacity(q, in_unit(b.design_low));
	state.alarms = alloc::vec![
		reported(AlarmKind::CriticalCapacity, b.state & BST_CRITICAL != 0),
		derived(AlarmKind::LowCapacity, below_threshold(&state.remaining, &warning)),
		derived(AlarmKind::CriticalCapacity, below_threshold(&state.remaining, &low)),
	];
	state
}

/// An AC adapter's decoded results: `_STA` when present, and `_PSR` - 1 on line, 0 off it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ac {
	pub status: Option<u32>,
	pub power_source: Option<u32>,
}

/// The adapter, normalised: presence and online state, each independently known or not. It measures
/// nothing else and advertises no control.
pub fn ac(a: &Ac) -> SourceState {
	let mut state = unmeasured(SourceKind::Ac);
	state.present = match a.status {
		Some(status) if status & STA_PRESENT != 0 => Tristate::Yes,
		Some(_) => Tristate::No,
		None => Tristate::Unknown,
	};
	state.online = match a.power_source {
		Some(1) => Tristate::Yes,
		Some(0) => Tristate::No,
		_ => Tristate::Unknown,
	};
	state
}

/// A thermal zone's decoded results. `relative` is `_RTV` evaluating nonzero; each trip is the
/// method's value when the zone defines it; `active` is `_AC0` onwards, at most ten.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Thermal<'a> {
	pub temperature: u32,
	pub relative: bool,
	pub critical: Option<u32>,
	pub hot: Option<u32>,
	pub passive: Option<u32>,
	pub active: &'a [u32],
}

/// The zone, normalised: its reading and trips on ONE reference, absolute or relative, and an
/// over-temperature alarm derived only by comparing the reading with the CRITICAL trip on that same
/// reference - the one threshold ACPI defines as the zone exceeding its limit. The trips are
/// observations; nothing here cools or shuts down anything.
pub fn thermal(t: &Thermal) -> SourceState {
	let mut state = unmeasured(SourceKind::ThermalZone);
	state.present = Tristate::Yes;
	let reference = if t.relative { TemperatureReference::Relative } else { TemperatureReference::Absolute };
	let reading = |raw: u32| temperature(reference, acpi_temperature(raw, t.relative));
	state.temperature = reading(t.temperature);
	let mut trips = Vec::new();
	if let Some(raw) = t.critical {
		trips.push(trip(TripKind::Critical, 0, reading(raw)));
	}
	if let Some(raw) = t.hot {
		trips.push(trip(TripKind::Hot, 0, reading(raw)));
	}
	if let Some(raw) = t.passive {
		trips.push(trip(TripKind::Passive, 0, reading(raw)));
	}
	for (index, raw) in t.active.iter().take(10).enumerate() {
		trips.push(trip(TripKind::Active, index as u8, reading(*raw)));
	}
	if let Some(critical) = trips.iter().find(|candidate| candidate.kind == TripKind::Critical) {
		state.alarms.push(derived(AlarmKind::OverTemperature, at_or_above(&state.temperature, &critical.temperature)));
	}
	state.trips = trips;
	state
}

#[cfg(test)]
mod tests;

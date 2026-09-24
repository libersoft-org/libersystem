//! A HID POWER DEVICE, FROM ITS DECODED FIELDS: a UPS as the USB HID Power Device class describes one,
//! through the Power Device and Battery System usage pages.
//!
//! The driver owns the report descriptor and the reports: it finds each usage's field, extracts its
//! bits and fills in `Ups`. This module owns what the bits mean - their width and signedness, their
//! logical range and null state, their Unit and Unit Exponent - and turns them into the canonical
//! record. Output-report construction for the controls stays with the driver too; what is here is
//! only whether a control is advertised.

use crate::canon::{below_threshold, capacity, derived, measure_i64, measure_u64, reported, state_of_charge, temperature, unmeasured};
use crate::convert::{Canonical, Field, HidCapacity, Tagged, hid_basis_points, hid_capacity, hid_convert, hid_logical};
use crate::schema::{AlarmKind, ChargeState, InvalidReason, Quantity, SourceKind, SourceState, TemperatureReference, Tristate};

/// One usage's value: the bits the driver extracted, and the field they came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Value {
	pub raw: u32,
	pub field: Field,
}

/// The PresentStatus flags a device reports, each only when it has the usage.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Status {
	pub ac_present: Option<bool>,
	pub charging: Option<bool>,
	pub discharging: Option<bool>,
	pub below_remaining_capacity_limit: Option<bool>,
	pub need_replacement: Option<bool>,
	pub overload: Option<bool>,
	pub internal_failure: Option<bool>,
	pub over_temperature: Option<bool>,
}

/// A UPS's decoded state. Every field is the usage of the same name, present only when the device's
/// descriptor has it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Ups {
	/// Battery System CapacityMode: 0 milliamp-hours, 1 milliwatt-hours, 2 percent, 3 boolean-only.
	pub capacity_mode: Option<u32>,
	pub remaining_capacity: Option<Value>,
	pub full_charge_capacity: Option<Value>,
	pub design_capacity: Option<Value>,
	/// The level below which the device raises BelowRemainingCapacityLimit.
	pub remaining_capacity_limit: Option<Value>,
	pub relative_state_of_charge: Option<Value>,
	pub run_time_to_empty: Option<Value>,
	pub voltage: Option<Value>,
	pub current: Option<Value>,
	/// The active power the device's output delivers.
	pub active_power: Option<Value>,
	pub percent_load: Option<Value>,
	pub temperature: Option<Value>,
	pub status: Status,
	/// How many outlets the device lets a host switch.
	pub switchable_outlets: u8,
	/// Whether DelayBeforeShutdown is writable: a turn-off can be scheduled, and cancelled.
	pub delay_before_shutdown: bool,
	pub source_time: Option<u64>,
}

fn logical(value: &Value) -> Tagged<i64> {
	hid_logical(value.raw, &value.field)
}

fn converted(value: Option<&Value>, target: Canonical) -> Tagged<i64> {
	let Some(value) = value else { return Tagged::Unsupported };
	match logical(value) {
		Tagged::Known(v) => hid_convert(v, value.field.unit, value.field.exponent, target),
		other => other,
	}
}

fn unsigned(tagged: Tagged<i64>) -> Tagged<u64> {
	match tagged {
		Tagged::Known(v) if v >= 0 => Tagged::Known(v as u64),
		Tagged::Known(_) => Tagged::Invalid(InvalidReason::Range),
		other => other.map(|_| 0),
	}
}

fn percent(value: Option<&Value>) -> Tagged<u64> {
	let Some(value) = value else { return Tagged::Unsupported };
	match logical(value) {
		Tagged::Known(v) => hid_basis_points(v, value.field.unit, value.field.exponent),
		other => other.map(|_| 0),
	}
}

// A capacity field through CapacityMode or its own Unit.
fn capacity_of(value: Option<&Value>, mode: Option<u32>) -> HidCapacity {
	let Some(value) = value else { return HidCapacity::None(Tagged::Unsupported) };
	match logical(value) {
		Tagged::Known(v) => hid_capacity(v, value.field.unit, value.field.exponent, mode),
		other => HidCapacity::None(other.map(|_| 0)),
	}
}

fn charge_state(status: &Status) -> ChargeState {
	match (status.charging, status.discharging) {
		(Some(true), Some(true)) => ChargeState::Invalid,
		(Some(true), _) => ChargeState::Charging,
		(_, Some(true)) => ChargeState::Discharging,
		(Some(false), Some(false)) => ChargeState::Idle,
		_ => ChargeState::Unknown,
	}
}

/// The UPS, normalised.
///
/// CAPACITY stays in the quantity the device reports - charge is never turned into energy with the
/// present voltage, so a missing nominal voltage is not an error. A percentage CapacityMode makes the
/// remaining capacity an explicit state of charge instead. CURRENT keeps the sign a signed field gives
/// it; an unsigned one takes its direction from the charging and discharging flags, and without a
/// direction a nonzero current is unknown. ACTIVE POWER is what the output delivers, so it is negative:
/// out of the source. LOAD is a percentage of rated load or nothing - no denominator is invented from
/// watts.
///
/// A REPORTED ALARM appears for each status usage the device has; the one DERIVED alarm is remaining
/// capacity against the device's own RemainingCapacityLimit, in the same quantity.
pub fn ups(u: &Ups) -> SourceState {
	let mut state = unmeasured(SourceKind::Ups);
	state.present = Tristate::Yes;
	state.online = match u.status.ac_present {
		Some(true) => Tristate::Yes,
		Some(false) => Tristate::No,
		None => Tristate::Unknown,
	};
	state.charge = charge_state(&u.status);

	let mut explicit = u.relative_state_of_charge.as_ref().map(|value| percent(Some(value)));
	let as_capacity = |found: HidCapacity, explicit: &mut Option<Tagged<u64>>, is_remaining: bool| -> crate::schema::Capacity {
		match found {
			HidCapacity::Quantity(quantity, tagged) => capacity(quantity, tagged),
			HidCapacity::Percent(basis) => {
				if is_remaining && explicit.is_none() {
					*explicit = Some(basis);
				}
				capacity(Quantity::Energy, Tagged::Unsupported)
			}
			HidCapacity::None(tagged) => capacity(Quantity::Energy, tagged),
		}
	};
	state.remaining = as_capacity(capacity_of(u.remaining_capacity.as_ref(), u.capacity_mode), &mut explicit, true);
	state.full = as_capacity(capacity_of(u.full_charge_capacity.as_ref(), u.capacity_mode), &mut explicit, false);
	state.design = as_capacity(capacity_of(u.design_capacity.as_ref(), u.capacity_mode), &mut explicit, false);
	state.state_of_charge = measure_u64(match (&u.relative_state_of_charge, &u.remaining_capacity) {
		(None, None) => Tagged::Unsupported,
		_ => state_of_charge(explicit, &state.remaining, &state.full),
	});

	state.runtime = measure_u64(unsigned(converted(u.run_time_to_empty.as_ref(), Canonical::Second)));
	state.voltage = measure_u64(unsigned(converted(u.voltage.as_ref(), Canonical::Microvolt)));
	state.current = measure_i64(match u.current.as_ref() {
		None => Tagged::Unsupported,
		Some(value) if value.field.logical_min < 0 => converted(Some(value), Canonical::Microamp),
		Some(value) => match converted(Some(value), Canonical::Microamp) {
			Tagged::Known(magnitude) => match state.charge {
				ChargeState::Charging => Tagged::Known(magnitude),
				ChargeState::Discharging => Tagged::Known(-magnitude),
				_ if magnitude == 0 => Tagged::Known(0),
				ChargeState::Invalid => Tagged::Invalid(InvalidReason::Contradiction),
				_ => Tagged::Unknown,
			},
			other => other,
		},
	});
	state.power = measure_i64(match converted(u.active_power.as_ref(), Canonical::Microwatt) {
		Tagged::Known(delivered) if delivered >= 0 => Tagged::Known(-delivered),
		Tagged::Known(_) => Tagged::Invalid(InvalidReason::Range),
		other => other,
	});
	state.load = measure_u64(percent(u.percent_load.as_ref()));
	state.temperature = temperature(TemperatureReference::Absolute, converted(u.temperature.as_ref(), Canonical::MillidegreeCelsius));

	let s = &u.status;
	let flags = [
		(AlarmKind::OnBattery, s.ac_present.map(|on_line| !on_line)),
		(AlarmKind::LowCapacity, s.below_remaining_capacity_limit),
		(AlarmKind::ReplaceBattery, s.need_replacement),
		(AlarmKind::Overload, s.overload),
		(AlarmKind::SourceFault, s.internal_failure),
		(AlarmKind::OverTemperature, s.over_temperature),
	];
	for (kind, active) in flags {
		if let Some(active) = active {
			state.alarms.push(reported(kind, active));
		}
	}
	if u.remaining_capacity_limit.is_some() {
		let limit = match capacity_of(u.remaining_capacity_limit.as_ref(), u.capacity_mode) {
			HidCapacity::Quantity(quantity, tagged) => capacity(quantity, tagged),
			HidCapacity::Percent(_) | HidCapacity::None(_) => capacity(Quantity::Energy, Tagged::Unknown),
		};
		state.alarms.push(derived(AlarmKind::LowCapacity, below_threshold(&state.remaining, &limit)));
	}

	state.controls.outlets = u.switchable_outlets;
	state.controls.set_output = u.switchable_outlets > 0;
	state.controls.schedule_off = u.delay_before_shutdown;
	state.controls.cancel_off = u.delay_before_shutdown;
	state.source_time = u.source_time;
	state
}

#[cfg(test)]
mod tests;

// A battery, an adapter and a thermal zone, each described by the integers their methods return, and
// the canonical record each must become. The expectations are the specification's units worked by
// hand: 48 000 mWh is 48 Wh, 11 100 mV is 11.1 V, 3002 tenths of a kelvin is 27.05 degrees Celsius.

use super::*;
use crate::canon::validate;
use crate::schema::{Alarm, ChargeState, Provenance, ValueState};

fn laptop() -> Battery {
	Battery { status: Some(0x1f), power_unit: 0, design_capacity: 50_000, last_full_capacity: 48_000, design_warning: 5_000, design_low: 2_000, state: 0b01, rate: 12_500, remaining: 24_000, voltage: 11_100 }
}

fn alarm(state: &SourceState, kind: AlarmKind, provenance: Provenance) -> Option<Tristate> {
	state.alarms.iter().find(|a: &&Alarm| a.kind == kind && a.provenance == provenance).map(|a| a.state)
}

#[test]
fn an_energy_battery_publishes_energy_and_power_and_no_current() {
	let state = battery(&laptop());
	assert_eq!(validate(&state), Ok(()));
	assert_eq!(state.kind, SourceKind::Battery);
	assert_eq!(state.present, Tristate::Yes);
	assert_eq!(state.charge, ChargeState::Discharging);
	assert_eq!((state.remaining.quantity, state.remaining.value), (Quantity::Energy, 24_000_000));
	assert_eq!(state.full.value, 48_000_000, "full is the last full charge");
	assert_eq!(state.design.value, 50_000_000, "and design stays separate");
	// 24 of 48 is half - over FULL, not over design, which would be 48 percent.
	assert_eq!(state.state_of_charge.value, 5000);
	assert_eq!(state.voltage.value, 11_100_000);
	// 12.5 W discharging, as POWER, because the unit is energy.
	assert_eq!((state.power.state, state.power.value), (ValueState::Known, -12_500_000));
	assert_eq!(state.current.state, ValueState::Unsupported, "current is not manufactured from power");
	assert_eq!(alarm(&state, AlarmKind::CriticalCapacity, Provenance::Reported), Some(Tristate::No));
	assert_eq!(alarm(&state, AlarmKind::LowCapacity, Provenance::Derived), Some(Tristate::No));
}

#[test]
fn a_charge_battery_publishes_charge_and_current_and_no_power() {
	let mut b = laptop();
	b.power_unit = 1;
	b.state = 0b10;
	let state = battery(&b);
	assert_eq!(validate(&state), Ok(()));
	assert_eq!((state.remaining.quantity, state.remaining.value), (Quantity::Charge, 24_000_000));
	assert_eq!((state.current.state, state.current.value), (ValueState::Known, 12_500_000), "charging is into storage");
	assert_eq!(state.power.state, ValueState::Unsupported);
}

#[test]
fn the_sentinel_and_the_contradiction_are_published_as_what_they_are() {
	let mut b = laptop();
	b.remaining = crate::convert::ACPI_UNKNOWN;
	b.state = 0b11;
	let state = battery(&b);
	assert_eq!(validate(&state), Ok(()));
	assert_eq!(state.remaining.state, ValueState::Unknown);
	assert_eq!(state.remaining.value, 0, "and not 4294967295000");
	assert_eq!(state.state_of_charge.state, ValueState::Unknown);
	assert_eq!(state.charge, ChargeState::Invalid);
	assert_eq!((state.power.state, state.power.reason), (ValueState::Invalid, InvalidReason::Contradiction));
	// Without a known remaining capacity, no capacity alarm can be derived.
	assert_eq!(alarm(&state, AlarmKind::LowCapacity, Provenance::Derived), Some(Tristate::Unknown));
}

#[test]
fn an_unknown_power_unit_makes_every_capacity_invalid() {
	let mut b = laptop();
	b.power_unit = 7;
	let state = battery(&b);
	assert_eq!(validate(&state), Ok(()));
	for capacity in [&state.remaining, &state.full, &state.design] {
		assert_eq!((capacity.state, capacity.reason), (ValueState::Invalid, InvalidReason::Unit));
	}
	assert_eq!(state.power.reason, InvalidReason::Unit);
	assert_eq!(state.voltage.value, 11_100_000, "a voltage is millivolts whatever the power unit says");
}

#[test]
fn capacity_alarms_are_reported_by_the_state_bit_and_derived_at_or_below_the_platforms_levels() {
	let mut b = laptop();
	b.remaining = 5_000;
	let at_warning = battery(&b);
	assert_eq!(alarm(&at_warning, AlarmKind::LowCapacity, Provenance::Derived), Some(Tristate::Yes), "at the warning level the condition has begun");
	assert_eq!(alarm(&at_warning, AlarmKind::CriticalCapacity, Provenance::Derived), Some(Tristate::No));
	b.remaining = 2_000;
	b.state = 0b101;
	let critical = battery(&b);
	assert_eq!(alarm(&critical, AlarmKind::CriticalCapacity, Provenance::Derived), Some(Tristate::Yes));
	assert_eq!(alarm(&critical, AlarmKind::CriticalCapacity, Provenance::Reported), Some(Tristate::Yes), "the platform's own critical bit is a separate, reported alarm");
	b.remaining = 5_001;
	b.state = 0b01;
	assert_eq!(alarm(&battery(&b), AlarmKind::LowCapacity, Provenance::Derived), Some(Tristate::No));
}

#[test]
fn an_empty_slot_is_a_source_that_measures_nothing() {
	let mut b = laptop();
	b.status = Some(0x0f);
	let state = battery(&b);
	assert_eq!(validate(&state), Ok(()));
	assert_eq!(state.present, Tristate::No);
	assert!(state.alarms.is_empty());
	for measured in [state.remaining.state, state.full.state, state.design.state, state.voltage.state, state.state_of_charge.state] {
		assert_ne!(measured, ValueState::Known);
	}
}

#[test]
fn an_adapter_knows_presence_and_line_power_independently() {
	let on_line = ac(&Ac { status: Some(0x0f), power_source: Some(1) });
	assert_eq!((on_line.kind, on_line.present, on_line.online), (SourceKind::Ac, Tristate::Yes, Tristate::Yes));
	assert_eq!(validate(&on_line), Ok(()));
	let off_line = ac(&Ac { status: None, power_source: Some(0) });
	assert_eq!((off_line.present, off_line.online), (Tristate::Unknown, Tristate::No));
	assert_eq!(ac(&Ac { status: Some(0x0f), power_source: Some(9) }).online, Tristate::Unknown);
	assert!(!on_line.controls.set_output && !on_line.controls.schedule_off, "an adapter advertises no control");
}

#[test]
fn an_absolute_zone_publishes_celsius_and_derives_over_temperature_against_critical() {
	// 300.2 K reading, critical at 368.2 K (95.05 C), hot at 363.2 K, passive at 353.2 K, two active.
	let zone = thermal(&Thermal { temperature: 3002, relative: false, critical: Some(3682), hot: Some(3632), passive: Some(3532), active: &[3432, 3332] });
	assert_eq!(validate(&zone), Ok(()));
	assert_eq!((zone.temperature.reference, zone.temperature.value), (TemperatureReference::Absolute, 27_050));
	assert_eq!(zone.trips.len(), 5);
	assert_eq!((zone.trips[0].kind, zone.trips[0].temperature.value), (TripKind::Critical, 95_050));
	assert_eq!((zone.trips[3].kind, zone.trips[3].index, zone.trips[3].temperature.value), (TripKind::Active, 0, 70_050));
	assert_eq!((zone.trips[4].index, zone.trips[4].temperature.value), (1, 60_050));
	assert_eq!(alarm(&zone, AlarmKind::OverTemperature, Provenance::Derived), Some(Tristate::No));
	let hot = thermal(&Thermal { temperature: 3682, relative: false, critical: Some(3682), hot: None, passive: None, active: &[] });
	assert_eq!(alarm(&hot, AlarmKind::OverTemperature, Provenance::Derived), Some(Tristate::Yes), "at the critical trip the zone is over it");
	assert!(!hot.controls.set_output && !hot.controls.schedule_off, "a zone advertises no control");
}

#[test]
fn a_relative_zone_keeps_its_offsets_and_invents_no_celsius() {
	// Eight kelvin below critical, and critical is the reference itself.
	let zone = thermal(&Thermal { temperature: (-80i32) as u32, relative: true, critical: Some(0), hot: Some((-20i32) as u32), passive: None, active: &[] });
	assert_eq!(validate(&zone), Ok(()));
	assert_eq!((zone.temperature.reference, zone.temperature.value), (TemperatureReference::Relative, -8_000));
	assert!(zone.trips.iter().all(|t| t.temperature.reference == TemperatureReference::Relative), "every value on the one reference");
	assert_eq!(zone.trips[1].temperature.value, -2_000);
	assert_eq!(alarm(&zone, AlarmKind::OverTemperature, Provenance::Derived), Some(Tristate::No));
	let reached = thermal(&Thermal { temperature: 0, relative: true, critical: Some(0), hot: None, passive: None, active: &[] });
	assert_eq!(alarm(&reached, AlarmKind::OverTemperature, Provenance::Derived), Some(Tristate::Yes));
}

#[test]
fn a_zone_without_a_critical_trip_derives_no_alarm_and_an_unknown_reading_derives_unknown() {
	let quiet = thermal(&Thermal { temperature: 3002, relative: false, critical: None, hot: Some(3632), passive: None, active: &[] });
	assert!(quiet.alarms.is_empty(), "no threshold is chosen in the critical trip's place");
	let unknown = thermal(&Thermal { temperature: crate::convert::ACPI_UNKNOWN, relative: false, critical: Some(3682), hot: None, passive: None, active: &[] });
	assert_eq!(unknown.temperature.state, ValueState::Unknown);
	assert_eq!(alarm(&unknown, AlarmKind::OverTemperature, Provenance::Derived), Some(Tristate::Unknown));
	// Ten active levels are the most ACPI names, and the eleventh is not read.
	let many = thermal(&Thermal { temperature: 3002, relative: false, critical: Some(3682), hot: Some(3632), passive: Some(3532), active: &[3000; 12] });
	assert_eq!(many.trips.len(), 13);
	assert_eq!(validate(&many), Ok(()));
}

// A UPS described by its decoded usages - the numbers a HID Power Device driver extracts, and the
// fields they came from - and the canonical record it must become. The units are the class's own:
// the volt as g cm^2 s^-3 A^-1 at exponent 7, the ampere-second, the kelvin, the second.

use super::*;
use crate::canon::validate;
use crate::schema::{Provenance, ValueState};

const VOLT: u32 = 0x00f0_d121;
const AMPERE: u32 = 0x0010_0001;
const WATT: u32 = 0x0000_d121;
const AMPERE_SECOND: u32 = 0x0010_1001;
const KELVIN: u32 = 0x0001_0001;
const SECOND: u32 = 0x0000_1001;

fn value(raw: u32, bits: u8, logical_min: i32, logical_max: i32, unit: u32, exponent: i8) -> Option<Value> {
	Some(Value { raw, field: Field { bits, logical_min, logical_max, null_state: false, unit, exponent } })
}

// A small line-interactive UPS on mains power: capacities in unitless milliamp-hours by CapacityMode,
// a 12 V battery, 230 V class output delivering 180 W at 36 percent load, 26.85 C inside.
fn ups_on_line() -> Ups {
	Ups { capacity_mode: Some(0), remaining_capacity: value(6_300, 16, 0, 65_535, 0, 0), full_charge_capacity: value(7_000, 16, 0, 65_535, 0, 0), design_capacity: value(7_200, 16, 0, 65_535, 0, 0), remaining_capacity_limit: value(700, 16, 0, 65_535, 0, 0), relative_state_of_charge: None, run_time_to_empty: value(2_400, 16, 0, 65_535, SECOND, 0), voltage: value(1_365, 16, 0, 65_535, VOLT, 5), current: value(0, 16, 0, 65_535, AMPERE, -2), active_power: value(180, 16, 0, 65_535, WATT, 7), percent_load: value(36, 8, 0, 255, 0, 0), temperature: value(300, 16, 0, 65_535, KELVIN, 0), status: Status { ac_present: Some(true), charging: Some(false), discharging: Some(false), below_remaining_capacity_limit: Some(false), need_replacement: Some(false), overload: Some(false), internal_failure: None, over_temperature: None }, switchable_outlets: 2, delay_before_shutdown: true, source_time: Some(77) }
}

fn alarm(state: &SourceState, kind: AlarmKind, provenance: Provenance) -> Option<Tristate> {
	state.alarms.iter().find(|a| a.kind == kind && a.provenance == provenance).map(|a| a.state)
}

#[test]
fn a_ups_on_line_power_normalises_with_exact_units() {
	let state = ups(&ups_on_line());
	assert_eq!(validate(&state), Ok(()));
	assert_eq!((state.kind, state.present, state.online, state.charge), (SourceKind::Ups, Tristate::Yes, Tristate::Yes, ChargeState::Idle));
	// 6300 mAh of a 7000 mAh full charge, and a 7200 mAh design.
	assert_eq!((state.remaining.quantity, state.remaining.value), (Quantity::Charge, 6_300_000));
	assert_eq!(state.full.value, 7_000_000);
	assert_eq!(state.design.value, 7_200_000);
	assert_eq!(state.state_of_charge.value, 9000, "6300 of 7000 is ninety percent");
	assert_eq!(state.runtime.value, 2_400);
	// 1365 hundredths of a volt is 13.65 V.
	assert_eq!(state.voltage.value, 13_650_000);
	assert_eq!((state.current.state, state.current.value), (ValueState::Known, 0));
	// 180 W delivered is 180 W out of the source.
	assert_eq!(state.power.value, -180_000_000);
	assert_eq!(state.load.value, 3_600);
	assert_eq!((state.temperature.reference, state.temperature.value), (TemperatureReference::Absolute, 26_850));
	assert_eq!(state.source_time, Some(77));
	assert_eq!((state.controls.set_output, state.controls.outlets, state.controls.schedule_off, state.controls.cancel_off), (true, 2, true, true));
	assert_eq!(alarm(&state, AlarmKind::OnBattery, Provenance::Reported), Some(Tristate::No));
	assert_eq!(alarm(&state, AlarmKind::LowCapacity, Provenance::Derived), Some(Tristate::No));
	assert_eq!(alarm(&state, AlarmKind::SourceFault, Provenance::Reported), None, "a status usage the device lacks is not an alarm");
}

#[test]
fn on_battery_the_flags_become_alarms_and_the_unsigned_current_takes_their_direction() {
	let mut u = ups_on_line();
	u.status.ac_present = Some(false);
	u.status.discharging = Some(true);
	u.status.below_remaining_capacity_limit = Some(true);
	u.remaining_capacity = value(700, 16, 0, 65_535, 0, 0);
	u.current = value(150, 16, 0, 65_535, AMPERE, -2);
	let state = ups(&u);
	assert_eq!(validate(&state), Ok(()));
	assert_eq!((state.online, state.charge), (Tristate::No, ChargeState::Discharging));
	assert_eq!(state.current.value, -1_500_000, "1.5 A out of storage");
	assert_eq!(alarm(&state, AlarmKind::OnBattery, Provenance::Reported), Some(Tristate::Yes));
	assert_eq!(alarm(&state, AlarmKind::LowCapacity, Provenance::Reported), Some(Tristate::Yes));
	// 700 at a limit of 700: at or below.
	assert_eq!(alarm(&state, AlarmKind::LowCapacity, Provenance::Derived), Some(Tristate::Yes));
}

#[test]
fn a_signed_current_keeps_its_own_sign_and_an_unsigned_one_without_direction_is_unknown() {
	let mut u = ups_on_line();
	u.current = value((-150i32 as u32) & 0xffff, 16, -32_768, 32_767, AMPERE, -2);
	assert_eq!(ups(&u).current.value, -1_500_000);
	u.current = value(150, 16, 0, 65_535, AMPERE, -2);
	u.status.charging = None;
	u.status.discharging = None;
	assert_eq!(ups(&u).current.state, ValueState::Unknown, "a nonzero current with no direction is not a positive one");
	u.status.charging = Some(true);
	u.status.discharging = Some(true);
	let contradiction = ups(&u);
	assert_eq!(contradiction.charge, ChargeState::Invalid);
	assert_eq!(contradiction.current.reason, InvalidReason::Contradiction);
}

#[test]
fn a_percentage_capacity_mode_is_a_state_of_charge_and_no_capacity() {
	let mut u = ups_on_line();
	u.capacity_mode = Some(2);
	u.remaining_capacity = value(64, 8, 0, 100, 0, 0);
	u.full_charge_capacity = value(100, 8, 0, 100, 0, 0);
	u.remaining_capacity_limit = value(10, 8, 0, 100, 0, 0);
	let state = ups(&u);
	assert_eq!(validate(&state), Ok(()));
	assert_eq!(state.state_of_charge.value, 6400);
	assert_eq!(state.remaining.state, ValueState::Unsupported, "no energy or charge was reported");
	assert_eq!(alarm(&state, AlarmKind::LowCapacity, Provenance::Derived), Some(Tristate::Unknown), "a percentage limit is not a same-quantity threshold");
	// An explicit relative state of charge wins over the capacity field.
	u.relative_state_of_charge = value(63, 8, 0, 100, 0, 0);
	assert_eq!(ups(&u).state_of_charge.value, 6300);
}

#[test]
fn charge_is_never_converted_to_energy_and_a_missing_voltage_is_no_error() {
	let mut u = ups_on_line();
	u.capacity_mode = None;
	// Ampere-seconds by Unit: 22 680 As is 6.3 Ah, and 25 200 As is 7 Ah.
	u.remaining_capacity = value(22_680, 16, 0, 65_535, AMPERE_SECOND, 0);
	u.full_charge_capacity = value(25_200, 16, 0, 65_535, AMPERE_SECOND, 0);
	u.design_capacity = None;
	u.voltage = None;
	let state = ups(&u);
	assert_eq!(validate(&state), Ok(()));
	assert_eq!((state.remaining.quantity, state.remaining.value), (Quantity::Charge, 6_300_000));
	assert_eq!(state.voltage.state, ValueState::Unsupported);
	assert_eq!(state.state_of_charge.value, 9000);
	// And the limit was unitless with no CapacityMode, so it declares nothing to compare against.
	assert_eq!(alarm(&state, AlarmKind::LowCapacity, Provenance::Derived), Some(Tristate::Unknown));
}

#[test]
fn a_field_in_the_wrong_unit_or_out_of_its_range_is_invalid_and_not_a_measurement() {
	let mut u = ups_on_line();
	u.voltage = value(230, 16, 0, 65_535, 0, 7);
	u.run_time_to_empty = value(2_400, 16, 0, 65_535, 0, 0);
	u.percent_load = value(0xff, 8, 0, 100, 0, 0);
	u.active_power = value((-5i32 as u32) & 0xffff, 16, -32_768, 32_767, WATT, 7);
	let state = ups(&u);
	assert_eq!(validate(&state), Ok(()));
	assert_eq!((state.voltage.state, state.voltage.reason), (ValueState::Invalid, InvalidReason::Unit), "unitless is not volts");
	assert_eq!(state.runtime.reason, InvalidReason::Unit, "a runtime must say it is in seconds");
	assert_eq!(state.load.reason, InvalidReason::Range);
	assert_eq!(state.power.reason, InvalidReason::Range, "an output cannot deliver negative power");
	// With a null state the same out-of-range load is simply no data.
	let mut nullable = ups_on_line();
	nullable.percent_load = Some(Value { raw: 0xff, field: Field { bits: 8, logical_min: 0, logical_max: 100, null_state: true, unit: 0, exponent: 0 } });
	assert_eq!(ups(&nullable).load.state, ValueState::Unknown);
}

#[test]
fn every_reported_flag_maps_to_its_own_alarm_and_nothing_else_does() {
	let mut u = ups_on_line();
	u.status = Status { ac_present: Some(true), charging: None, discharging: None, below_remaining_capacity_limit: None, need_replacement: Some(true), overload: Some(true), internal_failure: Some(true), over_temperature: Some(true) };
	u.remaining_capacity_limit = None;
	let state = ups(&u);
	assert_eq!(validate(&state), Ok(()));
	assert_eq!(alarm(&state, AlarmKind::ReplaceBattery, Provenance::Reported), Some(Tristate::Yes));
	assert_eq!(alarm(&state, AlarmKind::Overload, Provenance::Reported), Some(Tristate::Yes));
	assert_eq!(alarm(&state, AlarmKind::SourceFault, Provenance::Reported), Some(Tristate::Yes));
	assert_eq!(alarm(&state, AlarmKind::OverTemperature, Provenance::Reported), Some(Tristate::Yes));
	assert_eq!(alarm(&state, AlarmKind::LowCapacity, Provenance::Reported), None);
	assert_eq!(alarm(&state, AlarmKind::LowCapacity, Provenance::Derived), None, "no limit, no derivation");
	assert_eq!(alarm(&state, AlarmKind::CriticalCapacity, Provenance::Reported), None, "no usage of the class maps to it");
	assert_eq!(state.charge, ChargeState::Unknown);
}

#[test]
fn no_outlet_and_no_delay_advertise_no_control() {
	let mut u = ups_on_line();
	u.switchable_outlets = 0;
	u.delay_before_shutdown = false;
	let state = ups(&u);
	assert_eq!(validate(&state), Ok(()));
	assert_eq!((state.controls.set_output, state.controls.schedule_off, state.controls.cancel_off, state.controls.outlets), (false, false, false, 0));
}

use super::*;
use crate::schema::{ChargeState, SourceKind};

fn energy(value: u64) -> Capacity {
	capacity(Quantity::Energy, Tagged::Known(value))
}

fn charge(value: u64) -> Capacity {
	capacity(Quantity::Charge, Tagged::Known(value))
}

fn absolute(millidegrees: i64) -> Temperature {
	temperature(TemperatureReference::Absolute, Tagged::Known(millidegrees))
}

fn relative(millidegrees: i64) -> Temperature {
	temperature(TemperatureReference::Relative, Tagged::Known(millidegrees))
}

#[test]
fn an_untagged_number_cannot_be_built() {
	// Every constructor puts the number only where the tag says there is one.
	assert_eq!(measure_u64(Tagged::Unknown), MeasureU64 { state: ValueState::Unknown, value: 0, reason: InvalidReason::None });
	assert_eq!(measure_i64(Tagged::Invalid(InvalidReason::Overflow)), MeasureI64 { state: ValueState::Invalid, value: 0, reason: InvalidReason::Overflow });
	assert_eq!(measure_u64(Tagged::Known(0)).state, ValueState::Known, "a known zero is a measurement");
	assert_eq!(capacity_value(&energy(5)), Tagged::Known(5));
	assert_eq!(capacity_value(&capacity(Quantity::Charge, Tagged::Invalid(InvalidReason::Sentinel))), Tagged::Invalid(InvalidReason::Sentinel));
}

#[test]
fn state_of_charge_prefers_the_explicit_value_and_never_divides_by_design() {
	// 57 percent, as the source said it.
	assert_eq!(state_of_charge(Some(Tagged::Known(5700)), &energy(1), &energy(1000)), Tagged::Known(5700));
	assert_eq!(state_of_charge(Some(Tagged::Known(10_001)), &energy(1), &energy(2)), Tagged::Invalid(InvalidReason::Range));
	// 50 Wh of 100 Wh is half.
	assert_eq!(state_of_charge(None, &energy(50_000_000), &energy(100_000_000)), Tagged::Known(5000));
	// A third, truncated: 3333 basis points.
	assert_eq!(state_of_charge(None, &charge(1), &charge(3)), Tagged::Known(3333));
	assert_eq!(state_of_charge(None, &energy(101), &energy(100)), Tagged::Invalid(InvalidReason::Contradiction), "more remaining than full is not clamped to a healthy hundred percent");
	assert_eq!(state_of_charge(None, &energy(0), &energy(0)), Tagged::Unknown, "a zero full capacity divides nothing");
	assert_eq!(state_of_charge(None, &energy(50), &charge(100)), Tagged::Unknown, "energy over charge is not a fraction");
	// FULL UNKNOWN, DESIGN KNOWN: still unknown. Design is not full.
	let full = capacity(Quantity::Energy, Tagged::Unknown);
	assert_eq!(state_of_charge(None, &energy(50), &full), Tagged::Unknown);
	// An explicit value that is itself invalid says so when nothing else can answer.
	assert_eq!(state_of_charge(Some(Tagged::Invalid(InvalidReason::Unit)), &energy(1), &full), Tagged::Invalid(InvalidReason::Unit));
	// And a valid derivation answers when the explicit value is unknown.
	assert_eq!(state_of_charge(Some(Tagged::Unknown), &energy(25), &energy(100)), Tagged::Known(2500));
}

#[test]
fn a_capacity_alarm_is_at_or_below_a_threshold_in_the_same_quantity() {
	assert_eq!(below_threshold(&energy(1000), &energy(1000)), Tristate::Yes, "at the threshold the condition has begun");
	assert_eq!(below_threshold(&energy(999), &energy(1000)), Tristate::Yes);
	assert_eq!(below_threshold(&energy(1001), &energy(1000)), Tristate::No);
	assert_eq!(below_threshold(&charge(10), &energy(1000)), Tristate::Unknown, "no comparison across quantities");
	assert_eq!(below_threshold(&capacity(Quantity::Energy, Tagged::Unknown), &energy(1000)), Tristate::Unknown);
	assert_eq!(below_threshold(&energy(10), &capacity(Quantity::Energy, Tagged::Unknown)), Tristate::Unknown, "no threshold is invented");
}

#[test]
fn a_temperature_alarm_is_at_or_above_a_trip_on_the_same_reference() {
	assert_eq!(at_or_above(&absolute(95_000), &absolute(95_000)), Tristate::Yes);
	assert_eq!(at_or_above(&absolute(94_999), &absolute(95_000)), Tristate::No);
	assert_eq!(at_or_above(&relative(0), &relative(0)), Tristate::Yes);
	assert_eq!(at_or_above(&relative(-1), &relative(0)), Tristate::No);
	assert_eq!(at_or_above(&absolute(95_000), &relative(0)), Tristate::Unknown, "absolute and relative are not comparable");
	assert_eq!(at_or_above(&temperature(TemperatureReference::Absolute, Tagged::Unknown), &absolute(0)), Tristate::Unknown);
}

#[test]
fn an_alarm_transition_is_any_alarm_entering_leaving_or_changing() {
	let mut before = unmeasured(SourceKind::Ups);
	before.alarms = alloc::vec![reported(AlarmKind::OnBattery, false), derived(AlarmKind::LowCapacity, Tristate::No)];
	let mut after = before.clone();
	after.voltage = measure_u64(Tagged::Known(230_000_000));
	assert!(!alarm_transition(&before, &after), "a measurement changing is not an alarm transition");
	after.alarms[0] = reported(AlarmKind::OnBattery, true);
	assert!(alarm_transition(&before, &after));
	let mut added = before.clone();
	added.alarms.push(reported(AlarmKind::Overload, false));
	assert!(alarm_transition(&before, &added), "an alarm appearing is a transition");
	assert!(alarm_transition(&added, &before), "and one disappearing");
	// The same kind with the other provenance is a different alarm.
	let mut provenance = before.clone();
	provenance.alarms[1] = reported(AlarmKind::LowCapacity, false);
	assert!(alarm_transition(&before, &provenance));
}

#[test]
fn validation_refuses_what_a_canonical_record_cannot_be() {
	let good = unmeasured(SourceKind::Battery);
	assert_eq!(validate(&good), Ok(()));
	let refused = |mutate: &dyn Fn(&mut SourceState)| {
		let mut state = good.clone();
		mutate(&mut state);
		validate(&state)
	};
	// A number behind a tag that says there is none.
	assert_eq!(refused(&|s| s.voltage = MeasureU64 { state: ValueState::Unknown, value: 12, reason: InvalidReason::None }), Err(InvalidReason::Contradiction));
	// An invalid value that does not say why, and a known one that gives a reason.
	assert_eq!(refused(&|s| s.current = MeasureI64 { state: ValueState::Invalid, value: 0, reason: InvalidReason::None }), Err(InvalidReason::Contradiction));
	assert_eq!(refused(&|s| s.power = MeasureI64 { state: ValueState::Known, value: 5, reason: InvalidReason::Range }), Err(InvalidReason::Contradiction));
	assert_eq!(refused(&|s| s.state_of_charge = measure_u64(Tagged::Known(10_001))), Err(InvalidReason::Range));
	assert_eq!(refused(&|s| s.state_of_charge = measure_u64(Tagged::Known(10_000))), Ok(()));
	// An absent source with a measurement.
	assert_eq!(
		refused(&|s| {
			s.present = Tristate::No;
			s.remaining = energy(10);
		}),
		Err(InvalidReason::Contradiction)
	);
	// The same absent source measuring nothing is a real answer.
	assert_eq!(refused(&|s| s.present = Tristate::No), Ok(()));
	// One alarm twice.
	assert_eq!(refused(&|s| s.alarms = alloc::vec![reported(AlarmKind::Overload, true), reported(AlarmKind::Overload, false)]), Err(InvalidReason::Contradiction));
	assert_eq!(refused(&|s| s.alarms = alloc::vec![reported(AlarmKind::Overload, true), derived(AlarmKind::Overload, Tristate::No)]), Ok(()), "reported and derived are two alarms");
	// One trip twice, and more trips than a source carries.
	assert_eq!(refused(&|s| s.trips = alloc::vec![trip(TripKind::Active, 1, absolute(1)), trip(TripKind::Active, 1, absolute(2))]), Err(InvalidReason::Contradiction));
	assert_eq!(refused(&|s| s.trips = (0..=MAX_TRIPS as u8).map(|index| trip(TripKind::Active, index, absolute(1))).collect()), Err(InvalidReason::Range));
	// Outlets without the switch, and the switch without outlets.
	assert_eq!(refused(&|s| s.controls.outlets = 2), Err(InvalidReason::Contradiction));
	assert_eq!(refused(&|s| s.controls.set_output = true), Err(InvalidReason::Contradiction));
	assert_eq!(
		refused(&|s| {
			s.controls.set_output = true;
			s.controls.outlets = 2;
		}),
		Ok(())
	);
	// The charge state has its own invalid value, which is valid to publish: it is the contradiction,
	// reported as one.
	assert_eq!(refused(&|s| s.charge = ChargeState::Invalid), Ok(()));
}

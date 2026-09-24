//! THE CANONICAL RECORD: building one from tagged values, deriving what may be derived, and checking
//! that a record a provider published is one.
//!
//! DERIVATION IS NARROW ON PURPOSE. State of charge is remaining over full, never over design. A
//! capacity alarm is a known remaining capacity against a threshold the SOURCE provided, in the same
//! quantity; a temperature alarm is a known reading against a known trip on the same reference. No
//! percentage threshold is imposed, no hysteresis invented, and nothing here acts on an alarm: a
//! derived alarm is an observation, labelled as derived so nobody mistakes it for one the source
//! reported.

use alloc::vec::Vec;

use crate::convert::Tagged;
use crate::schema::{Alarm, AlarmKind, Capacity, InvalidReason, MeasureI64, MeasureU64, Provenance, Quantity, SourceState, Temperature, TemperatureReference, TripKind, TripPoint, Tristate, ValueState};

/// The most trip points one source carries: critical, hot, passive and ten active levels fit.
pub const MAX_TRIPS: usize = 16;
/// The most alarms one source carries: every kind, reported and derived.
pub const MAX_ALARMS: usize = 16;
/// Basis points in a whole.
pub const WHOLE: u64 = 10_000;

fn state_of<T>(tagged: &Tagged<T>) -> (ValueState, InvalidReason) {
	match tagged {
		Tagged::Known(_) => (ValueState::Known, InvalidReason::None),
		Tagged::Unknown => (ValueState::Unknown, InvalidReason::None),
		Tagged::Unsupported => (ValueState::Unsupported, InvalidReason::None),
		Tagged::Invalid(reason) => (ValueState::Invalid, *reason),
	}
}

pub fn measure_u64(tagged: Tagged<u64>) -> MeasureU64 {
	let (state, reason) = state_of(&tagged);
	MeasureU64 { state, value: tagged.known().unwrap_or(0), reason }
}

pub fn measure_i64(tagged: Tagged<i64>) -> MeasureI64 {
	let (state, reason) = state_of(&tagged);
	MeasureI64 { state, value: tagged.known().unwrap_or(0), reason }
}

pub fn capacity(quantity: Quantity, tagged: Tagged<u64>) -> Capacity {
	let (state, reason) = state_of(&tagged);
	Capacity { state, quantity, value: tagged.known().unwrap_or(0), reason }
}

pub fn temperature(reference: TemperatureReference, tagged: Tagged<i64>) -> Temperature {
	let (state, reason) = state_of(&tagged);
	Temperature { state, reference, value: tagged.known().unwrap_or(0), reason }
}

/// A capacity, back as a tagged value and its quantity.
pub fn capacity_value(capacity: &Capacity) -> Tagged<u64> {
	tagged_of(capacity.state, capacity.value, capacity.reason)
}

fn tagged_of<T>(state: ValueState, value: T, reason: InvalidReason) -> Tagged<T> {
	match state {
		ValueState::Known => Tagged::Known(value),
		ValueState::Unknown => Tagged::Unknown,
		ValueState::Unsupported => Tagged::Unsupported,
		ValueState::Invalid => Tagged::Invalid(reason),
	}
}

/// A record in which nothing is measured: the starting point every adapter fills in, so a field an
/// adapter does not mention is UNSUPPORTED rather than a zero somebody could read as a measurement.
pub fn unmeasured(kind: crate::schema::SourceKind) -> SourceState {
	let unsupported_capacity = capacity(Quantity::Energy, Tagged::Unsupported);
	SourceState { kind, present: Tristate::Unknown, online: Tristate::Unknown, charge: crate::schema::ChargeState::Unknown, remaining: unsupported_capacity.clone(), full: unsupported_capacity.clone(), design: unsupported_capacity, state_of_charge: measure_u64(Tagged::Unsupported), voltage: measure_u64(Tagged::Unsupported), current: measure_i64(Tagged::Unsupported), power: measure_i64(Tagged::Unsupported), runtime: measure_u64(Tagged::Unsupported), load: measure_u64(Tagged::Unsupported), temperature: temperature(TemperatureReference::Absolute, Tagged::Unsupported), trips: Vec::new(), alarms: Vec::new(), controls: crate::schema::Controls { set_output: false, schedule_off: false, cancel_off: false, outlets: 0 }, source_time: None }
}

/// State of charge in basis points.
///
/// A VALID EXPLICIT PERCENTAGE IS PRESERVED - the source measured it, and dividing two of its other
/// numbers would be a second opinion. Otherwise remaining over full, when both are known, in the same
/// quantity, and full is nonzero. NEVER OVER DESIGN CAPACITY: design is what the battery was when it
/// was new, and a worn battery at full charge is full. More remaining than full is the source
/// contradicting itself, and is invalid rather than clamped to a healthy hundred percent.
pub fn state_of_charge(explicit: Option<Tagged<u64>>, remaining: &Capacity, full: &Capacity) -> Tagged<u64> {
	match explicit {
		Some(Tagged::Known(basis)) if basis <= WHOLE => return Tagged::Known(basis),
		Some(Tagged::Known(_)) => return Tagged::Invalid(InvalidReason::Range),
		_ => {}
	}
	match (capacity_value(remaining), capacity_value(full)) {
		(Tagged::Known(r), Tagged::Known(f)) if remaining.quantity == full.quantity && f != 0 => {
			if r > f {
				Tagged::Invalid(InvalidReason::Contradiction)
			} else {
				Tagged::Known(((r as u128 * WHOLE as u128) / f as u128) as u64)
			}
		}
		// Nothing to derive from: an explicit value that was itself invalid says so; otherwise the
		// fraction is simply not known.
		_ => match explicit {
			Some(Tagged::Invalid(reason)) => Tagged::Invalid(reason),
			_ => Tagged::Unknown,
		},
	}
}

fn tristate(active: Option<bool>) -> Tristate {
	match active {
		Some(true) => Tristate::Yes,
		Some(false) => Tristate::No,
		None => Tristate::Unknown,
	}
}

/// A DERIVED capacity alarm: known remaining AT OR BELOW a same-quantity threshold the source gave.
/// `<=` because the threshold is the level at which the source says the condition begins. A value or
/// threshold that is not known, or one in the other quantity, is unknown.
pub fn below_threshold(remaining: &Capacity, threshold: &Capacity) -> Tristate {
	match (capacity_value(remaining), capacity_value(threshold)) {
		(Tagged::Known(r), Tagged::Known(t)) if remaining.quantity == threshold.quantity => tristate(Some(r <= t)),
		_ => Tristate::Unknown,
	}
}

/// A DERIVED temperature alarm: a known reading AT OR ABOVE a known trip ON THE SAME REFERENCE. An
/// absolute reading is never compared with a relative trip.
pub fn at_or_above(reading: &Temperature, trip: &Temperature) -> Tristate {
	let known = |t: &Temperature| if t.state == ValueState::Known { Some(t.value) } else { None };
	match (known(reading), known(trip)) {
		(Some(r), Some(t)) if reading.reference == trip.reference => tristate(Some(r >= t)),
		_ => Tristate::Unknown,
	}
}

pub fn reported(kind: AlarmKind, active: bool) -> Alarm {
	Alarm { kind, state: tristate(Some(active)), provenance: Provenance::Reported }
}

pub fn derived(kind: AlarmKind, state: Tristate) -> Alarm {
	Alarm { kind, state, provenance: Provenance::Derived }
}

pub fn trip(kind: TripKind, index: u8, temperature: Temperature) -> TripPoint {
	TripPoint { kind, index, temperature }
}

/// WHETHER AN UPDATE CHANGES AN ALARM: any alarm entering, leaving or changing state. Such an update is
/// never coalesced into a later one - a subscriber either sees the transition or loses the
/// subscription.
pub fn alarm_transition(before: &SourceState, after: &SourceState) -> bool {
	let key = |alarm: &Alarm| (alarm.kind as u8, alarm.provenance as u8);
	let state_in = |alarms: &[Alarm], wanted: (u8, u8)| alarms.iter().find(|alarm| key(alarm) == wanted).map(|alarm| alarm.state);
	before.alarms.iter().any(|alarm| state_in(&after.alarms, key(alarm)) != Some(alarm.state)) || after.alarms.iter().any(|alarm| state_in(&before.alarms, key(alarm)).is_none())
}

// ------------------------------------------------------------------ validation

fn check_tag(state: ValueState, reason: InvalidReason, zero: bool) -> Result<(), InvalidReason> {
	match state {
		// A known value carries no reason; an invalid one carries one.
		ValueState::Known if reason == InvalidReason::None => Ok(()),
		ValueState::Invalid if reason != InvalidReason::None && zero => Ok(()),
		// Nothing but a known value has a number, so a record cannot smuggle one behind a tag.
		ValueState::Unknown | ValueState::Unsupported if reason == InvalidReason::None && zero => Ok(()),
		_ => Err(InvalidReason::Contradiction),
	}
}

/// WHETHER A PUBLISHED RECORD IS CANONICAL. PowerService calls this on everything a provider sends and
/// converts nothing itself: a record failing it is refused, and the provider with it.
///
/// Every measurement's tag agrees with its number and reason; a state of charge is at most a whole; an
/// absent source measures nothing; trips and alarms are bounded and never name one threshold or one
/// alarm twice; outlets are advertised exactly when switching them is.
pub fn validate(state: &SourceState) -> Result<(), InvalidReason> {
	let unsigned = [&state.state_of_charge, &state.voltage, &state.runtime, &state.load];
	for measure in unsigned {
		check_tag(measure.state, measure.reason, measure.value == 0)?;
	}
	for measure in [&state.current, &state.power] {
		check_tag(measure.state, measure.reason, measure.value == 0)?;
	}
	for capacity in [&state.remaining, &state.full, &state.design] {
		check_tag(capacity.state, capacity.reason, capacity.value == 0)?;
	}
	check_tag(state.temperature.state, state.temperature.reason, state.temperature.value == 0)?;
	if state.state_of_charge.state == ValueState::Known && state.state_of_charge.value > WHOLE {
		return Err(InvalidReason::Range);
	}
	if state.trips.len() > MAX_TRIPS || state.alarms.len() > MAX_ALARMS {
		return Err(InvalidReason::Range);
	}
	for (at, trip) in state.trips.iter().enumerate() {
		check_tag(trip.temperature.state, trip.temperature.reason, trip.temperature.value == 0)?;
		if state.trips[..at].iter().any(|earlier| earlier.kind == trip.kind && earlier.index == trip.index) {
			return Err(InvalidReason::Contradiction);
		}
	}
	for (at, alarm) in state.alarms.iter().enumerate() {
		if state.alarms[..at].iter().any(|earlier| earlier.kind == alarm.kind && earlier.provenance == alarm.provenance) {
			return Err(InvalidReason::Contradiction);
		}
	}
	// AN ABSENT SOURCE MEASURES NOTHING. It is still a source - a battery slot with no battery - and
	// distinct from a present one whose measurements are unknown, which is exactly why it cannot carry
	// a known value.
	if state.present == Tristate::No {
		let known = [
			state.state_of_charge.state,
			state.voltage.state,
			state.current.state,
			state.power.state,
			state.runtime.state,
			state.load.state,
			state.remaining.state,
			state.full.state,
			state.design.state,
			state.temperature.state,
		];
		if known.contains(&ValueState::Known) {
			return Err(InvalidReason::Contradiction);
		}
	}
	if state.controls.set_output != (state.controls.outlets > 0) {
		return Err(InvalidReason::Contradiction);
	}
	Ok(())
}

#[cfg(test)]
mod tests;

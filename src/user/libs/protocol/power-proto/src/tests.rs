use crate::codec::Handles;
use crate::generated::liber::base::v1::Error;
use crate::generated::liber::power::v1::{Alarm, AlarmKind, Capacity, ChargeState, Controls, InvalidReason, MeasureI64, MeasureU64, PowerChange, Provenance, Quantity, SourceId, SourceKind, SourceSnapshot, SourceState, Temperature, TemperatureReference, TripKind, TripPoint, Tristate, ValueState, power};
use alloc::vec::Vec;

// THE STATE INTERFACE ANSWERS `sources` IN ONE REPLY, so the reply buffer PowerService serves it from has to hold the
// service's whole bound: 128 sources, each with every list at its wire bound and its own time given. This is the
// number that buffer is sized from; it was eight kibibytes, which held about twenty such sources.
const SOURCES: usize = 128;

fn largest() -> SourceSnapshot {
	let capacity = || Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: u64::MAX, reason: InvalidReason::None };
	let unsigned = || MeasureU64 { state: ValueState::Known, value: u64::MAX, reason: InvalidReason::None };
	let signed = || MeasureI64 { state: ValueState::Known, value: i64::MIN, reason: InvalidReason::None };
	let temperature = || Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: i64::MAX, reason: InvalidReason::None };
	SourceSnapshot { id: SourceId { slot: u32::MAX, generation: u32::MAX, binding_generation: u64::MAX, local: u32::MAX }, received: u64::MAX, state: SourceState { kind: SourceKind::Ups, present: Tristate::Yes, online: Tristate::Yes, charge: ChargeState::Charging, remaining: capacity(), full: capacity(), design: capacity(), state_of_charge: unsigned(), voltage: unsigned(), current: signed(), power: signed(), runtime: unsigned(), load: unsigned(), temperature: temperature(), trips: (0..16).map(|index| TripPoint { kind: TripKind::Active, index, temperature: temperature() }).collect(), alarms: (0..16).map(|_| Alarm { kind: AlarmKind::Overload, state: Tristate::Yes, provenance: Provenance::Derived }).collect(), controls: Controls { set_output: true, schedule_off: true, cancel_off: true, outlets: u8::MAX }, source_time: Some(u64::MAX) } }
}

#[test]
fn every_source_the_service_admits_is_one_sources_reply_of_under_sixty_four_kibibytes() {
	struct State;
	impl power::Service for State {
		fn sources(&mut self) -> Result<Vec<SourceSnapshot>, Error> {
			Ok((0..SOURCES).map(|_| largest()).collect())
		}
		fn subscribe(&mut self) -> Result<Vec<PowerChange>, Error> {
			Ok(Vec::new())
		}
	}
	let mut request = Vec::new();
	request.extend_from_slice(&power::OP_SOURCES.to_le_bytes());
	request.extend_from_slice(&7u32.to_le_bytes());
	let mut out = alloc::vec![0u8; 128 * 1024];
	let written = power::dispatch(&mut State, &request, &mut Handles::new(), &mut out, &mut Handles::new()).expect("every source at its bound is one reply");
	let one = largest().encode_vec().unwrap().len();
	assert_eq!(one, 409, "a snapshot at its record's bounds");
	assert_eq!(written, 7 + SOURCES * one, "the correlation, the result tag, the count and the snapshots");
	assert!(written <= 64 * 1024, "PowerService serves the state interface from sixty-four kibibytes: {written}");
	// And the reply it used to answer from held this many.
	assert_eq!((8192 - 7) / one, 20);
}

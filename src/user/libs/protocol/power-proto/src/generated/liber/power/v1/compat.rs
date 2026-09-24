use super::*;
use alloc::string::String;

#[test]
fn source_kind_wire_is_stable() {
	let sample = SourceKind::Ac;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(SourceKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn value_state_wire_is_stable() {
	let sample = ValueState::Known;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ValueState::decode(&bytes).unwrap(), sample);
}
#[test]
fn invalid_reason_wire_is_stable() {
	let sample = InvalidReason::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(InvalidReason::decode(&bytes).unwrap(), sample);
}
#[test]
fn tristate_wire_is_stable() {
	let sample = Tristate::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Tristate::decode(&bytes).unwrap(), sample);
}
#[test]
fn measure_u64_wire_is_stable() {
	let sample = MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(MeasureU64::decode(&bytes).unwrap(), sample);
}
#[test]
fn measure_i64_wire_is_stable() {
	let sample = MeasureI64 { state: ValueState::Known, value: 7, reason: InvalidReason::None };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(MeasureI64::decode(&bytes).unwrap(), sample);
}
#[test]
fn quantity_wire_is_stable() {
	let sample = Quantity::Energy;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Quantity::decode(&bytes).unwrap(), sample);
}
#[test]
fn capacity_wire_is_stable() {
	let sample = Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 7, 0, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Capacity::decode(&bytes).unwrap(), sample);
}
#[test]
fn charge_state_wire_is_stable() {
	let sample = ChargeState::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(ChargeState::decode(&bytes).unwrap(), sample);
}
#[test]
fn temperature_reference_wire_is_stable() {
	let sample = TemperatureReference::Absolute;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(TemperatureReference::decode(&bytes).unwrap(), sample);
}
#[test]
fn temperature_wire_is_stable() {
	let sample = Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 7, 0, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Temperature::decode(&bytes).unwrap(), sample);
}
#[test]
fn trip_kind_wire_is_stable() {
	let sample = TripKind::Passive;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(TripKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn trip_point_wire_is_stable() {
	let sample = TripPoint { kind: TripKind::Passive, index: 7, temperature: Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 1, 1, 7, 0, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(TripPoint::decode(&bytes).unwrap(), sample);
}
#[test]
fn alarm_kind_wire_is_stable() {
	let sample = AlarmKind::OnBattery;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(AlarmKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn provenance_wire_is_stable() {
	let sample = Provenance::Reported;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Provenance::decode(&bytes).unwrap(), sample);
}
#[test]
fn alarm_wire_is_stable() {
	let sample = Alarm { kind: AlarmKind::OnBattery, state: Tristate::Unknown, provenance: Provenance::Reported };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(Alarm::decode(&bytes).unwrap(), sample);
}
#[test]
fn controls_wire_is_stable() {
	let sample = Controls { set_output: true, schedule_off: true, cancel_off: true, outlets: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 1, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Controls::decode(&bytes).unwrap(), sample);
}
#[test]
fn source_state_wire_is_stable() {
	let sample = SourceState { kind: SourceKind::Ac, present: Tristate::Unknown, online: Tristate::Unknown, charge: ChargeState::Unknown, remaining: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, full: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, design: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, state_of_charge: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, voltage: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, current: MeasureI64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, power: MeasureI64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, runtime: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, load: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, temperature: Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None }, trips: alloc::vec![TripPoint { kind: TripKind::Passive, index: 7, temperature: Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None } }], alarms: alloc::vec![Alarm { kind: AlarmKind::OnBattery, state: Tristate::Unknown, provenance: Provenance::Reported }], controls: Controls { set_output: true, schedule_off: true, cancel_off: true, outlets: 7 }, source_time: Some(7) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		1,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		1,
		7,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		1,
		0,
		1,
		1,
		1,
		1,
		7,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(SourceState::decode(&bytes).unwrap(), sample);
}
#[test]
fn source_id_wire_is_stable() {
	let sample = SourceId { slot: 7, generation: 7, binding_generation: 7, local: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(SourceId::decode(&bytes).unwrap(), sample);
}
#[test]
fn source_snapshot_wire_is_stable() {
	let sample = SourceSnapshot { id: SourceId { slot: 7, generation: 7, binding_generation: 7, local: 7 }, received: 7, state: SourceState { kind: SourceKind::Ac, present: Tristate::Unknown, online: Tristate::Unknown, charge: ChargeState::Unknown, remaining: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, full: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, design: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, state_of_charge: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, voltage: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, current: MeasureI64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, power: MeasureI64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, runtime: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, load: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, temperature: Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None }, trips: alloc::vec![TripPoint { kind: TripKind::Passive, index: 7, temperature: Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None } }], alarms: alloc::vec![Alarm { kind: AlarmKind::OnBattery, state: Tristate::Unknown, provenance: Provenance::Reported }], controls: Controls { set_output: true, schedule_off: true, cancel_off: true, outlets: 7 }, source_time: Some(7) } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		1,
		7,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		1,
		0,
		1,
		1,
		1,
		1,
		7,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(SourceSnapshot::decode(&bytes).unwrap(), sample);
}
#[test]
fn change_kind_wire_is_stable() {
	let sample = ChangeKind::Snapshot;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ChangeKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn power_change_wire_is_stable() {
	let sample = PowerChange { epoch: 7, revision: 7, kind: ChangeKind::Snapshot, source: Some(SourceSnapshot { id: SourceId { slot: 7, generation: 7, binding_generation: 7, local: 7 }, received: 7, state: SourceState { kind: SourceKind::Ac, present: Tristate::Unknown, online: Tristate::Unknown, charge: ChargeState::Unknown, remaining: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, full: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, design: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, state_of_charge: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, voltage: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, current: MeasureI64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, power: MeasureI64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, runtime: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, load: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, temperature: Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None }, trips: alloc::vec![TripPoint { kind: TripKind::Passive, index: 7, temperature: Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None } }], alarms: alloc::vec![Alarm { kind: AlarmKind::OnBattery, state: Tristate::Unknown, provenance: Provenance::Reported }], controls: Controls { set_output: true, schedule_off: true, cancel_off: true, outlets: 7 }, source_time: Some(7) } }), gone: Some(SourceId { slot: 7, generation: 7, binding_generation: 7, local: 7 }) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		1,
		7,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		1,
		0,
		1,
		1,
		1,
		1,
		7,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(PowerChange::decode(&bytes).unwrap(), sample);
}
#[test]
fn control_outcome_wire_is_stable() {
	let sample = ControlOutcome::Done;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ControlOutcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn provider_source_wire_is_stable() {
	let sample = ProviderSource { local: 7, state: SourceState { kind: SourceKind::Ac, present: Tristate::Unknown, online: Tristate::Unknown, charge: ChargeState::Unknown, remaining: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, full: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, design: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, state_of_charge: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, voltage: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, current: MeasureI64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, power: MeasureI64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, runtime: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, load: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, temperature: Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None }, trips: alloc::vec![TripPoint { kind: TripKind::Passive, index: 7, temperature: Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None } }], alarms: alloc::vec![Alarm { kind: AlarmKind::OnBattery, state: Tristate::Unknown, provenance: Provenance::Reported }], controls: Controls { set_output: true, schedule_off: true, cancel_off: true, outlets: 7 }, source_time: Some(7) } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		0,
		0,
		0,
		1,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		1,
		7,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		1,
		0,
		1,
		1,
		1,
		1,
		7,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(ProviderSource::decode(&bytes).unwrap(), sample);
}
#[test]
fn provider_update_kind_wire_is_stable() {
	let sample = ProviderUpdateKind::Snapshot;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ProviderUpdateKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn provider_update_wire_is_stable() {
	let sample = ProviderUpdate { revision: 7, kind: ProviderUpdateKind::Snapshot, source: Some(ProviderSource { local: 7, state: SourceState { kind: SourceKind::Ac, present: Tristate::Unknown, online: Tristate::Unknown, charge: ChargeState::Unknown, remaining: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, full: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, design: Capacity { state: ValueState::Known, quantity: Quantity::Energy, value: 7, reason: InvalidReason::None }, state_of_charge: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, voltage: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, current: MeasureI64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, power: MeasureI64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, runtime: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, load: MeasureU64 { state: ValueState::Known, value: 7, reason: InvalidReason::None }, temperature: Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None }, trips: alloc::vec![TripPoint { kind: TripKind::Passive, index: 7, temperature: Temperature { state: ValueState::Known, reference: TemperatureReference::Absolute, value: 7, reason: InvalidReason::None } }], alarms: alloc::vec![Alarm { kind: AlarmKind::OnBattery, state: Tristate::Unknown, provenance: Provenance::Reported }], controls: Controls { set_output: true, schedule_off: true, cancel_off: true, outlets: 7 }, source_time: Some(7) } }), gone: Some(7) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		1,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		1,
		7,
		1,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		0,
		1,
		0,
		1,
		1,
		1,
		1,
		7,
		1,
		7,
		0,
		0,
		0,
		0,
		0,
		0,
		0,
		1,
		7,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(ProviderUpdate::decode(&bytes).unwrap(), sample);
}
#[test]
fn provider_command_kind_wire_is_stable() {
	let sample = ProviderCommandKind::SetOutput;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ProviderCommandKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn provider_command_wire_is_stable() {
	let sample = ProviderCommand { kind: ProviderCommandKind::SetOutput, local: 7, outlet: 7, on: true, delay_seconds: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 7, 1, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ProviderCommand::decode(&bytes).unwrap(), sample);
}
#[test]
fn fixture_field_wire_is_stable() {
	let sample = FixtureField::UpsRemaining;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(FixtureField::decode(&bytes).unwrap(), sample);
}
#[test]
fn fixture_command_wire_is_stable() {
	let sample = FixtureCommand { publication: 7, local: 7, kind: ProviderCommandKind::SetOutput, outlet: 7, on: true, delay_seconds: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 0, 0, 0, 1, 7, 1, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(FixtureCommand::decode(&bytes).unwrap(), sample);
}

use super::*;
use alloc::string::String;

#[test]
fn modem_id_wire_is_stable() {
	let sample = ModemId { slot: 7, generation: 7, binding_generation: 7, incarnation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ModemId::decode(&bytes).unwrap(), sample);
}
#[test]
fn context_id_wire_is_stable() {
	let sample = ContextId { modem: ModemId { slot: 7, generation: 7, binding_generation: 7, incarnation: 7 }, sim_generation: 7, context_generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ContextId::decode(&bytes).unwrap(), sample);
}
#[test]
fn sim_state_wire_is_stable() {
	let sample = SimState::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(SimState::decode(&bytes).unwrap(), sample);
}
#[test]
fn registration_wire_is_stable() {
	let sample = Registration::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Registration::decode(&bytes).unwrap(), sample);
}
#[test]
fn signal_wire_is_stable() {
	let sample = Signal { valid: true, rssi_dbm: 7, quality: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(Signal::decode(&bytes).unwrap(), sample);
}
#[test]
fn attempt_count_wire_is_stable() {
	let sample = AttemptCount { known: true, remaining: 7, sim_generation: 7, observed_ms: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(AttemptCount::decode(&bytes).unwrap(), sample);
}
#[test]
fn attempts_wire_is_stable() {
	let sample = Attempts { pin: AttemptCount { known: true, remaining: 7, sim_generation: 7, observed_ms: 7 }, puk: AttemptCount { known: true, remaining: 7, sim_generation: 7, observed_ms: 7 } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 7, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Attempts::decode(&bytes).unwrap(), sample);
}
#[test]
fn context_state_wire_is_stable() {
	let sample = ContextState::Inactive;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(ContextState::decode(&bytes).unwrap(), sample);
}
#[test]
fn context_status_wire_is_stable() {
	let sample = ContextStatus { id: ContextId { modem: ModemId { slot: 7, generation: 7, binding_generation: 7, incarnation: 7 }, sim_generation: 7, context_generation: 7 }, state: ContextState::Inactive, address: Some(7), prefix: 7, mtu: 7, transmit_refused: 7, receive_dropped: 7 };
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
		7,
		7,
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
		0,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(ContextStatus::decode(&bytes).unwrap(), sample);
}
#[test]
fn modem_status_wire_is_stable() {
	let sample = ModemStatus { id: ModemId { slot: 7, generation: 7, binding_generation: 7, incarnation: 7 }, manufacturer: String::from("x"), model: String::from("x"), sim: SimState::Unknown, sim_generation: 7, registration: Registration::Unknown, operator: Some(String::from("x")), signal: Signal { valid: true, rssi_dbm: 7, quality: 7 }, attempts: Attempts { pin: AttemptCount { known: true, remaining: 7, sim_generation: 7, observed_ms: 7 }, puk: AttemptCount { known: true, remaining: 7, sim_generation: 7, observed_ms: 7 } }, context: Some(ContextStatus { id: ContextId { modem: ModemId { slot: 7, generation: 7, binding_generation: 7, incarnation: 7 }, sim_generation: 7, context_generation: 7 }, state: ContextState::Inactive, address: Some(7), prefix: 7, mtu: 7, transmit_refused: 7, receive_dropped: 7 }), revision: 7 };
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
		0,
		0,
		0,
		0,
		1,
		0,
		120,
		1,
		0,
		120,
		0,
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
		0,
		120,
		1,
		7,
		0,
		0,
		0,
		7,
		1,
		7,
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
		7,
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
		7,
		7,
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
	];
	assert_eq!(bytes, golden);
	assert_eq!(ModemStatus::decode(&bytes).unwrap(), sample);
}
#[test]
fn modem_limits_wire_is_stable() {
	let sample = ModemLimits { providers: 7, providers_max: 7, clients: 7, clients_max: 7, contexts: 7, contexts_max: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 7, 7, 7, 7];
	assert_eq!(bytes, golden);
	assert_eq!(ModemLimits::decode(&bytes).unwrap(), sample);
}
#[test]
fn watch_kind_wire_is_stable() {
	let sample = WatchKind::Snapshot;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(WatchKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn modem_watch_wire_is_stable() {
	let sample = ModemWatch { kind: WatchKind::Snapshot, modems: alloc::vec![ModemStatus { id: ModemId { slot: 7, generation: 7, binding_generation: 7, incarnation: 7 }, manufacturer: String::from("x"), model: String::from("x"), sim: SimState::Unknown, sim_generation: 7, registration: Registration::Unknown, operator: Some(String::from("x")), signal: Signal { valid: true, rssi_dbm: 7, quality: 7 }, attempts: Attempts { pin: AttemptCount { known: true, remaining: 7, sim_generation: 7, observed_ms: 7 }, puk: AttemptCount { known: true, remaining: 7, sim_generation: 7, observed_ms: 7 } }, context: Some(ContextStatus { id: ContextId { modem: ModemId { slot: 7, generation: 7, binding_generation: 7, incarnation: 7 }, sim_generation: 7, context_generation: 7 }, state: ContextState::Inactive, address: Some(7), prefix: 7, mtu: 7, transmit_refused: 7, receive_dropped: 7 }), revision: 7 }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		1,
		1,
		0,
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
		0,
		0,
		0,
		0,
		1,
		0,
		120,
		1,
		0,
		120,
		0,
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
		0,
		120,
		1,
		7,
		0,
		0,
		0,
		7,
		1,
		7,
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
		7,
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
		7,
		7,
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
	];
	assert_eq!(bytes, golden);
	assert_eq!(ModemWatch::decode(&bytes).unwrap(), sample);
}
#[test]
fn identity_wire_is_stable() {
	let sample = Identity { imsi: Some(String::from("x")), iccid: Some(String::from("x")), msisdn: Some(String::from("x")) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 120, 1, 1, 0, 120, 1, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(Identity::decode(&bytes).unwrap(), sample);
}
#[test]
fn sim_pin_result_wire_is_stable() {
	let sample = SimPinResult::Accepted;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(SimPinResult::decode(&bytes).unwrap(), sample);
}
#[test]
fn sim_pin_outcome_wire_is_stable() {
	let sample = SimPinOutcome { result: SimPinResult::Accepted, attempts: Attempts { pin: AttemptCount { known: true, remaining: 7, sim_generation: 7, observed_ms: 7 }, puk: AttemptCount { known: true, remaining: 7, sim_generation: 7, observed_ms: 7 } } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 7, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 7, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(SimPinOutcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn grant_kind_wire_is_stable() {
	let sample = GrantKind::Data;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(GrantKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn data_policy_wire_is_stable() {
	let sample = DataPolicy { apn: String::from("x"), replace_uplink: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1];
	assert_eq!(bytes, golden);
	assert_eq!(DataPolicy::decode(&bytes).unwrap(), sample);
}

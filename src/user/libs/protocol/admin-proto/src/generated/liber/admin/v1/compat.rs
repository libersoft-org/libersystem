use super::*;
use alloc::string::String;

#[test]
fn admin_action_wire_is_stable() {
	let sample = AdminAction::FirmwareDownload;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(AdminAction::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_descriptor_wire_is_stable() {
	let sample = AdminDescriptor { version: 7, action: AdminAction::FirmwareDownload, executor: String::from("x"), executor_epoch: 7, target: String::from("x"), target_generation: 7, parameters: alloc::vec![7], payload_length: 7, payload_digest: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7, 7, 0, 0, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(AdminDescriptor::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_request_args_wire_is_stable() {
	let sample = AdminRequestArgs { action: AdminAction::FirmwareDownload, target: String::from("x"), parameters: alloc::vec![7], payload_length: 7, label: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 120, 1, 0, 7, 7, 0, 0, 0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(AdminRequestArgs::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_answer_wire_is_stable() {
	let sample = AdminAnswer::Declined;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(AdminAnswer::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_result_wire_is_stable() {
	let sample = AdminResult::Completed;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(AdminResult::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_scope_wire_is_stable() {
	let sample = AdminScope { actions: alloc::vec![AdminAction::FirmwareDownload], target_prefix: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 1, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(AdminScope::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_prepared_wire_is_stable() {
	let sample = AdminPrepared { operation: 7, descriptor: AdminDescriptor { version: 7, action: AdminAction::FirmwareDownload, executor: String::from("x"), executor_epoch: 7, target: String::from("x"), target_generation: 7, parameters: alloc::vec![7], payload_length: 7, payload_digest: alloc::vec![7] }, deadline_ms: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 1, 1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7, 7, 0, 0, 0, 1, 0, 7, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(AdminPrepared::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_event_wire_is_stable() {
	let sample = AdminEvent::Requested;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(AdminEvent::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_record_wire_is_stable() {
	let sample = AdminRecord { sequence: 7, broker_epoch: 7, request: 7, launch: 7, requester: String::from("x"), action: 7, digest: alloc::vec![7], event: AdminEvent::Requested, reason: String::from("x"), monotonic_ns: 7, utc_seconds: Some(7), utc_provenance: String::from("x") };
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
		7,
		1,
		0,
		7,
		1,
		1,
		0,
		120,
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
		0,
		0,
		0,
		0,
		1,
		0,
		120,
	];
	assert_eq!(bytes, golden);
	assert_eq!(AdminRecord::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_journal_page_wire_is_stable() {
	let sample = AdminJournalPage { records: alloc::vec![AdminRecord { sequence: 7, broker_epoch: 7, request: 7, launch: 7, requester: String::from("x"), action: 7, digest: alloc::vec![7], event: AdminEvent::Requested, reason: String::from("x"), monotonic_ns: 7, utc_seconds: Some(7), utc_provenance: String::from("x") }], oldest: 7, next: 7, evicted: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		1,
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
		1,
		0,
		120,
		7,
		1,
		0,
		7,
		1,
		1,
		0,
		120,
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
		0,
		0,
		0,
		0,
		1,
		0,
		120,
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
	assert_eq!(AdminJournalPage::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_probe_effects_wire_is_stable() {
	let sample = AdminProbeEffects { writes: 7, dispatches: 7, digest: alloc::vec![7], generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 1, 0, 7, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(AdminProbeEffects::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_probe_fault_wire_is_stable() {
	let sample = AdminProbeFault::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(AdminProbeFault::decode(&bytes).unwrap(), sample);
}
#[test]
fn admin_journal_fault_wire_is_stable() {
	let sample = AdminJournalFault::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(AdminJournalFault::decode(&bytes).unwrap(), sample);
}

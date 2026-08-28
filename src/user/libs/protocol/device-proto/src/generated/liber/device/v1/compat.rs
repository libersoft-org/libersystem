use super::*;
use alloc::string::String;

#[test]
fn device_type_wire_is_stable() {
	let sample = DeviceType::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceType::decode(&bytes).unwrap(), sample);
}
#[test]
fn device_entry_wire_is_stable() {
	let sample = DeviceEntry { index: 7, r#type: DeviceType::Unknown, mmio_len: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceEntry::decode(&bytes).unwrap(), sample);
}
#[test]
fn binding_state_wire_is_stable() {
	let sample = BindingState::Unbound;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(BindingState::decode(&bytes).unwrap(), sample);
}
#[test]
fn failure_cause_wire_is_stable() {
	let sample = FailureCause::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(FailureCause::decode(&bytes).unwrap(), sample);
}
#[test]
fn binding_record_wire_is_stable() {
	let sample = BindingRecord { index: 7, bus: 7, dev: 7, func: 7, generation: 7, state: BindingState::Unbound, cause: FailureCause::None, attempts: 7, artifact: String::from("x"), rule: 7, providers: 7, resources: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(BindingRecord::decode(&bytes).unwrap(), sample);
}
#[test]
fn provider_kind_wire_is_stable() {
	let sample = ProviderKind::Block;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ProviderKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn provider_info_wire_is_stable() {
	let sample = ProviderInfo { kind: ProviderKind::Block, bus: 7, dev: 7, func: 7, binding_generation: 7, slot: 7, provider_generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ProviderInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn policy_verb_wire_is_stable() {
	let sample = PolicyVerb::Disable;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PolicyVerb::decode(&bytes).unwrap(), sample);
}
#[test]
fn policy_outcome_wire_is_stable() {
	let sample = PolicyOutcome::Accepted;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PolicyOutcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn incident_report_wire_is_stable() {
	let sample = IncidentReport { present: true, bus: 7, dev: 7, func: 7, generation: 7, state: BindingState::Unbound, cause: FailureCause::None, last_opcode: 7, silent_for: 7, attempts: 7, domain_known: true, memory_used: 7, memory_peak: 7, handles_used: 7, threads_used: 7, dma_used: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
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
		7,
		0,
		0,
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
		7,
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
	];
	assert_eq!(bytes, golden);
	assert_eq!(IncidentReport::decode(&bytes).unwrap(), sample);
}
#[test]
fn usb_device_wire_is_stable() {
	let sample = UsbDevice { port: 7, speed: String::from("x"), vendor: 7, product: 7, class: 7, r#type: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(UsbDevice::decode(&bytes).unwrap(), sample);
}

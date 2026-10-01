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
	let sample = DeviceEntry { index: 7, r#type: DeviceType::Unknown, mmio_len: 7, bus: 7, dev: 7, func: 7, present: true, kind: RowKind::Pci, source: PlatformSource::None, state: PlatformState::None, identity: String::from("x"), ids: alloc::vec![PlatformId { kind: PlatformIdKind::None, text: String::from("x") }], resources: alloc::vec![PlatformResource { kind: PlatformResourceKind::Mmio, base: 7, length: 7, level: true, active_low: true, controller: 7 }], unresolved: true, companion: String::from("x"), parent: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		7,
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
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		1,
		0,
		0,
		0,
		1,
		0,
		120,
		1,
		0,
		0,
		1,
		0,
		120,
		1,
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
		1,
		7,
		0,
		0,
		0,
		1,
		1,
		0,
		120,
		1,
		0,
		120,
	];
	assert_eq!(bytes, golden);
	assert_eq!(DeviceEntry::decode(&bytes).unwrap(), sample);
}
#[test]
fn row_kind_wire_is_stable() {
	let sample = RowKind::Pci;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(RowKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn platform_source_wire_is_stable() {
	let sample = PlatformSource::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PlatformSource::decode(&bytes).unwrap(), sample);
}
#[test]
fn platform_state_wire_is_stable() {
	let sample = PlatformState::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PlatformState::decode(&bytes).unwrap(), sample);
}
#[test]
fn platform_id_kind_wire_is_stable() {
	let sample = PlatformIdKind::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PlatformIdKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn platform_id_wire_is_stable() {
	let sample = PlatformId { kind: PlatformIdKind::None, text: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(PlatformId::decode(&bytes).unwrap(), sample);
}
#[test]
fn platform_resource_kind_wire_is_stable() {
	let sample = PlatformResourceKind::Mmio;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PlatformResourceKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn platform_resource_wire_is_stable() {
	let sample = PlatformResource { kind: PlatformResourceKind::Mmio, base: 7, length: 7, level: true, active_low: true, controller: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 1, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(PlatformResource::decode(&bytes).unwrap(), sample);
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
	let sample = BindingRecord { index: 7, bus: 7, dev: 7, func: 7, generation: 7, state: BindingState::Unbound, cause: FailureCause::None, attempts: 7, artifact: String::from("x"), rule: 7, providers: 7, resources: 7, platform: Some(7) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 1, 7, 0, 0, 0];
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
	let sample = ProviderInfo { kind: ProviderKind::Block, bus: 7, dev: 7, func: 7, binding_generation: 7, slot: 7, provider_generation: 7, live: true, name: String::from("x"), platform: Some(7) };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 1, 1, 0, 120, 1, 7, 0, 0, 0];
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
	let sample = IncidentReport { present: true, bus: 7, dev: 7, func: 7, generation: 7, state: BindingState::Unbound, cause: FailureCause::None, last_opcode: 7, silent_for: 7, attempts: 7, domain_known: true, memory_used: 7, memory_peak: 7, handles_used: 7, threads_used: 7, dma_used: 7, platform: Some(7) };
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
		1,
		7,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(IncidentReport::decode(&bytes).unwrap(), sample);
}
#[test]
fn hci_packet_kind_wire_is_stable() {
	let sample = HciPacketKind::Command;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(HciPacketKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn hci_attachment_wire_is_stable() {
	let sample = HciAttachment { version: 7, iso: true, max_command: 7, max_event: 7, max_acl: 7, max_iso: 7, command_credits: 7, acl_credits: 7, acl_queue: 7, epoch: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(HciAttachment::decode(&bytes).unwrap(), sample);
}
#[test]
fn hci_packet_wire_is_stable() {
	let sample = HciPacket { kind: HciPacketKind::Command, epoch: 7, bytes: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(HciPacket::decode(&bytes).unwrap(), sample);
}
#[test]
fn hci_control_kind_wire_is_stable() {
	let sample = HciControlKind::Reset;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(HciControlKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn hci_control_event_wire_is_stable() {
	let sample = HciControlEvent { kind: HciControlKind::Reset, epoch: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(HciControlEvent::decode(&bytes).unwrap(), sample);
}
#[test]
fn console_attachment_wire_is_stable() {
	let sample = ConsoleAttachment { version: 7, max_frame: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ConsoleAttachment::decode(&bytes).unwrap(), sample);
}
#[test]
fn console_chunk_wire_is_stable() {
	let sample = ConsoleChunk { bytes: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(ConsoleChunk::decode(&bytes).unwrap(), sample);
}
#[test]
fn usb_device_wire_is_stable() {
	let sample = UsbDevice { port: 7, speed: String::from("x"), vendor: 7, product: 7, class: 7, r#type: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(UsbDevice::decode(&bytes).unwrap(), sample);
}
#[test]
fn switch_kind_wire_is_stable() {
	let sample = SwitchKind::Lid;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(SwitchKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn switch_state_wire_is_stable() {
	let sample = SwitchState { kind: SwitchKind::Lid, closed: true, sequence: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(SwitchState::decode(&bytes).unwrap(), sample);
}
#[test]
fn watchdog_description_wire_is_stable() {
	let sample = WatchdogDescription { device: String::from("x"), min_timeout_ms: 7, max_timeout_ms: 7, granularity_ms: 7, can_disarm: true, survives_reset: true, stops_in_suspend_to_idle: true, stops_in_s3: true, running_at_bind: true, last_reset_was_watchdog: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 1, 1, 1, 1, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(WatchdogDescription::decode(&bytes).unwrap(), sample);
}
#[test]
fn acpi_notification_wire_is_stable() {
	let sample = AcpiNotification { value: 7, sequence: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(AcpiNotification::decode(&bytes).unwrap(), sample);
}
#[test]
fn processor_register_wire_is_stable() {
	let sample = ProcessorRegister { space: 7, bits: 7, address: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ProcessorRegister::decode(&bytes).unwrap(), sample);
}
#[test]
fn processor_idle_state_wire_is_stable() {
	let sample = ProcessorIdleState { entry: 7, hint: 7, register: ProcessorRegister { space: 7, bits: 7, address: 7 }, latency_us: 7, residency_us: 7, power_mw: 7, flags: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 7, 0, 0, 0, 7, 7, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ProcessorIdleState::decode(&bytes).unwrap(), sample);
}
#[test]
fn processor_performance_state_wire_is_stable() {
	let sample = ProcessorPerformanceState { core_mhz: 7, power_mw: 7, latency_us: 7, control: 7, status: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ProcessorPerformanceState::decode(&bytes).unwrap(), sample);
}
#[test]
fn processor_throttling_state_wire_is_stable() {
	let sample = ProcessorThrottlingState { percent: 7, power_mw: 7, latency_us: 7, control: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ProcessorThrottlingState::decode(&bytes).unwrap(), sample);
}
#[test]
fn processor_domain_wire_is_stable() {
	let sample = ProcessorDomain { domain: 7, coordination: 7, processors: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ProcessorDomain::decode(&bytes).unwrap(), sample);
}
#[test]
fn processor_cppc_wire_is_stable() {
	let sample = ProcessorCppc { highest: 7, nominal: 7, lowest: 7, desired: ProcessorRegister { space: 7, bits: 7, address: 7 }, minimum: Some(ProcessorRegister { space: 7, bits: 7, address: 7 }), maximum: Some(ProcessorRegister { space: 7, bits: 7, address: 7 }), preference: Some(ProcessorRegister { space: 7, bits: 7, address: 7 }) };
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
		7,
		7,
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
	assert_eq!(ProcessorCppc::decode(&bytes).unwrap(), sample);
}
#[test]
fn processor_id_wire_is_stable() {
	let sample = ProcessorId { path: String::from("x"), uid: 7, cpu: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ProcessorId::decode(&bytes).unwrap(), sample);
}
#[test]
fn processor_power_wire_is_stable() {
	let sample = ProcessorPower { id: ProcessorId { path: String::from("x"), uid: 7, cpu: 7 }, idle: alloc::vec![ProcessorIdleState { entry: 7, hint: 7, register: ProcessorRegister { space: 7, bits: 7, address: 7 }, latency_us: 7, residency_us: 7, power_mw: 7, flags: 7 }], performance: alloc::vec![ProcessorPerformanceState { core_mhz: 7, power_mw: 7, latency_us: 7, control: 7, status: 7 }], pct_control: Some(ProcessorRegister { space: 7, bits: 7, address: 7 }), pct_status: Some(ProcessorRegister { space: 7, bits: 7, address: 7 }), ppc: 7, psd: Some(ProcessorDomain { domain: 7, coordination: 7, processors: 7 }), cppc: Some(ProcessorCppc { highest: 7, nominal: 7, lowest: 7, desired: ProcessorRegister { space: 7, bits: 7, address: 7 }, minimum: Some(ProcessorRegister { space: 7, bits: 7, address: 7 }), maximum: Some(ProcessorRegister { space: 7, bits: 7, address: 7 }), preference: Some(ProcessorRegister { space: 7, bits: 7, address: 7 }) }), throttling: alloc::vec![ProcessorThrottlingState { percent: 7, power_mw: 7, latency_us: 7, control: 7 }], ptc_control: Some(ProcessorRegister { space: 7, bits: 7, address: 7 }), tpc: 7, tsd: Some(ProcessorDomain { domain: 7, coordination: 7, processors: 7 }), refused: alloc::vec![String::from("x")] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		1,
		0,
		120,
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		1,
		0,
		7,
		7,
		0,
		0,
		0,
		7,
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
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		1,
		7,
		7,
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
		7,
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
		7,
		0,
		0,
		0,
		7,
		0,
		0,
		0,
		7,
		7,
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
		1,
		7,
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
		1,
		0,
		1,
		0,
		120,
	];
	assert_eq!(bytes, golden);
	assert_eq!(ProcessorPower::decode(&bytes).unwrap(), sample);
}
#[test]
fn processor_notification_wire_is_stable() {
	let sample = ProcessorNotification { path: String::from("x"), value: 7, sequence: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ProcessorNotification::decode(&bytes).unwrap(), sample);
}
#[test]
fn acpi_connection_kind_wire_is_stable() {
	let sample = AcpiConnectionKind::EventLine;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(AcpiConnectionKind::decode(&bytes).unwrap(), sample);
}

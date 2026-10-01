use super::*;
use alloc::string::String;

#[test]
fn process_info_wire_is_stable() {
	let sample = ProcessInfo { koid: 7, name: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(ProcessInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn resource_limits_wire_is_stable() {
	let sample = ResourceLimits { memory: 7, handles: 7, threads: 7, ipc_queue: 7, stack: 7, dma: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ResourceLimits::decode(&bytes).unwrap(), sample);
}
#[test]
fn shutdown_action_wire_is_stable() {
	let sample = ShutdownAction::PowerOff;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(ShutdownAction::decode(&bytes).unwrap(), sample);
}
#[test]
fn sleep_state_wire_is_stable() {
	let sample = SleepState::Idle;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(SleepState::decode(&bytes).unwrap(), sample);
}
#[test]
fn sleep_step_wire_is_stable() {
	let sample = SleepStep::None;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(SleepStep::decode(&bytes).unwrap(), sample);
}
#[test]
fn sleep_reason_wire_is_stable() {
	let sample = SleepReason::Requested;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(SleepReason::decode(&bytes).unwrap(), sample);
}
#[test]
fn sleep_outcome_wire_is_stable() {
	let sample = SleepOutcome::Running;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(SleepOutcome::decode(&bytes).unwrap(), sample);
}
#[test]
fn wake_reason_wire_is_stable() {
	let sample = WakeReason::Unknown;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(WakeReason::decode(&bytes).unwrap(), sample);
}
#[test]
fn core_park_wire_is_stable() {
	let sample = CorePark { cpu: 7, timer: 7, ipi: 7, device: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(CorePark::decode(&bytes).unwrap(), sample);
}
#[test]
fn sleep_record_wire_is_stable() {
	let sample = SleepRecord { state: SleepState::Idle, reason: SleepReason::Requested, outcome: SleepOutcome::Running, step: SleepStep::None, who: String::from("x"), why: String::from("x"), requested: 7, slept: 7, wake: WakeReason::Unknown, wake_detail: 7, cores: alloc::vec![CorePark { cpu: 7, timer: 7, ipi: 7, device: 7 }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 1, 0, 1, 0, 120, 1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 1, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(SleepRecord::decode(&bytes).unwrap(), sample);
}
#[test]
fn wake_source_wire_is_stable() {
	let sample = WakeSource { name: String::from("x"), armed: true, detail: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(WakeSource::decode(&bytes).unwrap(), sample);
}
#[test]
fn sleep_inhibitor_wire_is_stable() {
	let sample = SleepInhibitor { reason: String::from("x"), remaining_ms: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(SleepInhibitor::decode(&bytes).unwrap(), sample);
}
#[test]
fn sleep_status_wire_is_stable() {
	let sample = SleepStatus { idle: true, ram: true, disk: true, wake_sources: alloc::vec![WakeSource { name: String::from("x"), armed: true, detail: String::from("x") }], inhibitors: alloc::vec![SleepInhibitor { reason: String::from("x"), remaining_ms: 7 }], watchdog_cap_ms: 7, scheduled_wake: 7, hibernation_set_up: true, hibernation_why: String::from("x"), last_image: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 1, 1, 0, 1, 0, 120, 1, 1, 0, 120, 1, 0, 1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 120, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(SleepStatus::decode(&bytes).unwrap(), sample);
}
#[test]
fn drivers_suspended_wire_is_stable() {
	let sample = DriversSuspended { failed: String::from("x"), why: String::from("x"), wake_nodes: alloc::vec![String::from("x")], awake_by_ms: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 0, 120, 1, 0, 1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(DriversSuspended::decode(&bytes).unwrap(), sample);
}
#[test]
fn woke_wire_is_stable() {
	let sample = Woke { wake: WakeReason::Unknown, detail: 7, slept: 7, cores: alloc::vec![CorePark { cpu: 7, timer: 7, ipi: 7, device: 7 }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Woke::decode(&bytes).unwrap(), sample);
}
#[test]
fn hibernation_status_wire_is_stable() {
	let sample = HibernationStatus { set_up: true, why: String::from("x"), partition_bytes: 7, memory_bytes: 7, last_image: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(HibernationStatus::decode(&bytes).unwrap(), sample);
}

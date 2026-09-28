use super::*;
use alloc::string::String;

#[test]
fn component_type_wire_is_stable() {
	let sample = ComponentType::Service;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(ComponentType::decode(&bytes).unwrap(), sample);
}
#[test]
fn component_state_wire_is_stable() {
	let sample = ComponentState::Running;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(ComponentState::decode(&bytes).unwrap(), sample);
}
#[test]
fn counters_wire_is_stable() {
	let sample = Counters { messages_sent: 7, messages_received: 7, handles: 7, memory_bytes: 7, restarts: 7, watchdog_trips: 7, last_failure: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(Counters::decode(&bytes).unwrap(), sample);
}
#[test]
fn resource_count_wire_is_stable() {
	let sample = ResourceCount { name: String::from("x"), live: 7, bound: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ResourceCount::decode(&bytes).unwrap(), sample);
}
#[test]
fn component_wire_is_stable() {
	let sample = Component { name: String::from("x"), r#type: ComponentType::Service, state: ComponentState::Running, deps: alloc::vec![String::from("x")], counters: Counters { messages_sent: 7, messages_received: 7, handles: 7, memory_bytes: 7, restarts: 7, watchdog_trips: 7, last_failure: String::from("x") }, resources: alloc::vec![ResourceCount { name: String::from("x"), live: 7, bound: 7 }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		1,
		0,
		120,
		0,
		0,
		1,
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
		1,
		0,
		120,
		1,
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
	];
	assert_eq!(bytes, golden);
	assert_eq!(Component::decode(&bytes).unwrap(), sample);
}
#[test]
fn trace_span_wire_is_stable() {
	let sample = TraceSpan { name: String::from("x"), duration_ns: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(TraceSpan::decode(&bytes).unwrap(), sample);
}
#[test]
fn core_wake_source_wire_is_stable() {
	let sample = CoreWakeSource { source: 7, count: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(CoreWakeSource::decode(&bytes).unwrap(), sample);
}
#[test]
fn core_idle_wire_is_stable() {
	let sample = CoreIdle { cpu: 7, idle_ns: 7, halts: 7, wakes_timer: 7, wakes_ipi: 7, wakes_housekeeping: 7, wakes_device: 7, sources: alloc::vec![CoreWakeSource { source: 7, count: 7 }] };
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
		0,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(CoreIdle::decode(&bytes).unwrap(), sample);
}
#[test]
fn graph_wire_is_stable() {
	let sample = Graph { components: alloc::vec![Component { name: String::from("x"), r#type: ComponentType::Service, state: ComponentState::Running, deps: alloc::vec![String::from("x")], counters: Counters { messages_sent: 7, messages_received: 7, handles: 7, memory_bytes: 7, restarts: 7, watchdog_trips: 7, last_failure: String::from("x") }, resources: alloc::vec![ResourceCount { name: String::from("x"), live: 7, bound: 7 }] }], spans: alloc::vec![TraceSpan { name: String::from("x"), duration_ns: 7 }], cores: alloc::vec![CoreIdle { cpu: 7, idle_ns: 7, halts: 7, wakes_timer: 7, wakes_ipi: 7, wakes_housekeeping: 7, wakes_device: 7, sources: alloc::vec![CoreWakeSource { source: 7, count: 7 }] }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[
		1,
		0,
		1,
		0,
		120,
		0,
		0,
		1,
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
		1,
		0,
		120,
		1,
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
		1,
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
		0,
		0,
		0,
		0,
	];
	assert_eq!(bytes, golden);
	assert_eq!(Graph::decode(&bytes).unwrap(), sample);
}
#[test]
fn supervisor_stat_wire_is_stable() {
	let sample = SupervisorStat { name: String::from("x"), state: String::from("x"), desired: String::from("x"), epoch: 7, last_reason: String::from("x"), restarts: 7, watchdog_trips: 7, last_failure: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 0, 120, 1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(SupervisorStat::decode(&bytes).unwrap(), sample);
}

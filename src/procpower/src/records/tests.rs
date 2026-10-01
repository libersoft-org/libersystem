use super::*;

fn port(address: u64) -> abi::ProcessorRegister {
	abi::ProcessorRegister { space: 1, bits: 8, _pad: [0; 6], address }
}

#[test]
fn a_states_table_reads_with_its_domain_and_throttling() {
	let mut raw = abi::ProcessorPerfTable { control_kind: abi::PERF_TABLE_STATES, state_count: 2, control: port(0x800), status: port(0x801), coordination: 0xFC, domain: 3, processors: 2, throttle_count: 2, throttle_control: port(0x810), ..Default::default() };
	raw.states[0] = abi::ProcessorPerfState { core_mhz: 3000, power_mw: 15000, latency_us: 10, control: 0x10, status: 0x10 };
	raw.states[1] = abi::ProcessorPerfState { core_mhz: 1500, power_mw: 6000, latency_us: 10, control: 0x11, status: 0x11 };
	raw.throttle[0] = abi::ProcessorThrottleState { percent: 100, latency_us: 5, control: 0 };
	raw.throttle[1] = abi::ProcessorThrottleState { percent: 50, latency_us: 5, control: 0x14 };
	let table = perf_table_of(&raw).expect("a table it knows");
	assert_eq!(table.levels(), 3, "two states and one throttling state past T0");
	assert_eq!(table.setting(2), perf::Setting { performance: 0x11, throttle: Some(0x14) });
	assert_eq!(table.domain, Some(perf::Domain { domain: 3, coordination: perf::Coordination::SoftwareAll, processors: 2 }));
	raw.coordination = 0x10;
	assert_eq!(perf_table_of(&raw), Err(Unread::Unknown), "no such coordination");
	raw.coordination = 0;
	raw.control_kind = 9;
	assert_eq!(perf_table_of(&raw), Err(Unread::Unknown), "no such control");
}

#[test]
fn a_cppc_table_reads_its_optional_registers_by_address() {
	let mut raw = abi::ProcessorPerfTable { control_kind: abi::PERF_TABLE_CPPC, control: port(0x900), highest: 255, nominal: 200, lowest: 55, ..Default::default() };
	let table = perf_table_of(&raw).expect("a CPPC table");
	assert!(matches!(table.control, perf::Control::Cppc { minimum: None, maximum: None, preference: None, highest: 255, .. }));
	raw.preference = port(0x904);
	let table = perf_table_of(&raw).expect("a CPPC table with a preference register");
	assert_eq!(table.registers().unwrap().len(), 2, "the preference register is held with the desired one");
}

#[test]
fn an_idle_state_reads_its_entry_and_costs() {
	let raw = abi::ProcessorIdleState { entry: abi::IDLE_ENTRY_MWAIT, flags: abi::IDLE_STOPS_TIMER, parameter: 0x20, exit_latency_us: 100, target_residency_us: 300, _pad: 0, register: abi::ProcessorRegister::default() };
	let state = idle_state_of(&raw).expect("an MWAIT state");
	assert_eq!(state.entry, Entry::Mwait { hint: 0x20 });
	assert!(state.stops_timer && !state.loses_context);
	assert_eq!(idle_state_of(&abi::ProcessorIdleState { entry: 77, ..raw }), None);
}

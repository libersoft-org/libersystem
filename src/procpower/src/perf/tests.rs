use super::*;
use crate::Space;

fn memory(address: u64) -> Register {
	Register { space: Space::SystemMemory, bits: 32, address }
}

fn pss(count: u32) -> Table {
	let states = (0..count).map(|at| PerfState { core_mhz: 4000 - at * 100, power_mw: 15000 - at * 400, latency_us: 20, control: 0x10 + at, status: 0x10 + at }).collect();
	Table { control: Control::States { control: memory(0xFEB0_0000), status: memory(0xFEB0_0004), states }, domain: None, throttle: None }
}

#[test]
fn a_pss_table_is_admitted_and_its_levels_write_their_control_values() {
	let table = pss(4);
	assert_eq!(check_table(&table, Arch::X86_64), Ok(()));
	assert_eq!(table.levels(), 4);
	assert_eq!(table.setting(0), Setting { performance: 0x10, throttle: None });
	assert_eq!(table.setting(3), Setting { performance: 0x13, throttle: None });
	assert_eq!(table.registers().unwrap(), [memory(0xFEB0_0000), memory(0xFEB0_0004)]);
	assert_eq!(table.transition_us(), 20);
}

#[test]
fn throttling_states_extend_the_levels_past_the_slowest_performance_state() {
	let mut table = pss(2);
	table.throttle = Some(Throttle {
		control: memory(0xFEB0_0008),
		states: alloc::vec![
			ThrottleState { percent: 100, latency_us: 5, control: 0 },
			ThrottleState { percent: 50, latency_us: 5, control: 4 },
			ThrottleState { percent: 25, latency_us: 5, control: 6 }
		],
	});
	assert_eq!(check_table(&table, Arch::X86_64), Ok(()));
	assert_eq!(table.levels(), 4, "two performance states and T1, T2");
	assert_eq!(table.setting(1), Setting { performance: 0x11, throttle: None });
	assert_eq!(table.setting(2), Setting { performance: 0x11, throttle: Some(4) }, "T1 at the slowest performance state");
	assert_eq!(table.setting(3), Setting { performance: 0x11, throttle: Some(6) });
}

#[test]
fn cppc_levels_spread_from_the_highest_performance_to_the_lowest() {
	let table = Table { control: Control::Cppc { desired: memory(0xFEB0_1000), minimum: None, maximum: None, preference: None, highest: 255, nominal: 200, lowest: 55 }, domain: None, throttle: None };
	assert_eq!(check_table(&table, Arch::X86_64), Ok(()));
	assert_eq!(table.levels(), MAX_CPPC_LEVELS);
	assert_eq!(table.setting(0).performance, 255);
	assert_eq!(table.setting(MAX_CPPC_LEVELS - 1).performance, 55);
}

#[test]
fn a_hostile_performance_table_is_refused_whole() {
	let mut msr = pss(3);
	if let Control::States { control, .. } = &mut msr.control {
		*control = Register { space: Space::FixedHardware, bits: 64, address: 0x199 };
	}
	assert_eq!(check_table(&msr, Arch::X86_64), Err(Refusal::ModelSpecificRegister), "IA32_PERF_CTL is excluded");
	assert_eq!(check_table(&pss(33), Arch::X86_64), Err(Refusal::TooManyStates));
	assert_eq!(check_table(&pss(0), Arch::X86_64), Err(Refusal::Empty));
	let mut backwards = pss(3);
	if let Control::States { states, .. } = &mut backwards.control {
		states.reverse();
	}
	assert_eq!(check_table(&backwards, Arch::X86_64), Err(Refusal::OutOfOrder));
	let cppc = Table { control: Control::Cppc { desired: memory(0xFEB0_1000), minimum: None, maximum: None, preference: None, highest: 100, nominal: 200, lowest: 1 }, domain: None, throttle: None };
	assert_eq!(check_table(&cppc, Arch::X86_64), Err(Refusal::OutOfOrder));
	let mut throttled = pss(2);
	throttled.throttle = Some(Throttle { control: memory(0xFEB0_0008), states: alloc::vec![ThrottleState { percent: 50, latency_us: 5, control: 4 }] });
	assert_eq!(check_table(&throttled, Arch::X86_64), Err(Refusal::OutOfOrder), "T0 is full speed");
}

#[test]
fn the_governor_follows_utilisation_inside_its_window_no_faster_than_the_table_allows() {
	let all = Window::all(4);
	assert_eq!(next_level(3, 900, all, MIN_INTERVAL_US, 20), 0, "busy: the fastest level the window allows");
	assert_eq!(next_level(0, 100, all, MIN_INTERVAL_US, 20), 1, "idle: one level slower");
	assert_eq!(next_level(0, 30, all, MIN_INTERVAL_US, 20), 3, "at rest: the slowest level the window allows at once");
	assert_eq!(next_level(0, 30, Window { cap: 0, floor: 2 }, MIN_INTERVAL_US, 20), 2, "and never past the floor");
	assert_eq!(next_level(1, 500, all, MIN_INTERVAL_US, 20), 1, "in between: where it is");
	assert_eq!(next_level(3, 100, all, MIN_INTERVAL_US, 20), 3, "never past the slowest");
	assert_eq!(next_level(3, 900, all, MIN_INTERVAL_US - 1, 20), 3, "not twice within the minimum interval");
	assert_eq!(next_level(3, 900, all, 40_000, 50_000), 3, "nor within the table's transition latency");
}

#[test]
fn a_window_is_obeyed_at_once_and_a_bad_one_refused() {
	let capped = Window { cap: 2, floor: 3 };
	assert_eq!(capped.check(4), Ok(()));
	assert_eq!(next_level(0, 900, capped, 0, 20), 2, "a cap that moves is obeyed at once, interval or not");
	assert_eq!(next_level(3, 900, capped, MIN_INTERVAL_US, 20), 2, "busy: the cap");
	assert_eq!(Window { cap: 3, floor: 2 }.check(4), Err(Refusal::BadWindow));
	assert_eq!(Window { cap: 0, floor: 4 }.check(4), Err(Refusal::BadWindow));
}

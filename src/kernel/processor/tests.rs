#[cfg(target_arch = "x86_64")]
use crate::object::port_range::grants;

// Ports nothing on q35 decodes and nothing in the suite drives - clear of fw_cfg's 0x510..0x51B, which the kernel holds
// - for a `_CST`'s P_LVL2 and P_LVL3, and for the other registers below.
#[cfg(target_arch = "x86_64")]
const P_LVL2: u64 = 0x0E10;
#[cfg(target_arch = "x86_64")]
const P_LVL3: u64 = 0x0E11;
#[cfg(target_arch = "x86_64")]
const CLASHING: u64 = 0x0E12;
#[cfg(target_arch = "x86_64")]
const OTHER_ITEM: u16 = 0x0E20;
#[cfg(target_arch = "x86_64")]
const CPPC_DESIRED: u64 = 0x0E18;
#[cfg(target_arch = "x86_64")]
const CPPC_PREFERENCE: u64 = 0x0E19;

#[cfg(target_arch = "x86_64")]
fn port_state(address: u64, exit_latency_us: u32, target_residency_us: u32) -> abi::ProcessorIdleState {
	abi::ProcessorIdleState { entry: abi::IDLE_ENTRY_REGISTER, flags: 0, parameter: 0, exit_latency_us, target_residency_us, _pad: 0, register: abi::ProcessorRegister { space: 1, bits: 8, _pad: [0; 6], address } }
}

#[cfg(target_arch = "x86_64")]
fn halt_state() -> abi::ProcessorIdleState {
	abi::ProcessorIdleState { entry: abi::IDLE_ENTRY_HALT, flags: 0, parameter: 0, exit_latency_us: 1, target_residency_us: 1, _pad: 0, register: abi::ProcessorRegister::default() }
}

#[cfg(target_arch = "x86_64")]
fn cst() -> [abi::ProcessorIdleState; 3] {
	[halt_state(), port_state(P_LVL2, 50, 150), port_state(P_LVL3, 300, 900)]
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(a_cst_installed_on_every_core_holds_its_ports_once_and_lets_them_go_with_the_last, [Kernel], id = "kernel.processor.a_cst_installed_on_every_core_holds_its_ports_once_and_lets_them_go_with_the_last", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn a_cst_installed_on_every_core_holds_its_ports_once_and_lets_them_go_with_the_last() {
	let cores = crate::smp::cpu_count().min(2);
	for cpu in 0..cores {
		assert_eq!(super::install_idle(cpu, &cst()), 0, "core {cpu}'s table is admitted");
	}
	// HELD IN THE RESERVED SET: no other item installs them, and no mint could take them.
	assert!(grants::install(0x7e58, P_LVL2 as u16, 1).is_err(), "processor power holds P_LVL2");
	assert!(grants::recordable(P_LVL3 as u16, 1).is_err(), "and no row may record P_LVL3");
	assert_eq!(super::install_idle(0, &cst()), 0, "the same table again changes nothing");
	for cpu in 0..cores {
		assert_eq!(super::install_idle(cpu, &[]), 0);
	}
	assert!(grants::recordable(P_LVL2 as u16, 2).is_ok(), "both let go with the last table");
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(a_table_naming_a_model_specific_register_is_refused_whole_and_the_old_one_stands, [Kernel], id = "kernel.processor.a_table_naming_a_model_specific_register_is_refused_whole_and_the_old_one_stands", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn a_table_naming_a_model_specific_register_is_refused_whole_and_the_old_one_stands() {
	assert_eq!(super::install_idle(0, &cst()), 0);
	let mut msr = cst();
	// A WIDTH THE KERNEL COULD WRITE, so the refusal is the model-specific register's and nothing else's.
	msr[2].register = abi::ProcessorRegister { space: 0x7F, bits: 32, _pad: [0; 6], address: 0x199 };
	assert_eq!(super::install_idle(0, &msr), abi::ERR_INVALID, "a model-specific register refuses the table");
	assert!(grants::recordable(P_LVL3 as u16, 1).is_err(), "the old table still holds P_LVL3");
	// A PORT ANOTHER ITEM HOLDS: refused whole, and nothing it would have taken is left behind.
	grants::install(0x7e58, OTHER_ITEM, 1).expect("a free port for the test's own item");
	let clashing = [halt_state(), port_state(CLASHING, 50, 150), port_state(u64::from(OTHER_ITEM), 300, 900)];
	assert_eq!(super::install_idle(0, &clashing), abi::ERR_ACCESS_DENIED);
	assert!(grants::recordable(CLASHING as u16, 1).is_ok(), "the port it took before the refusal is given back");
	assert!(grants::uninstall(0x7e58, OTHER_ITEM, 1));
	assert_eq!(super::install_idle(0, &[]), 0);
}

crate::tagged_test!(the_smallest_live_latency_request_bounds_the_governor_and_its_end_releases_it, [Kernel], id = "kernel.processor.the_smallest_live_latency_request_bounds_the_governor_and_its_end_releases_it", covers = ["kernel"]);
fn the_smallest_live_latency_request_bounds_the_governor_and_its_end_releases_it() {
	let first = crate::object::latency_request::LatencyRequest::new().expect("a request");
	let second = crate::object::latency_request::LatencyRequest::new().expect("a request");
	use crate::object::KernelObject;
	super::latency_add(9_000_001, first.header().koid(), 500).expect("room for one");
	super::latency_add(9_000_001, second.header().koid(), 80).expect("and for another");
	assert_eq!(super::BOUND.load(core::sync::atomic::Ordering::Acquire), 80);
	drop(second);
	assert_eq!(super::BOUND.load(core::sync::atomic::Ordering::Acquire), 500, "the request's last handle gone, its bound is too");
	drop(first);
	assert_eq!(super::BOUND.load(core::sync::atomic::Ordering::Acquire), u32::MAX);
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(an_idle_period_enters_a_state_this_core_can_enter_and_counts_it, [Kernel], id = "kernel.processor.an_idle_period_enters_a_state_this_core_can_enter_and_counts_it", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn an_idle_period_enters_a_state_this_core_can_enter_and_counts_it() {
	let cpu = crate::sched::current_cpu_id();
	assert_eq!(super::install_idle(cpu, &cst()), 0);
	let deep_enterable = procpower::enterable(&procpower::IdleState { entry: procpower::Entry::Halt, exit_latency_us: 300, target_residency_us: 900, loses_context: false, stops_timer: false, bus_master_arbitration: false }, 2, &crate::arch::processor::cpu(), crate::arch::processor::ARCH).is_ok();
	let before: [u64; 3] = core::array::from_fn(|at| super::COUNTS[cpu].entries[at].load(core::sync::atomic::Ordering::Relaxed));
	// A LONG WAIT, with nothing else to wake the core: the halt's own path, masked, a one-shot five ticks out.
	let wake = crate::arch::apic::ticks() + 5;
	crate::idle::halt(Some(wake), || false);
	let after: [u64; 3] = core::array::from_fn(|at| super::COUNTS[cpu].entries[at].load(core::sync::atomic::Ordering::Relaxed));
	let entered: u64 = (0..3).map(|at| after[at] - before[at]).sum();
	assert_eq!(entered, 1, "one idle period, one entry counted");
	if !deep_enterable {
		assert_eq!(after[2], before[2], "a state this core cannot enter is never entered");
		assert_eq!(after[1], before[1]);
	}
	let info = crate::idle::info(cpu).expect("the core's record");
	assert_eq!(info.state_count, 3);
	assert_eq!(info.states[2].exit_latency_us, 300);
	assert_eq!(super::install_idle(cpu, &[]), 0);
}

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(a_cppc_preference_is_written_only_where_the_table_names_its_register, [Kernel], id = "kernel.processor.a_cppc_preference_is_written_only_where_the_table_names_its_register", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn a_cppc_preference_is_written_only_where_the_table_names_its_register() {
	let port = |address: u64| abi::ProcessorRegister { space: 1, bits: 8, _pad: [0; 6], address };
	let mut table = abi::ProcessorPerfTable { control_kind: abi::PERF_TABLE_CPPC, control: port(CPPC_DESIRED), highest: 255, nominal: 200, lowest: 55, ..Default::default() };
	assert_eq!(super::install_perf(0, Some(&table)), 0);
	assert_eq!(super::set_preference(0, 128), abi::ERR_UNSUPPORTED, "no preference register, nothing written");
	table.preference = port(CPPC_PREFERENCE);
	assert_eq!(super::install_perf(0, Some(&table)), 0, "the table replaced by one with a preference register");
	assert!(grants::recordable(CPPC_PREFERENCE as u16, 1).is_err(), "the preference register is held with the table");
	assert_eq!(super::set_preference(0, 128), 0);
	assert_eq!(super::install_perf(0, None), 0);
	assert!(grants::recordable(CPPC_PREFERENCE as u16, 1).is_ok(), "and let go with it");
	assert_eq!(super::set_preference(0, 128), abi::ERR_UNSUPPORTED);
}

// THE DEVICE TREE'S STATES, on the device-tree ports: three as `/cpus/idle-states` describes them - a retention state, a
// deeper one, and one that loses the core's context - installed on this core the way the boot installs a tree's,
// entered through the firmware (PSCI's CPU_SUSPEND, the SBI's HART_SUSPEND) by idle periods, the one that loses the
// context never, and a latency request below the deeper state's wake keeping the core in the retention state.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
crate::tagged_test!(a_trees_idle_states_are_entered_through_the_firmware_within_a_latency_request, [Kernel], id = "kernel.processor.a_trees_idle_states_are_entered_through_the_firmware_within_a_latency_request", covers = ["kernel"]);
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn a_trees_idle_states_are_entered_through_the_firmware_within_a_latency_request() {
	use core::sync::atomic::Ordering;
	// PSCI's power state in the original format (StateType is bit 16), and the SBI's default retentive and non-retentive
	// types - what QEMU's PSCI and this OpenSBI take.
	#[cfg(target_arch = "aarch64")]
	let (retention, deep, lost) = (0x0000_0001u32, 0x0000_0002u32, 0x0001_0003u32);
	#[cfg(target_arch = "riscv64")]
	let (retention, deep, lost) = (0x0000_0000u32, 0x0000_0000u32, 0x8000_0000u32);
	let cpu = crate::sched::current_cpu_id();
	let mut tree = fdt::TreeIdleStates { states: [fdt::TreeIdleState::default(); fdt::MAX_TREE_IDLE_STATES], state_count: 3, cpus: [(0, [0; fdt::MAX_TREE_IDLE_STATES], 0); fdt::MAX_TREE_IDLE_CPUS], cpu_count: 1 };
	tree.states[0] = fdt::TreeIdleState { phandle: 1, parameter: retention, entry_latency_us: 20, exit_latency_us: 40, min_residency_us: 80, local_timer_stop: false };
	tree.states[1] = fdt::TreeIdleState { phandle: 2, parameter: deep, entry_latency_us: 500, exit_latency_us: 1500, min_residency_us: 5000, local_timer_stop: false };
	// NOT `local-timer-stop`, so the reason it is held out is the context it loses - the gate's tree carries the timer's.
	tree.states[2] = fdt::TreeIdleState { phandle: 3, parameter: lost, entry_latency_us: 100, exit_latency_us: 250, min_residency_us: 1000, local_timer_stop: false };
	let mut names = [0u32; fdt::MAX_TREE_IDLE_STATES];
	names[..3].copy_from_slice(&[1, 2, 3]);
	tree.cpus[0] = (crate::smp::lapic_id(cpu), names, 3);
	super::install_tree_states(&tree);
	// THE TABLE AS THE BOOT INSTALLS ONE: the halt, then by how long each wake takes - entry and exit together.
	let info = crate::idle::info(cpu).expect("the core's record");
	assert_eq!(info.state_count, 4, "the halt and the tree's three");
	assert_eq!([info.states[1].exit_latency_us, info.states[2].exit_latency_us, info.states[3].exit_latency_us], [60, 350, 2000]);
	assert_ne!(info.states[2].unenterable, 0, "the state that loses the core's context is held out of the governor's choices");
	let refused = crate::arch::processor::firmware_refusals();
	let entries = || -> [u64; 4] { core::array::from_fn(|at| super::COUNTS[cpu].entries[at].load(Ordering::Relaxed)) };
	let lost = entries()[2];
	let idle = || {
		let wake = crate::arch::apic::ticks() + 5;
		crate::idle::halt(Some(wake), || false);
	};
	// THE HISTORY FIRST: long idle periods, so what the core predicts is what its timer says rather than what its busy
	// past did - the deep state's five milliseconds of residency fit only then.
	for _ in 0..16 {
		idle();
	}
	let before = entries();
	idle();
	let after = entries();
	assert_eq!(after[3], before[3] + 1, "a long idle period enters the deep state ({before:?}, then {after:?})");
	assert_eq!(after[2], before[2], "never the state that loses the context");
	// A REQUEST OF 1000 US: the deep state's 2000 us wake is past it, and the retention state is the deepest left.
	let request = crate::object::latency_request::LatencyRequest::new().expect("a request");
	use crate::object::KernelObject;
	super::latency_add(9_000_002, request.header().koid(), 1000).expect("room for one");
	let bounded = entries();
	idle();
	let within = entries();
	assert_eq!((within[1], within[3]), (bounded[1] + 1, bounded[3]), "within the request the retention state, not the deep one ({bounded:?}, then {within:?})");
	drop(request);
	assert_eq!(super::BOUND.load(Ordering::Acquire), u32::MAX);
	assert_eq!(entries()[2], lost, "the state that loses the context was never entered");
	assert_eq!(crate::arch::processor::firmware_refusals(), refused, "every entry was the firmware's, none the halt's instead");
	assert_eq!(super::install_idle(cpu, &[]), 0);
}

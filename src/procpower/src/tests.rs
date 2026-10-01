use super::*;

fn port(address: u64) -> Register {
	Register { space: Space::SystemIo, bits: 8, address }
}

fn state(entry: Entry, exit_latency_us: u32, target_residency_us: u32) -> IdleState {
	IdleState { entry, exit_latency_us, target_residency_us, loses_context: false, stops_timer: false, bus_master_arbitration: false }
}

// A `_CST` as QEMU-like firmware states it: C1 by halt, C2 and C3 by their `P_LVLx` ports.
fn cst() -> [IdleState; 3] {
	[state(Entry::Halt, 1, 1), state(Entry::Register(port(0x514)), 50, 150), state(Entry::Register(port(0x515)), 300, 900)]
}

#[test]
fn an_ordinary_table_is_admitted_and_names_its_registers_once() {
	assert_eq!(check_idle_table(&cst(), Arch::X86_64), Ok(()));
	let mut doubled = cst().to_vec();
	doubled.push(state(Entry::Register(port(0x515)), 400, 1000));
	assert_eq!(idle_registers(&doubled).unwrap(), [port(0x514), port(0x515)]);
}

#[test]
fn a_hostile_table_is_refused_whole() {
	assert_eq!(check_idle_table(&[], Arch::X86_64), Err(Refusal::Empty));
	let nine = [state(Entry::Halt, 1, 1); 9];
	assert_eq!(check_idle_table(&nine, Arch::X86_64), Err(Refusal::TooManyStates));
	let mut msr = cst();
	msr[2].entry = Entry::Register(Register { space: Space::FixedHardware, bits: 64, address: 0x199 });
	assert_eq!(check_idle_table(&msr, Arch::X86_64), Err(Refusal::ModelSpecificRegister), "one model-specific register refuses the whole table");
	let mut pcc = cst();
	pcc[1].entry = Entry::Register(Register { space: Space::PlatformChannel, bits: 32, address: 0 });
	assert_eq!(check_idle_table(&pcc, Arch::X86_64), Err(Refusal::PlatformChannel));
	let mut arbitrated = cst();
	arbitrated[2].bus_master_arbitration = true;
	assert_eq!(check_idle_table(&arbitrated, Arch::X86_64), Err(Refusal::BusMasterArbitration));
	let mut backwards = cst();
	backwards.swap(1, 2);
	assert_eq!(check_idle_table(&backwards, Arch::X86_64), Err(Refusal::OutOfOrder));
	assert_eq!(check_idle_table(&[state(Entry::Psci { parameter: 1 }, 10, 10)], Arch::X86_64), Err(Refusal::WrongArchitecture));
	assert_eq!(check_idle_table(&[state(Entry::Mwait { hint: 0x20 }, 10, 10)], Arch::Aarch64), Err(Refusal::WrongArchitecture));
	assert_eq!(check_idle_table(&[state(Entry::Register(port(0x514)), 10, 10)], Arch::Riscv64), Err(Refusal::OtherSpace), "no port space off x86");
	assert_eq!(check_idle_table(&[state(Entry::Register(Register { space: Space::SystemIo, bits: 32, address: 0xFFFE }), 10, 10)], Arch::X86_64), Err(Refusal::BadRegister), "a port running past the port space");
	assert_eq!(check_idle_table(&[state(Entry::Register(Register { space: Space::SystemIo, bits: 24, address: 0x514 }), 10, 10)], Arch::X86_64), Err(Refusal::BadRegister), "a width the kernel cannot access");
}

#[test]
fn a_state_this_core_cannot_enter_is_named_and_left_alone() {
	let none = Cpu::default();
	let all = Cpu { mwait: true, invariant_counter: true, timer_always_running: true, context_resume: true };
	let deep = cst()[2];
	assert_eq!(enterable(&cst()[0], 0, &none, Arch::X86_64), Ok(()), "the halt is always enterable");
	assert_eq!(enterable(&deep, 2, &none, Arch::X86_64), Err(Unenterable::CounterMayStop), "deeper than C1 needs the invariant counter on x86");
	assert_eq!(enterable(&deep, 2, &none, Arch::Aarch64), Ok(()), "the other two counters always run");
	assert_eq!(enterable(&deep, 2, &all, Arch::X86_64), Ok(()));
	assert_eq!(enterable(&state(Entry::Mwait { hint: 0x10 }, 2, 2), 1, &Cpu { invariant_counter: true, ..none }, Arch::X86_64), Err(Unenterable::NoMwait));
	let stopping = IdleState { stops_timer: true, ..deep };
	assert_eq!(enterable(&stopping, 2, &Cpu { invariant_counter: true, ..none }, Arch::X86_64), Err(Unenterable::TimerStops));
	let losing = IdleState { loses_context: true, ..deep };
	assert_eq!(enterable(&losing, 2, &Cpu { invariant_counter: true, timer_always_running: true, ..none }, Arch::X86_64), Err(Unenterable::ContextLost));
}

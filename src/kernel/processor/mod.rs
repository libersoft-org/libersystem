// PROCESSOR POWER, THE KERNEL'S HALF: each core's installed idle and performance tables, the idle governor's choice at
// every halt and the entry into it, the performance governor's choice at the scheduler's own points and the register
// write that carries it out, the idle injected into a core, and the latency bound. What a decision IS - the checks, the
// governors, the counting of held registers - is `procpower`'s, and host-tested there; this holds the state and does
// the work. ProcessorPowerService installs what the firmware describes, under the `ProcessorPower` privilege
// (`SYS_PROCESSOR_*`); a table it installed stays here across its restart, and a restarted instance installs it again.
//
// THE REGISTERS. Every register a table names is checked against everything else the kernel holds the first time a
// table names it (`procpower::holdings`): a port joins the reserved set under `grants::PROCESSOR_POWER`, so no mint can
// take it; memory is admitted by the firmware policy's view (`firmware::admit_processor_memory`) and mapped uncached in
// the architecture's window; and both are let go with the last table that names them. A table with one refused
// register is refused whole, and the one it would have replaced stands.
//
// THE IDLE PATH (`enter`) runs with interrupts masked, at the end of `idle::halt`: the prediction - the shorter of the
// one-shot just programmed and the core's recent idle periods - against each state's target residency and the latency
// bound, the deepest state that fits and that this core can enter, its entry, and its counts. A core with no table
// halts, as it always did.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use procpower::governor::{Candidate, Predictor};
use procpower::holdings::Holdings;
use procpower::latency::Requests;
use procpower::perf::{self, Window};
use procpower::records::{idle_state_of, perf_table_of};
use procpower::{Entry, IdleState, Register, Space};

use crate::arch;
use crate::smp::MAX_CPUS;
use crate::sync::SpinLock;

// A REGISTER AS THE KERNEL REACHES IT: the register, and where it is mapped when it is in memory.
#[derive(Clone, Copy)]
struct Access {
	register: Register,
	virt: u64,
}

impl Access {
	fn read(&self) -> u64 {
		let bytes = self.register.bytes().unwrap_or(1);
		match self.register.space {
			Space::SystemIo => arch::processor::port_read(self.register.address as u16, bytes),
			// SAFETY: a page of the processor window, mapped uncached for as long as a table names this register.
			_ => unsafe {
				match bytes {
					1 => u64::from((self.virt as *const u8).read_volatile()),
					2 => u64::from((self.virt as *const u16).read_volatile()),
					4 => u64::from((self.virt as *const u32).read_volatile()),
					_ => (self.virt as *const u64).read_volatile(),
				}
			},
		}
	}

	fn write(&self, value: u64) {
		let bytes = self.register.bytes().unwrap_or(1);
		match self.register.space {
			Space::SystemIo => arch::processor::port_write(self.register.address as u16, bytes, value),
			// SAFETY: as `read`.
			_ => unsafe {
				match bytes {
					1 => (self.virt as *mut u8).write_volatile(value as u8),
					2 => (self.virt as *mut u16).write_volatile(value as u16),
					4 => (self.virt as *mut u32).write_volatile(value as u32),
					_ => (self.virt as *mut u64).write_volatile(value),
				}
			},
		}
	}
}

// ONE INSTALLED IDLE STATE: the state, whether this core may enter it, and its register's access.
#[derive(Clone, Copy)]
struct Installed {
	state: IdleState,
	unenterable: Option<procpower::Unenterable>,
	access: Option<Access>,
}

struct PerfInstalled {
	table: perf::Table,
	control: Access,
	throttle: Option<Access>,
	// CPPC's energy-performance preference register, where the table names one.
	preference: Option<Access>,
	level: u32,
	changed_at_ns: u64,
}

struct Core {
	idle: Vec<Installed>,
	perf: Option<PerfInstalled>,
	window: Option<Window>,
	// The energy-performance preference written last, for the resume to write again.
	preference: Option<u8>,
	inject_permille: u32,
	predictor: Predictor,
	// The utilisation period the performance governor decides over: when it began, and the idle time inside it - and the
	// busy share the last one measured, which a change of the window decides from at once.
	period_start_ns: u64,
	period_idle_ns: u64,
	last_busy_permille: Option<u32>,
	// The idle injection's account: what the period owes, and since when the core is injecting.
	inject_owed_ns: u64,
}

impl Core {
	const fn new() -> Core {
		Core { idle: Vec::new(), perf: None, window: None, preference: None, inject_permille: 0, predictor: Predictor::new(), period_start_ns: 0, period_idle_ns: 0, last_busy_permille: None, inject_owed_ns: 0 }
	}
}

static CORES: [SpinLock<Core>; MAX_CPUS] = [const { SpinLock::new(Core::new()) }; MAX_CPUS];
static HOLDINGS: SpinLock<Holdings> = SpinLock::new(Holdings::new());
static LATENCY: SpinLock<Requests> = SpinLock::new(Requests::new());
// The smallest live request's bound, read on the idle path without a lock; `u32::MAX` for none.
static BOUND: AtomicU32 = AtomicU32::new(u32::MAX);

// THE COUNTS THE FREE READ ANSWERS, per core: each state's entries and residency, the time at each level, the level now
// and the injected idle.
struct Counts {
	entries: [AtomicU64; abi::PROCESSOR_MAX_IDLE_STATES],
	residency_ns: [AtomicU64; abi::PROCESSOR_MAX_IDLE_STATES],
	level_ns: [AtomicU64; abi::CPU_PERF_LEVELS],
	level: AtomicU32,
	level_since_ns: AtomicU64,
	injected_ns: AtomicU64,
}

static COUNTS: [Counts; MAX_CPUS] = [const { Counts { entries: [const { AtomicU64::new(0) }; abi::PROCESSOR_MAX_IDLE_STATES], residency_ns: [const { AtomicU64::new(0) }; abi::PROCESSOR_MAX_IDLE_STATES], level_ns: [const { AtomicU64::new(0) }; abi::CPU_PERF_LEVELS], level: AtomicU32::new(0), level_since_ns: AtomicU64::new(0), injected_ns: AtomicU64::new(0) } }; MAX_CPUS];

// The performance governor's period: how much of the core's time it decides over.
const PERIOD_NS: u64 = 20_000_000;

fn now_ns() -> u64 {
	arch::common::time::CLOCK.nanos(arch::tsc::now())
}

// ------------------------------------------------------------------ the registers

// WHY A REGISTER WAS NOT TAKEN, for the refusal's line.
fn take(register: &Register) -> Result<Access, &'static str> {
	match register.space {
		Space::SystemIo => {
			let bytes = u16::from(register.bytes().unwrap_or(1));
			crate::object::port_range::grants::install(crate::object::port_range::grants::PROCESSOR_POWER, register.address as u16, bytes).map_err(|_| "the port is in the reserved set or a grant")?;
			Ok(Access { register: *register, virt: 0 })
		}
		Space::SystemMemory => {
			let bytes = u64::from(register.bytes().unwrap_or(1));
			crate::firmware::admit_processor_memory(register.address, bytes)?;
			match arch::processor::map_register(register.address) {
				Ok(virt) => Ok(Access { register: *register, virt }),
				Err(why) => {
					crate::firmware::release_processor_memory(register.address, bytes);
					Err(why)
				}
			}
		}
		_ => Err("it is in a space this kernel writes no processor register in"),
	}
}

// A REGISTER LET GO - its port out of the reserved set, its memory released and its page unmapped where no register
// `holdings` still holds is on it. The caller holds the holdings' lock, and the register is not among them.
fn give_back(register: &Register, holdings: &Holdings) {
	match register.space {
		Space::SystemIo => {
			let bytes = u16::from(register.bytes().unwrap_or(1));
			crate::object::port_range::grants::uninstall(crate::object::port_range::grants::PROCESSOR_POWER, register.address as u16, bytes);
		}
		Space::SystemMemory => {
			let bytes = u64::from(register.bytes().unwrap_or(1));
			crate::firmware::release_processor_memory(register.address, bytes);
			if !holdings.registers().any(|held| held.space == Space::SystemMemory && held.address & !0xfff == register.address & !0xfff) {
				arch::processor::unmap_register(register.address);
			}
		}
		_ => {}
	}
}

// Where a held register is reached: a port as itself, memory through the window (its page is already mapped).
fn access_of(register: &Register) -> Access {
	match register.space {
		Space::SystemMemory => Access { register: *register, virt: arch::processor::map_register(register.address).unwrap_or(0) },
		_ => Access { register: *register, virt: 0 },
	}
}

// THE REPLACEMENT, in one step: every register `new` names that no table holds taken - all or none - then `new` counted
// up and `old` counted down, and what no table names any more let go.
fn replace_registers(old: &[Register], new: &[Register]) -> Result<(), i64> {
	let mut holdings = HOLDINGS.lock();
	// THE ROOM FIRST - the list, what is taken and the count's growth - so a short heap refuses the table whole, with
	// nothing taken.
	let Some(wanted) = holdings.to_take(new) else { return Err(abi::ERR_NO_MEMORY) };
	let mut taken: Vec<Register> = Vec::new();
	if taken.try_reserve_exact(wanted.len()).is_err() || !holdings.reserve(new) {
		return Err(abi::ERR_NO_MEMORY);
	}
	for register in &wanted {
		if let Err(why) = take(register) {
			crate::serial_println!("processor: a table is refused - its register {:?} {:#x} is not taken: {why}", register.space, register.address);
			for undone in &taken {
				give_back(undone, &holdings);
			}
			return Err(abi::ERR_ACCESS_DENIED);
		}
		taken.push(*register);
	}
	// COUNTED, or nothing is: a replacement the heap cannot hold gives back what this one took.
	let Some(released) = holdings.replace(old, new) else {
		for undone in &taken {
			give_back(undone, &holdings);
		}
		return Err(abi::ERR_NO_MEMORY);
	};
	for register in &released {
		give_back(register, &holdings);
	}
	Ok(())
}

// ------------------------------------------------------------------ the tables

fn unenterable_text(why: procpower::Unenterable) -> &'static str {
	match why {
		procpower::Unenterable::NoMwait => "this CPU offers no MWAIT",
		procpower::Unenterable::CounterMayStop => "this CPU has no invariant TSC, and the clock is the counter",
		procpower::Unenterable::TimerStops => "it stops the core's timer, and no broadcast timer stands in",
		procpower::Unenterable::ContextLost => "it loses the core's context, and no per-core resume path is built",
	}
}

// THE DEVICE TREE'S IDLE STATES, installed at boot on aarch64 and riscv64 for every core whose cpu node names them: the
// halt first, then each state the tree describes, ordered by how long its wake takes - the entry's latency and the
// exit's, which together are what a latency request bounds. The kernel reads the tree itself, with no interpreter
// between, and a table installed later replaces this one. A state that loses the core's context (PSCI's power-down
// StateType, a non-retentive SBI type) or stops its timer (`local-timer-stop`) is installed with that flag: the first is
// entered through the port's per-core resume path, and the second is held out by `procpower::enterable` - said on the
// console at the install.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn install_tree_states(tree: &fdt::TreeIdleStates) {
	#[cfg(target_arch = "aarch64")]
	let entry = abi::IDLE_ENTRY_PSCI;
	#[cfg(target_arch = "riscv64")]
	let entry = abi::IDLE_ENTRY_SBI;
	for cpu in 0..crate::smp::cpu_count() {
		let mut raw = [abi::ProcessorIdleState::default(); abi::PROCESSOR_MAX_IDLE_STATES];
		raw[0] = abi::ProcessorIdleState { entry: abi::IDLE_ENTRY_HALT, exit_latency_us: 1, target_residency_us: 1, ..abi::ProcessorIdleState::default() };
		let mut count = 1usize;
		for state in tree.for_cpu(crate::smp::lapic_id(cpu)).take(abi::PROCESSOR_MAX_IDLE_STATES - 1) {
			let mut flags = 0;
			if arch::processor::state_loses_context(state.parameter) {
				flags |= abi::IDLE_LOSES_CONTEXT;
			}
			if state.local_timer_stop {
				flags |= abi::IDLE_STOPS_TIMER;
			}
			raw[count] = abi::ProcessorIdleState { entry, flags, parameter: state.parameter, exit_latency_us: state.entry_latency_us.saturating_add(state.exit_latency_us), target_residency_us: state.min_residency_us, ..abi::ProcessorIdleState::default() };
			count += 1;
		}
		if count == 1 {
			continue;
		}
		raw[1..count].sort_unstable_by_key(|state| state.exit_latency_us);
		match install_idle(cpu, &raw[..count]) {
			0 => crate::serial_println!("processor: core {cpu}'s idle table from the device tree - {count} state(s)"),
			error => crate::serial_println!("processor: core {cpu}'s idle states from the device tree are refused ({error})"),
		}
	}
}

// INSTALL CORE `cpu`'S IDLE TABLE - `raw` empty uninstalls it. See `SYS_PROCESSOR_IDLE_TABLE`.
pub fn install_idle(cpu: usize, raw: &[abi::ProcessorIdleState]) -> i64 {
	if cpu >= crate::smp::cpu_count() {
		return abi::ERR_INVALID;
	}
	let mut states: Vec<IdleState> = Vec::new();
	if states.try_reserve_exact(raw.len()).is_err() {
		return abi::ERR_NO_MEMORY;
	}
	for record in raw {
		let Some(state) = idle_state_of(record) else { return abi::ERR_INVALID };
		states.push(state);
	}
	if !states.is_empty()
		&& let Err(refusal) = procpower::check_idle_table(&states, arch::processor::ARCH)
	{
		crate::serial_println!("processor: core {cpu}'s idle table is refused - {refusal:?}");
		return abi::ERR_INVALID;
	}
	let Some(new_registers) = procpower::idle_registers(&states) else { return abi::ERR_NO_MEMORY };
	let Some(old_registers) = crate::mem::heap::try_collect(CORES[cpu].lock().idle.iter().filter_map(|installed| installed.access.map(|access| access.register))) else { return abi::ERR_NO_MEMORY };
	// THE NEW TABLE'S ROOM, before a register is taken: past this point nothing can fail.
	let mut installed: Vec<Installed> = Vec::new();
	if installed.try_reserve_exact(states.len()).is_err() {
		return abi::ERR_NO_MEMORY;
	}
	if let Err(error) = replace_registers(&old_registers, &new_registers) {
		return error;
	}
	let cpu_offers = arch::processor::cpu();
	for (index, state) in states.iter().enumerate() {
		let unenterable = procpower::enterable(state, index, &cpu_offers, arch::processor::ARCH).err();
		if let Some(why) = unenterable {
			crate::serial_println!("processor: core {cpu}'s idle state {index} ({} us out) is not entered - {}", state.exit_latency_us, unenterable_text(why));
		}
		let access = match state.entry {
			Entry::Register(register) => Some(access_of(&register)),
			_ => None,
		};
		installed.push(Installed { state: *state, unenterable, access });
	}
	CORES[cpu].lock().idle = installed;
	0
}

fn control_register(table: &perf::Table) -> Register {
	match &table.control {
		perf::Control::States { control, .. } => *control,
		perf::Control::Cppc { desired, .. } => *desired,
	}
}

// INSTALL CORE `cpu`'S PERFORMANCE TABLE - None uninstalls it. The window becomes the whole table and the core runs at
// its fastest level until the governor has decided otherwise.
pub fn install_perf(cpu: usize, raw: Option<&abi::ProcessorPerfTable>) -> i64 {
	if cpu >= crate::smp::cpu_count() {
		return abi::ERR_INVALID;
	}
	let table = match raw {
		Some(raw) => {
			let table = match perf_table_of(raw) {
				Ok(table) => table,
				Err(procpower::records::Unread::Unknown) => return abi::ERR_INVALID,
				Err(procpower::records::Unread::NoMemory) => return abi::ERR_NO_MEMORY,
			};
			if let Err(refusal) = perf::check_table(&table, arch::processor::ARCH) {
				crate::serial_println!("processor: core {cpu}'s performance table is refused - {refusal:?}");
				return abi::ERR_INVALID;
			}
			Some(table)
		}
		None => None,
	};
	// The registers each table names - None for a short heap, refused before anything moves.
	let new_registers = match table.as_ref() {
		Some(table) => table.registers(),
		None => Some(Vec::new()),
	};
	let old_registers = match CORES[cpu].lock().perf.as_ref() {
		Some(installed) => installed.table.registers(),
		None => Some(Vec::new()),
	};
	let (Some(new_registers), Some(old_registers)) = (new_registers, old_registers) else { return abi::ERR_NO_MEMORY };
	if let Err(error) = replace_registers(&old_registers, &new_registers) {
		return error;
	}
	let mut core = CORES[cpu].lock();
	match table {
		Some(table) => {
			let window = Window::all(table.levels());
			let control = access_of(&control_register(&table));
			let throttle = table.throttle.as_ref().map(|throttle| access_of(&throttle.control));
			let preference = match &table.control {
				perf::Control::Cppc { preference: Some(register), .. } => Some(access_of(register)),
				_ => None,
			};
			let mut installed = PerfInstalled { table, control, throttle, preference, level: window.cap, changed_at_ns: now_ns() };
			apply(cpu, &mut installed, window.cap);
			core.perf = Some(installed);
			core.window = Some(window);
		}
		None => {
			core.perf = None;
			core.window = None;
			core.preference = None;
		}
	}
	0
}

// THE WINDOW, obeyed at once - and the level decided in it at once from the busy share the core's last period measured:
// a core in a long idle has no period ending to decide at, and would hold a level the old window wanted - the fastest a
// performance profile pinned it at, under a balanced one that would have let it rest.
pub fn set_window(cpu: usize, cap: u32, floor: u32) -> i64 {
	if cpu >= crate::smp::cpu_count() {
		return abi::ERR_INVALID;
	}
	let mut core = CORES[cpu].lock();
	let Some(levels) = core.perf.as_ref().map(|installed| installed.table.levels()) else { return abi::ERR_INVALID };
	let window = Window { cap, floor };
	if window.check(levels).is_err() {
		return abi::ERR_INVALID;
	}
	core.window = Some(window);
	let last_busy = core.last_busy_permille;
	if let Some(installed) = core.perf.as_mut() {
		let mut level = window.clamp(installed.level);
		if let Some(busy) = last_busy {
			let since = now_ns().saturating_sub(installed.changed_at_ns) / 1000;
			level = perf::next_level(level, busy, window, since, installed.table.transition_us());
		}
		if level != installed.level {
			apply(cpu, installed, level);
		}
	}
	0
}

// AFTER A SLEEP THAT LOST POWER: every core's level and preference written again - firmware may have reset the registers
// a table names - from what the kernel last wrote.
pub fn resumed() {
	for cpu in 0..crate::smp::cpu_count() {
		let mut core = CORES[cpu].lock();
		let preference = core.preference;
		if let Some(installed) = core.perf.as_mut() {
			let level = installed.level;
			apply(cpu, installed, level);
			if let (Some(access), Some(value)) = (installed.preference, preference) {
				access.write(u64::from(value));
			}
		}
	}
}

// CPPC'S ENERGY-PERFORMANCE PREFERENCE, written at once: 0 performance to 255 energy.
pub fn set_preference(cpu: usize, value: u8) -> i64 {
	if cpu >= crate::smp::cpu_count() {
		return abi::ERR_INVALID;
	}
	let mut core = CORES[cpu].lock();
	match core.perf.as_ref().and_then(|installed| installed.preference) {
		Some(access) => {
			access.write(u64::from(value));
			core.preference = Some(value);
			0
		}
		None => abi::ERR_UNSUPPORTED,
	}
}

pub fn set_injection(cpu: usize, permille: u32) -> i64 {
	if cpu >= crate::smp::cpu_count() || u64::from(permille) > abi::MAX_INJECT_PERMILLE {
		return abi::ERR_INVALID;
	}
	let mut core = CORES[cpu].lock();
	core.inject_permille = permille;
	core.inject_owed_ns = 0;
	0
}

// A LEVEL WRITTEN: its performance value to the control register, its throttling value where it throttles (T0 where it
// does not), and the time at the level it leaves counted.
fn apply(cpu: usize, installed: &mut PerfInstalled, level: u32) {
	let setting = installed.table.setting(level);
	installed.control.write(u64::from(setting.performance));
	if let Some(throttle) = installed.throttle {
		let t0 = installed.table.throttle.as_ref().map_or(0, |table| table.states[0].control);
		throttle.write(u64::from(setting.throttle.unwrap_or(t0)));
	}
	let now = now_ns();
	let counts = &COUNTS[cpu];
	let was = counts.level.swap(level, Ordering::Relaxed) as usize;
	let since = counts.level_since_ns.swap(now, Ordering::Relaxed);
	if let Some(slot) = counts.level_ns.get(was) {
		slot.fetch_add(now.saturating_sub(since), Ordering::Relaxed);
	}
	installed.level = level;
	installed.changed_at_ns = now;
}

// ------------------------------------------------------------------ the latency bound

// A NEW REQUEST of `bound_us` by process `owner`, known by `id` (the request object's koid).
pub fn latency_add(owner: u64, id: u64, bound_us: u32) -> Result<(), i64> {
	let mut requests = LATENCY.lock();
	requests.add(owner, id, bound_us).map_err(|_| abi::ERR_RESOURCE_EXHAUSTED)?;
	BOUND.store(requests.bound().unwrap_or(u32::MAX), Ordering::Release);
	Ok(())
}

// A request's last handle closed.
pub fn latency_ended(id: u64) {
	let mut requests = LATENCY.lock();
	if requests.remove(id) {
		BOUND.store(requests.bound().unwrap_or(u32::MAX), Ordering::Release);
	}
}

// ------------------------------------------------------------------ the idle path

// THE IDLE ENTRY, `idle::halt`'s last act with interrupts masked and the one-shot programmed for `wake_tick`: the state
// chosen, entered, and counted. Answers the index of the state entered.
pub fn enter(cpu: usize, wake_tick: Option<u64>) -> usize {
	// NEVER WAITED FOR: a core whose table is being replaced idles as a core with none would.
	let Some(core) = CORES[cpu].try_lock() else {
		default_idle(cpu);
		return 0;
	};
	if core.idle.is_empty() {
		drop(core);
		default_idle(cpu);
		return 0;
	}
	let until_timer_us = wake_tick.map(|tick| tick.saturating_sub(arch::apic::ticks()).saturating_mul(1_000_000 / abi::TICKS_PER_SECOND));
	let predicted = core.predictor.predict(until_timer_us);
	let bound = BOUND.load(Ordering::Acquire);
	// NOTHING ALLOCATED ON THE IDLE PATH: the candidates in an array of the table's bound.
	let mut candidates = [Candidate { exit_latency_us: 0, target_residency_us: 0, enterable: false }; procpower::MAX_IDLE_STATES];
	let count = core.idle.len().min(procpower::MAX_IDLE_STATES);
	for (slot, installed) in candidates.iter_mut().zip(core.idle.iter()) {
		*slot = Candidate { exit_latency_us: installed.state.exit_latency_us, target_residency_us: installed.state.target_residency_us, enterable: installed.unenterable.is_none() };
	}
	let chosen = procpower::governor::choose(&candidates[..count], predicted, (bound != u32::MAX).then_some(bound));
	let state = core.idle[chosen];
	drop(core);
	let started = now_ns();
	match state.state.entry {
		Entry::Halt => arch::idle_halt(),
		Entry::Mwait { hint } => arch::processor::mwait(hint),
		Entry::Register(_) => {
			// THE READ IS THE ENTRY on hardware that implements it, returning at the wake; where it returns at once the
			// halt after it is what waits.
			if let Some(access) = state.access {
				let _ = access.read();
			}
			arch::processor::after_register_entry();
		}
		Entry::Psci { parameter } | Entry::SbiSuspend { parameter } => arch::processor::firmware_suspend(parameter),
	}
	let slept = now_ns().saturating_sub(started);
	let counts = &COUNTS[cpu];
	if let Some(entries) = counts.entries.get(chosen) {
		entries.fetch_add(1, Ordering::Relaxed);
		counts.residency_ns[chosen].fetch_add(slept, Ordering::Relaxed);
	}
	if let Some(mut core) = CORES[cpu].try_lock() {
		core.predictor.observe(slept / 1000);
		core.period_idle_ns = core.period_idle_ns.saturating_add(slept);
		// IDLE THE CORE TOOK BY ITSELF is idle the injection does not have to take.
		core.inject_owed_ns = core.inject_owed_ns.saturating_sub(slept);
	}
	chosen
}

// WITH NO TABLE: what the CPU offers without one - MWAIT's C1 hint where CPUID offers MONITOR/MWAIT, the halt otherwise -
// counted as the record's one state.
fn default_idle(cpu: usize) {
	let started = now_ns();
	if arch::processor::offers_mwait() {
		arch::processor::mwait(0);
	} else {
		arch::idle_halt();
	}
	let counts = &COUNTS[cpu];
	counts.entries[0].fetch_add(1, Ordering::Relaxed);
	counts.residency_ns[0].fetch_add(now_ns().saturating_sub(started), Ordering::Relaxed);
}

// ------------------------------------------------------------------ the performance governor and the injection

// AT A SCHEDULER POINT - a busy core's tick, or an idle period's end: the performance governor decides once its period
// has passed, from the share of it the core was busy. Answers whether the core owes idle to the injection now.
pub fn on_tick(cpu: usize) -> bool {
	let Some(mut core) = CORES[cpu].try_lock() else { return false };
	let now = now_ns();
	if core.period_start_ns == 0 {
		core.period_start_ns = now;
		return false;
	}
	let elapsed = now.saturating_sub(core.period_start_ns);
	if elapsed < PERIOD_NS {
		return false;
	}
	let idle = core.period_idle_ns.min(elapsed);
	let busy_permille = ((elapsed - idle).saturating_mul(1000) / elapsed.max(1)) as u32;
	core.period_start_ns = now;
	core.period_idle_ns = 0;
	core.last_busy_permille = Some(busy_permille);
	// THE INJECTION'S ACCOUNT: each period owes its share as idle, taken whole when the core next gives way.
	if core.inject_permille != 0 {
		core.inject_owed_ns = core.inject_owed_ns.saturating_add(elapsed * u64::from(core.inject_permille) / 1000);
	}
	let window = core.window;
	if let (Some(installed), Some(window)) = (core.perf.as_mut(), window) {
		let since = now.saturating_sub(installed.changed_at_ns) / 1000;
		let next = perf::next_level(installed.level, busy_permille, window, since, installed.table.transition_us());
		if next != installed.level {
			apply(cpu, installed, next);
		}
	}
	core.inject_owed_ns >= PERIOD_NS / 4
}

// THE IDLE THE INJECTION OWES, taken: how long, in ticks, the core is to stay idle now, and the account settled.
pub fn take_injection(cpu: usize) -> u64 {
	let Some(mut core) = CORES[cpu].try_lock() else { return 0 };
	let owed = core.inject_owed_ns;
	core.inject_owed_ns = 0;
	COUNTS[cpu].injected_ns.fetch_add(owed, Ordering::Relaxed);
	owed.div_ceil(1_000_000_000 / abi::TICKS_PER_SECOND)
}

// ------------------------------------------------------------------ the free read

// CORE `cpu`'S SHARE OF ITS IDLE RECORD.
pub fn fill_info(cpu: usize, info: &mut abi::CpuIdleInfo) {
	let counts = &COUNTS[cpu];
	let core = CORES[cpu].lock();
	if core.idle.is_empty() {
		info.state_count = 1;
		let entry = if arch::processor::offers_mwait() { abi::IDLE_ENTRY_MWAIT } else { abi::IDLE_ENTRY_HALT };
		info.states[0] = abi::CpuIdleStateInfo { entry, unenterable: abi::IDLE_ENTERABLE, exit_latency_us: 0, target_residency_us: 0, entries: 0, residency_ns: 0 };
	} else {
		info.state_count = core.idle.len() as u32;
		for (at, installed) in core.idle.iter().enumerate().take(abi::PROCESSOR_MAX_IDLE_STATES) {
			let entry = match installed.state.entry {
				Entry::Halt => abi::IDLE_ENTRY_HALT,
				Entry::Mwait { .. } => abi::IDLE_ENTRY_MWAIT,
				Entry::Register(_) => abi::IDLE_ENTRY_REGISTER,
				Entry::Psci { .. } => abi::IDLE_ENTRY_PSCI,
				Entry::SbiSuspend { .. } => abi::IDLE_ENTRY_SBI,
			};
			let unenterable = match installed.unenterable {
				None => abi::IDLE_ENTERABLE,
				Some(procpower::Unenterable::NoMwait) => abi::IDLE_NO_MWAIT,
				Some(procpower::Unenterable::CounterMayStop) => abi::IDLE_COUNTER_MAY_STOP,
				Some(procpower::Unenterable::TimerStops) => abi::IDLE_TIMER_STOPS,
				Some(procpower::Unenterable::ContextLost) => abi::IDLE_CONTEXT_LOST,
			};
			info.states[at] = abi::CpuIdleStateInfo { entry, unenterable, exit_latency_us: installed.state.exit_latency_us, target_residency_us: installed.state.target_residency_us, entries: 0, residency_ns: 0 };
		}
	}
	for at in 0..abi::PROCESSOR_MAX_IDLE_STATES {
		info.states[at].entries = counts.entries[at].load(Ordering::Relaxed);
		info.states[at].residency_ns = counts.residency_ns[at].load(Ordering::Relaxed);
	}
	if let (Some(installed), Some(window)) = (core.perf.as_ref(), core.window) {
		info.perf_levels = installed.table.levels();
		info.perf_level = installed.level;
		info.window_cap = window.cap;
		info.window_floor = window.floor;
	}
	info.inject_permille = core.inject_permille;
	drop(core);
	info.injected_ns = counts.injected_ns.load(Ordering::Relaxed);
	for (at, slot) in counts.level_ns.iter().enumerate() {
		info.level_ns[at] = slot.load(Ordering::Relaxed);
	}
	// THE LEVEL NOW counts up to this reading.
	let level = counts.level.load(Ordering::Relaxed) as usize;
	if info.perf_levels != 0
		&& let Some(slot) = info.level_ns.get_mut(level)
	{
		*slot = slot.saturating_add(now_ns().saturating_sub(counts.level_since_ns.load(Ordering::Relaxed)));
	}
	let requests = LATENCY.lock();
	info.latency_requests = requests.len() as u32;
	info.latency_bound_us = requests.bound().unwrap_or(u32::MAX);
}

#[cfg(test)]
mod tests;

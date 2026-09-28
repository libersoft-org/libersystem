// IDLE WITHOUT A TICK: every halt this kernel makes, the one-shot its core's timer becomes first, what
// wakes a halted core, and the per-core record of it.
//
// A CORE WITH NOTHING TO RUN USED TO TAKE ITS PERIODIC TICK anyway, a hundred times a second, because the
// tick carried the clock and was the backstop that picked up work nobody had told it about. The clock is
// computed from the counter now (`arch::common::time::CLOCK`), so an idle core's timer is a ONE-SHOT for
// the earliest thing it has to wake for, and every other reason it had to wake is delivered instead:
//
//   work placed on another core      comes with a wake IPI (`wake_core`)
//   a deadline armed on another core  earlier than the wake the BSP published: a wake IPI to the BSP
//                                     (`deadline_armed`), which re-programs
//   what the BSP's idle hook watches  a lost SystemManager, a shell that starts listening: a wake IPI to
//                                     the BSP (`wake_bsp`)
//   polled housekeeping               hot-plug ports with no routed slot interrupt, PCI error reporting and
//                                     the IOMMU's fault queue run on the BSP at HOUSEKEEPING_BOUND - the
//                                     idle BSP's longest sleep while any of them exists on the machine
//   held console input, a non-empty   cap an idle core's sleep at one tick until they are empty - the latency
//   serial transmit ring              they had
//   a console UART with no receive    caps the idle BSP's sleep at one tick: its bytes are found by the poll
//   line its port could arm           alone
//   typed input                       raises the console UART's receive interrupt on every port
//
// A BUSY CORE KEEPS ITS PERIODIC TICK - the time slice, the kill point for a ring-3 loop, `SIG_STOP`, the
// TLB-shootdown answer, the serial drain - so the one-shot is armed only for a halt, and the periodic tick
// is back before the halt returns.
//
// THE WAKE THAT LANDS JUST BEFORE A HALT. The periodic tick also ended a halt whose wake came just before
// it; a one-shot does not. So every halt masks interrupts BEFORE its last check and before it programs
// its one-shot, and waits in the form that wakes on an interrupt pending under the mask (`arch::idle_halt`
// entered masked): an interrupt that arrives after the check is held pending and ends the halt at once.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::arch;
use crate::smp::MAX_CPUS;

// How long the idle BSP may sleep while any polled housekeeping exists on the machine: ten ticks, 100 ms.
pub const HOUSEKEEPING_BOUND: u64 = 10;

// THE POLLED HOUSEKEEPING THIS MACHINE HAS, one bit each, said by whoever finds it.
// (Arming hot-plug slots is the production boot tail's, which a test build does not run.)
#[cfg(not(test))]
pub const HOUSEKEEPING_HOT_PLUG: u32 = 1 << 0;
pub const HOUSEKEEPING_PCI_ERRORS: u32 = 1 << 1;
pub const HOUSEKEEPING_IOMMU_FAULTS: u32 = 1 << 2;
static HOUSEKEEPING: AtomicU32 = AtomicU32::new(0);

// Record that this machine has housekeeping of `kind`, which the BSP must poll at HOUSEKEEPING_BOUND.
pub fn add_housekeeping(kind: u32) {
	HOUSEKEEPING.fetch_or(kind, Ordering::Relaxed);
}

// THE CONSOLE UART IS POLLED on this machine: its port found no receive line it could arm, so the idle
// BSP sleeps one tick at most - the latency the poll had while every core took a tick.
static CONSOLE_POLLED: AtomicBool = AtomicBool::new(false);

#[cfg(not(test))]
pub fn console_polled() {
	CONSOLE_POLLED.store(true, Ordering::Relaxed);
}

// WHAT THE BSP PUBLISHED as its wake while it halts: AWAKE while it is not halting, HALTING while it is
// computing a wake (so a deadline armed meanwhile always sends the IPI), and the tick it will wake at
// otherwise - `u64::MAX` for none.
const AWAKE: u64 = 0;
const HALTING: u64 = u64::MAX;
static BSP_WAKE: AtomicU64 = AtomicU64::new(AWAKE);

// Why a halted core woke.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Cause {
	Timer,
	Ipi,
	// A device's interrupt, by the identity the architecture dispatches it under: the IDT vector on x86_64,
	// the INTID on aarch64, the interrupt-file identity on riscv64.
	Device(u32),
}

// `last` encodes a cause: nothing, the timer, an IPI, or a device's identity above DEVICE.
const LAST_NONE: u32 = 0;
const LAST_TIMER: u32 = 1;
const LAST_IPI: u32 = 2;
const LAST_DEVICE: u32 = 0x100;

// ONE CORE'S RECORD, read by the free syscall that answers one per core.
struct Core {
	idle_ns: AtomicU64,
	halts: AtomicU64,
	timer: AtomicU64,
	ipi: AtomicU64,
	housekeeping: AtomicU64,
	device: AtomicU64,
	// The device identities that woke it and how often, first come first kept; a wake past the table's
	// size is counted in `device` alone.
	sources: [AtomicU32; abi::CPU_IDLE_SOURCES],
	counts: [AtomicU64; abi::CPU_IDLE_SOURCES],
	// The last interrupt this core took, while a halt is watching for it.
	last: AtomicU32,
}

impl Core {
	const fn new() -> Core {
		Core { idle_ns: AtomicU64::new(0), halts: AtomicU64::new(0), timer: AtomicU64::new(0), ipi: AtomicU64::new(0), housekeeping: AtomicU64::new(0), device: AtomicU64::new(0), sources: [const { AtomicU32::new(0) }; abi::CPU_IDLE_SOURCES], counts: [const { AtomicU64::new(0) }; abi::CPU_IDLE_SOURCES], last: AtomicU32::new(LAST_NONE) }
	}
}

static CORES: [Core; MAX_CPUS] = [const { Core::new() }; MAX_CPUS];

// Set once per-core data can be asked which core an interrupt landed on. An interrupt before that
// records nothing per core - the timer's own count below is global and needs no core.
static READY: AtomicBool = AtomicBool::new(false);

// Start keeping per-core records: called once the scheduler (and with it every core's per-CPU block)
// is up.
pub fn init() {
	READY.store(true, Ordering::Release);
}

pub fn ready() -> bool {
	READY.load(Ordering::Acquire)
}

// A timer interrupt, from the architecture's handler.
pub fn timer_interrupt() {
	interrupt(Cause::Timer);
}

// AN INTERRUPT THIS CORE TOOK, from the architecture's handler: remembered, so a halt it ended can say
// what ended it.
pub fn interrupt(cause: Cause) {
	if !ready() {
		return;
	}
	let encoded = match cause {
		Cause::Timer => LAST_TIMER,
		Cause::Ipi => LAST_IPI,
		Cause::Device(identity) => LAST_DEVICE + (identity & 0x00ff_ffff),
	};
	CORES[crate::sched::current_cpu_id()].last.store(encoded, Ordering::Relaxed);
}

// A tick as `arch::apic::ticks()` reads it, on the clock the timer is programmed against. They are the same
// tick, except in a test build, whose `ticks()` adds the harness's skew - so a deadline read in those ticks is
// taken back to the clock before it becomes a compare value.
fn on_the_clock(tick: u64) -> u64 {
	#[cfg(test)]
	{
		tick.saturating_sub(crate::tests::clock_skew())
	}
	#[cfg(not(test))]
	{
		tick
	}
}

// The earlier of two optional ticks.
fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
	match (a, b) {
		(Some(a), Some(b)) => Some(a.min(b)),
		(a, None) => a,
		(None, b) => b,
	}
}

// HALT THIS CORE until an interrupt, its timer a one-shot for `until` - an absolute tick, or `None` for
// no bound of the caller's own - or the earlier of what this core must wake for anyway. `ready` is the
// LAST CHECK, made with interrupts masked and before anything is programmed: true when there is work,
// and then nothing halts. Returns with interrupts enabled and the periodic tick running.
pub fn halt(until: Option<u64>, ready: impl FnOnce() -> bool) {
	let cpu = crate::sched::current_cpu_id();
	let bsp = cpu == 0;
	arch::disable_interrupts();
	// HALTING BEFORE THE DEADLINES ARE READ: a deadline another core arms after this reads the list sees
	// a wake it is earlier than, and sends the IPI that the pending-under-mask halt then answers at once.
	if bsp {
		BSP_WAKE.store(HALTING, Ordering::SeqCst);
	}
	if ready() {
		if bsp {
			BSP_WAKE.store(AWAKE, Ordering::SeqCst);
		}
		arch::enable_interrupts();
		return;
	}
	let now = arch::apic::ticks();
	let mut wake = until;
	let mut housekeeping = false;
	if bsp {
		// DEADLINE EXPIRY IS THE BSP'S, for progress and housekeeping waits alike: `min_deadline` still
		// leaves the second out of what counts as progress, but not out of when the BSP wakes.
		wake = earliest(wake, crate::sched::earliest_deadline());
		if HOUSEKEEPING.load(Ordering::Relaxed) != 0 {
			let bound = now.saturating_add(HOUSEKEEPING_BOUND);
			if wake.is_none_or(|at| bound < at) {
				wake = Some(bound);
				housekeeping = true;
			}
		}
		if crate::console_input::held() || CONSOLE_POLLED.load(Ordering::Relaxed) {
			wake = earliest(wake, Some(now.saturating_add(1)));
		}
	}
	if arch::serial::tx_pending() {
		wake = earliest(wake, Some(now.saturating_add(1)));
	}
	if bsp {
		BSP_WAKE.store(wake.unwrap_or(u64::MAX), Ordering::SeqCst);
	}
	// A clock that cannot express the wake - not anchored yet - leaves the periodic tick running, which is
	// the halt this used to be.
	let one_shot = arch::apic::timer_one_shot(wake.map(on_the_clock));
	let core = &CORES[cpu];
	core.last.store(LAST_NONE, Ordering::Relaxed);
	#[cfg(test)]
	window(cpu, wake);
	let started = arch::tsc::now();
	// Entered masked: an interrupt pending since the check ends it at once.
	arch::idle_halt();
	let ended = arch::tsc::now();
	if one_shot {
		arch::apic::timer_periodic();
	}
	if bsp {
		BSP_WAKE.store(AWAKE, Ordering::SeqCst);
	}
	core.idle_ns.fetch_add(tickclock::cycles_to_ns(ended.wrapping_sub(started), arch::common::time::CLOCK.hz()), Ordering::Relaxed);
	core.halts.fetch_add(1, Ordering::Relaxed);
	match core.last.load(Ordering::Relaxed) {
		LAST_NONE => {}
		LAST_TIMER if housekeeping => {
			core.housekeeping.fetch_add(1, Ordering::Relaxed);
		}
		LAST_TIMER => {
			core.timer.fetch_add(1, Ordering::Relaxed);
		}
		LAST_IPI => {
			core.ipi.fetch_add(1, Ordering::Relaxed);
		}
		encoded => {
			core.device.fetch_add(1, Ordering::Relaxed);
			let identity = encoded - LAST_DEVICE;
			for (source, count) in core.sources.iter().zip(core.counts.iter()) {
				// Stored as identity + 1, so zero is a free slot.
				match source.compare_exchange(0, identity + 1, Ordering::Relaxed, Ordering::Relaxed) {
					Ok(_) => {
						count.fetch_add(1, Ordering::Relaxed);
						break;
					}
					Err(held) if held == identity + 1 => {
						count.fetch_add(1, Ordering::Relaxed);
						break;
					}
					Err(_) => {}
				}
			}
		}
	}
}

// A TEST'S WAY INTO THE WINDOW between a halt's last check and the halt itself, which no ordinary timing
// reaches on purpose: the next halt core `cpu` makes runs `hook` there once - interrupts masked, the
// one-shot programmed, the wake it was programmed for given - and then halts.
#[cfg(test)]
static WINDOW_CORE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(usize::MAX);
#[cfg(test)]
static WINDOW_HOOK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub fn in_the_next_window(cpu: usize, hook: fn(Option<u64>)) {
	WINDOW_HOOK.store(hook as usize, Ordering::SeqCst);
	WINDOW_CORE.store(cpu, Ordering::SeqCst);
}

// A window a test armed and never reached is taken back, so no later test's halt runs it.
#[cfg(test)]
pub fn no_window() {
	WINDOW_CORE.store(usize::MAX, Ordering::SeqCst);
	WINDOW_HOOK.store(0, Ordering::SeqCst);
}

#[cfg(test)]
fn window(cpu: usize, wake: Option<u64>) {
	if WINDOW_CORE.load(Ordering::SeqCst) != cpu {
		return;
	}
	WINDOW_CORE.store(usize::MAX, Ordering::SeqCst);
	let raw = WINDOW_HOOK.swap(0, Ordering::SeqCst);
	if raw != 0 {
		let hook: fn(Option<u64>) = unsafe { core::mem::transmute::<usize, fn(Option<u64>)>(raw) };
		hook(wake);
	}
}

// The wake a halting BSP has published, when it has one: None while it is awake, still computing its wake,
// or halting with no wake at all. A test's way to arm a deadline WHILE THE BSP SLEEPS past it.
#[cfg(test)]
pub fn bsp_asleep_until() -> Option<u64> {
	match BSP_WAKE.load(Ordering::SeqCst) {
		AWAKE | HALTING => None,
		wake => Some(wake),
	}
}

// A DEADLINE ARMED on this core: if it is not the BSP and the deadline is earlier than the wake the BSP
// published, the BSP is woken to re-program. Called after the deadline is on the list.
pub fn deadline_armed(deadline: u64) {
	let published = BSP_WAKE.load(Ordering::SeqCst);
	if published != AWAKE && deadline < published && crate::sched::current_cpu_id() != 0 {
		arch::apic::send_wake_ipi(crate::smp::lapic_id(0));
	}
}

// Something the BSP's idle hook watches changed - a lost SystemManager, a shell that started listening -
// and the BSP may be asleep: wake it.
pub fn wake_bsp() {
	if BSP_WAKE.load(Ordering::SeqCst) != AWAKE && crate::sched::current_cpu_id() != 0 {
		arch::apic::send_wake_ipi(crate::smp::lapic_id(0));
	}
}

// THE PROCESS WHOSE ENDING THE BSP'S IDLE HOOK WATCHES - the resident SystemManager - by koid, zero for
// none: its terminal transition wakes the BSP, which would otherwise notice a lost control plane only at its
// next timed wake.
static WATCHED: AtomicU64 = AtomicU64::new(0);

pub fn watch_process(koid: u64) {
	WATCHED.store(koid, Ordering::Release);
}

// A process reached its terminal state: if it is the one the idle hook watches, wake the BSP.
pub fn process_ended(koid: u64) {
	if koid != 0 && WATCHED.load(Ordering::Acquire) == koid {
		wake_bsp();
	}
}

// Work was placed on core `cpu` from another: wake it, since its timer is no backstop any more.
pub fn wake_core(cpu: usize) {
	if cpu != crate::sched::current_cpu_id() {
		arch::apic::send_wake_ipi(crate::smp::lapic_id(cpu));
	}
}

// Core `cpu`'s record, for the free read syscall; None past the online cores.
pub fn info(cpu: usize) -> Option<abi::CpuIdleInfo> {
	if cpu >= crate::smp::cpu_count() {
		return None;
	}
	let core = &CORES[cpu];
	let mut sources = [abi::CpuIdleSource { source: 0, _pad: 0, count: 0 }; abi::CPU_IDLE_SOURCES];
	let mut used = 0u32;
	for (slot, (source, count)) in sources.iter_mut().zip(core.sources.iter().zip(core.counts.iter())) {
		let held = source.load(Ordering::Relaxed);
		if held == 0 {
			break;
		}
		*slot = abi::CpuIdleSource { source: held - 1, _pad: 0, count: count.load(Ordering::Relaxed) };
		used += 1;
	}
	Some(abi::CpuIdleInfo { cpu: cpu as u32, source_count: used, idle_ns: core.idle_ns.load(Ordering::Relaxed), halts: core.halts.load(Ordering::Relaxed), wakes_timer: core.timer.load(Ordering::Relaxed), wakes_ipi: core.ipi.load(Ordering::Relaxed), wakes_housekeeping: core.housekeeping.load(Ordering::Relaxed), wakes_device: core.device.load(Ordering::Relaxed), sources })
}

#[cfg(test)]
mod tests;

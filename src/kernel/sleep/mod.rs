// THE KERNEL'S SLEEP ENTRY, the sleep types the firmware describes, and the clocks across a sleep.
//
// ONE PRIVILEGED OPERATION, `SYS_SYSTEM_SLEEP`, requiring MANAGE on the root Domain as `SYS_SYSTEM_POWER` does. It is
// the last step of the userspace transaction - the applications frozen, the writes held, every driver suspended, the
// platform prepared - and it answers when the machine is awake again, with what woke it.
//
// SUSPEND TO IDLE, on every architecture: no firmware transition. The entry puts its `sleep: entered` line on the wire
// synchronously, HOLDS THE MONOTONIC CLOCK at the counter's reading (`tickclock::Clock::suspend`), and parks every
// core: the others in the idle loop's sleep mode (`idle::halt`), which programs no timer at all and runs no deadline
// check, and the entry's own core here, whose one-shot is the timed wake alone, on the raw counter. An interrupt
// outside the wake set is handled and the core parks again. The first act on the way out is the REBASE: the counter
// that ran on through the sleep is taken out of the monotonic clock, so neither clock jumps and no deadline passes in
// a sleep. The boot-time clock takes the sleep in.
//
// SUSPEND TO RAM is the firmware's transition (`arch::sleep`), entered from the boot core's idle
// context once every other core is parked (`boot_core`). HIBERNATION's snapshot and a restore's replacement are entered
// there too, on every port: x86_64's own (`arch::x86_64::hibernate`), the device-tree ports' through the per-core resume
// path (`image`).

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use abi::{ERR_ACCESS_DENIED, ERR_INTERRUPTED, ERR_INVALID, ERR_UNSUPPORTED, SleepReport};

use crate::arch;
use crate::arch::common::time::CLOCK;

// ------------------------------------------------------------------ the sleep types the firmware registered

// Each state's `\_Sx` pair, indexed by x: `REGISTERED | b << 8 | a`, zero while nothing registered it.
const REGISTERED: u32 = 1 << 31;
static SLEEP_TYPES: [AtomicU32; 6] = [const { AtomicU32::new(0) }; 6];

fn index_of(state: u64) -> Option<usize> {
	match state {
		abi::SLEEP_STATE_RAM => Some(3),
		abi::SLEEP_STATE_DISK => Some(4),
		abi::SLEEP_STATE_SOFT_OFF => Some(5),
		_ => None,
	}
}

// THE ACPI SERVICE'S REGISTRATION of a state's pair. A registered value outlives the service that registered it: it
// is firmware data, and a service that dies after registering leaves it in force; a restarted one registers the same.
pub fn register(state: u64, typ_a: u64, typ_b: u64) -> i64 {
	let Some(index) = index_of(state) else { return ERR_INVALID };
	// SLP_TYP is three bits of PM1 control.
	if typ_a > 7 || typ_b > 7 {
		return ERR_INVALID;
	}
	if crate::firmware::sleep_registration_refused() {
		return ERR_UNSUPPORTED;
	}
	SLEEP_TYPES[index].store(REGISTERED | (typ_b as u32) << 8 | typ_a as u32, Ordering::Release);
	0
}

// The registered pair for `\_Sx`, if one is.
#[cfg(any(test, target_arch = "x86_64"))]
pub fn sleep_type(x: usize) -> Option<(u8, u8)> {
	let value = SLEEP_TYPES.get(x)?.load(Ordering::Acquire);
	(value & REGISTERED != 0).then_some(((value & 0x7) as u8, ((value >> 8) & 0x7) as u8))
}

// `SYS_SLEEP_STATES`: each architecture admits its own firmware transition. ACPI's registered _S3
// pair is an x86 requirement, not a prerequisite for PSCI or SBI system suspend.
pub fn states() -> u64 {
	let mut bits: u64 = 0;
	if arch::sleep::offers_idle() {
		bits |= 1 << abi::SLEEP_STATE_IDLE;
	}
	if arch::sleep::offers_ram() {
		bits |= 1 << abi::SLEEP_STATE_RAM;
	}
	// HIBERNATION'S KERNEL HALF: the snapshot, and `\_S4` or soft-off after it. Whether the machine is SET UP for it -
	// a hibernation partition, and whether a TPM seals the key - is the image component's to say.
	if arch::sleep::offers_disk() {
		bits |= 1 << abi::SLEEP_STATE_DISK;
	}
	bits | arch::sleep::fixed_buttons()
}

// ------------------------------------------------------------------ the wall clock

// THE WALL CLOCK A DRIVER HANDED, on a machine where the kernel reads no RTC of its own: Unix seconds at a boot-time
// instant. `SYS_CLOCK_RTC` answers it counted forward on the boot-time clock - so TimeService, StorageService's stamps and
// every other reader keep the one kernel read - and 0 before one is handed.
static BASE_UNIX: AtomicU64 = AtomicU64::new(0);
static BASE_BOOT_NS: AtomicU64 = AtomicU64::new(0);
// A SUSPEND TO RAM'S LENGTH NOT YET KNOWN: the counter restarted and no RTC was read, so the base handed at the resume
// is what the boot-time clock takes the sleep from, in one step.
static SLEPT_UNKNOWN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// `SYS_CLOCK_RTC`: the kernel's own RTC where it drives one, the handed base otherwise.
pub fn rtc_unix() -> u64 {
	if arch::rtc_present() {
		return arch::rtc::read_unix();
	}
	let base = BASE_UNIX.load(Ordering::Acquire);
	if base == 0 {
		return 0;
	}
	base + boot_ns().saturating_sub(BASE_BOOT_NS.load(Ordering::Acquire)) / 1_000_000_000
}

// `SYS_CLOCK_BASE`: the base handed. After an S3 whose length no RTC gave, the difference between it and what the old
// base counts forward to is the sleep, which the boot-time clock takes in before the new base is kept.
pub fn set_clock_base(unix: u64) -> i64 {
	if unix == 0 {
		return ERR_INVALID;
	}
	let old = BASE_UNIX.load(Ordering::Acquire);
	if SLEPT_UNKNOWN.swap(false, Ordering::AcqRel) && old != 0 {
		let expected = old + boot_ns().saturating_sub(BASE_BOOT_NS.load(Ordering::Acquire)) / 1_000_000_000;
		let slept = unix.saturating_sub(expected);
		add_slept(slept.saturating_mul(1_000_000_000));
		crate::serial_println!("sleep: the clock's source says the sleep lasted {slept} s");
	}
	BASE_BOOT_NS.store(boot_ns(), Ordering::Release);
	BASE_UNIX.store(unix, Ordering::Release);
	0
}

// A suspend to RAM or a restore with no RTC to measure it: its length comes with the next base.
pub fn slept_unknown() {
	SLEPT_UNKNOWN.store(true, Ordering::Release);
}

// ------------------------------------------------------------------ the boot-time clock

// EVERY SLEEP'S LENGTH, summed: the boot-time clock is the monotonic clock plus this. A suspend to idle's is the
// counter's difference; an S3's, whose counter restarted, is the RTC's.
static SLEPT_NS: AtomicU64 = AtomicU64::new(0);

// Nanoseconds since boot, sleep included.
pub fn boot_ns() -> u64 {
	CLOCK.nanos(arch::tsc::now()).saturating_add(SLEPT_NS.load(Ordering::Acquire))
}

pub fn add_slept(ns: u64) {
	SLEPT_NS.fetch_add(ns, Ordering::AcqRel);
}

// ------------------------------------------------------------------ what woke the machine

// THE FIRST WAKE THE WAKE SET SAW while the machine slept: `reason << 32 | detail`, zero for none. The fixed buttons
// are reported here and NOT delivered as a press - `system_wakeup` itself sets the power button's status on QEMU, and
// a delivered press would power the machine off.
static WOKE: AtomicU64 = AtomicU64::new(0);

// The core a suspend to idle parks its entry on, `usize::MAX` outside one.
static ENTRY_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);

pub fn woke_by(reason: u32, detail: u32) {
	if WOKE.compare_exchange(0, u64::from(reason) << 32 | u64::from(detail), Ordering::AcqRel, Ordering::Acquire).is_err() {
		return;
	}
	// THE ENTRY'S CORE IS WOKEN when the wake landed on another: it is halted with nothing but the timed wake armed,
	// and a button or a device routed elsewhere would otherwise wait for that.
	let entry = ENTRY_CPU.load(Ordering::Acquire);
	if entry != usize::MAX && entry != crate::sched::current_cpu_id() {
		arch::apic::send_wake_ipi(crate::smp::lapic_id(entry));
	}
}

// THE DEVICES' PART OF THE WAKE SET: the interrupts drivers marked in a `SUSPEND` step asked to arm wake
// (`SYS_INTERRUPT_WAKE`), by their architectural identity plus one - zero is a free slot. A suspend to idle leaves them
// unmasked, and the first to fire while the machine sleeps ends the sleep as a device's wake. A mark goes with the
// binding that made it.
const WAKE_SOURCES: usize = 32;
static WAKE_SET: [AtomicU32; WAKE_SOURCES] = [const { AtomicU32::new(0) }; WAKE_SOURCES];

// Marked or unmarked; false when the set is full.
pub fn mark_wake(identity: u32, on: bool) -> bool {
	let stored = identity.wrapping_add(1);
	if !on {
		for slot in WAKE_SET.iter() {
			let _ = slot.compare_exchange(stored, 0, Ordering::AcqRel, Ordering::Acquire);
		}
		return true;
	}
	is_wake(identity) || WAKE_SET.iter().any(|slot| slot.compare_exchange(0, stored, Ordering::AcqRel, Ordering::Acquire).is_ok())
}

pub fn is_wake(identity: u32) -> bool {
	let stored = identity.wrapping_add(1);
	WAKE_SET.iter().any(|slot| slot.load(Ordering::Acquire) == stored)
}

// A DEVICE INTERRUPT TAKEN WHILE THE MACHINE SLEEPS: one of the wake set's wakes it, its identity the detail.
pub fn device_interrupt(identity: u32) {
	if is_wake(identity) {
		woke_by(abi::WAKE_DEVICE, identity);
	}
}

fn take_woke() -> Option<(u32, u32)> {
	let value = WOKE.swap(0, Ordering::AcqRel);
	(value != 0).then_some(((value >> 32) as u32, value as u32))
}

// Whether the machine is in a sleep - for the handlers that report a wake rather than an event.
#[cfg(any(test, target_arch = "x86_64"))]
pub fn sleeping() -> bool {
	crate::idle::sleeping()
}

// ------------------------------------------------------------------ the entry

// `SYS_SYSTEM_SLEEP`.
pub fn sys_system_sleep(domain: &alloc::sync::Arc<crate::object::domain::Domain>, state: u64, timed_wake: u64) -> Result<SleepReport, i64> {
	if !alloc::sync::Arc::ptr_eq(domain, &crate::sched::root_domain()) {
		return Err(ERR_ACCESS_DENIED);
	}
	// A FORCED POWER-OFF DEADLINE ARMED refuses the entry: a sleep's held clock would postpone it.
	if crate::power::armed() {
		crate::serial_println!("sleep: not entered - a forced power-off deadline is armed");
		return Err(ERR_INTERRUPTED);
	}
	// THE TIMED WAKE, a deadline on the boot-time clock, as nanoseconds from now.
	let after = match timed_wake {
		0 => None,
		deadline => match deadline.checked_sub(boot_ns()) {
			Some(ns) if ns > 0 => Some(ns),
			_ => return Err(ERR_INVALID),
		},
	};
	match state {
		abi::SLEEP_STATE_IDLE => suspend_to_idle(after),
		abi::SLEEP_STATE_RAM => arch::sleep::suspend_to_ram(after),
		// HIBERNATION'S SNAPSHOT: answered twice - `WAKE_SNAPSHOT`, then `WAKE_RESTORED` in the machine restored from its
		// image (see `disk`). Offered where `\_S4` or soft-off can end it, which is every machine the entry runs on.
		abi::SLEEP_STATE_DISK => arch::sleep::snapshot(),
		// THE MACHINE OFF WITH ITS IMAGE WRITTEN.
		abi::SLEEP_STATE_DISK_ENTER => arch::sleep::enter_disk(),
		_ => Err(ERR_INVALID),
	}
}

// The counter reading `after` nanoseconds from now.
fn counter_after(ns: u64) -> u64 {
	let hz = CLOCK.hz().max(1) as u128;
	arch::tsc::now().wrapping_add((u128::from(ns) * hz / 1_000_000_000).min(u128::from(u64::MAX / 2)) as u64)
}

// THE COMMON PROLOGUE: a wake already pending refuses the entry; the line goes out synchronously; the clock is held.
pub fn prologue(what: &str) -> Result<u64, i64> {
	if arch::sleep::wake_pending() {
		return Err(ERR_INTERRUPTED);
	}
	let _ = WOKE.swap(0, Ordering::AcqRel);
	arch::serial::sleep_begin();
	crate::serial_println!("sleep: entered ({what}, at tick {})", arch::apic::ticks());
	let suspended_at = arch::tsc::now();
	CLOCK.suspend(suspended_at);
	Ok(suspended_at)
}

// THE COMMON EPILOGUE, after the rebase: the resume line, and the COM1 window closed - and after a sleep that lost power,
// every core's performance level and preference written again, since firmware may have reset the registers.
pub fn epilogue(report: &SleepReport, lost_settings: bool) {
	if lost_settings {
		crate::processor::resumed();
	}
	arch::serial::sleep_wake(lost_settings);
	crate::serial_println!("sleep: resumed ({}, after {} ms, at tick {})", wake_name(report.wake), report.slept_ns / 1_000_000, arch::apic::ticks());
	arch::serial::sleep_end();
}

fn wake_name(reason: u32) -> &'static str {
	match reason {
		abi::WAKE_TIMER => "the timed wake",
		abi::WAKE_POWER_BUTTON => "the power button",
		abi::WAKE_SLEEP_BUTTON => "the sleep button",
		abi::WAKE_DEVICE => "a device",
		abi::WAKE_RTC => "the RTC alarm",
		abi::WAKE_PLATFORM => "the platform",
		abi::WAKE_RESTORED => "the restore of a hibernation image",
		_ => "an unknown wake",
	}
}

fn suspend_to_idle(after: Option<u64>) -> Result<SleepReport, i64> {
	let wake_at = after.map(counter_after);
	let suspended_at = prologue("suspend to idle")?;
	arch::sleep::mask_device_lines();
	ENTRY_CPU.store(crate::sched::current_cpu_id(), Ordering::Release);
	crate::idle::begin_sleep();
	let (wake, detail) = park_here(wake_at);
	ENTRY_CPU.store(usize::MAX, Ordering::Release);
	// THE REBASE, FIRST: the counter that ran through the sleep leaves the monotonic clock.
	let resumed_at = arch::tsc::now();
	CLOCK.rebase(suspended_at, resumed_at);
	let slept_ns = tickclock::cycles_to_ns(resumed_at.wrapping_sub(suspended_at), CLOCK.hz());
	add_slept(slept_ns);
	crate::idle::end_sleep();
	arch::sleep::unmask_device_lines();
	arch::apic::timer_periodic();
	let mut report = SleepReport { wake, detail, slept_ns, ..SleepReport::default() };
	report.core_count = crate::idle::park_report(&mut report.cores) as u32;
	epilogue(&report, false);
	Ok(report)
}

// THE ENTRY'S OWN CORE PARKED: masked, a last look for a wake, the one-shot at the timed wake alone, and a halt an
// interrupt pending under the mask ends at once. What ended it is counted; anything but a wake parks again.
fn park_here(wake_at: Option<u64>) -> (u32, u32) {
	let cpu = crate::sched::current_cpu_id();
	// THE TICK THE PERIODIC TIMER RAISED BEFORE ITS MODE CHANGED, still pending, is taken here - interrupts open for one
	// instruction past `sti`'s shadow - and the counts start after it: it is the moment before the sleep, not the sleep.
	arch::disable_interrupts();
	arch::apic::timer_at_counter(wake_at);
	arch::enable_interrupts();
	core::hint::spin_loop();
	arch::disable_interrupts();
	crate::idle::forget_park(cpu);
	loop {
		arch::disable_interrupts();
		if let Some(woke) = take_woke() {
			arch::enable_interrupts();
			return woke;
		}
		if wake_at.is_some_and(|at| (arch::tsc::now().wrapping_sub(at) as i64) >= 0) {
			arch::enable_interrupts();
			return (abi::WAKE_TIMER, 0);
		}
		arch::apic::timer_at_counter(wake_at);
		crate::idle::park_once(cpu);
	}
}

#[cfg(test)]
mod tests;

pub mod boot_core;
pub mod disk;
#[cfg(not(target_arch = "x86_64"))]
pub mod image;
#[cfg(target_arch = "riscv64")]
pub mod system;

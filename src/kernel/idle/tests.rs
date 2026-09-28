use crate::{arch, sched, smp};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::object::thread::Thread;

// Koids nothing in the kernel hands out, so only the test that names one ever wakes a thread parked on it.
const HOLD: u64 = u64::MAX - 0x1980;
const TIMEOUT: u64 = u64::MAX - 0x1981;

// A kernel thread built and NOT started, so a test decides where and when it becomes runnable.
fn prepared(entry: extern "C" fn(u64)) -> Arc<Thread> {
	let process = crate::object::process::Process::new(sched::kernel_as(), sched::root_domain()).expect("out of memory for a test thread's process");
	sched::prepare_in_process(entry, 0, &process)
}

// Spin with interrupts masked until `done` or `ticks` have passed, answering TLB shootdowns meanwhile as
// any masked spin must; whether `done` came.
fn masked_until(ticks: u64, done: impl Fn() -> bool) -> bool {
	let give_up = arch::apic::ticks() + ticks;
	while !done() {
		if arch::apic::ticks() >= give_up {
			return false;
		}
		crate::mem::tlb::service_pending();
		// Not every spin: what is polled may take a lock the core being waited for needs.
		for _ in 0..64 {
			core::hint::spin_loop();
		}
	}
	true
}

// THE THREAD IS GONE, not merely finished: its core has dropped it, so its Domain charge is back and the next
// test starts from a machine this one left as it found it.
fn gone(thread: Arc<Thread>) {
	let mut spins = 0u64;
	while Arc::strong_count(&thread) > 1 {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 20_000_000_000, "a finished test thread was never reaped by its core");
	}
}

crate::tagged_test!(a_timeout_armed_on_another_core_expires_on_time_while_the_boot_processor_sleeps, [Scheduler, Smp], id = "kernel.idle.a_timeout_armed_on_another_core_expires_on_time_while_the_boot_processor_sleeps", covers = ["kernel"]);
fn a_timeout_armed_on_another_core_expires_on_time_while_the_boot_processor_sleeps() {
	// DEADLINE EXPIRY IS THE BOOT PROCESSOR'S, and it no longer wakes a hundred times a second to look. A core
	// that arms a timeout earlier than the wake the sleeping BSP published has to tell it, or the timeout
	// expires at whatever the BSP was going to wake for anyway - its own window here, a housekeeping bound on
	// a machine that has one. So the timeout is armed on core 1 only once the BSP is asleep with a wake at
	// least five ticks out, two ticks from now: it expires on time only if the BSP re-programmed.
	static PUBLISHED: AtomicU64 = AtomicU64::new(0);
	static DEADLINE: AtomicU64 = AtomicU64::new(0);
	static WOKE: AtomicU64 = AtomicU64::new(0);
	extern "C" fn holder(_arg: u64) {
		// A PROGRESS wait far away, so the boot processor's drain has something to wait for and halts.
		sched::block_on(HOLD, arch::apic::ticks() + 1_000);
	}
	extern "C" fn waiter(_arg: u64) {
		let give_up = arch::apic::ticks() + 500;
		let published = loop {
			let now = arch::apic::ticks();
			if let Some(wake) = super::bsp_asleep_until()
				&& wake >= now + 5
			{
				break wake;
			}
			if now >= give_up {
				sched::wake_object(HOLD);
				return;
			}
			core::hint::spin_loop();
		};
		let deadline = arch::apic::ticks() + 2;
		PUBLISHED.store(published, Ordering::SeqCst);
		DEADLINE.store(deadline, Ordering::SeqCst);
		// A koid nothing signals: only the deadline ends this wait.
		sched::block_on(TIMEOUT, deadline);
		WOKE.store(arch::apic::ticks(), Ordering::SeqCst);
		// And the boot processor's drain may end: the holder goes.
		sched::wake_object(HOLD);
	}
	if smp::cpu_count() < 2 {
		return;
	}
	PUBLISHED.store(0, Ordering::SeqCst);
	DEADLINE.store(0, Ordering::SeqCst);
	WOKE.store(0, Ordering::SeqCst);
	// Output left in the ring caps an idle core's sleep at a tick, which would wake the BSP on time for
	// nothing: emptied first, and nothing is printed until the drain returns.
	arch::serial::drain_sync();
	let ipi_before = super::info(0).map_or(0, |record| record.wakes_ipi);
	sched::spawn(holder, 0);
	let waiter_thread = sched::spawn_on(1, waiter, 0);
	sched::run_until_idle_until(arch::apic::ticks() + 600);
	// WHAT A LATE RUN LEFT BEHIND, run to its end before anything is asserted: a drain whose window closed
	// returns with a woken thread still queued, and waiting for that thread to be reaped would never end.
	sched::run_until_idle();
	gone(waiter_thread);
	let (published, deadline, woke) = (PUBLISHED.load(Ordering::SeqCst), DEADLINE.load(Ordering::SeqCst), WOKE.load(Ordering::SeqCst));
	let ipis = super::info(0).map_or(0, |record| record.wakes_ipi) - ipi_before;
	assert!(deadline != 0, "the boot processor never slept with a wake five ticks out, so no timeout was armed while it slept");
	assert!(woke != 0, "the timeout armed on core 1 never expired inside the boot processor's window");
	assert!(woke >= deadline, "a timeout expired early: at tick {woke}, armed for {deadline}");
	assert!(woke <= deadline + 1, "the timeout armed for tick {deadline} expired at {woke}: the boot processor slept on toward the wake it had published, tick {published}");
	assert!(ipis > 0, "and the boot processor was woken by an IPI, which is how it hears of the earlier deadline");
	crate::serial_println!("    a timeout armed on core 1 for tick {deadline} while the boot processor slept toward {published} expired at {woke}");
}

crate::tagged_test!(a_thread_made_runnable_between_the_last_check_and_the_halt_runs_within_a_tick, [Scheduler, Smp], id = "kernel.idle.a_thread_made_runnable_between_the_last_check_and_the_halt_runs_within_a_tick", covers = ["kernel"]);
fn a_thread_made_runnable_between_the_last_check_and_the_halt_runs_within_a_tick() {
	// THE WAKE THAT LANDS JUST BEFORE A HALT. The periodic tick used to end a halt whose wake came after its
	// last check; a one-shot for a far wake does not. Here the window between the boot processor's last check
	// and its halt is where core 1 puts a thread on the boot processor's run queue - with the wake IPI every
	// placement on another core sends - and the thread must run within a tick. It does only if the halt was
	// entered with interrupts masked BEFORE the check and waits in the form that an interrupt pending under
	// the mask ends: otherwise the IPI is taken before the halt, the halt then sleeps to its one-shot, and the
	// thread waits for that.
	static WAKE: AtomicU64 = AtomicU64::new(0);
	static IN_WINDOW: AtomicU64 = AtomicU64::new(0);
	static RAN: AtomicU64 = AtomicU64::new(0);
	static GO: AtomicBool = AtomicBool::new(false);
	static PLACED: AtomicBool = AtomicBool::new(false);
	static FINISHED: AtomicBool = AtomicBool::new(false);
	static THREAD: crate::sync::SpinLock<Option<Arc<Thread>>> = crate::sync::SpinLock::new(None);
	extern "C" fn holder(_arg: u64) {
		sched::block_on(HOLD, arch::apic::ticks() + 1_000);
	}
	extern "C" fn stamp(_arg: u64) {
		RAN.store(arch::apic::ticks(), Ordering::SeqCst);
		sched::wake_object(HOLD);
	}
	extern "C" fn placer(_arg: u64) {
		while !GO.load(Ordering::SeqCst) {
			if FINISHED.load(Ordering::SeqCst) {
				return;
			}
			core::hint::spin_loop();
		}
		if let Some(thread) = THREAD.lock().take() {
			sched::start_thread_on(0, &thread);
		}
		PLACED.store(true, Ordering::SeqCst);
	}
	fn in_the_window(wake: Option<u64>) {
		IN_WINDOW.store(arch::apic::ticks(), Ordering::SeqCst);
		WAKE.store(wake.unwrap_or(u64::MAX), Ordering::SeqCst);
		GO.store(true, Ordering::SeqCst);
		// Until the placement and its IPI are done: the IPI is then pending under the mask when the halt begins.
		masked_until(500, || PLACED.load(Ordering::SeqCst));
	}
	if smp::cpu_count() < 2 {
		return;
	}
	WAKE.store(0, Ordering::SeqCst);
	IN_WINDOW.store(0, Ordering::SeqCst);
	RAN.store(0, Ordering::SeqCst);
	GO.store(false, Ordering::SeqCst);
	PLACED.store(false, Ordering::SeqCst);
	FINISHED.store(false, Ordering::SeqCst);
	let thread = prepared(stamp);
	*THREAD.lock() = Some(thread.clone());
	arch::serial::drain_sync();
	sched::spawn(holder, 0);
	let placer_thread = sched::spawn_on(1, placer, 0);
	// THE NEXT HALT THE BOOT PROCESSOR MAKES is the drain's deadline wait below, with the holder parked.
	super::in_the_next_window(0, in_the_window);
	sched::run_until_idle_until(arch::apic::ticks() + 300);
	FINISHED.store(true, Ordering::SeqCst);
	// A window never reached leaves the hook armed and the thread unstarted in its slot: both are taken back.
	super::no_window();
	THREAD.lock().take();
	sched::run_until_idle();
	gone(placer_thread);
	gone(thread);
	let (wake, in_window, ran) = (WAKE.load(Ordering::SeqCst), IN_WINDOW.load(Ordering::SeqCst), RAN.load(Ordering::SeqCst));
	assert!(in_window != 0, "the boot processor never halted inside the drain, so the window was never reached");
	assert!(PLACED.load(Ordering::SeqCst), "core 1 never placed the thread inside the window");
	assert!(wake >= in_window + 3, "the halt the window belonged to would have woken at tick {wake} anyway, {} ticks after the window - nothing was proven", wake.saturating_sub(in_window));
	assert!(ran != 0, "the thread made runnable inside the window never ran");
	assert!(ran <= in_window + 1, "the thread made runnable in the window at tick {in_window} ran at tick {ran}: the halt slept toward its one-shot, tick {wake}");
	crate::serial_println!("    a thread made runnable on the boot processor between its last check and its halt (one-shot at tick {wake}) ran at tick {ran}, placed at {in_window}");
}

crate::tagged_test!(a_one_shot_that_expires_between_the_last_check_and_the_halt_ends_the_halt, [Scheduler, Smp], id = "kernel.idle.a_one_shot_that_expires_between_the_last_check_and_the_halt_ends_the_halt", covers = ["kernel"]);
fn a_one_shot_that_expires_between_the_last_check_and_the_halt_ends_the_halt() {
	// THE OTHER WAKE THAT LANDS IN THE WINDOW: the halt's own one-shot, expired between being programmed and
	// the halt. Taken before the halt, its handler would run and the halt would then sleep with no timer left
	// at all - so core 1 stands by to end it with an IPI after thirty ticks, and says that it had to.
	static WAKE: AtomicU64 = AtomicU64::new(0);
	static LEFT_WINDOW: AtomicU64 = AtomicU64::new(0);
	static RAN: AtomicU64 = AtomicU64::new(0);
	static RESCUED: AtomicBool = AtomicBool::new(false);
	static FINISHED: AtomicBool = AtomicBool::new(false);
	extern "C" fn sleeper(_arg: u64) {
		sched::block_on(TIMEOUT, arch::apic::ticks() + 3);
		RAN.store(arch::apic::ticks(), Ordering::SeqCst);
	}
	extern "C" fn rescuer(_arg: u64) {
		while !FINISHED.load(Ordering::SeqCst) && RAN.load(Ordering::SeqCst) == 0 {
			let left = LEFT_WINDOW.load(Ordering::SeqCst);
			if left != 0 && arch::apic::ticks() >= left + 30 {
				RESCUED.store(true, Ordering::SeqCst);
				arch::apic::send_wake_ipi(smp::lapic_id(0));
				return;
			}
			core::hint::spin_loop();
		}
	}
	fn in_the_window(wake: Option<u64>) {
		let Some(wake) = wake else {
			LEFT_WINDOW.store(arch::apic::ticks(), Ordering::SeqCst);
			return;
		};
		WAKE.store(wake, Ordering::SeqCst);
		// Past the one-shot, so its interrupt is pending under the mask when the halt begins.
		masked_until(wake.saturating_sub(arch::apic::ticks()) + 50, || arch::apic::ticks() > wake);
		LEFT_WINDOW.store(arch::apic::ticks(), Ordering::SeqCst);
	}
	if smp::cpu_count() < 2 {
		return;
	}
	WAKE.store(0, Ordering::SeqCst);
	LEFT_WINDOW.store(0, Ordering::SeqCst);
	RAN.store(0, Ordering::SeqCst);
	RESCUED.store(false, Ordering::SeqCst);
	FINISHED.store(false, Ordering::SeqCst);
	arch::serial::drain_sync();
	let sleeper_thread = sched::spawn(sleeper, 0);
	let rescuer_thread = sched::spawn_on(1, rescuer, 0);
	super::in_the_next_window(0, in_the_window);
	sched::run_until_idle_until(arch::apic::ticks() + 300);
	FINISHED.store(true, Ordering::SeqCst);
	super::no_window();
	sched::run_until_idle();
	gone(rescuer_thread);
	gone(sleeper_thread);
	let (wake, left, ran) = (WAKE.load(Ordering::SeqCst), LEFT_WINDOW.load(Ordering::SeqCst), RAN.load(Ordering::SeqCst));
	assert!(wake != 0, "the halt the window belonged to had no one-shot, so none could expire in it");
	assert!(left > wake, "the window was left at tick {left}, before its one-shot at {wake} expired");
	assert!(!RESCUED.load(Ordering::SeqCst), "the halt entered after its one-shot expired slept on until core 1 woke it: the expiry was taken before the halt and lost");
	assert!(ran != 0 && ran <= left + 1, "the sleeper whose deadline the one-shot was for ran at tick {ran}, the window left at {left}");
	crate::serial_println!("    a one-shot for tick {wake} expired inside the window, and the halt after it ended at once (sleeper ran at {ran})");
}

crate::tagged_test!(
	#[cfg(target_arch = "x86_64")]
	a_transmit_burst_from_a_core_that_goes_idle_reaches_the_wire_whole,
	[Scheduler, Smp, Console],
	id = "kernel.idle.a_transmit_burst_from_a_core_that_goes_idle_reaches_the_wire_whole",
	covers = ["kernel"]
);
#[cfg(target_arch = "x86_64")]
fn a_transmit_burst_from_a_core_that_goes_idle_reaches_the_wire_whole() {
	// THE TRANSMIT RING DRAINED ON EVERY CORE'S TICK, and an idle core has none. A burst written by a thread
	// that then ends leaves the rest of the ring behind on a UART still sending the first FIFO load - so an
	// idle core with bytes queued sleeps one tick at most, and comes back for the next load. The UART is paced
	// here (QEMU's takes everything at once) and the boot processor neither ticks nor drains while it waits,
	// so the ring empties only if the idle core keeps coming back.
	const BURST: &[u8] = b"    a transmit burst from a core that goes idle: 0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ end of burst\n";
	static ACCEPTED: AtomicU64 = AtomicU64::new(0);
	static WRITTEN: AtomicBool = AtomicBool::new(false);
	extern "C" fn writer(_arg: u64) {
		ACCEPTED.store(arch::serial::write_bytes(BURST) as u64, Ordering::SeqCst);
		WRITTEN.store(true, Ordering::SeqCst);
	}
	if smp::cpu_count() < 2 {
		return;
	}
	ACCEPTED.store(0, Ordering::SeqCst);
	WRITTEN.store(false, Ordering::SeqCst);
	arch::serial::drain_sync();
	arch::serial::pace(true);
	let thread = sched::spawn_on(1, writer, 0);
	let saved = arch::interrupts_enabled();
	arch::disable_interrupts();
	let written = masked_until(500, || WRITTEN.load(Ordering::SeqCst));
	let started = arch::apic::ticks();
	let emptied = masked_until(100, || !arch::serial::tx_pending());
	let took = arch::apic::ticks() - started;
	if saved {
		arch::enable_interrupts();
	}
	arch::serial::pace(false);
	gone(thread);
	arch::serial::drain_sync();
	assert!(written, "the writer on core 1 never ran");
	assert_eq!(ACCEPTED.load(Ordering::SeqCst), BURST.len() as u64, "the ring took the whole burst");
	assert!(emptied, "a burst of {} bytes stayed in the ring for 100 ticks after its core went idle", BURST.len());
	crate::serial_println!("    {} bytes left the ring {took} ticks after their core went idle, one FIFO load a drain", BURST.len());
}

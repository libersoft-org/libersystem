use crate::{arch, sched, smp};

crate::tagged_test!(scheduler_multiplexes_threads, [Scheduler, Smoke], id = "kernel.sched.scheduler_multiplexes_threads", covers = ["kernel"]);
fn scheduler_multiplexes_threads() {
	use core::sync::atomic::{AtomicU32, Ordering};
	static COUNTER: AtomicU32 = AtomicU32::new(0);
	static DONE: AtomicU32 = AtomicU32::new(0);
	extern "C" fn worker(iterations: u64) {
		// Yield between increments so the threads genuinely interleave rather
		// than each running to completion in one go.
		for _ in 0..iterations {
			COUNTER.fetch_add(1, Ordering::SeqCst);
			sched::yield_now();
		}
		DONE.fetch_add(1, Ordering::SeqCst);
	}
	let threads = 4u32;
	let iterations = 10u32;
	for _ in 0..threads {
		sched::spawn(worker, iterations as u64);
	}
	sched::run_until_idle();
	assert_eq!(DONE.load(Ordering::SeqCst), threads);
	assert_eq!(COUNTER.load(Ordering::SeqCst), threads * iterations);
}

// A CONTEXT SWITCH DOES NOT LOSE THE FLOATING-POINT STATE IT IS RESPONSIBLE FOR, on every target.
//
// One id, three bodies, because the three backends save DIFFERENT sets and a test that asked about
// the wrong one would be asking about something the kernel never promised: x86_64 saves the whole
// FPU/SSE state with `fxsave64`, aarch64 the callee-saved `d8..d15`, riscv64 the callee-saved
// `fs0..fs11`. Neither of the latter two saves vector registers or the FP control word, so the id
// lost the `xmm` it used to carry - what is asserted is that the saved set survives, and the saved
// set is the backend's to name. Getting this wrong is silent data corruption, which is why it is
// worth asserting on the two targets that had no equivalent at all.
crate::tagged_test!(
	#[cfg(target_arch = "x86_64")]
	scheduler_preserves_saved_fp_state,
	[Scheduler],
	id = "kernel.sched.scheduler_preserves_saved_fp_state",
	covers = ["kernel"]
);
#[cfg(target_arch = "x86_64")]
fn scheduler_preserves_saved_fp_state() {
	use core::arch::asm;
	use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
	static FAILED: AtomicBool = AtomicBool::new(false);
	static DONE: AtomicU32 = AtomicU32::new(0);
	extern "C" fn worker(value: u64) {
		unsafe { asm!("movq xmm15, {}", in(reg) value, options(nostack, preserves_flags)) };
		for _ in 0..64 {
			sched::yield_now();
			let mut observed: u64;
			unsafe { asm!("movq {}, xmm15", out(reg) observed, options(nostack, preserves_flags)) };
			if observed != value {
				FAILED.store(true, Ordering::SeqCst);
			}
		}
		DONE.fetch_add(1, Ordering::SeqCst);
	}
	FAILED.store(false, Ordering::SeqCst);
	DONE.store(0, Ordering::SeqCst);
	sched::spawn(worker, 0x1122_3344_5566_7788);
	sched::spawn(worker, 0x8877_6655_4433_2211);
	sched::run_until_idle();
	assert_eq!(DONE.load(Ordering::SeqCst), 2);
	assert!(!FAILED.load(Ordering::SeqCst), "one thread observed another thread's XMM state");
}

crate::tagged_test!(
	#[cfg(target_arch = "aarch64")]
	scheduler_preserves_saved_fp_state,
	[Scheduler],
	id = "kernel.sched.scheduler_preserves_saved_fp_state",
	covers = ["kernel"]
);
#[cfg(target_arch = "aarch64")]
fn scheduler_preserves_saved_fp_state() {
	use core::arch::asm;
	use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
	static FAILED: AtomicBool = AtomicBool::new(false);
	static DONE: AtomicU32 = AtomicU32::new(0);
	// `d8` and `d15` are the ends of the callee-saved range `switch_context` stores, so a switch
	// that dropped the block or mis-sized the frame shows up on one of them.
	extern "C" fn worker(value: u64) {
		unsafe {
			asm!("fmov d8, {}", "fmov d15, {}", in(reg) value, in(reg) !value, options(nostack, preserves_flags));
		}
		for _ in 0..64 {
			sched::yield_now();
			let low: u64;
			let high: u64;
			unsafe {
				asm!("fmov {}, d8", "fmov {}, d15", out(reg) low, out(reg) high, options(nostack, preserves_flags));
			}
			if low != value || high != !value {
				FAILED.store(true, Ordering::SeqCst);
			}
		}
		DONE.fetch_add(1, Ordering::SeqCst);
	}
	FAILED.store(false, Ordering::SeqCst);
	DONE.store(0, Ordering::SeqCst);
	sched::spawn(worker, 0x1122_3344_5566_7788);
	sched::spawn(worker, 0x8877_6655_4433_2211);
	sched::run_until_idle();
	assert_eq!(DONE.load(Ordering::SeqCst), 2);
	assert!(!FAILED.load(Ordering::SeqCst), "one thread observed another thread's d8/d15 state");
}

crate::tagged_test!(
	#[cfg(target_arch = "riscv64")]
	scheduler_preserves_saved_fp_state,
	[Scheduler],
	id = "kernel.sched.scheduler_preserves_saved_fp_state",
	covers = ["kernel"]
);
#[cfg(target_arch = "riscv64")]
fn scheduler_preserves_saved_fp_state() {
	use core::arch::asm;
	use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
	static FAILED: AtomicBool = AtomicBool::new(false);
	static DONE: AtomicU32 = AtomicU32::new(0);
	// `fs0` and `fs11` are the ends of the callee-saved range `switch_context` stores.
	extern "C" fn worker(value: u64) {
		unsafe {
			asm!("fmv.d.x fs0, {}", "fmv.d.x fs11, {}", in(reg) value, in(reg) !value, options(nostack, preserves_flags));
		}
		for _ in 0..64 {
			sched::yield_now();
			let low: u64;
			let high: u64;
			unsafe {
				asm!("fmv.x.d {}, fs0", "fmv.x.d {}, fs11", out(reg) low, out(reg) high, options(nostack, preserves_flags));
			}
			if low != value || high != !value {
				FAILED.store(true, Ordering::SeqCst);
			}
		}
		DONE.fetch_add(1, Ordering::SeqCst);
	}
	FAILED.store(false, Ordering::SeqCst);
	DONE.store(0, Ordering::SeqCst);
	sched::spawn(worker, 0x1122_3344_5566_7788);
	sched::spawn(worker, 0x8877_6655_4433_2211);
	sched::run_until_idle();
	assert_eq!(DONE.load(Ordering::SeqCst), 2);
	assert!(!FAILED.load(Ordering::SeqCst), "one thread observed another thread's fs0/fs11 state");
}

crate::tagged_test!(preemption_preempts_a_cpu_bound_thread, [Scheduler], id = "kernel.sched.preemption_preempts_a_cpu_bound_thread", covers = ["kernel"]);
fn preemption_preempts_a_cpu_bound_thread() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static STOP: AtomicBool = AtomicBool::new(false);
	static MATE_RAN: AtomicBool = AtomicBool::new(false);
	// A CPU-bound thread that never yields: it spins until another thread sets STOP.
	// Only timer-driven preemption can let that other thread run, so without
	// preemption this spins forever and hangs the test.
	extern "C" fn hog(_arg: u64) {
		while !STOP.load(Ordering::SeqCst) {
			core::hint::spin_loop();
		}
	}
	// The cohabiting thread records that it ran, then releases the hog so the run
	// queue can drain.
	extern "C" fn mate(_arg: u64) {
		MATE_RAN.store(true, Ordering::SeqCst);
		STOP.store(true, Ordering::SeqCst);
	}
	STOP.store(false, Ordering::SeqCst);
	MATE_RAN.store(false, Ordering::SeqCst);
	// Both land on this core's run queue; the hog runs first and never yields.
	sched::spawn(hog, 0);
	sched::spawn(mate, 0);
	sched::run_until_idle();
	assert!(MATE_RAN.load(Ordering::SeqCst), "the cohabiting thread never ran: the never-yielding thread was not preempted");
}

crate::tagged_test!(a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick, [Scheduler], id = "kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick", covers = ["kernel"]);
fn a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick() {
	use core::sync::atomic::{AtomicU64, Ordering};
	static RAN_AT: AtomicU64 = AtomicU64::new(0);
	extern "C" fn stamp(_arg: u64) {
		RAN_AT.store(1, Ordering::SeqCst);
	}
	// THE THREAD IS GONE, not merely finished: its core has dropped it, so its Domain charge is back and the
	// next test starts from a machine this one left as it found it.
	fn gone(thread: alloc::sync::Arc<crate::object::thread::Thread>) {
		let mut spins = 0u64;
		while alloc::sync::Arc::strong_count(&thread) > 1 {
			core::hint::spin_loop();
			spins += 1;
			assert!(spins < 20_000_000_000, "a finished remote spawn was never reaped by its core");
		}
	}
	// Spin until the stamp, for at most `ticks` ticks; whether it came.
	fn ran_within(ticks: u64) -> bool {
		let give_up = arch::apic::ticks() + ticks;
		while RAN_AT.load(Ordering::SeqCst) == 0 {
			if arch::apic::ticks() >= give_up {
				return false;
			}
			core::hint::spin_loop();
		}
		true
	}
	if smp::cpu_count() < 2 {
		return;
	}
	// THE WAKE IS THE ONLY THING THAT BRINGS AN IDLE CORE TO QUEUED WORK NOW. It used to be the faster of two:
	// a halted core without the IPI picked the thread up at its next 100 Hz tick, so this test measured the
	// IPI's saving against half a tick period - which emulation made a coin toss, and the history of this test
	// is three attempts at a threshold. An idle core's timer is a one-shot for nothing at all now, so the
	// property is exact rather than a margin: WITH the wake the thread runs, and WITHOUT it it does not run
	// until the wake is sent.
	//
	// A warmup trip whose result is not counted: the first cross-core spawn pays one-time costs.
	RAN_AT.store(0, Ordering::SeqCst);
	let warmup = sched::spawn_on(1, stamp, 0);
	assert!(ran_within(1_000), "a woken remote spawn ran");
	gone(warmup);
	// Twenty woken trips, each of which must arrive.
	let start = arch::tsc::now();
	for _ in 0..20 {
		RAN_AT.store(0, Ordering::SeqCst);
		let thread = sched::spawn_on(1, stamp, 0);
		assert!(ran_within(1_000), "every woken remote spawn runs");
		gone(thread);
	}
	let woken = arch::tsc::now().wrapping_sub(start);
	crate::serial_println!("    twenty woken remote spawns: {woken} cycles");
	// THE CONTROL. Nothing else may wake core 1 meanwhile: output left in the serial ring caps an idle
	// core's sleep at a tick, so the ring is emptied first and core 1 given time to settle into a halt
	// with no timer at all - and nothing is printed until the control is over.
	arch::serial::flush_sync();
	let settle = arch::apic::ticks() + 3;
	while arch::apic::ticks() < settle {
		core::hint::spin_loop();
	}
	RAN_AT.store(0, Ordering::SeqCst);
	let control = sched::spawn_on_unwoken(1, stamp, 0);
	let unwoken_ran = ran_within(10);
	// AND NOW THE WAKE, which is all that is missing.
	crate::idle::wake_core(1);
	let woken_ran = ran_within(1_000);
	gone(control);
	assert!(!unwoken_ran, "a thread queued on an idle core without the wake ran within ten ticks - the core is still taking a tick it should not");
	assert!(woken_ran, "and it ran once the wake was sent");
	crate::serial_println!("    an unwoken remote spawn waited ten ticks unrun on an idle core, and ran once woken");
}

crate::tagged_test!(a_bounded_drain_gives_up_on_a_thread_that_keeps_requeueing_itself, [Scheduler, Kernel], id = "kernel.sched.a_bounded_drain_gives_up_on_a_thread_that_keeps_requeueing_itself", covers = ["kernel"]);
fn a_bounded_drain_gives_up_on_a_thread_that_keeps_requeueing_itself() {
	// THE HALF A CAPPED WAIT DOES NOT COVER.
	//
	// `run_until_idle` is `while !run_queue.is_empty() { reschedule(Requeue) }` and only THEN a
	// bounded wait. A thread that yields and requeues itself keeps the queue non-empty for as long
	// as it likes, so a caller that bounded only the wait would have bounded nothing against exactly
	// the workload a boot failing to settle produces.
	//
	// AND THE DRAIN CANNOT CHECK ITS OWN DEADLINE EITHER, which is what this test proved by hanging
	// for three minutes when it was written that way: `reschedule` stashes the pump's stack in the
	// core's IDLE slot rather than requeueing it as a thread, so the pump resumes only when
	// `pop_front()` finds nothing. The deadline lives on `CpuSched` and is read where the switch is
	// decided.
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	static STOP: AtomicBool = AtomicBool::new(false);
	static SPINS: AtomicU64 = AtomicU64::new(0);
	extern "C" fn yielder(_arg: u64) {
		while !STOP.load(Ordering::SeqCst) {
			SPINS.fetch_add(1, Ordering::SeqCst);
			sched::yield_now();
		}
	}
	STOP.store(false, Ordering::SeqCst);
	SPINS.store(0, Ordering::SeqCst);
	sched::spawn(yielder, 0);
	let deadline = arch::apic::ticks() + 3;
	let idled = sched::run_until_idle_until(deadline);
	assert!(!idled, "a thread that requeues itself never lets the queue empty, so this cannot have idled");
	assert!(arch::apic::ticks() >= deadline, "and it came back because the deadline passed");
	assert!(SPINS.load(Ordering::SeqCst) > 0, "the thread did run - this is a bounded drain, not a refused spawn");
	// Let it finish so the queue is clean for whatever runs next.
	STOP.store(true, Ordering::SeqCst);
	sched::run_until_idle();
}

crate::tagged_test!(a_bounded_wait_wakes_on_the_callers_window_and_not_the_nearest_timer, [Scheduler, Kernel], id = "kernel.sched.a_bounded_wait_wakes_on_the_callers_window_and_not_the_nearest_timer", covers = ["kernel"]);
fn a_bounded_wait_wakes_on_the_callers_window_and_not_the_nearest_timer() {
	// THE OTHER HALF: a thread that WAITS past the window.
	//
	// With nothing runnable, the wait sleeps to the nearest progress deadline - and if that is
	// further away than the caller's window, sleeping to it overshoots by the difference. So the
	// wait sleeps to whichever comes first, and this is the case where they differ.
	use core::sync::atomic::{AtomicBool, Ordering};
	static WOKE: AtomicBool = AtomicBool::new(false);
	extern "C" fn sleeper(_arg: u64) {
		// A koid nothing ever signals, so only the deadline can end this wait.
		sched::block_on(u64::MAX, arch::apic::ticks() + 500);
		WOKE.store(true, Ordering::SeqCst);
	}
	WOKE.store(false, Ordering::SeqCst);
	sched::spawn(sleeper, 0);
	// Let it reach the block, so the run queue is empty and the wait is what runs.
	let settle = arch::apic::ticks() + 2;
	sched::run_until_idle_until(settle);
	let deadline = arch::apic::ticks() + 3;
	let idled = sched::run_until_idle_until(deadline);
	let after = arch::apic::ticks();
	assert!(!idled, "a thread blocked on a far deadline is not an idle machine");
	assert!(after >= deadline, "the wait returned at the window");
	assert!(after < deadline + 100, "and NOT at the sleeper's own deadline, which is hundreds of ticks away: got {after}, window ended at {deadline}");
	assert!(!WOKE.load(Ordering::SeqCst), "the sleeper has not been woken - this measured the wait, not its subject");
}

crate::tagged_test!(scheduler_runs_across_cores, [Scheduler], id = "kernel.sched.scheduler_runs_across_cores", covers = ["kernel"]);
fn scheduler_runs_across_cores() {
	use core::sync::atomic::{AtomicU32, Ordering};
	static CROSS: AtomicU32 = AtomicU32::new(0);
	extern "C" fn application_processor_worker(_arg: u64) {
		CROSS.fetch_add(1, Ordering::SeqCst);
	}
	// Spawn one thread onto every application processor; each runs the worker in
	// its idle loop. With a single core this is a no-op and the test trivially
	// holds.
	let other_cores = smp::cpu_count() - 1;
	for cpu in 1..smp::cpu_count() {
		sched::spawn_on(cpu, application_processor_worker, 0);
	}
	// Wait (bounded) for every AP to run its thread on its own core.
	let mut spins = 0u64;
	while (CROSS.load(Ordering::SeqCst) as usize) < other_cores {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 2_000_000_000, "AP threads did not run");
	}
	assert_eq!(CROSS.load(Ordering::SeqCst) as usize, other_cores);
}

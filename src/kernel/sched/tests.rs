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

#[cfg(target_arch = "x86_64")]
crate::tagged_test!(scheduler_preserves_xmm_state, [Scheduler], id = "kernel.sched.scheduler_preserves_xmm_state", covers = ["kernel"]);
#[cfg(target_arch = "x86_64")]
fn scheduler_preserves_xmm_state() {
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
	if smp::cpu_count() < 2 {
		return;
	}
	// MEASURED IN TICKS, NOT NANOSECONDS.
	//
	// The property is "a queued thread does not wait for the next tick", and it used to be checked
	// by timing the trip and comparing against 4 ms - a number chosen because a halted core without
	// the IPI waits for its next 100 Hz tick, so its trips average about 5 ms, while a woken one is
	// microseconds. That gap is real on x86_64 and aarch64 and it CLOSES under emulation: measured
	// on riscv64 on 2026-08-20, a woken trip costs 3.3-3.6 ms with excursions past 5.7, because
	// every guest instruction costs about twenty-five times what it does natively. Against a 4 ms
	// bound that is a coin toss, and the coin decides whether 165 later tests run at all.
	//
	// Counting TICK BOUNDARIES instead measures the property directly and cares nothing for how
	// long the emulator takes to get there: a trip that crossed no tick boundary did not wait for
	// one. Without the IPI every trip waits for the next tick and therefore crosses one, so the
	// discrimination is exact rather than a margin - which is what the old bound had stopped being.
	//
	// The IPI itself was verified separately and works on all three: instrumenting
	// `sbi_send_ipi` and the `code == 1` handler on riscv64 showed 203 sent, 0 errors, 203 received.
	//
	// A warmup trip whose result is not counted: the first cross-core spawn pays one-time costs.
	RAN_AT.store(0, Ordering::SeqCst);
	sched::spawn_on(1, stamp, 0);
	while RAN_AT.load(Ordering::SeqCst) == 0 {
		core::hint::spin_loop();
	}
	// The BEST of twenty, not every one of them. A trip that costs a third of a tick period crosses
	// a boundary about a third of the time by luck alone, and this suite runs under emulation on a
	// shared host where that fraction is not stable. Twenty trips make "every single one of them
	// happened to straddle a tick" the only way to pass wrongly, and that is what a broken IPI
	// looks like: without it, crossing is not luck but the mechanism.
	let mut best = u64::MAX;
	for _ in 0..20 {
		RAN_AT.store(0, Ordering::SeqCst);
		let start = arch::apic::ticks();
		sched::spawn_on(1, stamp, 0);
		while RAN_AT.load(Ordering::SeqCst) == 0 {
			core::hint::spin_loop();
		}
		best = best.min(arch::apic::ticks().wrapping_sub(start));
	}
	assert_eq!(best, 0, "every one of twenty remote spawns crossed a tick boundary: the wake IPI did not reach the halted core");
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

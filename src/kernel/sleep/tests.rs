use crate::object::address_space::AddressSpace;
use crate::object::domain::Domain;
use crate::object::process::Process;
use crate::{arch, sched, smp};

crate::tagged_test!(the_wake_the_sleep_saw_first_is_the_one_reported, [Kernel], id = "kernel.sleep.the_wake_the_sleep_saw_first_is_the_one_reported", covers = ["kernel"]);
fn the_wake_the_sleep_saw_first_is_the_one_reported() {
	assert!(!super::sleeping(), "nothing sleeps while the suite runs");
	let _ = super::take_woke();
	super::woke_by(abi::WAKE_POWER_BUTTON, 0);
	super::woke_by(abi::WAKE_DEVICE, 7);
	assert_eq!(super::take_woke(), Some((abi::WAKE_POWER_BUTTON, 0)), "the first wake, not the last");
	assert_eq!(super::take_woke(), None, "and taken once");
}

crate::tagged_test!(a_marked_interrupt_wakes_the_sleep_and_nothing_else_does, [Kernel], id = "kernel.sleep.a_marked_interrupt_wakes_the_sleep_and_nothing_else_does", covers = ["kernel"]);
fn a_marked_interrupt_wakes_the_sleep_and_nothing_else_does() {
	let _ = super::take_woke();
	super::device_interrupt(0x42);
	assert_eq!(super::take_woke(), None, "an interrupt nobody marked is no wake");
	assert!(super::mark_wake(0x42, true));
	assert!(super::mark_wake(0x42, true), "marking twice is one mark");
	super::device_interrupt(0x41);
	assert_eq!(super::take_woke(), None, "only the marked identity");
	super::device_interrupt(0x42);
	assert_eq!(super::take_woke(), Some((abi::WAKE_DEVICE, 0x42)), "a device's wake, naming its interrupt");
	assert!(super::mark_wake(0x42, false));
	assert!(!super::is_wake(0x42), "and unmarked once, however often it was marked");
	// THE SET IS BOUNDED, and a full one says so rather than dropping a mark.
	for identity in 0..super::WAKE_SOURCES as u32 {
		assert!(super::mark_wake(0x1000 + identity, true));
	}
	assert!(!super::mark_wake(0x2000, true), "a full wake set refuses");
	for identity in 0..super::WAKE_SOURCES as u32 {
		super::mark_wake(0x1000 + identity, false);
	}
	assert!(super::mark_wake(0x2000, true), "and takes one again once emptied");
	super::mark_wake(0x2000, false);
}

crate::tagged_test!(a_sleep_type_is_checked_registered_and_kept, [Kernel], id = "kernel.sleep.a_sleep_type_is_checked_registered_and_kept", covers = ["kernel"]);
fn a_sleep_type_is_checked_registered_and_kept() {
	assert_eq!(super::register(abi::SLEEP_STATE_RAM, 8, 0), abi::ERR_INVALID, "SLP_TYP is three bits");
	assert_eq!(super::register(abi::SLEEP_STATE_IDLE, 1, 1), abi::ERR_INVALID, "suspend to idle has no pair");
	assert_eq!(super::register(abi::SLEEP_STATE_RAM, 5, 1), 0);
	assert_eq!(super::sleep_type(3), Some((5, 1)));
	assert_eq!(super::register(abi::SLEEP_STATE_RAM, 1, 0), 0, "a restarted service registers again");
	assert_eq!(super::sleep_type(3), Some((1, 0)));
	assert_eq!(super::sleep_type(2), None, "S2 was never registered");
}

crate::tagged_test!(a_freeze_holds_the_subtree_and_its_newcomers_and_a_thaw_leaves_a_stop_alone, [Kernel, Domain, Process], id = "kernel.sleep.a_freeze_holds_the_subtree_and_its_newcomers_and_a_thaw_leaves_a_stop_alone", covers = ["kernel"]);
fn a_freeze_holds_the_subtree_and_its_newcomers_and_a_thaw_leaves_a_stop_alone() {
	let applications = Domain::new_child(&sched::root_domain(), u64::MAX, u64::MAX, u64::MAX).expect("a Domain");
	let launched = Domain::new_child(&applications, u64::MAX, u64::MAX, u64::MAX).expect("a child Domain");
	let make = |domain: &alloc::sync::Arc<Domain>| Process::new(AddressSpace::create().expect("an address space"), domain.clone()).expect("a process");
	let before = make(&launched);
	let stopped = make(&applications);
	stopped.set_stopped(true);
	assert!(!before.is_frozen());
	applications.set_frozen(true);
	assert!(applications.is_frozen() && launched.is_frozen(), "the whole subtree");
	assert!(before.is_frozen() && stopped.is_frozen(), "every process in it");
	// NOTHING ESCAPES BY BEING CREATED DURING THE FREEZE.
	let during = make(&launched);
	assert!(during.is_frozen(), "a process created in a frozen subtree starts frozen");
	let nested = Domain::new_child(&launched, u64::MAX, u64::MAX, u64::MAX).expect("a Domain created during the freeze");
	assert!(nested.is_frozen() && make(&nested).is_frozen(), "and so does a Domain, and what it holds");
	assert!(applications.quiescent(), "no thread of the subtree runs user code - none has a thread");
	// THE THAW RELEASES THE FREEZE AND NOTHING ELSE: a job a person stopped is still stopped.
	applications.set_frozen(false);
	assert!(!before.is_frozen() && !during.is_frozen() && !stopped.is_frozen());
	assert!(stopped.is_stopped(), "the thaw does not clear SIG_STOP's flag");
	// And SIG_CONT's flag change does not reach a freeze.
	applications.set_frozen(true);
	stopped.set_stopped(false);
	assert!(stopped.is_frozen() && stopped.is_held(), "SIG_CONT does not clear the freeze");
	applications.set_frozen(false);
	assert!(!stopped.is_held());
	// A DOMAIN OUTSIDE THE SUBTREE IS UNTOUCHED.
	let beside = Domain::new_child(&sched::root_domain(), u64::MAX, u64::MAX, u64::MAX).expect("a Domain");
	applications.set_frozen(true);
	assert!(!beside.is_frozen() && !make(&beside).is_frozen());
	applications.set_frozen(false);
}

crate::tagged_test!(a_suspend_to_idle_parks_every_core_until_its_timed_wake_and_no_clock_jumps, [Kernel, Smp, Scheduler], id = "kernel.sleep.a_suspend_to_idle_parks_every_core_until_its_timed_wake_and_no_clock_jumps", covers = ["kernel"]);
fn a_suspend_to_idle_parks_every_core_until_its_timed_wake_and_no_clock_jumps() {
	const WAKE_NS: u64 = 300_000_000;
	// The slack between two readings of the clocks taken one after the other - an emulated core can be descheduled
	// between them.
	const READ_SLACK_NS: u64 = 10_000_000;
	let clock = &arch::common::time::CLOCK;
	let (boot_before, mono_before, ticks_before) = (super::boot_ns(), clock.nanos(arch::tsc::now()), arch::apic::ticks());
	let deadline = boot_before + WAKE_NS;
	let report = super::sys_system_sleep(&sched::root_domain(), abi::SLEEP_STATE_IDLE, deadline).expect("the machine slept");
	let (boot_after, mono_after, ticks_after) = (super::boot_ns(), clock.nanos(arch::tsc::now()), arch::apic::ticks());
	assert_eq!(report.wake, abi::WAKE_TIMER, "the timed wake ended it");
	// NOT BEFORE ITS DEADLINE - which is on the boot-time clock, so the entry's own work, slow under emulation, is part
	// of the wait and the sleep itself is what is left of it.
	assert!(boot_after >= deadline, "woken {} ns before the timed wake's deadline", deadline - boot_after);
	assert!(report.slept_ns > 0 && boot_after - boot_before >= report.slept_ns, "the boot-time clock takes the sleep in");
	// THE MONOTONIC CLOCK AND THE TICK EXCLUDE IT: the boot-time clock moved by exactly the sleep more than the monotonic
	// one, and the tick by no more than the monotonic clock's own advance - the work around the sleep, not the sleep.
	let excluded = (boot_after - boot_before) - (mono_after - mono_before);
	assert!(excluded.abs_diff(report.slept_ns) <= READ_SLACK_NS, "the boot-time clock moved {excluded} ns more than the monotonic one across a {} ns sleep", report.slept_ns);
	let tick_ns = 1_000_000_000 / u64::from(arch::common::time::TICK_HZ);
	assert!(ticks_after - ticks_before <= (mono_after - mono_before) / tick_ns + 1, "the tick jumped by {} across the sleep, the monotonic clock by {} ns", ticks_after - ticks_before, mono_after - mono_before);
	assert_eq!(report.core_count as usize, smp::cpu_count(), "every core's parked record");
	assert!(!super::sleeping(), "and the sleep ended");
	// NO PERIODIC TICK WHILE PARKED: a core woken a tick at a time for 300 ms would have taken 30 timer wakes.
	for core in &report.cores[..report.core_count as usize] {
		assert!(core.timer <= 2, "core {} took {} timer wakes while parked", core.cpu, core.timer);
	}
	// A wake already past is refused before anything is held.
	assert_eq!(super::sys_system_sleep(&sched::root_domain(), abi::SLEEP_STATE_IDLE, 1).err(), Some(abi::ERR_INVALID));
	// And only the root Domain may ask.
	let other = Domain::new_child(&sched::root_domain(), u64::MAX, u64::MAX, u64::MAX).expect("a Domain");
	assert_eq!(super::sys_system_sleep(&other, abi::SLEEP_STATE_IDLE, 0).err(), Some(abi::ERR_ACCESS_DENIED));
}

crate::tagged_test!(the_freeze_syscall_needs_manage_and_answers_once_the_subtree_is_quiet, [Kernel, Domain, Syscall], id = "kernel.sleep.the_freeze_syscall_needs_manage_and_answers_once_the_subtree_is_quiet", covers = ["kernel"]);
fn the_freeze_syscall_needs_manage_and_answers_once_the_subtree_is_quiet() {
	use crate::object::rights::Rights;
	use core::sync::atomic::{AtomicI64, Ordering};
	static RESULT: AtomicI64 = AtomicI64::new(1);
	extern "C" fn freeze(handle: u64) {
		RESULT.store(unsafe { crate::arch::syscall::invoke(abi::SYS_DOMAIN_FREEZE, handle, 1, 0, 0) } as i64, Ordering::SeqCst);
	}
	extern "C" fn thaw(handle: u64) {
		RESULT.store(unsafe { crate::arch::syscall::invoke(abi::SYS_DOMAIN_FREEZE, handle, 0, 0, 0) } as i64, Ordering::SeqCst);
	}
	extern "C" fn nonsense(handle: u64) {
		RESULT.store(unsafe { crate::arch::syscall::invoke(abi::SYS_DOMAIN_FREEZE, handle, 2, 0, 0) } as i64, Ordering::SeqCst);
	}
	let domain = Domain::new_child(&sched::root_domain(), u64::MAX, u64::MAX, u64::MAX).expect("a Domain");
	let process = Process::new(AddressSpace::create().expect("an address space"), domain.clone()).expect("a process");
	sched::spawn_with_object(freeze, domain.clone(), Rights::READ | Rights::WAIT);
	sched::run_until_idle();
	assert_eq!(RESULT.load(Ordering::SeqCst), abi::ERR_ACCESS_DENIED, "freezing a subtree is its killer's right, MANAGE");
	assert!(!process.is_frozen());
	sched::spawn_with_object(freeze, domain.clone(), Rights::ALL);
	sched::run_until_idle();
	assert_eq!(RESULT.load(Ordering::SeqCst), 0, "a subtree with no running thread is quiet at once");
	assert!(process.is_frozen());
	sched::spawn_with_object(nonsense, domain.clone(), Rights::ALL);
	sched::run_until_idle();
	assert_eq!(RESULT.load(Ordering::SeqCst), abi::ERR_INVALID);
	sched::spawn_with_object(thaw, domain.clone(), Rights::ALL);
	sched::run_until_idle();
	assert_eq!(RESULT.load(Ordering::SeqCst), 0);
	assert!(!process.is_frozen());
}

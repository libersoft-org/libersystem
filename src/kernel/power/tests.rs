use core::sync::atomic::Ordering;

crate::tagged_test!(a_forced_deadline_keeps_the_earliest_and_fires_at_its_tick, [Kernel], id = "kernel.power.a_forced_deadline_keeps_the_earliest_and_fires_at_its_tick", covers = ["kernel"]);
fn a_forced_deadline_keeps_the_earliest_and_fires_at_its_tick() {
	super::FIRED.store(0, Ordering::Release);
	let first = super::arm_within(10);
	assert_eq!(super::arm_within(20), first, "a later deadline never moves it");
	let earlier = super::arm_within(5);
	assert!(earlier < first, "an earlier one does");
	assert!(super::armed());
	super::check(earlier - 1);
	assert_eq!(super::FIRED.load(Ordering::Acquire), 0, "not before its tick");
	super::check(earlier);
	assert_eq!(super::FIRED.load(Ordering::Acquire), earlier, "at its tick, the power-off");
	assert!(!super::armed(), "the suite's record takes it down");
}

crate::tagged_test!(a_forced_deadline_fires_from_the_timer_interrupt_while_a_thread_keeps_the_core_busy, [Kernel], id = "kernel.power.a_forced_deadline_fires_from_the_timer_interrupt_while_a_thread_keeps_the_core_busy", covers = ["kernel"]);
fn a_forced_deadline_fires_from_the_timer_interrupt_while_a_thread_keeps_the_core_busy() {
	super::FIRED.store(0, Ordering::Release);
	let at = super::arm_within(1);
	// BUSY, never blocking and never reaching the deadline path: only the timer interrupt can see the tick pass.
	let started = crate::arch::apic::ticks();
	while super::FIRED.load(Ordering::Acquire) == 0 && crate::arch::apic::ticks() < started + 5 * abi::TICKS_PER_SECOND {
		core::hint::spin_loop();
	}
	assert_eq!(super::FIRED.load(Ordering::Acquire), at, "the timer interrupt carried the power-off out");
}

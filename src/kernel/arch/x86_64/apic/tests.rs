use super::ticks;

crate::tagged_test!(timer_ticks_advance, [Apic, Kernel, ArchX86_64]);
fn timer_ticks_advance() {
	// Interrupts are enabled by kmain before the tests run, so the periodic
	// LAPIC timer must keep incrementing the tick counter.
	let start = ticks();
	while ticks() == start {
		core::hint::spin_loop();
	}
	assert!(ticks() > start);
}

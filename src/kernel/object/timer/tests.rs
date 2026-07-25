use super::Timer;
use crate::arch;

crate::tagged_test!(timer_object_expires_and_cancels, [Object, Kernel]);
fn timer_object_expires_and_cancels() {
	let timer = Timer::create();
	assert!(!timer.is_expired());
	let deadline = arch::apic::ticks() + 2;
	timer.set(deadline);
	let mut spins = 0u64;
	while !timer.is_expired() {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 2_000_000_000, "timer never expired");
	}
	assert!(timer.is_expired());
	timer.cancel();
	assert!(!timer.is_expired());
}

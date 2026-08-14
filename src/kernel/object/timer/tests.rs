use super::Timer;
use crate::arch;

crate::tagged_test!(timer_object_expires_and_cancels, [Object, Kernel], id = "kernel.object.timer.timer_object_expires_and_cancels", covers = ["kernel"]);
fn timer_object_expires_and_cancels() {
	let timer = Timer::create().expect("a test timer");
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

crate::tagged_test!(timer_syscall_arms_and_polls, [Object, Kernel, Syscall], id = "kernel.object.timer.timer_syscall_arms_and_polls", covers = ["kernel"]);
fn timer_syscall_arms_and_polls() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// The syscall path needs a current thread's handle table, so it runs inside a
	// spawned kernel thread.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let timer = crate::arch::syscall::invoke(crate::syscall::SYS_TIMER_CREATE, 0, 0, 0, 0);
			assert!(!crate::syscall::sys_is_err(timer));
			// Not armed means not expired.
			assert_eq!(crate::arch::syscall::invoke(crate::syscall::SYS_TIMER_POLL, timer, 0, 0, 0), 0);
			// A deadline already reached reports expired immediately.
			let now = crate::arch::syscall::invoke(crate::syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
			crate::arch::syscall::invoke(crate::syscall::SYS_TIMER_SET, timer, now, 0, 0);
			assert_eq!(crate::arch::syscall::invoke(crate::syscall::SYS_TIMER_POLL, timer, 0, 0, 0), 1);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	crate::sched::spawn(body, 0);
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

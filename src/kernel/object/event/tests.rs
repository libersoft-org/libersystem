use super::Event;

crate::tagged_test!(event_object_latches_and_clears, [Object, Kernel], id = "kernel.object.event.event_object_latches_and_clears", covers = ["kernel"]);
fn event_object_latches_and_clears() {
	let event = Event::create();
	assert!(!event.is_signaled());
	event.signal();
	assert!(event.is_signaled());
	event.clear();
	assert!(!event.is_signaled());
}

crate::tagged_test!(event_syscall_latches_and_polls, [Object, Kernel, Syscall], id = "kernel.object.event.event_syscall_latches_and_polls", covers = ["kernel"]);
fn event_syscall_latches_and_polls() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// The syscall path needs a current thread's handle table, so it runs inside a
	// spawned kernel thread.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let event = crate::arch::syscall::invoke(crate::syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			assert!(!crate::syscall::sys_is_err(event));
			assert_eq!(crate::arch::syscall::invoke(crate::syscall::SYS_EVENT_POLL, event, 0, 0, 0), 0);
			crate::arch::syscall::invoke(crate::syscall::SYS_EVENT_SIGNAL, event, 0, 0, 0);
			assert_eq!(crate::arch::syscall::invoke(crate::syscall::SYS_EVENT_POLL, event, 0, 0, 0), 1);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	crate::sched::spawn(body, 0);
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

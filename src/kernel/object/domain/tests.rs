use super::{Domain, UNLIMITED};

crate::tagged_test!(domain_hierarchy_charges_aggregate_and_refund, [Domain, Kernel], covers = ["kernel"]);
fn domain_hierarchy_charges_aggregate_and_refund() {
	// A child Domain's charges also count against its parent, and the parent's
	// aggregate limit binds even when the child itself is unbounded. The parent
	// caps memory at two pages; the unbounded child may charge two pages but not a
	// third.
	let parent = Domain::new(8192, UNLIMITED, UNLIMITED);
	let child = Domain::new_child(&parent, UNLIMITED, UNLIMITED, UNLIMITED).expect("a live parent takes a child");
	assert!(child.try_charge_memory(4096));
	assert_eq!(parent.account().memory().used(), 4096, "charge propagates to the parent");
	assert!(child.try_charge_memory(4096));
	assert!(!child.try_charge_memory(4096), "parent aggregate binds though the child is unbounded");
	assert_eq!(child.account().memory().used(), 8192, "the refused charge was rolled back at the child");
	assert_eq!(parent.account().memory().used(), 8192, "and left the parent unchanged");
	assert_eq!(child.account().memory().peak(), 8192, "a refused aggregate charge does not raise the child high-water mark");
	assert_eq!(parent.account().memory().peak(), 8192, "the parent records its successful aggregate high-water mark");
	child.uncharge_memory(8192);
	assert_eq!(parent.account().memory().used(), 0, "uncharge propagates to the parent");
	assert_eq!(parent.account().memory().peak(), 8192, "the high-water mark survives refunds");
}

crate::tagged_test!(object_property_set_bounds_a_domain, [Domain, Kernel, Syscall], covers = ["kernel"]);
fn object_property_set_bounds_a_domain() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(handle: u64) {
		unsafe {
			// Set the Domain's memory limit to 8192 bytes via the property syscall.
			let result = crate::arch::syscall::invoke(crate::syscall::SYS_OBJECT_PROPERTY_SET, handle, crate::syscall::PROP_MEMORY_LIMIT, 8192, 0);
			assert_eq!(result as i64, 0, "set memory limit failed");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let domain = Domain::new(UNLIMITED, UNLIMITED, UNLIMITED);
	crate::sched::spawn_with_object(body, domain.clone(), crate::object::rights::Rights::ALL, 0);
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	assert_eq!(domain.account().memory().limit(), 8192);
}

crate::tagged_test!(domain_quota_enforced_cleanly, [Domain, Kernel, Syscall], covers = ["kernel"]);
fn domain_quota_enforced_cleanly() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// A thread accounted to a bounded Domain exercises the create-boundary
	// quotas. Reaching a cap must return ERR_RESOURCE_EXHAUSTED, not crash. The
	// create syscalls charge the current thread's Domain, so the sequence runs
	// inside a spawned thread; a failed assertion panics it and fails the run.
	extern "C" fn body(_arg: u64) {
		unsafe {
			// Memory: the cap is 8192 bytes = two pages. Two objects fit exactly,
			// the third is refused cleanly without allocating anything.
			let first_memory = crate::arch::syscall::invoke(crate::syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!crate::syscall::sys_is_err(first_memory));
			let second_memory = crate::arch::syscall::invoke(crate::syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!crate::syscall::sys_is_err(second_memory));
			let third_memory = crate::arch::syscall::invoke(crate::syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert_eq!(third_memory as i64, crate::syscall::ERR_RESOURCE_EXHAUSTED);
			// Closing the two objects refunds their memory and their handles.
			assert_eq!(crate::arch::syscall::invoke(crate::syscall::SYS_HANDLE_CLOSE, first_memory, 0, 0, 0) as i64, 0);
			assert_eq!(crate::arch::syscall::invoke(crate::syscall::SYS_HANDLE_CLOSE, second_memory, 0, 0, 0) as i64, 0);
			// Handles: the cap is 4. Four events fit, the fifth is refused cleanly.
			for _ in 0..4 {
				let event = crate::arch::syscall::invoke(crate::syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
				assert!(!crate::syscall::sys_is_err(event));
			}
			let over = crate::arch::syscall::invoke(crate::syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			assert_eq!(over as i64, crate::syscall::ERR_RESOURCE_EXHAUSTED);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	// 8192 bytes of memory (two pages), 4 handles, 4 threads.
	let domain = Domain::new(8192, 4, 4);
	// Do not keep the returned Arc, so the thread is free to be reaped (and its
	// charges refunded) once it exits.
	assert!(crate::sched::spawn_in(domain.clone(), body, 0).is_some());
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	// Tearing the thread down returned every resource: the four still-open events
	// are refunded by the handle table's drop and the thread slot by the thread's
	// drop, so the bounded Domain is back to zero - clean refusal, no leak.
	assert_eq!(domain.account().memory().used(), 0);
	assert_eq!(domain.account().handles().used(), 0);
	assert_eq!(domain.account().threads().used(), 0);
}

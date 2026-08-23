use super::{Domain, UNLIMITED};

crate::tagged_test!(domain_hierarchy_charges_aggregate_and_refund, [Domain, Kernel], id = "kernel.object.domain.domain_hierarchy_charges_aggregate_and_refund", covers = ["kernel"]);
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

crate::tagged_test!(object_property_set_bounds_a_domain, [Domain, Kernel, Syscall], id = "kernel.object.domain.object_property_set_bounds_a_domain", covers = ["kernel"]);
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
	crate::sched::spawn_with_object(body, domain.clone(), crate::object::rights::Rights::ALL);
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	assert_eq!(domain.account().memory().limit(), 8192);
}

crate::tagged_test!(domain_quota_enforced_cleanly, [Domain, Kernel, Syscall], id = "kernel.object.domain.domain_quota_enforced_cleanly", covers = ["kernel"]);
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

crate::tagged_test!(a_processs_image_counts_against_its_domains_memory_limit, [Domain, Memory, Process, Kernel], id = "kernel.object.domain.a_processs_image_counts_against_its_domains_memory_limit", covers = ["kernel"]);
fn a_processs_image_counts_against_its_domains_memory_limit() {
	// The account's own header said it caps "physical memory held", and the only production charge
	// against it was `MemoryObject::create_in` - so a process's IMAGE, the largest thing it holds,
	// sat entirely outside the limit whose name implied it was inside. A Domain could be given a
	// small memory cap and a process loaded into it would consume many times that in image frames
	// without the counter moving.
	//
	// The property is checkable without a loader: `reserve_adopt` is the booking every image load
	// goes through, and `try_adopt_frame` is what a growing stack uses.
	use crate::mem::frame::PAGE_SIZE;
	use crate::object::process::Process;

	let domain = Domain::new(4 * PAGE_SIZE, UNLIMITED, UNLIMITED);
	let space = crate::object::address_space::AddressSpace::create().expect("an address space");
	let process = Process::new(space, domain.clone()).expect("a process in a bounded domain");
	let before = domain.account().memory().used();

	// Two pages of image fit.
	assert!(process.reserve_adopt(2, 0), "a booking within the limit is granted");
	assert_eq!(domain.account().memory().used(), before + 2 * PAGE_SIZE, "the image booking reached the account");

	// A booking past the cap is refused, and refused WITHOUT charging.
	let at_limit = domain.account().memory().used();
	assert!(!process.reserve_adopt(8, 0), "a booking past the limit is refused");
	assert_eq!(domain.account().memory().used(), at_limit, "a refused booking leaves the account where it was");

	// And a booking that is abandoned gives its charge back.
	process.release_adopt_charge(2);
	assert_eq!(domain.account().memory().used(), before, "an abandoned booking is refunded");

	// The stack-growth path charges too, and refuses when the limit is reached.
	let frame = crate::mem::frame::allocate().expect("a frame");
	assert!(process.try_adopt_frame(frame), "one page fits");
	assert_eq!(domain.account().memory().used(), before + PAGE_SIZE, "an adopted frame reached the account");
}

crate::tagged_test!(page_tables_are_charged_to_the_domain_that_caused_them, [Kernel, Memory], id = "kernel.object.domain.page_tables_are_charged_to_the_domain_that_caused_them", covers = ["kernel"]);
fn page_tables_are_charged_to_the_domain_that_caused_them() {
	// A process that maps pages makes the kernel allocate frames for its OWN page tables, and until
	// this nothing charged them: a mapping loop was a way to consume kernel memory without meeting
	// any limit.
	//
	// The property has three halves and all three are here: the root table is charged when the
	// space is made; a mapping that would exceed the limit is refused WITHOUT leaving a partial
	// result; and everything comes back when the space dies.
	use crate::arch::paging;
	use crate::mem::frame::{self, PAGE_SIZE};
	use crate::memlayout::USER_MMAP_BASE;
	use crate::object::address_space::AddressSpace;

	let domain = Domain::new(64 * PAGE_SIZE, UNLIMITED, UNLIMITED);
	let before = domain.account().memory().used();
	let space = AddressSpace::create_in(&domain).expect("an address space in a bounded domain");
	assert_eq!(domain.account().memory().used(), before + PAGE_SIZE, "the root page table is charged when the space is made");

	// One mapping, which builds the levels below the root on a fresh space.
	let data = frame::allocate().expect("a data frame");
	let at = USER_MMAP_BASE;
	space.try_map(at, data, paging::PRESENT | paging::WRITABLE | paging::USER | paging::NO_EXECUTE).expect("the first mapping fits");
	let after_first = domain.account().memory().used();
	assert!(after_first > before + PAGE_SIZE, "the intermediate tables the walk built are charged too");
	assert!(after_first <= before + (1 + paging::MAX_NEW_TABLES as u64) * PAGE_SIZE, "and only what the walk actually built - the rest of the reservation goes back");

	// A DOMAIN WITH NO ROOM LEFT CANNOT BE MADE TO BUILD ONE MORE TABLE. The limit is filled with
	// an ordinary charge, then a mapping far enough away to need fresh levels is asked for.
	let room = 64 * PAGE_SIZE - (after_first - before);
	assert!(domain.try_charge_memory(room), "the rest of the limit is taken");
	let at_limit = domain.account().memory().used();
	let far = USER_MMAP_BASE + (1u64 << 30);
	let second = frame::allocate().expect("a second data frame");
	assert!(space.try_map(far, second, paging::PRESENT | paging::WRITABLE | paging::USER | paging::NO_EXECUTE).is_err(), "a mapping the limit has no room for is refused");
	assert_eq!(domain.account().memory().used(), at_limit, "a refused mapping leaves the account exactly where it was");
	// AND NOTHING WAS LEFT AT THE REFUSED ADDRESS. The walk refuses to replace a live mapping, so a
	// second attempt succeeding is exactly the proof that the first left no leaf and no half-built
	// path behind it.
	domain.uncharge_memory(room);
	space.try_map(far, second, paging::PRESENT | paging::WRITABLE | paging::USER | paging::NO_EXECUTE).expect("the same address maps once there is room, so the refusal left nothing behind");

	// AND THE SPACE GIVES IT ALL BACK. Dropping it frees the tables and uncharges the same pages
	// that were charged, from the count kept as they were taken.
	drop(space);
	assert_eq!(domain.account().memory().used(), before, "an address space that dies returns every page table it was charged for");
	unsafe { frame::deallocate(data) };
}

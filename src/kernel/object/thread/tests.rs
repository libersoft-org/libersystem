use super::super::address_space::AddressSpace;
use super::super::process::Process;
use super::super::{KernelObject, ObjectType};
use super::{Thread, ThreadState};
use crate::sched;

crate::tagged_test!(thread_object_basics, [Object, Process, Smoke], id = "kernel.object.thread.thread_object_basics", covers = ["kernel"]);
fn thread_object_basics() {
	extern "C" fn noop(_arg: u64) {}
	let process = Process::new(AddressSpace::kernel(), sched::root_domain()).expect("a test process");
	let thread = Thread::new(noop, 0, process).expect("a thread");
	assert_eq!(thread.object_type(), ObjectType::Thread);
	assert_eq!(thread.state(), ThreadState::Ready);
	assert!(thread.tid() >= 1);
}

crate::tagged_test!(the_deepest_kernel_path_leaves_headroom_on_its_stack, [Object, Process, Memory, Syscall], id = "kernel.object.thread.the_deepest_kernel_path_leaves_headroom_on_its_stack", covers = ["kernel"]);
fn the_deepest_kernel_path_leaves_headroom_on_its_stack() {
	// KERNEL_STACK_SIZE was a round number for as long as this kernel has existed, and running off
	// the end of it is not a failure anyone gets to read: aarch64's exception entry saves an
	// 816-byte frame before it does anything else, so the handler for a stack overflow faults on
	// its own first store, re-enters the same vector and loops - writing register pairs down
	// through the kernel window until the machine is unrecognisable. It cost this milestone four
	// days and produced no diagnostic of any kind, because the loop never reaches a print.
	//
	// So the ceiling is held by a MEASUREMENT of the deepest path the kernel has rather than by
	// judgement. The stack is zeroed at allocation, so the lowest byte anything ever wrote is the
	// high-water mark; the spawner holds the `Arc`, so it is read from outside the thread and the
	// reading does not deepen what it measures.
	//
	// THE PATH MATTERS AS MUCH AS THE NUMBER. `SYS_PROCESS_LOAD` is the deepest call chain that
	// exists here - a syscall into the ELF loader into the mapper, on a debug build with nothing
	// inlined - and it is the one that actually overflowed. Anything deeper appearing later is what
	// this test is for.
	use crate::arch::paging::{PRESENT, USER, WRITABLE};
	use crate::mem::frame::PAGE_SIZE;
	use core::sync::atomic::{AtomicI64, Ordering};

	const AT: u64 = crate::memlayout::USER_VA_END / 2 - 16 * PAGE_SIZE;
	static ANSWER: AtomicI64 = AtomicI64::new(0);

	extern "C" fn spawner(_bootstrap: u64) {
		unsafe {
			let child = crate::arch::syscall::invoke(crate::syscall::SYS_PROCESS_CREATE, 0, 0, 0, 0);
			assert!((child as i64) > 0, "the child process is created");
			ANSWER.store(crate::arch::syscall::invoke(crate::syscall::SYS_PROCESS_LOAD, child, AT, 2 * PAGE_SIZE, 0) as i64, Ordering::SeqCst);
		}
	}

	// A refusable image: the magic, and nothing else that parses. What is being measured is the
	// depth of the call, not the success of the load.
	let first = crate::mem::frame::allocate().expect("a frame for the first page");
	let second = crate::mem::frame::allocate().expect("a frame for the second page");
	crate::arch::paging::map_page(AT, first, PRESENT | WRITABLE | USER);
	crate::arch::paging::map_page(AT + PAGE_SIZE, second, PRESENT | WRITABLE | USER);
	crate::arch::paging::user_access(|| unsafe {
		core::ptr::write_bytes(AT as *mut u8, 0, 2 * PAGE_SIZE as usize);
		core::ptr::copy_nonoverlapping(b"\x7fELF".as_ptr(), AT as *mut u8, 4);
	});

	let (_kernel_ep, user_ep) = crate::object::channel::Channel::create();
	let thread = crate::sched::spawn_with_object(spawner, user_ep, crate::object::rights::Rights::ALL);
	crate::sched::run_until_idle();

	assert!(ANSWER.load(Ordering::SeqCst) < 0, "a malformed image must be refused, so the whole loader path really ran");

	let used = thread.kstack_used();
	let capacity = thread.kstack_capacity();
	crate::serial_println!("kernel stack high-water on the spawn path: {used} of {capacity} bytes ({}%)", used * 100 / capacity);

	// A QUARTER SPARE, and the margin is not decoration: an interrupt taken at the deepest point
	// costs another 816 bytes on aarch64 before the handler runs, and a nested one costs it again.
	// The number this is protecting against is not the average, it is the worst moment.
	assert!(used * 4 <= capacity * 3, "the spawn path used {used} of {capacity} bytes of kernel stack, leaving less than a quarter spare - raise KERNEL_STACK_SIZE, because an overflow here is an unrecoverable exception loop rather than a panic");
	// Deliberately NOT asserted in the other direction. A ceiling that is too generous costs memory
	// per thread and nothing else, it differs by architecture (x86_64's exception frame and call
	// depth are both smaller than aarch64's), and an assertion that fires when the kernel gets
	// CHEAPER is an assertion that gets deleted. The printed figure above is the record; whoever
	// wants the stack back has the number to argue from.

	// AND THE BRING-UP SLICES, which are all `SEC_STACKS` still carries.
	//
	// A secondary core stands on its slice only from the boot stub to `KernelStack::allocate`, then
	// switches to a guarded stack and never returns to it - so what this measures now is the depth
	// of the bring-up prologue, and it should be small. It is kept because the slices are still a
	// static array with no guard page between them: if this number ever climbs, that is a core doing
	// real work on unguarded memory, and it is the only thing that would say so.
	//
	// The idle stacks themselves need no measurement any more. They have an unmapped page below
	// them, so an overflow is a fault at the instruction that causes it rather than a number to
	// watch.
	#[cfg(target_arch = "aarch64")]
	{
		let capacity = crate::arch::psci::secondary_stack_capacity();
		let mut deepest = 0;
		for cpu in 1..crate::smp::cpu_count() {
			let used = crate::arch::psci::secondary_stack_used(cpu);
			deepest = deepest.max(used);
			crate::serial_println!("secondary bring-up slice high-water: cpu {cpu} used {used} of {capacity} bytes ({}%)", used * 100 / capacity);
		}
		assert!(deepest * 4 <= capacity * 3, "a secondary core used {deepest} of {capacity} bytes of its BRING-UP slice, which has no guard page below it - a core should leave this for a guarded stack long before it gets deep");
	}

	crate::arch::paging::unmap_page(AT);
	crate::arch::paging::unmap_page(AT + PAGE_SIZE);
	unsafe {
		crate::mem::frame::deallocate(first);
		crate::mem::frame::deallocate(second);
	}
}

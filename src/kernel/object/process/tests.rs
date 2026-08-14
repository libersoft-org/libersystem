use super::Process;
use crate::object::address_space::AddressSpace;
use crate::{elf, sched};

crate::tagged_test!(dynamic_symbol_names_accept_rust_mangling_with_a_bound, [Dynamic, Memory, Process], id = "kernel.object.process.dynamic_symbol_names_accept_rust_mangling_with_a_bound", covers = ["kernel"]);
fn dynamic_symbol_names_accept_rust_mangling_with_a_bound() {
	let address_space = AddressSpace::create().expect("address space");
	let process = Process::new(address_space, sched::root_domain()).expect("a test process");
	let accepted = alloc::string::String::from_utf8(alloc::vec![b'x'; elf::MAX_DYNAMIC_SYMBOL_NAME]).expect("ASCII symbol");
	assert!(process.register_dynamic_symbols(&[(accepted, 0x2000_1000)]), "the bounded Rust symbol is accepted");
	let rejected = alloc::string::String::from_utf8(alloc::vec![b'y'; elf::MAX_DYNAMIC_SYMBOL_NAME + 1]).expect("ASCII symbol");
	assert!(!process.register_dynamic_symbols(&[(rejected, 0x2000_2000)]), "an overlong symbol is rejected");
}

crate::tagged_test!(a_process_that_has_torn_down_never_gains_a_thread, [Process, Scheduler, Kernel], id = "kernel.object.process.a_process_that_has_torn_down_never_gains_a_thread", covers = ["kernel"]);
fn a_process_that_has_torn_down_never_gains_a_thread() {
	// The other side of `terminate`: the cleanup must see a set that cannot grow under it.
	//
	// `terminate` sets a flag, unmaps, closes every handle and only then marks the process killed.
	// A `sys_thread_create` that passed its liveness check before the flag went up used to be free
	// to finish its work afterwards, so a thread could be constructed, registered and started
	// INSIDE a process whose handles and mappings were already gone - a thread with no capabilities,
	// no address space and nothing to reap it, holding a Domain's thread charge until the machine
	// went down.
	//
	// WHAT IS ASSERTED IS THAT THE SET DOES NOT GROW, which is what teardown promises. Threads that
	// were already registered stay registered until they exit - they exit at their next kill point,
	// which a thread that was never started never reaches - and that is the process's business, not
	// this barrier's.
	//
	// The late arrival is performed directly rather than raced for: there is no way to hold a second
	// core between the check and the registration, and the half that was reachable is the half that
	// lands after teardown, which is exactly where this puts it.
	use crate::object::thread::Thread;

	extern "C" fn nothing(_: u64) {}

	let address_space = AddressSpace::create().expect("address space");
	let process = Process::new(address_space, sched::root_domain()).expect("a test process");
	let before = Thread::new(nothing, 0, process.clone()).expect("a live process takes a thread");
	assert_eq!(process.live_threads().len(), 1, "the thread joined the process");
	assert!(!process.is_terminating(), "the process is live");

	process.terminate();
	assert!(process.is_terminating(), "termination is published");
	let after_cleanup = process.live_threads().len();

	// Nothing can register any more, by either door: the registration itself, or the constructor
	// that would have to perform it.
	assert!(!process.register_thread(&before), "a registration arriving after teardown is refused");
	assert!(Thread::new(nothing, 0, process.clone()).is_none(), "a thread is not even constructed into a process that is going away - it would be one nothing could signal, reap or account");

	// Sixteen more, because a barrier that holds once and leaks afterwards is the same defect with a
	// longer fuse - and because a refused build must give back everything it took.
	let domain = sched::root_domain();
	let charged = domain.account().threads().used();
	for _ in 0..16 {
		assert!(Thread::new(nothing, 0, process.clone()).is_none(), "still refused");
	}
	assert_eq!(domain.account().threads().used(), charged, "sixteen refused builds charged nothing");
	assert_eq!(process.live_threads().len(), after_cleanup, "no thread was registered after cleanup");

	// And the thread that WAS legitimately built cannot be enqueued now. `try_start` only ever
	// answered "was this thread started before", which says nothing about the process it belongs
	// to, so this was the way a thread got into a killed process even without the race above.
	assert!(before.process().is_terminating(), "the check `sys_thread_start` makes before enqueueing");

	drop(before);
	drop(process);
	sched::run_until_idle();
}

crate::tagged_test!(the_lifecycle_guard_covers_the_whole_operation_and_not_just_its_first_line, [Kernel, Memory, Dma], id = "kernel.object.process.the_lifecycle_guard_covers_the_whole_operation_and_not_just_its_first_line", covers = ["kernel"]);
fn the_lifecycle_guard_covers_the_whole_operation_and_not_just_its_first_line() {
	// A resource-extending syscall used to read `is_terminating()` at its top and record its result
	// at its bottom, with a reservation and a page-table walk in between - and `terminate` publishes
	// the flag and takes its snapshot in between ITS two steps. Both halves of that are tested here,
	// because a flag read and a barrier look identical until they are raced:
	//
	//   1. A guard taken before teardown keeps "the process is live" true for as long as it is held.
	//   2. A guard cannot be taken once teardown has begun.
	//   3. A DmaBuffer that joined the registry is marked orphaned even though it is in NO handle
	//      table - which is the case `take_for_transfer` produces and the handle-table pass misses.
	use crate::mem::frame::PAGE_SIZE;
	use crate::object::dma_buffer::DmaBuffer;

	let process = Process::new(AddressSpace::create().expect("address space"), sched::root_domain()).expect("a test process");

	// 1 and 3. A buffer created under the guard, held only by this test - no handle table anywhere
	//    names it - and registered while the process is live.
	const DEVICE: u32 = 0xD2;
	let buffer = {
		let guard = process.begin_extend().expect("a live process gives out the guard");
		let Ok(buffer) = DmaBuffer::create_for(&sched::root_domain(), 2 * PAGE_SIZE as usize, Some(DEVICE)) else {
			panic!("a 2-page DMA buffer should allocate");
		};
		assert!(guard.record_dma_buffer(&buffer), "the buffer joins the creating process's registry");
		buffer
	};
	assert!(!buffer.is_orphaned_for_test(), "a live process's buffer is not orphaned");

	// 2. Teardown refuses new guards.
	process.terminate();
	assert!(process.begin_extend().is_none(), "an operation starting after teardown is refused the guard");

	// 3. And the pass reached the buffer through the registry, which is the only place it was.
	assert!(buffer.is_orphaned_for_test(), "a buffer the handle table never held is still marked - the registry is what the orphan rule now depends on, not the table");

	// Its frames are therefore held rather than recycled, which is the point of marking it.
	let frames = buffer.frames().len();
	drop(buffer);
	assert_eq!(crate::object::dma_buffer::held_frames_for_test(DEVICE), frames, "and so its frames wait for the device to be reset");
	crate::object::dma_buffer::forget_for_test(DEVICE);

	drop(process);
	sched::run_until_idle();
}

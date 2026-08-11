use super::Process;
use crate::object::address_space::AddressSpace;
use crate::{elf, sched};

crate::tagged_test!(dynamic_symbol_names_accept_rust_mangling_with_a_bound, [Dynamic, Memory, Process], id = "kernel.object.process.dynamic_symbol_names_accept_rust_mangling_with_a_bound", covers = ["kernel"]);
fn dynamic_symbol_names_accept_rust_mangling_with_a_bound() {
	let address_space = AddressSpace::create().expect("address space");
	let process = Process::new(address_space, sched::root_domain());
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
	let process = Process::new(address_space, sched::root_domain());
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

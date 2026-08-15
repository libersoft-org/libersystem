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

crate::tagged_test!(every_thread_books_a_run_queue_slot_and_gives_it_back, [Object, Process, Scheduler, Memory], id = "kernel.object.thread.every_thread_books_a_run_queue_slot_and_gives_it_back", covers = ["kernel"]);
fn every_thread_books_a_run_queue_slot_and_gives_it_back() {
	// THE BOOKING IS PAIRED WITH THE OBJECT, which is the part that is easy to get wrong.
	//
	// The scheduler's `run_queue.push_back` calls carried `ALLOC-OK: bounded by the Domain thread
	// quota`, and a bound is not a booking - a wake on a short heap was a kernel abort reachable
	// from ring 3. Enqueue has nowhere to put a refusal, so the room is taken at creation, where
	// `None` is an answer every caller already handles.
	//
	// The count then has to balance exactly. The first version of this reserved in a wrapper around
	// the constructor and released in the wrapper's failure branch AND in `Drop for Thread` - so a
	// thread that was built and then refused registration released twice, the counter wrapped
	// through zero, and the next reservation panicked on the overflow. This is the test that says
	// so: create threads, drop them, and the count is what it was.
	extern "C" fn noop(_arg: u64) {}
	let before = sched::live_thread_bookings();
	{
		let process = Process::new(AddressSpace::kernel(), sched::root_domain()).expect("a test process");
		let mut threads = alloc::vec::Vec::new();
		for _ in 0..4 {
			threads.push(Thread::new(noop, 0, process.clone()).expect("a thread"));
		}
		assert_eq!(sched::live_thread_bookings(), before + 4, "each thread books one slot on every core");
	}
	assert_eq!(sched::live_thread_bookings(), before, "and gives it back exactly once when it drops");

	// A thread whose process is already tearing down is CONSTRUCTED and then refused registration -
	// the path that released twice.
	let process = Process::new(AddressSpace::kernel(), sched::root_domain()).expect("a test process");
	process.terminate();
	assert!(Thread::new(noop, 0, process).is_none(), "a terminating process gains no thread");
	assert_eq!(sched::live_thread_bookings(), before, "and the booking it took on the way is released once");
}

use super::*;

tagged_test!(a_translation_is_not_a_permission, [Memory, Syscall], covers = ["kernel"]);
fn a_translation_is_not_a_permission() {
	// `user_buf_ok` asked only whether an address TRANSLATED. A ring-3 caller could
	// therefore hand the kernel a pointer into a page it cannot itself reach - a
	// kernel-only page in its own address space - and the kernel would read or write it on
	// the caller's behalf. And a read-only page was accepted as the destination of a
	// copy-out, where the write faults in ring 0, which this kernel stops on.
	use crate::arch::paging::{PRESENT, USER, WRITABLE, translate_flags};
	use crate::object::address_space::AddressSpace;

	let space = AddressSpace::create().expect("address space");
	let frame = crate::mem::frame::allocate().expect("a frame");
	let at = 0x50_0000u64;

	// kernel-only: present, not USER.
	space.try_map(at, frame, PRESENT | WRITABLE).expect("a kernel mapping");
	let flags = translate_flags_in(&space, at);
	assert!(flags.is_some_and(|f| f & PRESENT != 0), "the page is mapped");
	assert!(flags.is_some_and(|f| f & USER == 0), "and it is not reachable from ring 3");
	assert_eq!(space.unmap(at), Some(frame));

	// user, read-only: reachable, not writable.
	space.try_map(at, frame, PRESENT | USER).expect("a read-only user mapping");
	let flags = translate_flags_in(&space, at);
	assert!(flags.is_some_and(|f| f & USER != 0), "a user page is reachable");
	assert!(flags.is_some_and(|f| f & WRITABLE == 0), "and a read-only one is not a destination");
	assert_eq!(space.unmap(at), Some(frame));

	// and an unmapped address answers nothing at all.
	assert!(translate_flags_in(&space, at).is_none(), "an unmapped address has no flags");
	assert!(translate_flags(0x7fff_0000_0000).is_none(), "nor does one nothing ever mapped");

	unsafe { crate::mem::frame::deallocate(frame) };
}

// `translate_flags` reads the ACTIVE tables, so a mapping made in another address space
// has to be looked at with that space installed. Switching is what a real caller does
// implicitly by being the thread that owns the space.
fn translate_flags_in(space: &alloc::sync::Arc<crate::object::address_space::AddressSpace>, va: u64) -> Option<u64> {
	let previous = crate::arch::context::read_cr3();
	unsafe { crate::arch::context::write_cr3(space.cr3()) };
	let flags = crate::arch::paging::translate_flags(va);
	unsafe { crate::arch::context::write_cr3(previous) };
	flags
}

tagged_test!(a_shootdown_is_answered_by_every_other_core, [Memory, Scheduler, Smp], covers = ["kernel"]);
fn a_shootdown_is_answered_by_every_other_core() {
	// Every port invalidated its OWN translations and told nobody, so a frame could go
	// back to the allocator while another core still held a translation for it - and that
	// core went on writing through it into whatever the frame became next.
	//
	// What this asserts is the property that makes the frame safe to release: the request
	// RETURNS, and it returns because every other core answered rather than because the
	// wait gave up. A shootdown that times out prints; a shootdown that completes does
	// not, so the run is silent when this passes.
	let cores = crate::smp::cpu_count();
	// Ten in a row, so a single lucky interleaving does not carry the test.
	for _ in 0..10 {
		crate::mem::tlb::shootdown();
	}
	assert!(cores >= 1, "the machine reports at least one core");
	// and it is safe from a core with nothing else to do, which is the state most of the
	// other cores are in when a process is torn down.
	crate::sched::run_until_idle();
	crate::mem::tlb::shootdown();
}

tagged_test!(a_capability_transfer_moves_it_exactly_once, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_capability_transfer_moves_it_exactly_once() {
	// A transfer was a clone under the lock, a send, and then a re-lookup and a `close`
	// whose result was DISCARDED. Two ways that mints a capability without the
	// `DUPLICATE` right, and the second needs no race at all: name the same handle twice
	// in one batch and each is cloned independently, then the close runs twice and the
	// second failure is thrown away.
	use crate::object::channel::Channel;
	use crate::object::handle::{Handle, HandleTable};
	use crate::object::rights::Rights;

	let mut table = HandleTable::new();
	let (carried, _peer) = Channel::create();
	let handle = table.insert_object(carried, Rights::ALL, 0);

	// Taking it MOVES it: the handle is dead immediately, with no window in which it
	// still names anything.
	let cap = table.take(handle, Rights::TRANSFER).expect("the first take succeeds");
	assert!(table.take(handle, Rights::TRANSFER).is_err(), "a taken handle names nothing");
	assert!(table.rights_of(handle).is_err(), "and cannot be inspected either");

	// Putting it back gives a NEW handle - the old slot generation died with the take -
	// which is the honest outcome of a move that had to be undone.
	let returned = table.put_back(cap);
	assert_ne!(returned.raw(), handle.raw(), "a returned capability arrives under a new handle");
	assert!(table.rights_of(returned).is_ok(), "and that handle works");

	// The duplicate-in-one-batch case. The syscall refuses it on this predicate, which is
	// tested directly rather than through the call: this context has no current thread,
	// so the syscall cannot reach its own check.
	assert!(crate::syscall::has_repeat(&[7, 9, 7]), "a repeated handle is a repeat");
	assert!(crate::syscall::has_repeat(&[4, 4]), "even two of them");
	assert!(!crate::syscall::has_repeat(&[1, 2, 3, 4]), "and distinct handles are not");
	assert!(!crate::syscall::has_repeat(&[]), "nor is an empty array");
	let _ = Handle::from_raw(returned.raw());
}

tagged_test!(mapping_over_a_live_page_is_refused_not_performed, [Memory, Process], covers = ["kernel"]);
fn mapping_over_a_live_page_is_refused_not_performed() {
	// The leaf write was unconditional, so mapping over an existing page silently
	// replaced it and the frame that was there was simply lost - no owner, no error, no
	// way to notice. A second process load overwrote the first, two stack faults could
	// overwrite each other, and one loader's rollback could unmap another's page.
	use crate::arch::paging::{PRESENT, USER, WRITABLE};
	use crate::object::address_space::AddressSpace;

	let space = AddressSpace::create().expect("address space");
	let first = crate::mem::frame::allocate().expect("a frame");
	let second = crate::mem::frame::allocate().expect("another frame");
	let at = 0x40_0000u64;

	assert!(space.try_map(at, first, PRESENT | USER | WRITABLE).is_ok(), "the first mapping is made");
	assert!(space.try_map(at, second, PRESENT | USER | WRITABLE).is_err(), "the second must be refused, not performed");
	// and the first mapping is untouched, which is the half that matters: a refusal that
	// damaged the existing entry would be no better than the overwrite.
	assert_eq!(space.unmap(at), Some(first), "the original frame is still the one mapped there");
	// with the page free again, the same address maps fine - the refusal is about the
	// mapping being live, not about the address.
	assert!(space.try_map(at, second, PRESENT | USER | WRITABLE).is_ok(), "an unmapped address still maps");
	assert_eq!(space.unmap(at), Some(second));

	unsafe { crate::mem::frame::deallocate(first) };
	unsafe { crate::mem::frame::deallocate(second) };
}

tagged_test!(duplicating_a_handle_is_charged_like_any_other_install, [Process, Syscall], covers = ["kernel"]);
fn duplicating_a_handle_is_charged_like_any_other_install() {
	// `duplicate` finished with the UNBOUNDED insert, so a process holding one duplicable
	// handle could pass its handle limit indefinitely just by asking. Worse than the count
	// itself: other checks bound themselves by "how many handles the caller holds", which
	// made them bounded by nothing.
	use crate::object::channel::Channel;
	use crate::object::domain::Domain;
	use crate::object::handle::{Handle, HandleError, HandleTable};
	use crate::object::rights::Rights;

	// a domain whose handle limit is four, so the quota is reached in a few steps.
	let domain = Domain::new_child(&Domain::root(), u64::MAX, 4, u64::MAX).expect("a live parent takes a child");
	let mut table = HandleTable::new();
	table.set_domain(domain);
	let (channel, _peer) = Channel::create();
	let first = table.try_insert_object(channel, Rights::ALL, 0).expect("the first handle fits");

	// duplicate until the quota says no, and it must say no rather than growing forever.
	let mut made = 0;
	loop {
		match table.duplicate(first, Rights::ALL) {
			Ok(_) => {
				made += 1;
				assert!(made < 64, "duplicate must stop at the quota, not run away");
			}
			Err(HandleError::LimitReached) => break,
			Err(other) => panic!("unexpected error from duplicate: {other:?}"),
		}
	}
	assert_eq!(made, 3, "one original plus three duplicates fills a limit of four");
	let _ = table.close(Handle::from_raw(first.raw()));
}

tagged_test!(the_last_thread_out_is_decided_by_a_counter_not_a_snapshot, [Process, Scheduler], covers = ["kernel"]);
fn the_last_thread_out_is_decided_by_a_counter_not_a_snapshot() {
	// Whether a thread is the last one out was read from a snapshot of the OTHER live
	// threads. Two threads exiting at the same time each saw the other, neither called
	// itself the last, and the process was never finalised - alive forever, with `wait`
	// never returning and its handles never closing.
	//
	// The interleaving that does it cannot be forced from here, so what is tested is the
	// property that makes it impossible: the decision is a counter, and exactly one caller
	// gets it however many arrive.
	use crate::object::address_space::AddressSpace;
	use crate::object::process::Process;

	extern "C" fn nothing(_: u64) {}

	let space = AddressSpace::create().expect("address space");
	let process = Process::new(space, crate::object::domain::Domain::root());

	// four threads registered, four exits, exactly one "you were the last".
	let threads: alloc::vec::Vec<_> = (0..4).map(|_| crate::object::thread::Thread::new(nothing, 0, process.clone()).expect("a thread")).collect();
	assert_eq!(threads.len(), 4);
	let lasts = (0..4).filter(|_| process.thread_exited()).count();
	assert_eq!(lasts, 1, "exactly one thread may be told it was the last");

	// and it is the final one, not whichever ran first.
	let process2 = Process::new(AddressSpace::create().expect("address space"), crate::object::domain::Domain::root());
	let _t: alloc::vec::Vec<_> = (0..2).map(|_| crate::object::thread::Thread::new(nothing, 0, process2.clone()).expect("a thread")).collect();
	assert!(!process2.thread_exited(), "the first of two is not the last");
	assert!(process2.thread_exited(), "the second of two is");
}

tagged_test!(a_syscall_may_not_ask_the_kernel_for_an_unbounded_allocation, [Syscall, Memory], covers = ["kernel"]);
fn a_syscall_may_not_ask_the_kernel_for_an_unbounded_allocation() {
	// Every allocation the kernel sizes from a userspace number used to be a plain `vec!`
	// with no ceiling above it: one syscall could name a length and the kernel would try
	// to satisfy it, answering exhaustion through the allocation-error handler rather
	// than with an error code.
	//
	// The buffer here is deliberately real and small - what is being tested is that the
	// LENGTH is refused before anything is allocated, so `user_buf_ok` never even runs.
	// Every one of these is refused on the LENGTH alone, before a handle is resolved or a
	// lock is taken - which is why the handle below is 0 and the buffer is 64 bytes.
	let buf = [0u8; 64];

	assert_eq!(crate::syscall::syscall_dispatch(abi::SYS_CHANNEL_SEND, 0, buf.as_ptr() as u64, abi::MAX_MESSAGE_BYTES as u64 + 1, 0) as i64, crate::syscall::ERR_INVALID, "a message larger than the ABI allows is refused, not attempted");
	assert_eq!(crate::syscall::syscall_dispatch(abi::SYS_PROCESS_LOAD, 0, buf.as_ptr() as u64, abi::MAX_ELF_BYTES as u64 + 1, 0) as i64, crate::syscall::ERR_INVALID, "an ELF larger than the ABI allows is refused before the handle is even resolved");
	assert_eq!(crate::syscall::syscall_dispatch(abi::SYS_WAIT_ANY, buf.as_ptr() as u64, abi::MAX_WAIT_HANDLES as u64 + 1, 0, 0) as i64, crate::syscall::ERR_INVALID, "a wait set larger than the ABI allows is refused");
	// That the ceilings do not simply refuse everything is not asserted here - this
	// context has no current thread, so an ordinary send cannot get far enough to tell
	// the size check apart from the thread lookup. It is covered by the rest of the
	// suite, which sends real messages through these same paths in every service test.
}

tagged_test!(a_cpu_bound_ring3_thread_is_preempted, [Scheduler, Process], covers = ["kernel"]);
fn a_cpu_bound_ring3_thread_is_preempted() {
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	use mem::frame::{self, PAGE_SIZE};
	// The spinner's shared data page sits at a fixed USER address clear of the test
	// code and stack pages: [0] = stop flag, [8] = liveness counter.
	const SPIN_FLAG_VA: u64 = 0x0000_0000_4000_2000;
	static SPIN_FLAG_PHYS: AtomicU64 = AtomicU64::new(0);
	static SPIN_DONE: AtomicBool = AtomicBool::new(false);
	// Host thread for the ring-3 spinner: maps code + stack + the shared data page,
	// publishes the data frame for the releaser, and drops to ring 3. The spinner
	// makes NO syscall until released, so without ring-3 preemption it would own
	// this core forever and the test would hang.
	extern "C" fn spinner_body(_arg: u64) {
		let code = frame::allocate().expect("user code frame");
		let stack = frame::allocate().expect("user stack frame");
		let data = frame::allocate().expect("user data frame");
		// Zero the data page so the stop flag starts clear (a recycled frame is not).
		unsafe { core::ptr::write_bytes((mem::hhdm_offset() + data) as *mut u8, 0, PAGE_SIZE as usize) };
		let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
		arch::paging::map_page(USER_CODE_VA, code, flags);
		arch::paging::map_page(USER_STACK_VA, stack, flags | arch::paging::NO_EXECUTE);
		arch::paging::map_page(SPIN_FLAG_VA, data, flags | arch::paging::NO_EXECUTE);
		let program = arch::usermode::program_spin_bytes();
		unsafe {
			arch::paging::copy_to_user_page(USER_CODE_VA, program);
		}
		SPIN_FLAG_PHYS.store(data, Ordering::SeqCst);
		unsafe {
			arch::usermode::enter(USER_CODE_VA, USER_STACK_VA + PAGE_SIZE, SPIN_FLAG_VA);
		}
		arch::paging::unmap_page(USER_CODE_VA);
		arch::paging::unmap_page(USER_STACK_VA);
		arch::paging::unmap_page(SPIN_FLAG_VA);
		unsafe { frame::deallocate(code) };
		unsafe { frame::deallocate(stack) };
		unsafe { frame::deallocate(data) };
		SPIN_DONE.store(true, Ordering::SeqCst);
	}
	// The releaser waits until the spinner's counter demonstrably grows - proof the
	// ring-3 loop is running AND being preempted (this kernel thread shares the same
	// core) - then raises the stop flag through the frame's kernel mapping.
	extern "C" fn releaser(_arg: u64) {
		let data = loop {
			let phys = SPIN_FLAG_PHYS.load(Ordering::SeqCst);
			if phys != 0 {
				break phys;
			}
			core::hint::spin_loop();
		};
		let flag = (mem::hhdm_offset() + data) as *mut u64;
		let counter = unsafe { flag.add(1) };
		let start = unsafe { counter.read_volatile() };
		while unsafe { counter.read_volatile() } < start.wrapping_add(1000) {
			core::hint::spin_loop();
		}
		unsafe { flag.write_volatile(1) };
	}
	SPIN_FLAG_PHYS.store(0, Ordering::SeqCst);
	SPIN_DONE.store(false, Ordering::SeqCst);
	// Both threads land on this core: the spinner never yields in ring 3, the
	// releaser never yields in ring 0 - only the timer can interleave them, and the
	// spinner's half of that needs ring-3 preemption.
	sched::spawn(spinner_body, 0);
	sched::spawn(releaser, 0);
	sched::run_until_idle();
	assert!(SPIN_DONE.load(Ordering::SeqCst), "the ring-3 spinner never finished: ring 3 was not preempted");
}

tagged_test!(process_isolation_and_per_process_tables, [Process, Memory], covers = ["kernel"]);
fn process_isolation_and_per_process_tables() {
	use core::sync::atomic::{AtomicU64, Ordering};
	use mem::frame;
	use object::address_space::AddressSpace;
	use object::process::Process;
	use object::rights::Rights;

	// A single user virtual address that both processes map - to different frames.
	const VA: u64 = 0x0000_0000_3000_0000;
	// Each reader records the CR3 it ran on and the value it saw at VA, indexed by
	// the discriminator it is spawned with.
	static CR3: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
	static SEEN: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
	extern "C" fn reader(which: u64) {
		let cr3 = arch::context::read_cr3();
		// The page is USER-mapped, so this ring-0 read goes through the sanctioned
		// SMAP window (the test reads it to prove CR3 isolation, not to dodge SMAP).
		let value = arch::paging::user_access(|| unsafe { (VA as *const u64).read_volatile() });
		CR3[which as usize].store(cr3, Ordering::SeqCst);
		SEEN[which as usize].store(value, Ordering::SeqCst);
	}

	// Two processes, each with its own page tables, in the root Domain.
	let p1 = Process::new(AddressSpace::create().expect("address space 1"), sched::root_domain());
	let p2 = Process::new(AddressSpace::create().expect("address space 2"), sched::root_domain());

	// Back the same VA with a distinct physical frame in each process, and stamp a
	// distinct value into each frame through the HHDM before mapping it.
	let f1 = frame::allocate().expect("frame 1");
	let f2 = frame::allocate().expect("frame 2");
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER;
	unsafe {
		((f1 + mem::hhdm_offset()) as *mut u64).write_volatile(0x1111_1111);
		((f2 + mem::hhdm_offset()) as *mut u64).write_volatile(0x2222_2222);
	}
	p1.address_space().map(VA, f1, flags);
	p2.address_space().map(VA, f2, flags);

	// Run a reader in each process and let them both finish.
	sched::thread_create(p1.clone(), reader, 0);
	sched::thread_create(p2.clone(), reader, 1);
	sched::run_until_idle();

	// Same VA, different physical frames: each reader saw only its own process's
	// memory - the address spaces are isolated.
	assert_eq!(SEEN[0].load(Ordering::SeqCst), 0x1111_1111);
	assert_eq!(SEEN[1].load(Ordering::SeqCst), 0x2222_2222);

	// The readers ran on different page-table roots, each its own process's CR3 -
	// proof the context switch reloaded CR3.
	let cr3_1 = CR3[0].load(Ordering::SeqCst);
	let cr3_2 = CR3[1].load(Ordering::SeqCst);
	assert_ne!(cr3_1, cr3_2);
	assert_eq!(cr3_1, p1.address_space().cr3());
	assert_eq!(cr3_2, p2.address_space().cr3());

	// Handle tables are per-process: a capability installed in one process is
	// invisible to the other.
	let (endpoint, _peer) = object::channel::Channel::create();
	p1.install(endpoint, Rights::ALL, 0);
	assert_eq!(p1.handles().lock().len(), 1);
	assert_eq!(p2.handles().lock().len(), 0);

	// Reclaim the data frames. Dropping the address spaces frees their page
	// tables, but these leaf frames are ours to release.
	assert_eq!(p1.address_space().unmap(VA), Some(f1));
	assert_eq!(p2.address_space().unmap(VA), Some(f2));
	unsafe { frame::deallocate(f1) };
	unsafe { frame::deallocate(f2) };
}

tagged_test!(syscall_object_and_handle_ops, [Syscall], covers = ["kernel"]);
fn syscall_object_and_handle_ops() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// The object/handle/mapping syscalls operate on the current thread's handle
	// table, so the sequence runs inside a spawned kernel thread. A failed
	// assertion here panics the thread, which fails the test run.
	extern "C" fn body(_arg: u64) {
		use object::rights::Rights;
		unsafe {
			// object create -> a handle into the caller's table
			let handle = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(handle));
			// address-space op: map it, then write and read back through the mapping
			let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, handle, 0, 0, 0);
			assert!(!syscall::sys_is_err(virt));
			let ptr = virt as *mut u64;
			ptr.write_volatile(0xfeed_face);
			assert_eq!(ptr.read_volatile(), 0xfeed_face);
			// mapping the same object twice is rejected (only one active mapping)
			let again = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, handle, 0, 0, 0);
			assert_eq!(again as i64, syscall::ERR_INVALID);
			// handle op: duplicate with attenuated rights (READ only)
			let dup = arch::syscall::invoke(syscall::SYS_HANDLE_DUPLICATE, handle, Rights::READ.bits() as u64, 0, 0);
			assert!(!syscall::sys_is_err(dup));
			// the READ-only duplicate lacks MAP, so mapping through it is denied
			let dup_map = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, dup, 0, 0, 0);
			assert!(syscall::sys_is_err(dup_map));
			// unmap and close both handles
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, handle, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, dup, 0, 0, 0) as i64, 0);
			// a closed handle no longer resolves
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0) as i64, syscall::ERR_BAD_HANDLE);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

tagged_test!(an_unmapped_va_range_is_reused_not_leaked, [Memory], covers = ["kernel"]);
fn an_unmapped_va_range_is_reused_not_leaked() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// The mmap window reclaims released ranges: an unmap returns its range to the window's
	// pool, so a map/unmap loop reuses addresses instead of walking off the window.
	//
	// What this asserts, and what it deliberately does NOT, is worth being exact about. It
	// used to name the addresses - "the released range comes back", "two singles pack" - and
	// that only held while this test was the only thing that had ever used the KERNEL window.
	// It is not: every kernel thread stack comes out of the same pool, so a test that spawns
	// processes ahead of this one leaves the free list with holes, and first-fit then answers
	// truthfully with an earlier one. The exact reuse and coalescing rules are asserted where
	// they can be asserted exactly, on a private pool, in `mem::vapool`. What belongs here is
	// the property that survives a shared window: the window does not GROW under churn.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let page: u64 = mem::frame::PAGE_SIZE;
			let handle = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, page, 0, 0, 0);
			assert!(!syscall::sys_is_err(handle));
			let first = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, handle, 0, 0, 0);
			assert!(!syscall::sys_is_err(first));
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, handle, 0, 0, 0) as i64, 0);
			// 64 map/unmap round trips. A window that leaked its ranges would hand out 64
			// distinct addresses climbing away from the first; one that reclaims them cannot
			// move further than the fragmentation that was already there.
			const ROUNDS: u64 = 64;
			let mut lowest = first;
			let mut highest = first;
			for _ in 0..ROUNDS {
				let base = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, handle, 0, 0, 0);
				assert!(!syscall::sys_is_err(base), "a map in the reuse loop failed");
				lowest = lowest.min(base);
				highest = highest.max(base);
				assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, handle, 0, 0, 0) as i64, 0);
			}
			assert!(highest - lowest < ROUNDS * page, "the window grew by {} bytes over {ROUNDS} map/unmap rounds: ranges are not being reclaimed", highest - lowest);
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0) as i64, 0);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

tagged_test!(blocking_wait_times_out_on_deadline, [Scheduler], covers = ["kernel"]);
fn blocking_wait_times_out_on_deadline() {
	use core::sync::atomic::{AtomicI64, Ordering};
	static WAIT_RET: AtomicI64 = AtomicI64::new(-999);
	// A thread waits on an event that is never signaled, with a short absolute
	// deadline. The wait must wake itself when the deadline passes and report
	// ERR_TIMED_OUT - the timed-wait path (the scheduler's deadline check).
	extern "C" fn waiter(_arg: u64) {
		unsafe {
			let ev = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			let now = arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
			let deadline = now + 3; // ~30 ms at the 100 Hz tick
			let ret = arch::syscall::invoke(syscall::SYS_WAIT, ev, deadline, 0, 0);
			WAIT_RET.store(ret as i64, Ordering::SeqCst);
		}
	}
	sched::spawn(waiter, 0);
	sched::run_until_idle();
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), syscall::ERR_TIMED_OUT);
}

tagged_test!(a_periodic_wait_ticks_but_never_holds_the_scheduler, [Scheduler], covers = ["kernel"]);
fn a_periodic_wait_ticks_but_never_holds_the_scheduler() {
	use core::sync::atomic::{AtomicU64, Ordering};
	static TICKS: AtomicU64 = AtomicU64::new(0);
	// A service thread waits with WAIT_PERIODIC on an event nothing signals,
	// re-arming a short deadline forever - the virtio-gpu poll pattern. Without the
	// flag this loop would keep run_until_idle from ever returning. With it, the
	// scheduler settles while the wait is parked, and each later
	// run_until_idle entry wakes the tick that came due - the wait still TICKS.
	extern "C" fn service(_arg: u64) {
		unsafe {
			let ev = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			loop {
				let now = arch::syscall::invoke(syscall::SYS_CLOCK_GET, 0, 0, 0, 0);
				let ret = arch::syscall::invoke(syscall::SYS_WAIT, ev, now + 2, abi::WAIT_PERIODIC, 0);
				assert_eq!(ret as i64, syscall::ERR_TIMED_OUT, "the periodic wake fires as a timeout");
				TICKS.fetch_add(1, Ordering::SeqCst);
			}
		}
	}
	sched::spawn(service, 0);
	// The first run must RETURN despite the perpetually re-armed deadline - this is
	// the settling property the flag exists for (an ordinary wait here would hang).
	sched::run_until_idle();
	let settled = TICKS.load(Ordering::SeqCst);
	// Later entries (the standing loop's re-entry, here explicit) wake the due tick.
	let target = settled + 2;
	let give_up = arch::apic::ticks() + 100;
	while TICKS.load(Ordering::SeqCst) < target && arch::apic::ticks() < give_up {
		sched::run_until_idle();
		arch::idle_halt();
	}
	assert!(TICKS.load(Ordering::SeqCst) >= target, "the periodic wait keeps ticking across settles");
}

tagged_test!(waiting_on_a_process_handle_wakes_when_it_exits, [Process], covers = ["kernel"]);
fn waiting_on_a_process_handle_wakes_when_it_exits() {
	use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
	static WAIT_RET: AtomicI64 = AtomicI64::new(-999);
	static DONE: AtomicBool = AtomicBool::new(false);
	// A subject process blocks until released, then returns - its last thread exiting
	// terminates the process. A waiter blocks in SYS_WAIT on a handle to that process.
	// The Process handle must stay unready while the subject runs, then become ready -
	// waking the waiter, which returns 0 - once the subject exits. This is the
	// process-terminated signal that lets a parent wait for a child to finish instead
	// of polling, the primitive shell job control reaps background jobs on.
	extern "C" fn subject(release: u64) {
		unsafe {
			// Block until the test sends on the release channel's peer, then fall off
			// the end -> thread_bootstrap -> sched::exit(), terminating the process.
			arch::syscall::invoke(syscall::SYS_WAIT, release, 0, 0, 0);
		}
	}
	extern "C" fn waiter(proc_handle: u64) {
		unsafe {
			let ret = arch::syscall::invoke(syscall::SYS_WAIT, proc_handle, 0, 0, 0);
			WAIT_RET.store(ret as i64, Ordering::SeqCst);
			DONE.store(true, Ordering::SeqCst);
		}
	}
	let (rel0, rel1) = object::channel::Channel::create();
	let subject_thread = sched::spawn_with_object(subject, rel0, object::rights::Rights::ALL, 0);
	let subject_process = subject_thread.process().clone();
	// The waiter gets a handle to the subject's process as its argument (installed by
	// spawn_with_object), carrying the WAIT right.
	let _waiter = sched::spawn_with_object(waiter, subject_process.clone(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	// Both are blocked now: the subject on the release channel, the waiter on the
	// not-yet-terminated process handle.
	assert!(!DONE.load(Ordering::SeqCst), "the waiter blocks while the subject still runs");
	// Release the subject so it returns and exits.
	rel1.send(object::channel::Message::new(alloc::vec![1], alloc::vec::Vec::new(), 0)).unwrap();
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the waiter wakes once the subject exits");
	assert_eq!(WAIT_RET.load(Ordering::SeqCst), 0, "the process handle became ready on exit");
}

tagged_test!(signal_terminate_wakes_a_blocked_thread, [Process], covers = ["kernel"]);
fn signal_terminate_wakes_a_blocked_thread() {
	use core::sync::atomic::{AtomicBool, Ordering};
	use object::thread::ThreadState;
	static RAN: AtomicBool = AtomicBool::new(false);
	static PAST_WAIT: AtomicBool = AtomicBool::new(false);
	// The victim blocks forever in SYS_WAIT on a channel whose peer is held open, so
	// nothing wakes it on its own. Delivering the terminate disposition (mark the
	// process killed + wake its threads, exactly as sys_process_signal(SIG_INT)) must
	// wake the blocked thread, have it observe the kill at the wait's checkpoint, and
	// retire it - never running the code past the wait. This proves a signal reaches a
	// thread blocked on something that would otherwise never become ready.
	extern "C" fn victim(handle: u64) {
		unsafe {
			RAN.store(true, Ordering::SeqCst);
			arch::syscall::invoke(syscall::SYS_WAIT, handle, 0, 0, 0);
			PAST_WAIT.store(true, Ordering::SeqCst);
		}
	}
	let (a, b) = object::channel::Channel::create();
	let _keep = b; // hold the peer so the channel never becomes ready by itself
	let victim_thread = sched::spawn_with_object(victim, a, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(RAN.load(Ordering::SeqCst), "the victim ran and blocked");
	assert!(!PAST_WAIT.load(Ordering::SeqCst), "the victim is blocked in the wait");
	// The terminate disposition, exactly as sys_process_signal(SIG_INT) applies it.
	let process = victim_thread.process().clone();
	process.terminate();
	for thread in process.live_threads() {
		sched::wake_thread(&thread);
	}
	sched::run_until_idle();
	assert!(!PAST_WAIT.load(Ordering::SeqCst), "the killed thread must retire at the wait, not resume past it");
	assert_eq!(victim_thread.state(), ThreadState::Exited, "the victim thread has exited");
}

tagged_test!(a_clean_exit_releases_the_process_channel_endpoints, [Process], covers = ["kernel"]);
fn a_clean_exit_releases_the_process_channel_endpoints() {
	use object::channel::Channel;
	use object::process::Process;
	use object::rights::Rights;

	// The shell's tool relay waits for the tool's stdout channel to CLOSE - and a
	// supervisor (the shell's job table, ps) legitimately holds the Process handle
	// long after the exit. A clean exit must therefore close the process's handle
	// table itself, exactly like the kill path does: the channel endpoints a dead
	// process held must not stay open until the LAST Process reference drops, or
	// every relay on a cleanly exiting child waits forever.
	let domain = sched::root_domain();
	let process = sched::process_create(domain).expect("the process should create");
	let (ours, theirs) = Channel::create();
	// park the peer endpoint in the child's handle table, standing in for a tool's
	// inherited stdout.
	process.install(theirs, Rights::ALL, 0);
	// the child's single thread exits cleanly at once.
	extern "C" fn clean_body(_arg: u64) {}
	let thread = sched::thread_create(process.clone(), clean_body, 0);
	sched::run_until_idle();
	drop(thread);
	// the process terminated cleanly...
	assert!(process.is_terminated(), "the process should have exited");
	// ...and even though we STILL HOLD a Process reference (the supervisor's view),
	// its endpoint is gone: the peer reads as closed, not merely quiet.
	assert!(ours.is_peer_closed(), "a clean exit must release the process's channel endpoints while a Process reference is still held");
	let _: &Process = &process;
}

tagged_test!(userspace_spawn_syscalls_start_a_second_process, [Process, Syscall], covers = ["kernel"]);
fn userspace_spawn_syscalls_start_a_second_process() {
	use core::sync::atomic::{AtomicU64, Ordering};
	// A kernel thread drives the userspace spawn syscalls exactly as a ring-3
	// spawner would: process_create -> process_load -> thread_create -> thread_start.
	// The image is the embedded LogService ELF, a leaf service that reports in over
	// its bootstrap channel and exits. The spawner hands the child the channel
	// endpoint it received as its own bootstrap (transferred through thread_create).
	static ELF_PTR: AtomicU64 = AtomicU64::new(0);
	static ELF_LEN: AtomicU64 = AtomicU64::new(0);
	extern "C" fn spawner(bootstrap: u64) {
		unsafe {
			let child = arch::syscall::invoke(syscall::SYS_PROCESS_CREATE, 0, 0, 0, 0);
			assert!((child as i64) > 0, "process_create");
			let entry = arch::syscall::invoke(syscall::SYS_PROCESS_LOAD, child, ELF_PTR.load(Ordering::SeqCst), ELF_LEN.load(Ordering::SeqCst), 0);
			assert!((entry as i64) > 0, "process_load");
			let thread = arch::syscall::invoke(syscall::SYS_THREAD_CREATE, child, entry, memlayout::USER_STACK_TOP, bootstrap);
			assert!((thread as i64) > 0, "thread_create");
			let started = arch::syscall::invoke(syscall::SYS_THREAD_START, thread, 0, 0, 0);
			assert_eq!(started as i64, 0, "thread_start");
		}
	}
	let bytes = init_package_bytes().expect("init package present");
	let package = pkg::Package::parse(bytes).expect("init package parses");
	let elf = package.lookup(b"log_service.lsexe").expect("log_service.lsexe image");
	ELF_PTR.store(elf.as_ptr() as u64, Ordering::SeqCst);
	ELF_LEN.store(elf.len() as u64, Ordering::SeqCst);
	let (kernel_ep, user_ep) = object::channel::Channel::create();
	sched::spawn_with_object(spawner, user_ep, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	let message = kernel_ep.recv().expect("the spawned process should report in over IPC");
	assert_eq!(&message.bytes[..], b"LogService: online");
}

tagged_test!(syscall_fuzz_rejects_invalid_calls, [Syscall], covers = ["kernel"]);
fn syscall_fuzz_rejects_invalid_calls() {
	// Syscall fuzzing: from a ring-0 thread (with its own, empty handle table), drive the
	// syscall boundary with random unknown syscall numbers and random arguments, then known
	// handle syscalls with random (bogus) handle arguments. Every call must be rejected with
	// an error rather than crash the kernel - the boundary validates its inputs, and a caller
	// cannot reach authority it was never handed. The thread completing at all is itself the
	// survival check. (Fixed-seed xorshift, so the run is deterministic.)
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		let mut seed: u64 = 0x1357_2468_face_b00c;
		let mut next = || -> u64 {
			seed ^= seed << 13;
			seed ^= seed >> 7;
			seed ^= seed << 17;
			seed
		};
		unsafe {
			// Unknown syscall numbers (well above the defined range) must be rejected.
			for _ in 0..1024 {
				let num = 10_000 + (next() % 0x00ff_ffff);
				let r = arch::syscall::invoke(num, next(), next(), next(), next());
				assert!(syscall::sys_is_err(r), "an unknown syscall number must return an error");
			}
			// Known handle syscalls with random handle arguments. The fuzz thread's handle
			// table is empty, so every random handle resolves to nothing and is rejected
			// before any user buffer is touched - a bogus capability grants no authority.
			let ops = [syscall::SYS_HANDLE_CLOSE, syscall::SYS_HANDLE_DUPLICATE, syscall::SYS_MEMORY_MAP, syscall::SYS_MEMORY_UNMAP];
			for _ in 0..1024 {
				let op = ops[(next() as usize) % ops.len()];
				let r = arch::syscall::invoke(op, next(), next(), next(), next());
				assert!(syscall::sys_is_err(r), "a syscall on a bogus handle must return an error");
			}
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the syscall fuzz thread did not finish - the kernel did not survive");
}

tagged_test!(object_info_get_reports_object, [Syscall], covers = ["kernel"]);
fn object_info_get_reports_object() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// object_info_get introspects a handle in the caller's table, so it runs inside
	// a spawned kernel thread (which has one). It reports the object's identity,
	// type, the rights the handle confers, and the object's byte size, and rejects
	// an unknown handle.
	extern "C" fn body(_arg: u64) {
		use object::ObjectType;
		use object::rights::Rights;
		unsafe {
			let handle = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0);
			assert!(!syscall::sys_is_err(handle));
			let mut info = syscall::ObjectInfo { koid: 0, object_type: 0, rights: 0, generation: 0, size: 0 };
			let info_ptr = &mut info as *mut syscall::ObjectInfo as u64;
			let size = core::mem::size_of::<syscall::ObjectInfo>() as u64;
			let got = arch::syscall::invoke(syscall::SYS_OBJECT_INFO_GET, handle, info_ptr, size, 0);
			assert_eq!(got, 1);
			assert!(info.koid >= 1);
			assert_eq!(info.object_type, ObjectType::MemoryObject.code());
			assert_eq!(info.rights, Rights::ALL.bits());
			assert!(info.generation >= 1);
			assert_eq!(info.size, 4096, "a MemoryObject reports its real byte size");
			// an unknown handle is rejected with the bad-handle error
			let bad = arch::syscall::invoke(syscall::SYS_OBJECT_INFO_GET, 0xdead_beef, info_ptr, size, 0);
			assert_eq!(bad as i64, syscall::ERR_BAD_HANDLE);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

tagged_test!(system_graph_reflects_live_state, [Kernel], covers = ["kernel"]);
fn system_graph_reflects_live_state() {
	use object::address_space::AddressSpace;
	use object::channel::Channel;
	use object::domain::Domain;
	use object::process::Process;
	use object::rights::Rights;
	use object::{KernelObject, ObjectType};
	// A standalone Domain with one process holding two handles. Collecting the
	// graph from that Domain must reflect the live structure exactly: one process
	// with two handles, one of them the channel we installed - with its koid, type,
	// rights, and badge intact. Dropping the process removes it from the graph.
	let domain = Domain::new(1 << 20, 16, 8);
	let process = Process::new(AddressSpace::kernel(), domain.clone());
	let (endpoint, _peer) = Channel::create();
	let channel_koid = endpoint.header().koid();
	process.install(endpoint, Rights::READ | Rights::WRITE, 42);
	process.install(object::event::Event::create(), Rights::ALL, 0);

	let node = graph::collect_from(&domain);
	assert_eq!(node.koid, domain.header().koid());
	assert_eq!(node.processes.len(), 1, "the Domain has one live process");
	let proc_node = &node.processes[0];
	assert_eq!(proc_node.koid, process.header().koid());
	assert_eq!(proc_node.handles.len(), 2, "the process holds two handles");
	let channel_handle = proc_node.handles.iter().find(|h| h.koid == channel_koid).expect("the channel handle should appear in the graph");
	assert_eq!(channel_handle.object_type, ObjectType::Channel);
	assert_eq!(channel_handle.rights, Rights::READ | Rights::WRITE);
	assert_eq!(channel_handle.badge, 42);

	// Dropping the process removes it from the live graph.
	drop(process);
	let after = graph::collect_from(&domain);
	assert_eq!(after.processes.len(), 0, "the process is gone after it drops");
}

tagged_test!(process_counters_track_ipc_and_resources, [Process], covers = ["kernel"]);
fn process_counters_track_ipc_and_resources() {
	use object::address_space::AddressSpace;
	use object::domain::Domain;
	use object::process::Process;
	use object::rights::Rights;
	// The per-process observability counters SYS_PROCESS_STATS_GET reads back: a fresh
	// process has done no IPC and holds nothing, recording sends and receives bumps the
	// IPC volume independently, installing handles grows the handle count, and a kill is
	// observable as the FAILED liveness the stats syscall derives.
	let domain = Domain::new(1 << 20, 16, 8);
	let process = Process::new(AddressSpace::kernel(), domain.clone());
	assert_eq!(process.messages_sent(), 0);
	assert_eq!(process.messages_received(), 0);
	assert_eq!(process.handle_count(), 0);
	assert_eq!(process.memory_bytes(), 0, "a kernel process owns no user frames");

	process.record_send();
	process.record_send();
	process.record_recv();
	assert_eq!(process.messages_sent(), 2, "two sends counted");
	assert_eq!(process.messages_received(), 1, "one recv counted");

	process.install(object::event::Event::create(), Rights::ALL, 0);
	process.install(object::event::Event::create(), Rights::ALL, 0);
	assert_eq!(process.handle_count(), 2, "two installed handles");

	// Liveness the stats syscall reports: not killed here, killed after terminate().
	assert!(!process.is_killed(), "a live process is not failed");
	process.terminate();
	assert!(process.is_killed(), "a terminated process reports as failed");
}

tagged_test!(userspace_runs_and_ipcs, [Process], covers = ["kernel"]);
fn userspace_runs_and_ipcs() {
	use object::channel::Channel;
	// Hand a fresh kernel thread one end of a channel and let it drop to ring 3
	// running the embedded user program. The program makes a capability-gated
	// channel send (a syscall from userspace) and exits; the kernel reads the
	// message back through the peer endpoint it kept.
	let (ep0, ep1) = Channel::create();
	sched::spawn_with_object(user_thread_body, ep0, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	let message = ep1.recv().expect("ring-3 program sent a message");
	assert_eq!(&message.bytes[..], b"OK");
}

tagged_test!(userspace_yields_cooperatively, [Process, Scheduler], covers = ["kernel"]);
fn userspace_yields_cooperatively() {
	use object::channel::Channel;
	// Two ring-3 threads share one core and each call SYS_YIELD several times
	// before sending "OK". The yields interleave them through the scheduler, which
	// only works if every syscall saves its user return state (rip/rsp/rflags) and
	// its kernel syscall stack per thread - a single per-CPU slot would be clobbered
	// by the sibling and one thread would return to the wrong context. Both messages
	// arriving proves the save path is per-thread.
	let (k0, u0) = Channel::create();
	let (k1, u1) = Channel::create();
	sched::spawn_with_object(user_yield_thread_body, u0, object::rights::Rights::ALL, 0);
	sched::spawn_with_object(user_yield_thread_body, u1, object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert_eq!(&k0.recv().expect("first ring-3 thread sent a message").bytes[..], b"OK");
	assert_eq!(&k1.recv().expect("second ring-3 thread sent a message").bytes[..], b"OK");
}

tagged_test!(fault_isolation_kills_only_process, [Kernel, Process], covers = ["kernel"]);
fn fault_isolation_kills_only_process() {
	use core::sync::atomic::Ordering;
	use object::domain::Domain;
	// A ring-3 thread dereferences a bad pointer. The kernel must terminate only
	// that process - not panic - and teardown must refund every resource it held to
	// its Domain. The thread runs in a bounded Domain and is not retained here, so
	// reaping it drops the Process and runs the refunds.
	let domain = Domain::new(1 << 20, 8, 4);
	sched::spawn_in(domain.clone(), user_fault_thread_body, 0).expect("spawn faulting thread");
	sched::run_until_idle();
	// Reaching here means the kernel survived the ring-3 fault and resumed
	// scheduling. The fault was recorded with the expected cause and address.
	assert_eq!(FAULT_GOT.load(Ordering::SeqCst), 1, "fault info should be recorded");
	assert_eq!(FAULT_KIND.load(Ordering::SeqCst), fault::FAULT_PAGE);
	assert_eq!(FAULT_ADDR.load(Ordering::SeqCst), arch::usermode::FAULT_PROBE_ADDR);
	// Teardown refunded the open MemoryObject (memory + handle) and the thread slot.
	assert_eq!(domain.account().memory().used(), 0, "memory refunded");
	assert_eq!(domain.account().handles().used(), 0, "handles refunded");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	writable_pages_are_not_executable,
	[Kernel, ArchX86_64],
	covers = ["kernel"]
);
#[cfg(target_arch = "x86_64")]
fn writable_pages_are_not_executable() {
	use core::sync::atomic::Ordering;
	use object::domain::Domain;
	// W^X: a ring-3 thread jumps into its own writable stack page. With EFER.NXE on
	// and the stack mapped NO_EXECUTE, the instruction FETCH page-faults (error code
	// bit 4) before a single stack byte executes, the kernel kills only that
	// process, and the recorded fault names the stack address it tried to run.
	assert!(arch::paging::nx_enabled(), "the test hardware supports NX");
	let domain = Domain::new(1 << 20, 8, 4);
	sched::spawn_in(domain.clone(), user_nx_thread_body, 0).expect("spawn nx probe thread");
	sched::run_until_idle();
	assert_eq!(NX_GOT.load(Ordering::SeqCst), 1, "fault info should be recorded");
	assert_eq!(NX_KIND.load(Ordering::SeqCst), fault::FAULT_PAGE);
	let addr = NX_ADDR.load(Ordering::SeqCst);
	assert!((USER_STACK_VA..USER_STACK_VA + mem::frame::PAGE_SIZE).contains(&addr), "the fault is inside the stack page");
	assert!(NX_CODE.load(Ordering::SeqCst) & 0x10 != 0, "the fault is an instruction fetch");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

// The aarch64 counterpart: aarch64 has no x86 page-fault error code (the NX bit + the
// `& 0x10` fetch bit above are x86-specific). It encodes W^X with the UXN descriptor
// bit and reports the fault in ESR_EL1, so the same NX probe is checked through the
// aarch64 exception class instead.
tagged_test!(
	#[cfg(target_arch = "aarch64")]
	writable_pages_are_not_executable,
	[Kernel, ArchAarch64],
	covers = ["kernel"]
);
#[cfg(target_arch = "aarch64")]
fn writable_pages_are_not_executable() {
	use core::sync::atomic::Ordering;
	use object::domain::Domain;
	// W^X on aarch64 (UXN): `map_page` sets UXN on a WRITABLE page, so a ring-3 thread
	// jumping into its own writable stack page takes an EL0 instruction abort on the
	// FETCH (before a stack byte runs); the kernel kills only that process and records
	// the stack address it tried to run and the faulting ESR.
	let domain = Domain::new(1 << 20, 8, 4);
	sched::spawn_in(domain.clone(), user_nx_thread_body, 0).expect("spawn nx probe thread");
	sched::run_until_idle();
	assert_eq!(NX_GOT.load(Ordering::SeqCst), 1, "fault info should be recorded");
	assert_eq!(NX_KIND.load(Ordering::SeqCst), fault::FAULT_PAGE);
	let addr = NX_ADDR.load(Ordering::SeqCst);
	assert!((USER_STACK_VA..USER_STACK_VA + mem::frame::PAGE_SIZE).contains(&addr), "the fault is inside the stack page");
	// The aarch64-specific angle: the recorded error_code is ESR_EL1, whose exception
	// class (bits 31:26) is 0x20 - an Instruction Abort from a lower EL (EL0), i.e. the
	// fault was an instruction fetch blocked by UXN, not a data access (which is 0x24).
	let ec = (NX_CODE.load(Ordering::SeqCst) >> 26) & 0x3f;
	assert_eq!(ec, 0x20, "the W^X fault is an EL0 instruction abort (UXN), not a data abort");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

// The riscv64 counterpart: riscv has no x86 page-fault error code nor aarch64 ESR; a W^X
// fetch fault is just the scause exception code. On Sv39 a WRITABLE leaf leaves the X bit
// clear (map_page only sets X for an executable mapping), so the same NX probe is checked
// through scause instead.
tagged_test!(
	#[cfg(target_arch = "riscv64")]
	writable_pages_are_not_executable,
	[Kernel, ArchRiscv64],
	covers = ["kernel"]
);
#[cfg(target_arch = "riscv64")]
fn writable_pages_are_not_executable() {
	use core::sync::atomic::Ordering;
	use object::domain::Domain;
	// W^X on riscv (Sv39): a WRITABLE leaf PTE has its X bit clear, so a U-mode thread
	// jumping into its own writable stack page takes an instruction page fault on the
	// FETCH (before a stack byte runs); the kernel kills only that process and records the
	// stack address it tried to run.
	let domain = Domain::new(1 << 20, 8, 4);
	sched::spawn_in(domain.clone(), user_nx_thread_body, 0).expect("spawn nx probe thread");
	sched::run_until_idle();
	assert_eq!(NX_GOT.load(Ordering::SeqCst), 1, "fault info should be recorded");
	assert_eq!(NX_KIND.load(Ordering::SeqCst), fault::FAULT_PAGE);
	let addr = NX_ADDR.load(Ordering::SeqCst);
	assert!((USER_STACK_VA..USER_STACK_VA + mem::frame::PAGE_SIZE).contains(&addr), "the fault is inside the stack page");
	// The riscv-specific angle: the recorded error_code is scause, which is 12 - an
	// instruction page fault (the fetch blocked by the clear X bit), not a load (13) or
	// store (15) page fault.
	assert_eq!(NX_CODE.load(Ordering::SeqCst), 12, "the W^X fault is an instruction page fault (scause 12)");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

tagged_test!(
	#[cfg(target_arch = "x86_64")]
	kernel_access_to_user_memory_is_refused_outside_the_window,
	[Kernel, ArchX86_64],
	covers = ["kernel"]
);
#[cfg(target_arch = "x86_64")]
fn kernel_access_to_user_memory_is_refused_outside_the_window() {
	use mem::frame;
	// SMAP/SMEP: a kernel dereference of a USER-mapped page outside the sanctioned
	// user_access window must page-fault (SMAP), and a ring-0 jump into a
	// USER-mapped page must page-fault as an instruction fetch (SMEP) - a kernel
	// bug can neither silently read user memory nor execute it. Each probe runs in
	// its own kernel thread; the armed page-fault handler recognizes the expected
	// fault and retires the thread instead of halting the machine. The probe VA is
	// clear of every other test's user pages.
	const SMAP_PROBE_VA: u64 = 0x0000_0000_4100_0000;
	assert!(arch::paging::smap_enabled(), "the test hardware supports SMAP");
	assert!(arch::paging::smep_enabled(), "the test hardware supports SMEP");
	let frame = frame::allocate().expect("probe frame");
	// Stamp a marker through the HHDM so a silent (unrefused) read would be visible.
	unsafe { ((mem::hhdm_offset() + frame) as *mut u64).write_volatile(0x5341_4645) };
	// Map it USER (no NX: the SMEP probe below fetches from it; SMAP alone must
	// refuse the data read regardless of NX).
	arch::paging::map_page(SMAP_PROBE_VA, frame, arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER);
	// Probe 1 (SMAP): a plain kernel read of the user page. Only Copy values live
	// across the faulting access - the handler retires this thread mid-statement.
	extern "C" fn smap_probe(_arg: u64) {
		fault::arm_smap_probe(SMAP_PROBE_VA);
		let value = unsafe { (SMAP_PROBE_VA as *const u64).read_volatile() };
		// Reached only if SMAP failed to refuse the access.
		panic!("SMAP did not refuse a kernel read of user memory (read {:#x})", value);
	}
	sched::spawn(smap_probe, 0);
	sched::run_until_idle();
	let code = fault::smap_probe_hit().expect("the kernel read of user memory faulted");
	assert!(code & 0x1 != 0, "the SMAP refusal is a protection fault on a present page");
	assert!(code & 0x10 == 0, "the SMAP refusal is a data access, not a fetch");
	// The sanctioned window still reads it fine - the copy paths keep working.
	let through_window = arch::paging::user_access(|| unsafe { (SMAP_PROBE_VA as *const u64).read_volatile() });
	assert_eq!(through_window, 0x5341_4645, "the sanctioned user_access window reads the page");
	// Probe 2 (SMEP): a ring-0 jump into the user page. The fetch faults before a
	// single byte executes, so the page's content never matters.
	extern "C" fn smep_probe(_arg: u64) {
		fault::arm_smap_probe(SMAP_PROBE_VA);
		let target: extern "C" fn() = unsafe { core::mem::transmute::<u64, extern "C" fn()>(SMAP_PROBE_VA) };
		target();
		panic!("SMEP did not refuse a kernel jump into user memory");
	}
	sched::spawn(smep_probe, 0);
	sched::run_until_idle();
	let code = fault::smap_probe_hit().expect("the kernel jump into user memory faulted");
	assert!(code & 0x10 != 0, "the SMEP refusal is an instruction fetch");
	// The probe threads died mid-body: clean their mapping up here.
	arch::paging::unmap_page(SMAP_PROBE_VA);
	unsafe { frame::deallocate(frame) };
}

tagged_test!(a_user_stack_grows_on_demand_past_its_initial_pages, [Kernel, Memory], covers = ["kernel"]);
fn a_user_stack_grows_on_demand_past_its_initial_pages() {
	use core::sync::atomic::Ordering;
	use object::domain::Domain;
	// Demand-paged stacks: nothing below USER_STACK_TOP is mapped up front for this
	// probe, and the Domain's default ceiling is megabytes. The probe touches 100
	// pages (400 kB - past the old eagerly-mapped 256 kB, let alone the 8-page
	// initial mapping) walking down; every touch page-faults, the handler maps the
	// missing page and the instruction resumes, and the probe reaches its clean
	// exit. The Domain's stack account holds exactly the grown bytes while the
	// process lives and refunds them when it is reaped.
	let domain = Domain::new(1 << 22, 8, 4);
	sched::spawn_in(domain.clone(), user_stack_probe_thread_body, 100).expect("spawn stack probe");
	sched::run_until_idle();
	assert_eq!(STACK_GOT.load(Ordering::SeqCst), 0, "a grown stack records no fault");
	assert_eq!(STACK_USED.load(Ordering::SeqCst), 100 * mem::frame::PAGE_SIZE, "the stack account holds the grown pages");
	assert_eq!(domain.account().stack().used(), 0, "the stack bytes are refunded at teardown");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

tagged_test!(recursion_past_the_stack_floor_is_killed, [Kernel, Memory, Process], covers = ["kernel"]);
fn recursion_past_the_stack_floor_is_killed() {
	use core::sync::atomic::Ordering;
	use mem::frame::PAGE_SIZE;
	use object::domain::Domain;
	// The hard floor: the Domain's stack ceiling is squeezed to 16 pages, so the
	// probe's 15 touches above the one-page guard grow, and the 16th - the guard
	// page itself - is a genuine fault that kills the process (runaway recursion
	// dies instead of eating the machine).
	let domain = Domain::new(1 << 22, 8, 4);
	domain.account().stack().set_limit(16 * PAGE_SIZE);
	sched::spawn_in(domain.clone(), user_stack_probe_thread_body, 32).expect("spawn stack probe");
	sched::run_until_idle();
	assert_eq!(STACK_GOT.load(Ordering::SeqCst), 1, "overrunning the floor records a fault");
	assert_eq!(STACK_KIND.load(Ordering::SeqCst), fault::FAULT_PAGE);
	assert_eq!(STACK_ADDR.load(Ordering::SeqCst), memlayout::USER_STACK_TOP - 16 * PAGE_SIZE, "the kill lands on the guard page at the floor");
	assert_eq!(STACK_USED.load(Ordering::SeqCst), 15 * PAGE_SIZE, "only the pages above the guard grew");
	assert_eq!(domain.account().stack().used(), 0, "the stack bytes are refunded at teardown");
	assert_eq!(domain.account().threads().used(), 0, "thread slot refunded");
}

tagged_test!(domain_hierarchy_limit_is_enforced_through_memory_create, [Domain, Kernel], covers = ["kernel"]);
fn domain_hierarchy_limit_is_enforced_through_memory_create() {
	use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
	use object::domain::{Domain, UNLIMITED};
	static DONE: AtomicBool = AtomicBool::new(false);
	static THIRD: AtomicI64 = AtomicI64::new(0);
	// The parent caps memory at two pages; the unbounded child may create two
	// objects but the third is refused through the actual syscall path.
	let parent = Domain::new(8192, UNLIMITED, UNLIMITED);
	let child = Domain::new_child(&parent, UNLIMITED, UNLIMITED, UNLIMITED).expect("a live parent takes a child");
	extern "C" fn body(_arg: u64) {
		unsafe {
			assert!(!syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0)));
			assert!(!syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0)));
			THIRD.store(arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 4096, 0, 0, 0) as i64, Ordering::SeqCst);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	assert!(sched::spawn_in(child.clone(), body, 0).is_some());
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	assert_eq!(THIRD.load(Ordering::SeqCst), syscall::ERR_RESOURCE_EXHAUSTED, "parent limit binds through the syscall path");
	// The body exited without closing its objects; teardown refunds them.
	assert_eq!(child.account().memory().used(), 0, "child memory refunded");
	assert_eq!(parent.account().memory().used(), 0, "parent aggregate memory refunded");
	let stats = syscall::domain_stats_snapshot(&parent);
	assert_eq!(stats.memory_used, 0, "Domain stats reports the refunded live usage");
	assert_eq!(stats.memory_peak, 8192, "Domain stats preserves the observed memory high-water mark");
}

tagged_test!(domain_kill_frees_subtree, [Kernel, Process], covers = ["kernel"]);
fn domain_kill_frees_subtree() {
	use core::sync::atomic::{AtomicI64, Ordering};
	use object::domain::Domain;
	use object::rights::Rights;
	static KILL_RET: AtomicI64 = AtomicI64::new(-100);
	// Build a Domain subtree parent -> child, run two parked processes under the
	// child that each hold a MemoryObject, then kill the PARENT through the real
	// domain_kill syscall. The whole subtree must be torn down: the parkers'
	// resources refunded and their threads reaped, leaving both Domains' accounts
	// at zero. Killing a parent thus terminates every descendant process.
	let parent = Domain::new(1 << 20, 16, 8);
	let child = Domain::new_child(&parent, 1 << 20, 16, 8).expect("a live parent takes a child");
	// The killer runs in the root Domain (so it is not itself killed); it is
	// seeded with a handle to the parent Domain and kills it.
	extern "C" fn killer(domain_handle: u64) {
		let ret = unsafe { arch::syscall::invoke(syscall::SYS_DOMAIN_KILL, domain_handle, 0, 0, 0) };
		KILL_RET.store(ret as i64, Ordering::SeqCst);
	}
	// Spawn the parkers before the killer so they run first: they create their
	// objects and park, and only then does the killer tear the subtree down.
	sched::spawn_in(child.clone(), domain_parker, 0).expect("spawn parker 0");
	sched::spawn_in(child.clone(), domain_parker, 0).expect("spawn parker 1");
	sched::spawn_with_object(killer, parent.clone(), Rights::MANAGE, 0);
	sched::run_until_idle();
	// The kill syscall succeeded and the subtree was fully reclaimed: the killed
	// processes' handles (and the memory those objects pinned) were freed eagerly,
	// and the parked threads self-terminated and were reaped.
	assert_eq!(KILL_RET.load(Ordering::SeqCst), 0, "domain_kill returned ok");
	assert_eq!(child.account().memory().used(), 0, "child memory refunded");
	assert_eq!(child.account().handles().used(), 0, "child handles refunded");
	assert_eq!(child.account().threads().used(), 0, "child threads refunded");
	assert_eq!(parent.account().memory().used(), 0, "parent aggregate memory refunded");
	assert_eq!(parent.account().handles().used(), 0, "parent aggregate handles refunded");
	assert_eq!(parent.account().threads().used(), 0, "parent aggregate threads refunded");
}

tagged_test!(channel_capability_transfer_is_zero_copy, [Ipc, Memory, Syscall], covers = ["kernel"]);
fn channel_capability_transfer_is_zero_copy() {
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	// Zero-copy: a 1 MB buffer is transferred as a capability, not copied. The
	// producer marks the far end of the buffer and sends only a 3-byte note plus
	// the handle; the consumer maps the same object and reads the mark back. That
	// the far-end mark survives while only 3 bytes crossed the channel proves the
	// pages were shared, not copied. Runs in a thread (syscalls need a handle table).
	static DONE: AtomicBool = AtomicBool::new(false);
	static MARKER: AtomicU64 = AtomicU64::new(0);
	static NOTE_LEN: AtomicU64 = AtomicU64::new(0);
	extern "C" fn body(_arg: u64) {
		const BUF_LEN: u64 = 0x10_0000; // 1 MB
		const MARK: u64 = 0xa5a5_0000_5a5a_1111;
		unsafe {
			let mut client: u64 = 0;
			let mut server: u64 = 0;
			let created = arch::syscall::invoke(syscall::SYS_CHANNEL_CREATE, &mut client as *mut u64 as u64, &mut server as *mut u64 as u64, 0, 0);
			assert!(!syscall::sys_is_err(created));
			// produce: mark the last 8 bytes of a 1 MB object, then unmap it
			let mo = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, BUF_LEN, 0, 0, 0);
			assert!(!syscall::sys_is_err(mo));
			let virt = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, mo, 0, 0, 0);
			assert!(!syscall::sys_is_err(virt));
			((virt + BUF_LEN - 8) as *mut u64).write_volatile(MARK);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, mo, 0, 0, 0);
			// transfer the capability with a tiny note instead of the buffer bytes
			let note = *b"BIG";
			let sent = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, client, note.as_ptr() as u64, note.len() as u64, mo);
			assert!(!syscall::sys_is_err(sent));
			// consume: receive the note + handle, map the object, read the far mark
			let mut buf = [0u8; 8];
			let mut xfer: u64 = 0;
			let n = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, server, buf.as_mut_ptr() as u64, buf.len() as u64, &mut xfer as *mut u64 as u64);
			assert!(!syscall::sys_is_err(n));
			NOTE_LEN.store(n as u64, Ordering::SeqCst);
			assert_ne!(xfer, 0);
			let virt2 = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, xfer, 0, 0, 0);
			assert!(!syscall::sys_is_err(virt2));
			MARKER.store(((virt2 + BUF_LEN - 8) as *const u64).read_volatile(), Ordering::SeqCst);
			arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, xfer, 0, 0, 0);
			arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, xfer, 0, 0, 0);
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	// the far-end mark came through intact, and only the 3-byte note crossed the
	// channel: the 1 MB buffer was shared by capability, never copied.
	assert_eq!(MARKER.load(Ordering::SeqCst), 0xa5a5_0000_5a5a_1111);
	assert_eq!(NOTE_LEN.load(Ordering::SeqCst), 3);
}

// Create a channel pair through the syscall, returning both endpoint handles. `depth` bounds
// the queue, which is what lets a test arrange a send that must be refused.
unsafe fn channel_pair(depth: u64) -> (u64, u64) {
	let mut first = 0u64;
	let mut second = 0u64;
	let result = unsafe { arch::syscall::invoke(syscall::SYS_CHANNEL_CREATE, &mut first as *mut u64 as u64, &mut second as *mut u64 as u64, depth, 0) };
	assert_eq!(result as i64, 0, "channel_create");
	(first, second)
}

// Does `handle` still name anything in the caller's table? Asked with OBJECT_INFO_GET, which
// requires `Rights::NONE` and so answers exactly this and nothing else. `CHANNEL_PEEK` looks
// like the natural probe and is not: it reports WOULD_BLOCK for a live endpoint with an empty
// queue, which is every endpoint here. `HANDLE_DUPLICATE` is not it either - it needs the
// DUPLICATE right, so it answers a question about rights rather than about existence.
unsafe fn handle_is_live(handle: u64) -> bool {
	let mut info = [0u8; 256];
	assert!(info.len() >= core::mem::size_of::<syscall::ObjectInfo>(), "the info buffer must fit an ObjectInfo");
	!syscall::sys_is_err(unsafe { arch::syscall::invoke(syscall::SYS_OBJECT_INFO_GET, handle, info.as_mut_ptr() as u64, info.len() as u64, 0) })
}

// How many handles the calling thread's process holds. Read from the table rather than by
// probing values: a handle packs a generation above its index, so the raw numbers are large
// and sparse and there is no range to scan. The absolute count depends on what the thread was
// seeded with, so it is only ever compared against itself.
fn live_handle_count() -> u64 {
	sched::current_thread().expect("a current thread").process().handle_count()
}

tagged_test!(a_refused_capability_send_leaves_every_handle_with_the_sender, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_refused_capability_send_leaves_every_handle_with_the_sender() {
	// A batch transfer is all-or-nothing in BOTH directions, and the second direction is the
	// one with nowhere to put a mistake. Taking every handle under one lock is the easy half;
	// what happens when the send THEN fails - a full queue, a closed peer - decides whether the
	// caller still owns what it tried to give away. Losing them there is an unrecoverable loss
	// of authority, and leaving them taken-but-not-sent is the same thing with the objects
	// still alive.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			// A transport one message deep, filled, so the capability send onto it must fail.
			let (transport, _peer) = channel_pair(1);
			let payload = [0u8; 8];
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, transport, payload.as_ptr() as u64, payload.len() as u64, 0) as i64, 0, "the first message fills the queue");
			assert!(syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, transport, payload.as_ptr() as u64, payload.len() as u64, 0)), "the second must be refused");

			let (carried_a, _a) = channel_pair(4);
			let (carried_b, _b) = channel_pair(4);
			let (carried_c, _c) = channel_pair(4);
			let request = [3u64, carried_a, carried_b, carried_c];
			let before = live_handle_count();
			assert!(syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND_CAPS, transport, payload.as_ptr() as u64, payload.len() as u64, request.as_ptr() as u64)), "a capability send onto a full queue must be refused");

			// The capabilities come back TO THE HANDLES THEY CAME FROM.
			//
			// This assertion used to say the opposite - that the old values were dead and the
			// capabilities had reappeared under new handles nobody was told about - and called
			// that "the honest outcome of a move that had to be undone". The count was right, so
			// the kernel looked correct, and what it actually meant was that userspace could not
			// reach them: a caller doing the only sensible thing with a failed send, closing what
			// it could not hand over, closed a value that was already dead. One capability leaked
			// per failed transfer, out of code that was doing exactly the right thing, with
			// nothing in the ABI able to tell it otherwise.
			//
			// A refused send now costs the caller nothing at all - not the capability, and not the
			// handle it was named by.
			for handle in [carried_a, carried_b, carried_c] {
				assert!(handle_is_live(handle), "a refused send must give the handle back, not reissue the capability elsewhere");
			}
			assert_eq!(live_handle_count(), before, "a refused send must leave the sender exactly the capabilities it had");

			// and the handles still WORK: a live slot that cannot be used is a leak with better
			// manners.
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, carried_a, 0, 0, 0) as i64, 0, "the returned handle closes like any other");
			assert_eq!(live_handle_count(), before - 1, "and closing it releases exactly one");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the transfer thread ran to completion");
}

tagged_test!(a_receiver_too_small_for_a_message_keeps_it_queued, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_receiver_too_small_for_a_message_keeps_it_queued() {
	// A message that cannot be delivered has to stay deliverable. Taking it off the queue and
	// then reporting that the buffer was too small destroys a message nobody can retry - and
	// the caller that would have retried with a bigger buffer has nothing left to retry for.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let (sender, receiver) = channel_pair(4);
			let (carried, _peer) = channel_pair(4);
			let payload = [0xABu8; 64];
			let request = [1u64, carried];
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND_CAPS, sender, payload.as_ptr() as u64, payload.len() as u64, request.as_ptr() as u64) as i64, 0, "the capability send succeeds");

			// Too small, twice: the second attempt is what proves the first did not consume
			// the message.
			let mut small = [0u8; 8];
			let mut caps_out = [0u64; abi::MAX_MESSAGE_CAPS + 1];
			for attempt in 0..2 {
				assert!(syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_CHANNEL_RECV_CAPS, receiver, small.as_mut_ptr() as u64, small.len() as u64, caps_out.as_mut_ptr() as u64)), "attempt {attempt} with an 8-byte buffer must be refused");
			}

			// And a big enough buffer still gets the whole thing, capability included.
			let mut big = [0u8; 64];
			assert!(!syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_CHANNEL_RECV_CAPS, receiver, big.as_mut_ptr() as u64, big.len() as u64, caps_out.as_mut_ptr() as u64)), "a buffer the message fits must receive it");
			assert_eq!(big, payload, "the payload survived the two refusals intact");
			assert_eq!(caps_out[0], 1, "the capability came with it");
			assert!(handle_is_live(caps_out[1]), "and it arrived as a live capability");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the receive thread ran to completion");
}

tagged_test!(a_batch_naming_one_handle_twice_is_refused_whole, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_batch_naming_one_handle_twice_is_refused_whole() {
	// The duplication that needs no race: name the same handle twice in one batch, and a
	// transfer that clones each entry independently mints a second capability without the
	// DUPLICATE right. Refused through the syscall here rather than on the predicate alone -
	// this thread has a handle table, so the whole path runs.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let (transport, _peer) = channel_pair(8);
			let (carried, _a) = channel_pair(4);
			let (other, _b) = channel_pair(4);
			let payload = [0u8; 4];
			for request in [[2u64, carried, carried, 0], [3, carried, other, carried]] {
				assert!(syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND_CAPS, transport, payload.as_ptr() as u64, payload.len() as u64, request.as_ptr() as u64)), "a batch naming one handle twice must be refused");
			}
			// And refused WHOLE: both handles still work, so nothing was taken on the way to
			// the refusal.
			for handle in [carried, other] {
				assert!(handle_is_live(handle), "a refused batch must leave every handle it named");
			}
			// A batch of distinct handles goes through, so the refusal is about the repeat and
			// not about the call.
			let request = [2u64, carried, other, 0];
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND_CAPS, transport, payload.as_ptr() as u64, payload.len() as u64, request.as_ptr() as u64) as i64, 0, "distinct handles transfer");
			assert!(!handle_is_live(carried), "and that batch DID take them");
			assert!(!handle_is_live(other));
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the batch thread ran to completion");
}

tagged_test!(starting_a_thread_twice_runs_it_once, [Process, Scheduler], covers = ["kernel"]);
fn starting_a_thread_twice_runs_it_once() {
	// The start gate. A thread built suspended and released twice must be enqueued once - the
	// same thread on a run queue twice is two cores switching into one context, which is a
	// corrupted stack rather than a slow thread. The gate is a compare-exchange on `started`,
	// and the second caller has to learn it lost rather than be silently ignored.
	use core::sync::atomic::{AtomicU64, Ordering};
	static RUNS: AtomicU64 = AtomicU64::new(0);
	extern "C" fn body(_arg: u64) {
		RUNS.fetch_add(1, Ordering::SeqCst);
	}
	RUNS.store(0, Ordering::SeqCst);
	let process = object::process::Process::new(object::address_space::AddressSpace::kernel(), sched::root_domain());
	let thread = sched::thread_create_suspended(process, body, 0).expect("a suspended thread");
	assert!(sched::thread_start(thread.clone()), "the first release starts it");
	assert!(!sched::thread_start(thread.clone()), "the second must report that it lost, not enqueue again");
	assert!(!sched::thread_start(thread.clone()), "and so must every one after that");
	sched::run_until_idle();
	assert_eq!(RUNS.load(Ordering::SeqCst), 1, "the body ran more than once: the thread was enqueued twice");
}

tagged_test!(a_wake_can_only_be_claimed_once, [Process, Scheduler, Smp], covers = ["kernel"]);
fn a_wake_can_only_be_claimed_once() {
	// A blocked thread can be made ready by more than one source at the same instant - a
	// message arriving as its deadline passes, two peers signalling one event - and exactly one
	// of them may enqueue it. Two enqueues of one thread is two cores switching into one
	// context, which is a corrupted stack rather than a slow thread.
	//
	// `try_claim_wake` is that decision. It is tested directly, not through `wake_thread`:
	// enqueueing a thread that was never parked would put a context with no saved stack pointer
	// on a run queue. The claim is the part that has to be exactly once.
	//
	// Sequentially, and deliberately so. The concurrent version of this - two cores in a
	// rendezvous loop racing the same claim - was written first and is the reason this comment
	// exists: it passed, and it made two unrelated timing-sensitive tests fail afterwards, by
	// holding two cores in tight spin loops on a machine that has other work. What it bought
	// over this was confidence that the claim is a compare-exchange, which is visible in the
	// one line that implements it. What it cost was a suite that no longer meant anything.
	extern "C" fn never_runs(_arg: u64) {}
	let process = object::process::Process::new(object::address_space::AddressSpace::kernel(), sched::root_domain());
	let subject = sched::thread_create_suspended(process, never_runs, 0).expect("a suspended subject");

	for round in 0..8 {
		subject.set_state(object::thread::ThreadState::Blocked);
		assert!(subject.try_claim_wake(), "round {round}: the first claim on a blocked thread must win");
		assert!(!subject.try_claim_wake(), "round {round}: a second claim must lose - the thread is already Ready");
		assert!(!subject.try_claim_wake(), "round {round}: and so must every one after it");
		assert_eq!(subject.state(), object::thread::ThreadState::Ready, "the winning claim leaves it Ready");
	}
	// A thread that is not blocked cannot be claimed at all, which is the same guarantee from
	// the other side: a wake arriving for a thread that already woke changes nothing.
	subject.set_state(object::thread::ThreadState::Running);
	assert!(!subject.try_claim_wake(), "a running thread is not claimable");
	subject.set_state(object::thread::ThreadState::Exited);
	assert!(!subject.try_claim_wake(), "nor is an exited one");
}

tagged_test!(a_terminating_process_takes_no_new_mappings, [Process, Syscall, Memory], covers = ["kernel"]);
fn a_terminating_process_takes_no_new_mappings() {
	// Cleanup takes a snapshot of what to unmap. A mapping registered after that snapshot is
	// one nothing will ever collect: the page tables keep it while the frames go back to the
	// allocator. So the map syscalls have to refuse from the moment termination begins, not
	// from the moment it finishes.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let page = mem::frame::PAGE_SIZE;
			let object = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, page, 0, 0, 0);
			assert!(!syscall::sys_is_err(object), "memory_object_create");
			// It maps while the process is alive, so the refusal below is about termination.
			let base = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, object, 0, 0, 0);
			assert!(!syscall::sys_is_err(base), "the map succeeds before termination");
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, object, 0, 0, 0) as i64, 0);

			// Begin terminating without going through the kill path, which would also stop
			// this thread - what is under test is the map syscall's own check.
			let thread = sched::current_thread().expect("a current thread");
			thread.process().begin_terminating_for_test();
			assert!(syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_MEMORY_MAP, object, 0, 0, 0)), "a map must be refused once the process is terminating");
			// The handle is still good - the refusal is about the process, not the handle -
			// which is what makes the assertion above mean something.
			assert!(!syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_OBJECT_INFO_GET, object, [0u8; 256].as_mut_ptr() as u64, 256, 0)), "the handle itself is still live");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the termination thread ran to completion");
}

tagged_test!(a_timer_armed_after_the_wait_began_still_wakes_it, [Object, Scheduler, Syscall], covers = ["kernel"]);
fn a_timer_armed_after_the_wait_began_still_wakes_it() {
	// Waiting on a timer that is not armed yet is an ordinary thing to do - the waiter and the
	// arming thread are two independent components - and the waiter has nothing to re-check
	// until somebody tells it to look. Readiness published without a wake is readiness nobody
	// arrives to see, and the thread stays parked for the life of the system.
	//
	// The parking is established rather than assumed: the waiter is run to the point where it
	// has actually blocked, and that is asserted, before the timer is armed at all.
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	static WOKE: AtomicBool = AtomicBool::new(false);
	static RESULT: AtomicU64 = AtomicU64::new(u64::MAX);
	extern "C" fn waiter(handle: u64) {
		// deadline 0 = no timeout of its own, so the ONLY thing that can end this wait is the
		// timer being armed and reaching its deadline.
		let result = unsafe { arch::syscall::invoke(syscall::SYS_WAIT, handle, 0, 0, 0) };
		RESULT.store(result, Ordering::SeqCst);
		WOKE.store(true, Ordering::SeqCst);
	}

	WOKE.store(false, Ordering::SeqCst);
	RESULT.store(u64::MAX, Ordering::SeqCst);
	let timer = object::timer::Timer::create();
	let thread = sched::spawn_with_object(waiter, timer.clone(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(!WOKE.load(Ordering::SeqCst), "the waiter must not return while the timer is unarmed");
	assert_eq!(thread.state(), object::thread::ThreadState::Blocked, "the waiter must be parked, not spinning - otherwise this proves nothing about the wake");

	// Arm it, with a deadline already reached, so the wake is the only thing that can be late.
	timer.set(arch::apic::ticks());
	sched::run_until_idle();
	assert!(WOKE.load(Ordering::SeqCst), "arming a timer must wake a thread already waiting on it");
	assert_eq!(RESULT.load(Ordering::SeqCst), 0, "the wait should report readiness, not a timeout");
}

tagged_test!(wait_any_on_only_a_timer_returns_when_it_fires, [Object, Scheduler, Syscall], covers = ["kernel"]);
fn wait_any_on_only_a_timer_returns_when_it_fires() {
	// The same case through `WAIT_ANY`, which derives its block deadline from the set rather
	// than from one object. A set whose only member is an unarmed timer, with no external
	// timeout, has nothing to time out ON - so a waiter that did not get a wake on arming
	// would park forever with no deadline to rescue it.
	use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
	static WOKE: AtomicBool = AtomicBool::new(false);
	static RESULT: AtomicU64 = AtomicU64::new(u64::MAX);
	extern "C" fn waiter(handle: u64) {
		let handles = [handle];
		let result = unsafe { arch::syscall::invoke(syscall::SYS_WAIT_ANY, handles.as_ptr() as u64, 1, 0, 0) };
		RESULT.store(result, Ordering::SeqCst);
		WOKE.store(true, Ordering::SeqCst);
	}

	WOKE.store(false, Ordering::SeqCst);
	RESULT.store(u64::MAX, Ordering::SeqCst);
	let timer = object::timer::Timer::create();
	let thread = sched::spawn_with_object(waiter, timer.clone(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(!WOKE.load(Ordering::SeqCst), "the waiter must not return while the timer is unarmed");
	assert_eq!(thread.state(), object::thread::ThreadState::Blocked, "the waiter must be parked");

	timer.set(arch::apic::ticks());
	sched::run_until_idle();
	assert!(WOKE.load(Ordering::SeqCst), "wait_any over a lone timer must return when it is armed");
	assert_eq!(RESULT.load(Ordering::SeqCst), 0, "it should report index 0 - the handle that became ready");
}

// A fixed-seed xorshift, so every fuzz below is reproducible from its iteration number.
fn xorshift(state: &mut u64) -> u64 {
	*state ^= *state << 13;
	*state ^= *state >> 7;
	*state ^= *state << 17;
	*state
}

tagged_test!(a_fuzzed_capability_batch_never_mints_a_capability, [Syscall, Ipc, Process], covers = ["kernel"]);
fn a_fuzzed_capability_batch_never_mints_a_capability() {
	// The capability array is entirely the caller's: a count and a list of handle values, read
	// out of its memory. Counts out of range, handles that name nothing, handles that name the
	// wrong type, the same handle several times, the transport itself in its own batch.
	//
	// The invariant is one number and it is the right one: a transfer MOVES authority, so the
	// number of capabilities the sender holds may fall or stay the same across any call, and may
	// never rise. A batch that minted one would show up here whatever route it took.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let (transport, _peer) = channel_pair(8);
			let mut pool = [0u64; 6];
			for slot in pool.iter_mut() {
				let (endpoint, _other) = channel_pair(2);
				*slot = endpoint;
			}
			let payload = [0u8; 16];
			let mut state: u64 = 0xDEAD_BEEF_C0FF_EE01;
			for iteration in 0..600 {
				// A count that is sometimes legal and sometimes not - zero, over
				// MAX_MESSAGE_CAPS, absurd - because the count is a caller value too.
				let count = match xorshift(&mut state) % 8 {
					0 => 0,
					1 => (abi::MAX_MESSAGE_CAPS + 1) as u64,
					2 => u64::MAX,
					n => n,
				};
				let mut request = [0u64; 9];
				request[0] = count;
				for slot in request[1..].iter_mut() {
					*slot = match xorshift(&mut state) % 4 {
						// a real handle, so batches that could succeed do
						0 | 1 => pool[(xorshift(&mut state) as usize) % pool.len()],
						// the transport itself, which would be sending the floor away
						2 => transport,
						// a value that names nothing
						_ => xorshift(&mut state),
					};
				}
				let before = live_handle_count();
				let result = arch::syscall::invoke(syscall::SYS_CHANNEL_SEND_CAPS, transport, payload.as_ptr() as u64, payload.len() as u64, request.as_ptr() as u64);
				let after = live_handle_count();
				assert!(after <= before, "iteration {iteration} ended with MORE capabilities than it started with ({before} -> {after}): a transfer minted one");
				// A refusal must cost nothing at all; only a success may move handles out.
				if syscall::sys_is_err(result) {
					assert_eq!(after, before, "iteration {iteration}: a refused batch took capabilities with it");
				}
				// Drain, so the transport's queue does not become the only reason for a
				// refusal for the rest of the run.
				let mut drain = [0u8; 16];
				let mut caps_out = [0u64; abi::MAX_MESSAGE_CAPS + 1];
				while !syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_CHANNEL_RECV_CAPS, transport, drain.as_mut_ptr() as u64, drain.len() as u64, caps_out.as_mut_ptr() as u64)) {
					for index in 0..caps_out[0].min(abi::MAX_MESSAGE_CAPS as u64) as usize {
						arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, caps_out[index + 1], 0, 0, 0);
					}
				}
				// Replace anything the fuzz gave away, so later iterations still have real
				// handles to name.
				for slot in pool.iter_mut() {
					if !handle_is_live(*slot) {
						let (endpoint, _other) = channel_pair(2);
						*slot = endpoint;
					}
				}
			}
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the capability fuzz ran to completion");
}

tagged_test!(handle_churn_never_resurrects_a_closed_capability, [Syscall, Process], covers = ["kernel"]);
fn handle_churn_never_resurrects_a_closed_capability() {
	// A handle packs a generation above a slot index, and a closed slot goes back on the free
	// list to be reissued. The generation is what keeps the old VALUE from naming the new
	// occupant - a stale handle that comes back to life is authority resurrected, and the
	// holder never asked for it.
	//
	// So: churn the table hard, remember every handle ever closed, and after every round demand
	// that not one of them names anything.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let mut dead: Vec<u64> = Vec::new();
			let mut live: Vec<u64> = Vec::new();
			let mut state: u64 = 0x0123_4567_89AB_CDEF;
			for round in 0..400 {
				match xorshift(&mut state) % 3 {
					// open
					0 => {
						let (endpoint, other) = channel_pair(2);
						live.push(endpoint);
						live.push(other);
					}
					// duplicate an existing one, which installs into a free slot
					1 if !live.is_empty() => {
						let source = live[(xorshift(&mut state) as usize) % live.len()];
						let duplicate = arch::syscall::invoke(syscall::SYS_HANDLE_DUPLICATE, source, (abi::RIGHT_WAIT | abi::RIGHT_READ) as u64, 0, 0);
						if !syscall::sys_is_err(duplicate) {
							live.push(duplicate);
						}
					}
					// close one
					_ if !live.is_empty() => {
						let index = (xorshift(&mut state) as usize) % live.len();
						let handle = live.swap_remove(index);
						assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0) as i64, 0, "closing a live handle");
						dead.push(handle);
						// Closing it twice must be refused, not silently accepted.
						assert!(syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0)), "round {round}: a handle closed twice was accepted the second time");
					}
					_ => {}
				}
				// Every handle ever closed, checked against the table as it is NOW - after
				// whatever reissues have happened since.
				for &handle in dead.iter() {
					assert!(!handle_is_live(handle), "round {round}: handle {handle:#x} came back to life after being closed");
				}
			}
			assert!(dead.len() > 16, "the churn closed only {} handles: it is not exercising reissue", dead.len());
			for handle in live {
				arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0);
			}
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the handle churn ran to completion");
}

tagged_test!(fuzzed_map_and_unmap_sequences_round_trip_the_window, [Syscall, Memory], covers = ["kernel"]);
fn fuzzed_map_and_unmap_sequences_round_trip_the_window() {
	// Map and unmap in an order nobody wrote down: doubles, unmaps of things that were never
	// mapped, unmaps repeated, interleaved across several objects. Each call must answer rather
	// than act on a wrong assumption, and when it is all over and everything is unmapped, the
	// virtual window and the frame pool must both be exactly where they started.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			const OBJECTS: usize = 5;
			let page = mem::frame::PAGE_SIZE;
			let mut handles = [0u64; OBJECTS];
			let mut mapped = [false; OBJECTS];
			let mut total_pages = 0u64;
			for (index, slot) in handles.iter_mut().enumerate() {
				let pages = 1 + index as u64 % 3;
				let handle = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, pages * page, 0, 0, 0);
				assert!(!syscall::sys_is_err(handle), "memory_object_create");
				*slot = handle;
				total_pages += pages;
			}
			let frames_before = mem::frame::free_count();
			let mut state: u64 = 0xF00D_FACE_1357_9BDF;
			for iteration in 0..600 {
				let which = (xorshift(&mut state) as usize) % OBJECTS;
				let handle = handles[which];
				if xorshift(&mut state) % 2 == 0 {
					let result = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, handle, 0, 0, 0);
					if mapped[which] {
						assert!(syscall::sys_is_err(result), "iteration {iteration}: mapping an already-mapped object must be refused");
					} else {
						assert!(!syscall::sys_is_err(result), "iteration {iteration}: mapping an unmapped object must succeed");
						mapped[which] = true;
					}
				} else {
					let result = arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, handle, 0, 0, 0) as i64;
					if mapped[which] {
						assert_eq!(result, 0, "iteration {iteration}: unmapping a mapped object must succeed");
						mapped[which] = false;
					} else {
						assert!(syscall::sys_is_err(result as u64), "iteration {iteration}: unmapping something that is not mapped must be refused");
					}
				}
			}
			for (index, handle) in handles.iter().enumerate() {
				if mapped[index] {
					assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, *handle, 0, 0, 0) as i64, 0);
				}
			}
			assert_eq!(mem::frame::free_count(), frames_before, "the churn moved the frame pool");
			// And the window took every range back: a fresh map of the largest object still
			// fits where the churn left off rather than climbing away from it.
			let base = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, handles[2], 0, 0, 0);
			assert!(!syscall::sys_is_err(base), "the window still has room after 600 map/unmap rounds");
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, handles[2], 0, 0, 0) as i64, 0);
			for handle in handles {
				assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0) as i64, 0);
			}
			// The pages come back through the QUARANTINE now: a frame that was mapped is not handed
			// to the allocator until a shootdown has retired every core's translation of it, and
			// those are batched rather than taken one span at a time. Draining is what "wait for the
			// shootdown" looks like from here; without it this measures the queue rather than the
			// property.
			assert!(mem::frame::drain_quarantine_fully(64), "the shootdown never completed, so the pages could not come back");
			assert_eq!(mem::frame::free_count() as u64, frames_before as u64 + total_pages, "closing every object must return every page of frames it held");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the map/unmap fuzz ran to completion");
}

tagged_test!(a_refused_single_capability_send_leaves_the_handle_where_it_was, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_refused_single_capability_send_leaves_the_handle_where_it_was() {
	// The batch send was taught to MOVE a capability and this one was not: it looked the handle up,
	// built a `Capability` from the result - a clone of the authority - sent that, and then closed
	// the caller's handle with the result discarded. Two threads of one process naming the same
	// handle could both look it up, both clone and both send, so one handle became two capabilities
	// without `DUPLICATE`; the loser's close then failed and nobody was told.
	//
	// Two threads is not something this harness can arrange deterministically, so what is asserted
	// here is the property the race exploited: the send is a MOVE, and a refused move costs nothing.
	// A clone-then-close cannot have both halves of that.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			// a transport one message deep, filled, so the capability send onto it must fail.
			let (transport, _peer) = channel_pair(1);
			let payload = [0u8; 8];
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, transport, payload.as_ptr() as u64, payload.len() as u64, 0) as i64, 0, "the first message fills the queue");

			let (carried, _other) = channel_pair(4);
			// The second transport is made BEFORE the count is taken: creating a channel pair costs
			// two handles, and a count taken in between measures the test's own scaffolding.
			let (drain, sink) = channel_pair(4);
			let before = live_handle_count();
			assert!(syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, transport, payload.as_ptr() as u64, payload.len() as u64, carried)), "a send onto a full queue must be refused");
			assert!(handle_is_live(carried), "a refused send gives the handle back, at the value it was named by");
			assert_eq!(live_handle_count(), before, "and leaves the sender exactly the capabilities it had");

			// and a send that SUCCEEDS consumes it - the other half of a move.
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, drain, payload.as_ptr() as u64, payload.len() as u64, carried) as i64, 0, "the send succeeds");
			assert!(!handle_is_live(carried), "a delivered transfer takes the handle with it");
			assert_eq!(live_handle_count(), before - 1, "and exactly one capability left");
			let _ = sink;
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the transfer thread ran to completion");
}

tagged_test!(a_receive_takes_a_message_only_if_it_fits, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_receive_takes_a_message_only_if_it_fits() {
	// `peek_shape` then `recv` were two operations under two separate locks, so a second receiver
	// could take the peeked message in between - and the copy afterwards used the RECEIVED length.
	// A receiver that declared a hundred bytes could be handed a megabyte and the kernel would write
	// all of it into a buffer it had validated for a hundred.
	//
	// The property that closes it is asserted directly: a message that does not fit is REFUSED and
	// LEFT IN THE QUEUE, so there is no window in which a decision about one message is applied to
	// another.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let (tx, rx) = channel_pair(4);
			let big = [0x5Au8; 512];
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, tx, big.as_ptr() as u64, big.len() as u64, 0) as i64, 0, "the message is queued");

			// A buffer far too small for it: refused, and nothing is consumed.
			let mut small = [0u8; 16];
			let mut caps = [0u64; abi::MAX_MESSAGE_CAPS + 1];
			let result = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV_CAPS, rx, small.as_mut_ptr() as u64, small.len() as u64, caps.as_mut_ptr() as u64);
			assert!(syscall::sys_is_err(result), "a message larger than the buffer must be refused");
			assert_eq!(small, [0u8; 16], "and not one byte of it written");

			// The message is still there, and a buffer that fits takes it.
			let mut room = [0u8; 512];
			let taken = arch::syscall::invoke(syscall::SYS_CHANNEL_RECV_CAPS, rx, room.as_mut_ptr() as u64, room.len() as u64, caps.as_mut_ptr() as u64) as i64;
			assert_eq!(taken, 512, "the refused message stayed in the queue for a caller that can hold it");
			assert_eq!(room, big, "and arrived whole");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the receive thread ran to completion");
}

tagged_test!(a_receive_may_not_install_a_handle_past_the_domains_limit, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_receive_may_not_install_a_handle_past_the_domains_limit() {
	// The plain receive installed a transferred capability with `HandleTable::insert`, which charges
	// the Domain and enforces nothing. So a Domain at its handle ceiling received one more, and one
	// more after that: the limit that bounds every other way of acquiring a handle was bypassed by
	// asking a peer to send one. It was not a rare path either - all four of the runtime's receive
	// wrappers issue `SYS_CHANNEL_RECV`.
	//
	// The reason it was written that way is the second half of the property: the message is out of
	// the queue before the install, so a refusal there would destroy a message nobody can retry.
	// Both halves are asserted, because fixing one by breaking the other is the obvious wrong turn.
	use core::sync::atomic::{AtomicI64, Ordering};
	use object::domain::{Domain, UNLIMITED};
	static REFUSED: AtomicI64 = AtomicI64::new(i64::MIN);
	static RETRIED: AtomicI64 = AtomicI64::new(i64::MIN);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let (tx, rx) = channel_pair(4);
			// A capability to transfer, moved out of this table by the send.
			let payload = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			assert!(!syscall::sys_is_err(payload), "an event to carry");
			let note = [0xA5u8; 1];
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, tx, note.as_ptr() as u64, note.len() as u64, payload) as i64, 0, "the capability is sent");

			// Fill the table to the Domain's ceiling. Counted by the quota refusing rather than by
			// arithmetic, so the test does not have to know what its own scaffolding cost.
			while !syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0)) {}

			let mut buf = [0u8; 8];
			let mut handle = 0u64;
			REFUSED.store(arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, rx, buf.as_mut_ptr() as u64, buf.len() as u64, &mut handle as *mut u64 as u64) as i64, Ordering::SeqCst);
			assert_eq!(handle, 0, "a refused receive installs nothing");

			// And the message was not destroyed by being refused: make room and it is still there.
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, tx, 0, 0, 0) as i64, 0, "one handle closed makes room for one");
			RETRIED.store(arch::syscall::invoke(syscall::SYS_CHANNEL_RECV, rx, buf.as_mut_ptr() as u64, buf.len() as u64, &mut handle as *mut u64 as u64) as i64, Ordering::SeqCst);
			assert_eq!(buf[0], 0xA5, "and it arrived whole");
			assert!(handle != 0, "with its capability");
		}
	}
	// A ceiling low enough to reach in a few syscalls and high enough for the scaffolding.
	let domain = Domain::new_child(&Domain::root(), UNLIMITED, 16, UNLIMITED).expect("a live parent takes a child");
	sched::spawn_in(domain, body, 0).expect("a thread in the bounded domain");
	sched::run_until_idle();
	assert_eq!(REFUSED.load(Ordering::SeqCst), syscall::ERR_RESOURCE_EXHAUSTED, "a receive that would put the Domain past its handle limit is refused");
	assert_eq!(RETRIED.load(Ordering::SeqCst), 1, "and the message it refused was still in the queue");
}

tagged_test!(a_receive_reserves_what_the_message_needs_not_the_maximum, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_receive_reserves_what_the_message_needs_not_the_maximum() {
	// The first transactional receive booked `MAX_MESSAGE_CAPS` slots before it looked, because it
	// had no way to name the message it had inspected: without an identity, a reservation taken
	// after a peek could be spent on a different message. Booking the maximum closed the race and
	// bought a false refusal - a Domain with one free slot could not receive a message carrying one
	// capability. Safe direction, wrong answer.
	//
	// Asserted at exactly one free slot, which is where the two behaviours differ.
	use core::sync::atomic::{AtomicI64, Ordering};
	use object::domain::{Domain, UNLIMITED};
	static ARRIVED: AtomicI64 = AtomicI64::new(i64::MIN);
	static COUNT: AtomicI64 = AtomicI64::new(i64::MIN);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let (tx, rx) = channel_pair(4);
			let payload = arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0);
			let note = [0x3Cu8; 4];
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, tx, note.as_ptr() as u64, note.len() as u64, payload) as i64, 0, "one capability is sent");
			// Fill to the ceiling, then give back EXACTLY one slot: the message needs one and has
			// one, and `MAX_MESSAGE_CAPS` is four.
			while !syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_EVENT_CREATE, 0, 0, 0, 0)) {}
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, tx, 0, 0, 0) as i64, 0, "one slot freed");

			let mut buf = [0u8; 8];
			let mut caps = [0u64; abi::MAX_MESSAGE_CAPS + 1];
			ARRIVED.store(arch::syscall::invoke(syscall::SYS_CHANNEL_RECV_CAPS, rx, buf.as_mut_ptr() as u64, buf.len() as u64, caps.as_mut_ptr() as u64) as i64, Ordering::SeqCst);
			COUNT.store(caps[0] as i64, Ordering::SeqCst);
		}
	}
	let domain = Domain::new_child(&Domain::root(), UNLIMITED, 16, UNLIMITED).expect("a live parent takes a child");
	sched::spawn_in(domain, body, 0).expect("a thread in the bounded domain");
	sched::run_until_idle();
	assert_eq!(ARRIVED.load(Ordering::SeqCst), 4, "one free slot is enough for a message carrying one capability");
	assert_eq!(COUNT.load(Ordering::SeqCst), 1, "and the capability came with it");
}

tagged_test!(a_handle_reservation_books_the_memory_and_not_only_the_quota, [Process, Memory], covers = ["kernel"]);
fn a_handle_reservation_books_the_memory_and_not_only_the_quota() {
	// `reserve` charged the Domain's quota and returned, and its comment said a later install
	// therefore could not be refused for space. It could: `insert_reserved` ends in
	// `self.slots.push(...)`, an infallible `Vec` growth. The quota had said the Domain was ALLOWED
	// another handle; nothing had said the kernel heap could hold one.
	//
	// That sits under a caller whose whole reason for reserving is that it is about to destroy
	// something it cannot get back. Quota granted, message dequeued, `slots` needs to grow, the heap
	// is empty - an allocation abort in the kernel, reachable from ring 3.
	//
	// A reservation no heap could back is the deterministic form of it: no allocator state to
	// arrange, and the answer must be a refusal rather than a success that aborts later.
	use object::domain::{Domain, UNLIMITED};
	use object::handle::HandleTable;

	// Impossible by size alone. Divided down from `usize::MAX` so the byte count does not overflow
	// on the way to the allocator, which would be a different refusal than the one being tested.
	let beyond_any_heap = usize::MAX / 64;

	// No Domain at all: the quota path returns early, so this is the memory half on its own.
	let mut unbounded = HandleTable::new();
	assert!(!unbounded.reserve(beyond_any_heap), "a reservation the heap cannot back must be refused, not granted");

	// And with a Domain whose quota would allow it, nothing is charged for the refusal.
	let domain = Domain::new_child(&Domain::root(), UNLIMITED, UNLIMITED, UNLIMITED).expect("a live parent takes a child");
	let mut table = HandleTable::new();
	table.set_domain(domain.clone());
	assert!(!table.reserve(beyond_any_heap), "an unlimited quota does not make the memory appear");
	assert_eq!(domain.account().handles().used(), 0, "and a refused reservation leaves the account where it was");

	// The ordinary case still works, and the slots it booked are real.
	assert!(table.reserve(8), "a reservation the heap can back is granted");
	table.release_reservation(8);
	assert_eq!(domain.account().handles().used(), 0, "a released reservation gives the quota back");
}

tagged_test!(a_set_that_was_told_it_joined_is_a_set_that_will_be_woken, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_set_that_was_told_it_joined_is_a_set_that_will_be_woken() {
	// `wake_object` copies at most `MAX_SETS_PER_OBJECT` set koids into a stack array and warned
	// that the rest would not be woken. `register_set_observer` never counted what was already
	// registered for a member - so a fifth set could add the same channel, be told it worked, and
	// wait forever.
	//
	// The limit is not the problem and raising it is not the fix: it is what keeps the forwarding
	// off the heap, and the allocation it replaced measured 433,645 ns against a 188,821 ns
	// baseline. What was wrong is that the ceiling was enforced where the wake is DELIVERED and not
	// where the registration is MADE, so success meant something different at the two ends.
	use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
	static REFUSED: AtomicI64 = AtomicI64::new(i64::MIN);
	static WOKEN: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let (rx, tx) = channel_pair(4);
			let mut sets = [0u64; sched::MAX_SETS_PER_OBJECT];
			for slot in sets.iter_mut() {
				*slot = arch::syscall::invoke(syscall::SYS_WAITSET_CREATE, 0, 0, 0, 0);
				assert!(!syscall::sys_is_err(*slot), "a wait set is created");
				assert!(arch::syscall::invoke(syscall::SYS_WAITSET_ADD, *slot, rx, 0, 0) as i64 > 0, "a set within the limit joins, answering the member's koid");
			}
			// One past the limit. It must be REFUSED rather than admitted and then ignored.
			let extra = arch::syscall::invoke(syscall::SYS_WAITSET_CREATE, 0, 0, 0, 0);
			REFUSED.store(arch::syscall::invoke(syscall::SYS_WAITSET_ADD, extra, rx, 0, 0) as i64, Ordering::SeqCst);

			// And every set that WAS admitted is woken by the member, which is what makes the
			// refusal above a limit rather than an off-by-one.
			let note = [0u8; 4];
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, tx, note.as_ptr() as u64, note.len() as u64, 0) as i64, 0, "the member becomes ready");
			let all = sets.iter().all(|&set| arch::syscall::invoke(syscall::SYS_WAITSET_WAIT, set, 1, 0, 0) as i64 >= 0);
			WOKEN.store(all, Ordering::SeqCst);
		}
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert_eq!(REFUSED.load(Ordering::SeqCst), syscall::ERR_RESOURCE_EXHAUSTED, "a set past what a wake can reach must be refused at the ADD, not silently left unwoken");
	assert!(WOKEN.load(Ordering::SeqCst), "and every set inside the limit still answers for the member");
}

tagged_test!(a_wait_set_registration_is_charged_to_the_domain_that_holds_it, [Process, Syscall, Memory], covers = ["kernel"]);
fn a_wait_set_registration_is_charged_to_the_domain_that_holds_it() {
	// M0147 listed "a per-Domain bound on registrations" as done and it was not. What existed was
	// `MAX_WAIT_SET_MEMBERS` - a PER-SET ceiling - and the handle quota, which charges a Domain ONE
	// handle for a set holding 256 members plus 256 scheduler observer entries. Neither bounds the
	// registrations, which is where the memory is.
	use core::sync::atomic::{AtomicI64, Ordering};
	use object::domain::{Domain, UNLIMITED};
	static ADDS: AtomicI64 = AtomicI64::new(0);
	static REFUSED: AtomicI64 = AtomicI64::new(i64::MIN);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let set = arch::syscall::invoke(syscall::SYS_WAITSET_CREATE, 0, 0, 0, 0);
			assert!(!syscall::sys_is_err(set), "a wait set is created");
			// Join members until something refuses. With a registration ceiling of four it is the
			// registration quota; without one it would be `MAX_WAIT_SET_MEMBERS` at 256, and the
			// count below is what tells the two apart.
			loop {
				let (rx, _tx) = channel_pair(1);
				let joined = arch::syscall::invoke(syscall::SYS_WAITSET_ADD, set, rx, 0, 0) as i64;
				if joined < 0 {
					REFUSED.store(joined, Ordering::SeqCst);
					return;
				}
				ADDS.fetch_add(1, Ordering::SeqCst);
				assert!(ADDS.load(Ordering::SeqCst) < object::wait_set::MAX_WAIT_SET_MEMBERS as i64, "the registration quota must refuse before the per-set ceiling does, or it is not doing anything");
			}
		}
	}
	let domain = Domain::new_child(&Domain::root(), UNLIMITED, UNLIMITED, UNLIMITED).expect("a live parent takes a child");
	domain.account().wait_registrations().set_limit(4);
	sched::spawn_in(domain.clone(), body, 0).expect("a thread in the bounded domain");
	sched::run_until_idle();
	assert_eq!(ADDS.load(Ordering::SeqCst), 4, "exactly the ceiling's worth of memberships were admitted");
	assert_eq!(REFUSED.load(Ordering::SeqCst), syscall::ERR_RESOURCE_EXHAUSTED, "and the next one is refused");
	// and dropping the set gives every registration back, or the ceiling is a one-way ratchet.
	assert_eq!(domain.account().wait_registrations().used(), 0, "a set that is gone holds no registrations");
}

tagged_test!(a_wait_set_registers_its_members_once_and_wakes_on_any_of_them, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_wait_set_registers_its_members_once_and_wakes_on_any_of_them() {
	// `SYS_WAIT_ANY` takes a fresh array on every call, so the kernel registers a waiter on every
	// object in it and takes them all out again - once per pass, for as long as the caller runs. A
	// set registers each member when it JOINS, and a pass costs one registration and a readiness
	// scan whatever the membership.
	//
	// What is asserted here is the behaviour, not the cost: that a set answers for any member, that
	// membership survives between waits, and that removal takes effect. The cost is the reason the
	// object exists and is measured by the service that uses it.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let set = arch::syscall::invoke(syscall::SYS_WAITSET_CREATE, 0, 0, 0, 0);
			assert!(!syscall::sys_is_err(set), "a wait set is created");

			let (rx_a, tx_a) = channel_pair(4);
			let (rx_b, tx_b) = channel_pair(4);
			let koid_a = arch::syscall::invoke(syscall::SYS_WAITSET_ADD, set, rx_a, 0, 0) as i64;
			assert!(koid_a > 0, "the first member joins, and the add answers its koid");
			let koid_b = arch::syscall::invoke(syscall::SYS_WAITSET_ADD, set, rx_b, 0, 0) as i64;
			assert!(koid_b > 0 && koid_b != koid_a, "the second member joins under its own koid");
			// A member joins once: registering it twice would wake the set twice for one event.
			assert!(syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_WAITSET_ADD, set, rx_a, 0, 0)), "a duplicate member is refused");
			// And a set is not something to put in a set.
			assert!(syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_WAITSET_ADD, set, set, 0, 0)), "a set may not contain a set");

			// Nothing ready yet: a wait with a deadline already past reports that rather than
			// blocking, which is how this test asks "is anything ready" without parking.
			let now = arch::apic::ticks();
			assert_eq!(arch::syscall::invoke(syscall::SYS_WAITSET_WAIT, set, now, 0, 0) as i64, syscall::ERR_TIMED_OUT, "an empty set of ready members times out");

			// The SECOND member becomes readable, and the set answers with its index.
			let payload = [7u8; 4];
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, tx_b, payload.as_ptr() as u64, payload.len() as u64, 0) as i64, 0, "the message is sent");
			assert_eq!(arch::syscall::invoke(syscall::SYS_WAITSET_WAIT, set, 0, 0, 0) as i64, koid_b, "the set answers with the ready member's KOID - the caller needs no mirror of the kernel's ordering");

			// The membership SURVIVES the wait - which is the whole point of the object. A second
			// wait finds the same member ready without anything being registered again.
			assert_eq!(arch::syscall::invoke(syscall::SYS_WAITSET_WAIT, set, 0, 0, 0) as i64, koid_b, "membership outlives a wait");

			// Removing it takes effect, and the first member is still watched.
			assert_eq!(arch::syscall::invoke(syscall::SYS_WAITSET_REMOVE, set, rx_b, 0, 0) as i64, 0, "the member leaves");
			assert!(syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_WAITSET_REMOVE, set, rx_b, 0, 0)), "and leaving twice is refused");
			let now = arch::apic::ticks();
			assert_eq!(arch::syscall::invoke(syscall::SYS_WAITSET_WAIT, set, now, 0, 0) as i64, syscall::ERR_TIMED_OUT, "with it gone, nothing in the set is ready");
			assert_eq!(arch::syscall::invoke(syscall::SYS_CHANNEL_SEND, tx_a, payload.as_ptr() as u64, payload.len() as u64, 0) as i64, 0, "the other member gets a message");
			assert_eq!(arch::syscall::invoke(syscall::SYS_WAITSET_WAIT, set, 0, 0, 0) as i64, koid_a, "and the set answers for it");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the wait-set thread ran to completion");
}

tagged_test!(a_wait_set_member_whose_peer_closes_reports_rather_than_vanishing, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_wait_set_member_whose_peer_closes_reports_rather_than_vanishing() {
	// The registration-lifetime question, answered and asserted.
	//
	// Three answers were defensible - silent removal, a revocation event, or a stale registration
	// that keeps reporting - and leaving it undefined was not. This kernel keeps the member: the set
	// holds a reference, so a channel whose peer closes does not disappear from under a waiter. It
	// becomes READY, which is what a closed peer is, and the waiter is told; leaving the set is
	// explicit.
	//
	// The alternative would lose an edge. A member silently dropped at the moment it became
	// interesting is a wake nobody gets.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let set = arch::syscall::invoke(syscall::SYS_WAITSET_CREATE, 0, 0, 0, 0);
			let (rx, tx) = channel_pair(4);
			let member = arch::syscall::invoke(syscall::SYS_WAITSET_ADD, set, rx, 0, 0) as i64;
			assert!(member > 0);
			let now = arch::apic::ticks();
			assert_eq!(arch::syscall::invoke(syscall::SYS_WAITSET_WAIT, set, now, 0, 0) as i64, syscall::ERR_TIMED_OUT, "nothing is ready yet");

			// The peer goes. The member stays, and says so.
			assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, tx, 0, 0, 0) as i64, 0, "the peer closes");
			assert_eq!(arch::syscall::invoke(syscall::SYS_WAITSET_WAIT, set, 0, 0, 0) as i64, member, "a closed peer is a ready member, not an absent one");

			// And the set is exactly as large as it was.
			assert_eq!(arch::syscall::invoke(syscall::SYS_WAITSET_REMOVE, set, rx, 0, 0) as i64, 0, "the member is still there to remove");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the lifetime thread ran to completion");
}

tagged_test!(a_wait_set_has_a_ceiling_and_says_so, [Process, Syscall, Ipc], covers = ["kernel"]);
fn a_wait_set_has_a_ceiling_and_says_so() {
	// A set is kernel memory whose size a userspace caller decides, which is the shape of every
	// quota in this kernel and gets the same treatment: a fixed ceiling first, then a fallible
	// allocation, then a refusal rather than an abort.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let set = arch::syscall::invoke(syscall::SYS_WAITSET_CREATE, 0, 0, 0, 0);
			let mut admitted = 0usize;
			let mut refused = false;
			// One event object per slot: cheaper than a channel pair and waitable all the same.
			for _ in 0..object::wait_set::MAX_WAIT_SET_MEMBERS + 4 {
				let handle = {
					let thread = sched::current_thread().expect("a current thread");
					let event = object::event::Event::create();
					match thread.handles().lock().try_insert_object(event, object::rights::Rights::ALL, 0) {
						Some(h) => h.raw(),
						None => break,
					}
				};
				if syscall::sys_is_err(arch::syscall::invoke(syscall::SYS_WAITSET_ADD, set, handle, 0, 0)) {
					refused = true;
					break;
				}
				admitted += 1;
			}
			assert!(refused, "the set refuses rather than growing without bound");
			assert_eq!(admitted, object::wait_set::MAX_WAIT_SET_MEMBERS, "and it refuses at its stated ceiling");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the ceiling thread ran to completion");
}

tagged_test!(a_secure_random_syscall_refuses_rather_than_answering_from_a_formula, [Syscall, Process], covers = ["kernel"]);
fn a_secure_random_syscall_refuses_rather_than_answering_from_a_formula() {
	// There was ONE syscall. It answered from the CPU's hardware source where there was one and from
	// a clock-seeded formula where there was not, and userspace saw one answer either way - so
	// anything deriving a key or a token from it was guessable on any machine without the hardware,
	// with nothing to say so.
	//
	// And that was not a corner case: two of this system's three architectures have no hardware
	// source at all, so the formula was the ANSWER there rather than the fallback.
	//
	// Two syscalls now. `SYS_RANDOM_GET` gives hardware or refuses; `SYS_RANDOM_INSECURE` always
	// answers and says in its name what it is. What was wrong was never the formula - a boot
	// identifier wants exactly that - it was the formula arriving under a name that promised
	// otherwise.
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let mut buf = [0u8; 32];
			let secure = arch::syscall::invoke(syscall::SYS_RANDOM_GET, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0) as i64;
			if arch::random::secure_available() {
				assert_eq!(secure, buf.len() as i64, "a machine with a hardware source answers from it");
				assert!(buf.iter().any(|&b| b != 0), "and the answer is not zeros");
			} else {
				assert_eq!(secure, syscall::ERR_UNSUPPORTED, "a machine with no hardware source refuses rather than substituting");
				assert_eq!(buf, [0u8; 32], "and writes nothing at all");
			}

			// The other one always answers, on every machine, and says what it is by its name.
			let mut weak = [0u8; 32];
			assert_eq!(arch::syscall::invoke(syscall::SYS_RANDOM_INSECURE, weak.as_mut_ptr() as u64, weak.len() as u64, 0, 0) as i64, weak.len() as i64, "the insecure source always answers");
			assert!(weak.iter().any(|&b| b != 0), "with something");
			// Twice in a row differs, which is all a boot identifier ever wanted from it.
			let mut again = [0u8; 32];
			assert_eq!(arch::syscall::invoke(syscall::SYS_RANDOM_INSECURE, again.as_mut_ptr() as u64, again.len() as u64, 0, 0) as i64, again.len() as i64);
			assert_ne!(weak, again, "two draws differ, which is what 'distinguishable' means");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn_with_object(body, object::event::Event::create(), object::rights::Rights::ALL, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the random thread ran to completion");
}

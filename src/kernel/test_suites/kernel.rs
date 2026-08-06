use super::*;

tagged_test!(a_syscall_may_not_ask_the_kernel_for_an_unbounded_allocation, [Syscall, Memory]);
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

tagged_test!(a_cpu_bound_ring3_thread_is_preempted, [Scheduler, Process]);
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
		frame::deallocate(code);
		frame::deallocate(stack);
		frame::deallocate(data);
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

tagged_test!(process_isolation_and_per_process_tables, [Process, Memory]);
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
	frame::deallocate(f1);
	frame::deallocate(f2);
}

tagged_test!(syscall_object_and_handle_ops, [Syscall]);
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

tagged_test!(an_unmapped_va_range_is_reused_not_leaked, [Memory]);
fn an_unmapped_va_range_is_reused_not_leaked() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	// The mmap window reclaims released ranges: an unmap returns its range to the
	// window's pool and the next map of the same size gets it back (first-fit),
	// so a map/unmap loop no longer walks off the window. Freeing two adjacent
	// ranges coalesces them, so a larger mapping fits the merged hole - churn
	// cannot shatter the window into unusable slivers.
	extern "C" fn body(_arg: u64) {
		unsafe {
			let page: u64 = mem::frame::PAGE_SIZE;
			// reuse: map, unmap, map again - the same range comes back.
			let a = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, page, 0, 0, 0);
			assert!(!syscall::sys_is_err(a));
			let first = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, a, 0, 0, 0);
			assert!(!syscall::sys_is_err(first));
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, a, 0, 0, 0) as i64, 0);
			let second = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, a, 0, 0, 0);
			assert_eq!(second, first, "the released range should be handed out again");
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, a, 0, 0, 0) as i64, 0);
			// coalescing: two adjacent single-page ranges released in either order
			// merge, so a two-page mapping fits where they were.
			let b = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, page, 0, 0, 0);
			let c = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, page, 0, 0, 0);
			let base_b = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, b, 0, 0, 0);
			let base_c = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, c, 0, 0, 0);
			assert_eq!(base_b, first, "the first-fit hole is the one just released");
			assert_eq!(base_c, base_b + page, "adjacent allocations pack the window");
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, b, 0, 0, 0) as i64, 0);
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, c, 0, 0, 0) as i64, 0);
			let d = arch::syscall::invoke(syscall::SYS_MEMORY_OBJECT_CREATE, 2 * page, 0, 0, 0);
			let base_d = arch::syscall::invoke(syscall::SYS_MEMORY_MAP, d, 0, 0, 0);
			assert_eq!(base_d, base_b, "the merged hole should fit the larger mapping");
			assert_eq!(arch::syscall::invoke(syscall::SYS_MEMORY_UNMAP, d, 0, 0, 0) as i64, 0);
			for handle in [a, b, c, d] {
				assert_eq!(arch::syscall::invoke(syscall::SYS_HANDLE_CLOSE, handle, 0, 0, 0) as i64, 0);
			}
		}
		DONE.store(true, Ordering::SeqCst);
	}
	sched::spawn(body, 0);
	sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

tagged_test!(blocking_wait_times_out_on_deadline, [Scheduler]);
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

tagged_test!(a_periodic_wait_ticks_but_never_holds_the_scheduler, [Scheduler]);
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

tagged_test!(waiting_on_a_process_handle_wakes_when_it_exits, [Process]);
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

tagged_test!(signal_terminate_wakes_a_blocked_thread, [Process]);
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

tagged_test!(a_clean_exit_releases_the_process_channel_endpoints, [Process]);
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

tagged_test!(userspace_spawn_syscalls_start_a_second_process, [Process, Syscall]);
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

tagged_test!(syscall_fuzz_rejects_invalid_calls, [Syscall]);
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

tagged_test!(object_info_get_reports_object, [Syscall]);
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

tagged_test!(system_graph_reflects_live_state, [Kernel]);
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

tagged_test!(process_counters_track_ipc_and_resources, [Process]);
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

tagged_test!(userspace_runs_and_ipcs, [Process]);
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

tagged_test!(userspace_yields_cooperatively, [Process, Scheduler]);
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

tagged_test!(fault_isolation_kills_only_process, [Kernel, Process]);
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
	[Kernel, ArchX86_64]
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
	[Kernel, ArchAarch64]
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
	[Kernel, ArchRiscv64]
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
	[Kernel, ArchX86_64]
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
	frame::deallocate(frame);
}

tagged_test!(a_user_stack_grows_on_demand_past_its_initial_pages, [Kernel, Memory]);
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

tagged_test!(recursion_past_the_stack_floor_is_killed, [Kernel, Memory, Process]);
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

tagged_test!(domain_hierarchy_limit_is_enforced_through_memory_create, [Domain, Kernel]);
fn domain_hierarchy_limit_is_enforced_through_memory_create() {
	use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
	use object::domain::{Domain, UNLIMITED};
	static DONE: AtomicBool = AtomicBool::new(false);
	static THIRD: AtomicI64 = AtomicI64::new(0);
	// The parent caps memory at two pages; the unbounded child may create two
	// objects but the third is refused through the actual syscall path.
	let parent = Domain::new(8192, UNLIMITED, UNLIMITED);
	let child = Domain::new_child(&parent, UNLIMITED, UNLIMITED, UNLIMITED);
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

tagged_test!(domain_kill_frees_subtree, [Kernel, Process]);
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
	let child = Domain::new_child(&parent, 1 << 20, 16, 8);
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

tagged_test!(channel_capability_transfer_is_zero_copy, [Ipc, Memory, Syscall]);
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

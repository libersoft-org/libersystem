use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::object::address_space::AddressSpace;
use crate::object::process::Process;
use crate::object::rights::Rights;
use crate::object::thread::{Thread, ThreadState};
use crate::sync::SpinLock;
use crate::{arch, sched, smp};

fn prepared(entry: extern "C" fn(u64), argument: u64) -> Arc<Thread> {
	let process = Process::new(AddressSpace::kernel(), sched::root_domain()).expect("test process");
	Thread::new(entry, argument, process).expect("test thread")
}

fn until(mut ready: impl FnMut() -> bool) {
	let deadline = arch::apic::ticks() + 1_000;
	while !ready() && arch::apic::ticks() < deadline {
		sched::run_until_idle_until(arch::apic::ticks() + 1);
	}
	assert!(ready(), "the placement fixture did not finish before its deadline");
}

crate::tagged_test!(a_placed_thread_keeps_its_cpu_across_wakes_and_yields, [Scheduler, Syscall], id = "kernel.sched.a_placed_thread_keeps_its_cpu_across_wakes_and_yields", covers = ["kernel"]);
fn a_placed_thread_keeps_its_cpu_across_wakes_and_yields() {
	static PHASE: AtomicUsize = AtomicUsize::new(0);
	static RELEASE: AtomicUsize = AtomicUsize::new(0);
	static SEEN: [AtomicUsize; 5] = [const { AtomicUsize::new(usize::MAX) }; 5];
	const KOID: u64 = u64::MAX - 129;
	extern "C" fn worker(_argument: u64) {
		SEEN[0].store(sched::current_cpu_id(), Ordering::Release);
		sched::yield_now();
		SEEN[1].store(sched::current_cpu_id(), Ordering::Release);
		PHASE.store(1, Ordering::Release);
		sched::block_on_flagged(KOID, sched::NO_DEADLINE, false, || RELEASE.load(Ordering::Acquire) >= 1);
		SEEN[2].store(sched::current_cpu_id(), Ordering::Release);
		PHASE.store(2, Ordering::Release);
		sched::block_on(KOID, arch::apic::ticks() + 2);
		SEEN[3].store(sched::current_cpu_id(), Ordering::Release);
		PHASE.store(3, Ordering::Release);
		sched::block_on_flagged(KOID, sched::NO_DEADLINE, false, || RELEASE.load(Ordering::Acquire) >= 3);
		SEEN[4].store(sched::current_cpu_id(), Ordering::Release);
		PHASE.store(4, Ordering::Release);
	}
	assert!(smp::cpu_count() >= 2 && smp::is_online(1), "this multicore fixture requires CPU 1");
	assert_eq!(sched::current_cpu_id(), 0, "the test driver is the remote waker on CPU 0");
	for placed in [false, true] {
		PHASE.store(0, Ordering::Release);
		RELEASE.store(0, Ordering::Release);
		for seen in &SEEN {
			seen.store(usize::MAX, Ordering::Release);
		}
		let thread = prepared(worker, 0);
		if placed {
			assert!(sched::thread_start_on(thread.clone(), 1));
		} else {
			// Existing one-shot test placement uses the same legacy start claim: the first wake
			// must still move this unplaced thread to its waker's CPU.
			assert!(sched::start_thread_on(1, &thread));
		}
		// Blocked is published BEFORE the ready recheck. Wait for the saved stack too:
		// otherwise raising RELEASE can let the worker cancel its own park on CPU 1,
		// which never exercises the remote wake or its legacy migration policy.
		until(|| PHASE.load(Ordering::Acquire) == 1 && thread.state() == ThreadState::Blocked && thread.kstack_ptr_load() != 0);
		RELEASE.store(1, Ordering::Release);
		sched::wake_object(KOID);
		until(|| PHASE.load(Ordering::Acquire) == 3 && thread.state() == ThreadState::Blocked && thread.kstack_ptr_load() != 0);
		RELEASE.store(3, Ordering::Release);
		sched::wake_thread(&thread); // The common signal/termination wake path.
		until(|| thread.state() == ThreadState::Exited);
		assert_eq!(PHASE.load(Ordering::Acquire), 4);
		let seen = core::array::from_fn::<_, 5, _>(|i| SEEN[i].load(Ordering::Acquire));
		assert_eq!(&seen[..2], &[1, 1], "initial start and yield keep their CPU");
		assert_eq!(&seen[2..], &[if placed { 1 } else { 0 }; 3], "object, deadline and explicit wake honor the chosen policy");
		assert_eq!(thread.cpu_placement(), placed.then_some(1));
	}
}

crate::tagged_test!(thread_start_placement_refusals_leave_the_start_claim_intact, [Scheduler, Syscall, Process], id = "kernel.sched.thread_start_placement_refusals_leave_the_start_claim_intact", covers = ["kernel"]);
fn thread_start_placement_refusals_leave_the_start_claim_intact() {
	static TARGET: SpinLock<Option<Arc<Thread>>> = SpinLock::new(None);
	static CHECKED: AtomicUsize = AtomicUsize::new(0);
	static RAN: AtomicUsize = AtomicUsize::new(0);
	static OLD_SEEN: AtomicUsize = AtomicUsize::new(usize::MAX);
	extern "C" fn worker(_argument: u64) {
		RAN.store(sched::current_cpu_id() + 1, Ordering::Release);
	}
	extern "C" fn old_worker(_argument: u64) {
		OLD_SEEN.store(sched::current_cpu_id(), Ordering::Release);
	}
	extern "C" fn caller(read_only: u64) {
		let target = TARGET.lock().as_ref().unwrap().clone();
		let current = sched::current_thread().unwrap();
		let handle = current.process().install(target.clone(), Rights::MANAGE).unwrap();
		let invoke = |number, thread, cpu| unsafe { arch::syscall::invoke(number, thread, cpu, 0, 0) as i64 };
		assert_eq!(invoke(abi::SYS_THREAD_START_ON, 0, 1), abi::ERR_BAD_HANDLE);
		assert_eq!(invoke(abi::SYS_THREAD_START_ON, read_only, 1), abi::ERR_ACCESS_DENIED);
		for cpu in [u64::MAX, smp::cpu_count() as u64] {
			assert_eq!(invoke(abi::SYS_THREAD_START_ON, handle, cpu), abi::ERR_INVALID);
		}
		for cpu in 0..smp::cpu_count() {
			if !smp::is_online(cpu) {
				assert_eq!(invoke(abi::SYS_THREAD_START_ON, handle, cpu as u64), abi::ERR_INVALID);
			}
		}
		assert_eq!(target.cpu_placement(), None);
		assert_eq!(RAN.load(Ordering::Acquire), 0, "a refused start ran nothing");
		assert_eq!(invoke(abi::SYS_THREAD_START_ON, handle, 1), 0, "same handle retries after refusal");
		assert_eq!(invoke(abi::SYS_THREAD_START_ON, handle, 0), abi::ERR_INVALID, "a second start cannot move it");
		assert_eq!(invoke(abi::SYS_THREAD_START, handle, 0), abi::ERR_INVALID, "legacy start cannot claim it again");
		assert_eq!(target.cpu_placement(), Some(1));
		// A process which began teardown is refused without claiming or installing placement.
		let stopped = prepared(worker, 0);
		stopped.process().terminate();
		let stopped_handle = current.process().install(stopped.clone(), Rights::MANAGE).unwrap();
		assert_eq!(invoke(abi::SYS_THREAD_START_ON, stopped_handle, 1), abi::ERR_INVALID);
		assert_eq!(stopped.cpu_placement(), None);
		// The old syscall remains a current-core, unplaced start; extra arguments stay ignored.
		let old = prepared(old_worker, 0);
		let old_handle = current.process().install(old.clone(), Rights::MANAGE).unwrap();
		assert_eq!(invoke(abi::SYS_THREAD_START, old_handle, u64::MAX), 0);
		assert_eq!(old.cpu_placement(), None);
		CHECKED.store(1, Ordering::Release);
	}
	assert!(smp::cpu_count() >= 2 && smp::is_online(1));
	CHECKED.store(0, Ordering::Release);
	RAN.store(0, Ordering::Release);
	OLD_SEEN.store(usize::MAX, Ordering::Release);
	let target = prepared(worker, 0);
	*TARGET.lock() = Some(target.clone());
	let controller = sched::spawn_with_object(caller, target.clone(), Rights::READ);
	until(|| CHECKED.load(Ordering::Acquire) == 1 && controller.state() == ThreadState::Exited && target.state() == ThreadState::Exited && OLD_SEEN.load(Ordering::Acquire) != usize::MAX);
	assert_eq!(target.cpu_placement(), Some(1));
	assert_eq!(RAN.load(Ordering::Acquire), 2, "the successful placed syscall actually runs on CPU 1");
	assert_eq!(OLD_SEEN.load(Ordering::Acquire), 0, "the old syscall starts on its caller CPU");
	TARGET.lock().take();
}

crate::tagged_test!(racing_placed_starts_publish_one_immutable_cpu, [Scheduler, Process], id = "kernel.sched.racing_placed_starts_publish_one_immutable_cpu", covers = ["kernel"]);
fn racing_placed_starts_publish_one_immutable_cpu() {
	static TARGET: SpinLock<Option<Arc<Thread>>> = SpinLock::new(None);
	static READY: AtomicUsize = AtomicUsize::new(0);
	static GO: AtomicUsize = AtomicUsize::new(0);
	static WINNER: AtomicUsize = AtomicUsize::new(0);
	static SUCCESSES: AtomicUsize = AtomicUsize::new(0);
	static SEEN: AtomicUsize = AtomicUsize::new(usize::MAX);
	extern "C" fn worker(_argument: u64) {
		SEEN.store(sched::current_cpu_id(), Ordering::Release);
	}
	extern "C" fn racer(argument: u64) {
		let cpu = argument & 0xff;
		let legacy = argument & 0x100 != 0;
		let target = TARGET.lock().as_ref().unwrap().clone();
		READY.fetch_add(1, Ordering::AcqRel);
		while GO.load(Ordering::Acquire) == 0 {
			core::hint::spin_loop();
		}
		let started = if legacy { sched::thread_start(target) } else { sched::thread_start_on(target, cpu as usize) };
		if started {
			WINNER.store(cpu as usize, Ordering::Release);
			SUCCESSES.fetch_add(1, Ordering::AcqRel);
		}
	}
	assert!(smp::cpu_count() >= 3 && smp::is_online(1) && smp::is_online(2));
	for mixed_legacy in [false, true] {
		for counter in [&READY, &GO, &WINNER, &SUCCESSES] {
			counter.store(0, Ordering::Release);
		}
		SEEN.store(usize::MAX, Ordering::Release);
		let target = prepared(worker, 0);
		*TARGET.lock() = Some(target.clone());
		let a = sched::spawn_on(1, racer, if mixed_legacy { 0x101 } else { 1 });
		let b = sched::spawn_on(2, racer, 2);
		until(|| READY.load(Ordering::Acquire) == 2);
		GO.store(1, Ordering::Release);
		until(|| a.state() == ThreadState::Exited && b.state() == ThreadState::Exited && target.state() == ThreadState::Exited);
		assert_eq!(SUCCESSES.load(Ordering::Acquire), 1);
		let winner = WINNER.load(Ordering::Acquire);
		assert_eq!(target.cpu_placement(), if mixed_legacy && winner == 1 { None } else { Some(winner) });
		assert_eq!(SEEN.load(Ordering::Acquire), winner, "only the winning CPU executes the thread");
		TARGET.lock().take();
	}
}

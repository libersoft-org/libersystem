// Threads, run queues, and the scheduler.
//
// Each core owns a run queue and a "current thread" slot behind a per-CPU
// spinlock, so the design is SMP-correct from the start. Scheduling is
// cooperative round-robin: a running thread calls yield_now() or returns (which
// exits it), and the scheduler context-switches to the next ready thread on the
// same core. Threads do not migrate between cores in this milestone, so a core
// only ever touches its own queue; cross-core balancing is a later refinement.
//
// The bootstrap/idle context of each core (the stack the kernel booted on, and
// the AP idle loop) is the fallback that runs when no thread is ready. Its stack
// pointer is saved in CpuSched::idle_sp on the way out and restored on the way in.

#![allow(dead_code)]

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch;
use crate::arch::percpu::MAX_CPUS;
use crate::object::address_space::AddressSpace;
use crate::object::domain::Domain;
use crate::object::process::Process;
use crate::object::rights::Rights;
use crate::object::thread::{Thread, ThreadState};
use crate::object::KernelObject;
use crate::sync::SpinLock;

// How the scheduler should treat the outgoing thread when switching away.
#[derive(Clone, Copy)]
enum Disposition {
	// Thread yielded and remains runnable: put it back on the run queue.
	Requeue,
	// Thread has exited: move it aside to be reaped, never run it again.
	Retire,
}

struct CpuSchedInner {
	run_queue: VecDeque<Arc<Thread>>,
	current: Option<Arc<Thread>>,
	// A thread that just exited on this core, awaiting reap by the next context.
	zombie: Option<Arc<Thread>>,
}

struct CpuSched {
	inner: SpinLock<CpuSchedInner>,
	// Saved stack pointer of this core's idle/bootstrap context.
	idle_sp: AtomicU64,
}

impl CpuSched {
	const fn new() -> Self {
		Self { inner: SpinLock::new(CpuSchedInner { run_queue: VecDeque::new(), current: None, zombie: None }), idle_sp: AtomicU64::new(0) }
	}
}

static SCHED: [CpuSched; MAX_CPUS] = [const { CpuSched::new() }; MAX_CPUS];

// The kernel address space shared by all kernel threads. Set once at init().
static KERNEL_AS: SpinLock<Option<Arc<AddressSpace>>> = SpinLock::new(None);

// The root resource Domain. Kernel threads are accounted here; it has no quotas,
// so existing behavior is unchanged. Bounded Domains are created explicitly.
static ROOT_DOMAIN: SpinLock<Option<Arc<Domain>>> = SpinLock::new(None);

// The kernel address space's CR3, cached for the scheduler hot path. The
// idle/bootstrap context runs on this; the scheduler restores it when a core goes
// idle so a dead process's page tables are freed while off their own CR3.
static KERNEL_CR3: AtomicU64 = AtomicU64::new(0);

fn current_cpu_id() -> usize {
	arch::percpu::this_cpu().cpu_id() as usize
}

// Capture the kernel address space and create the root Domain so spawned threads
// can reference them. Called on the BSP once per-CPU data is up.
pub fn init() {
	let kernel_as = AddressSpace::kernel();
	KERNEL_CR3.store(kernel_as.cr3(), Ordering::Release);
	*KERNEL_AS.lock() = Some(kernel_as);
	*ROOT_DOMAIN.lock() = Some(Domain::root());
}

// The root (unlimited) resource Domain.
pub fn root_domain() -> Arc<Domain> {
	ROOT_DOMAIN.lock().clone().expect("scheduler not initialized")
}

// A handle to the kernel address space (shared higher-half kernel mappings).
fn kernel_as() -> Arc<AddressSpace> {
	KERNEL_AS.lock().clone().expect("scheduler not initialized")
}

// Create a kernel thread on the current core's run queue.
pub fn spawn(entry: extern "C" fn(u64), arg: u64) -> Arc<Thread> {
	spawn_on(current_cpu_id(), entry, arg)
}

// Create a kernel thread on a specific core's run queue. The thread gets its own
// single-thread process in the kernel address space, accounted to the root
// Domain - so a kernel thread's table is reclaimed when the thread is reaped.
pub fn spawn_on(cpu: usize, entry: extern "C" fn(u64), arg: u64) -> Arc<Thread> {
	let process = Process::new(kernel_as(), root_domain());
	let thread = Thread::new(entry, arg, process);
	SCHED[cpu].inner.lock().run_queue.push_back(thread.clone());
	thread
}

// Create a kernel thread on the current core, pre-seeded with a handle to
// `object` (delivered to the thread as its bootstrap-handle argument).
pub fn spawn_with_object(entry: extern "C" fn(u64), object: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> Arc<Thread> {
	let process = Process::new(kernel_as(), root_domain());
	let arg = process.install(object, rights, badge);
	let thread = Thread::new(entry, arg, process);
	SCHED[current_cpu_id()].inner.lock().run_queue.push_back(thread.clone());
	thread
}

// Create a kernel thread accounted to `domain` on the current core, enforcing the
// Domain's thread quota. Returns None (spawning nothing) if the Domain is at its
// thread cap - a clean refusal rather than a crash.
pub fn spawn_in(domain: Arc<Domain>, entry: extern "C" fn(u64), arg: u64) -> Option<Arc<Thread>> {
	let process = Process::new(kernel_as(), domain);
	let thread = Thread::new_in(entry, arg, process)?;
	SCHED[current_cpu_id()].inner.lock().run_queue.push_back(thread.clone());
	Some(thread)
}

// Create a new process with its own address space, accounted to `domain`. Returns
// None if no frame is available for the address space's top-level page table.
pub fn process_create(domain: Arc<Domain>) -> Option<Arc<Process>> {
	let address_space = AddressSpace::create()?;
	Some(Process::new(address_space, domain))
}

// Create a thread in an existing `process` on the current core's run queue. The
// thread shares the process's address space and handle table with its siblings.
pub fn thread_create(process: Arc<Process>, entry: extern "C" fn(u64), arg: u64) -> Arc<Thread> {
	let thread = Thread::new(entry, arg, process);
	SCHED[current_cpu_id()].inner.lock().run_queue.push_back(thread.clone());
	thread
}

// Yield the current core to the next ready thread, if any.
pub fn yield_now() {
	reschedule(Disposition::Requeue);
	// A cooperative kill point: if this thread's process was terminated while it
	// was descheduled (by a fault or a Domain kill), exit now instead of resuming.
	// The current-thread Arc must be released before exit(): exit() never returns,
	// so holding the Arc across it would leak a reference and pin the thread,
	// keeping its slot from ever being refunded.
	let killed = current_thread().map_or(false, |thread| thread.process().is_killed());
	if killed {
		exit();
	}
}

// The thread currently running on this core, if any (None in the idle context).
pub fn current_thread() -> Option<Arc<Thread>> {
	SCHED[current_cpu_id()].inner.lock().current.clone()
}

// Terminate the calling thread. Never returns.
pub fn exit() -> ! {
	reschedule(Disposition::Retire);
	// The scheduler always switches away from a retiring thread; reaching here
	// would mean it failed to, so halt rather than run on a corrupt stack.
	arch::halt_loop()
}

// Run ready threads on the current core until the run queue drains, then return.
// Used by the bootstrap context to drive cooperative kernel threads to completion.
pub fn run_until_idle() {
	while !SCHED[current_cpu_id()].inner.lock().run_queue.is_empty() {
		reschedule(Disposition::Requeue);
	}
	reap(&SCHED[current_cpu_id()]);
}

// Idle loop for application processors: run any ready thread, otherwise spin and
// re-check. Cores park here instead of halting so threads can be scheduled onto
// them; a power-friendly wait (halt + wakeup IPI) is a later refinement.
pub fn cpu_idle_loop() -> ! {
	loop {
		reschedule(Disposition::Requeue);
		core::hint::spin_loop();
	}
}

// Drop a thread that exited on this core. Runs in the context switched to after
// the exit, so the dead thread's stack is guaranteed no longer in use.
fn reap(sched: &CpuSched) {
	let dead = sched.inner.lock().zombie.take();
	drop(dead);
}

// Load `want_cr3` into CR3 unless it is already active. All kernel code and
// stacks live in the higher half, mapped identically in every address space, so
// switching the active address space mid-context-switch keeps the running code
// and both stacks mapped.
fn switch_address_space(want_cr3: u64) {
	if arch::context::read_cr3() != want_cr3 {
		unsafe { arch::context::write_cr3(want_cr3) };
	}
}

// Core scheduling step: pick the next ready thread and context-switch to it.
fn reschedule(disp: Disposition) {
	let sched = &SCHED[current_cpu_id()];
	reap(sched);

	let mut guard = sched.inner.lock();
	let next = guard.run_queue.pop_front();
	let prev = guard.current.take();

	match next {
		Some(next) => {
			let old_sp = stash_prev(&mut guard, sched, prev, disp);
			next.set_state(ThreadState::Running);
			let new_sp = next.kstack_ptr_load();
			let new_cr3 = next.address_space().cr3();
			guard.current = Some(next);
			drop(guard);
			switch_address_space(new_cr3);
			unsafe { arch::context::switch_context(old_sp, new_sp) };
		}
		None => match prev {
			// Idle context with nothing to run: return to the idle loop.
			None => {}
			Some(prev) => match disp {
				Disposition::Retire => {
					// Current thread exited and nothing else is ready: switch
					// back to this core's idle context on the kernel address
					// space, so reaping the dead thread frees its page tables
					// while off their own CR3.
					let old_sp = prev.kstack_ptr_addr();
					prev.set_state(ThreadState::Exited);
					guard.zombie = Some(prev);
					guard.current = None;
					let new_sp = sched.idle_sp.load(Ordering::Acquire);
					drop(guard);
					switch_address_space(KERNEL_CR3.load(Ordering::Acquire));
					unsafe { arch::context::switch_context(old_sp, new_sp) };
				}
				Disposition::Requeue => {
					// Sole runnable thread yielded: keep running it, no switch.
					prev.set_state(ThreadState::Running);
					guard.current = Some(prev);
				}
			},
		},
	}
}

// Move the outgoing thread into the run queue (yield) or the zombie slot (exit),
// and return the address its stack pointer must be saved to. For the idle context
// (no current thread) this is the per-CPU idle save slot.
fn stash_prev(inner: &mut CpuSchedInner, sched: &CpuSched, prev: Option<Arc<Thread>>, disp: Disposition) -> *mut u64 {
	match prev {
		None => sched.idle_sp.as_ptr(),
		Some(prev) => {
			let slot = prev.kstack_ptr_addr();
			match disp {
				Disposition::Retire => {
					prev.set_state(ThreadState::Exited);
					inner.zombie = Some(prev);
				}
				Disposition::Requeue => {
					prev.set_state(ThreadState::Ready);
					inner.run_queue.push_back(prev);
				}
			}
			slot
		}
	}
}

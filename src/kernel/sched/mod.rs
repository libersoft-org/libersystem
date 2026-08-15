// Threads, run queues, and the scheduler.
//
// Each core owns a run queue and a "current thread" slot behind a per-CPU
// spinlock, so the design is SMP-correct from the start. Scheduling is
// cooperative round-robin: a running thread calls yield_now() or returns (which
// exits it), and the scheduler context-switches to the next ready thread on the
// same core. Threads do not migrate between cores in the current design, so a core
// only ever touches its own queue; cross-core balancing is a later refinement.
//
// The bootstrap/idle context of each core (the stack the kernel booted on, and
// the AP idle loop) is the fallback that runs when no thread is ready. Its stack
// pointer is saved in CpuSched::idle_sp on the way out and restored on the way in.

#![allow(dead_code)]

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use crate::arch;
use crate::object::KernelObject;
use crate::object::address_space::AddressSpace;
use crate::object::domain::Domain;
use crate::object::process::Process;
use crate::object::rights::Rights;
use crate::object::thread::{Thread, ThreadState};
use crate::sync::SpinLock;

// How the scheduler should treat the outgoing thread when switching away.
#[derive(Clone, Copy)]
enum Disposition {
	// Thread yielded and remains runnable: put it back on the run queue.
	Requeue,
	// Thread has exited: move it aside to be reaped, never run it again.
	Retire,
	// Thread blocked in `wait`: deschedule it without requeueing. It is kept alive
	// by the wait registry (the per-object buckets) and re-enqueued when woken.
	Block,
}

// A FIFO of threads that ALLOCATES NOTHING, because the link lives in the thread.
//
// This was a `VecDeque<Arc<Thread>>`, and the push that grew it ran under the per-CPU scheduler
// lock - asking the kernel heap for memory while a core is locked out of its own scheduler, which is
// asking for a TLB shootdown under that lock, which waits for the very core that is spinning on it.
// The `ALLOC-OK` markers said "bounded by the Domain thread quota", and a bound is not a booking:
// the quota says the queue will never hold a millionth entry and nothing about whether the heap can
// hold its first.
//
// Booking a slot on every core at thread creation was tried and withdrawn - it moved the allocation
// under N locks instead of one. This is the answer the audit named: a thread is in at most one run
// queue at a time (it is running, queued on one core, or blocked), so ONE link in the `Thread` is
// enough, and enqueueing becomes three pointer stores. There is nothing left to allocate, so there
// is nothing left to refuse, and `push` cannot fail.
struct RunQueue {
	head: Option<Arc<Thread>>,
	tail: Option<Arc<Thread>>,
	len: usize,
}

impl RunQueue {
	const fn new() -> Self {
		Self { head: None, tail: None, len: 0 }
	}

	fn is_empty(&self) -> bool {
		self.head.is_none()
	}

	fn len(&self) -> usize {
		self.len
	}

	// The queue holds `head` strongly and each thread's link holds the next, so the chain keeps
	// every queued thread alive without a second list to keep in step.
	fn push_back(&mut self, thread: Arc<Thread>) {
		*thread.run_link() = None;
		match self.tail.take() {
			Some(tail) => *tail.run_link() = Some(thread.clone()),
			None => self.head = Some(thread.clone()),
		}
		self.tail = Some(thread);
		self.len += 1;
	}

	fn pop_front(&mut self) -> Option<Arc<Thread>> {
		let head = self.head.take()?;
		match head.run_link().take() {
			Some(next) => self.head = Some(next),
			// The last one out takes the tail with it: a queue with no head has no tail.
			None => self.tail = None,
		}
		self.len -= 1;
		Some(head)
	}
}

struct CpuSchedInner {
	run_queue: RunQueue,
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
		Self { inner: SpinLock::new(CpuSchedInner { run_queue: RunQueue::new(), current: None, zombie: None }), idle_sp: AtomicU64::new(0) }
	}
}

static SCHED: AtomicPtr<CpuSched> = AtomicPtr::new(core::ptr::null_mut());

// WHY THE RUN QUEUES ARE NOT BOOKED AHEAD, and what was tried.
//
// The `push_back` calls below carry `ALLOC-OK: bounded by the Domain thread quota`, and that marker
// is HONEST ABOUT BEING INCOMPLETE: a bound says the queue will never hold a millionth entry and
// says nothing about whether the heap can hold its first. `SYS_THREAD_START` and every wake reach
// those pushes, so a short heap is a kernel abort reachable from ring 3.
//
// The obvious answer - book a slot on every core when a thread is CREATED, where a refusal can be
// carried - was implemented and REVERTED, because it deadlocks. Reserving takes each core's
// scheduler lock, and asking the heap for memory under that lock is asking for a TLB shootdown under
// it: mapping a new heap page waits for every other core to acknowledge, and a core spinning for the
// lock this one holds never will. Building the larger queue outside the lock and swapping it in
// under it fixes that particular hazard and the suite still stopped, one test further along - the
// mechanism takes N spinlocks on a path reached from a syscall, and that is a scheduling hazard
// whatever the allocation does.
//
// The real answer is the one the audit that raised this named: an INTRUSIVE run queue, where the
// link lives in the `Thread` and enqueue moves pointers rather than growing a container. Then there
// is nothing to book because there is nothing to allocate. That is a rewrite of this file's data
// structure and it is a task rather than a patch - recorded in P02M0133 with this evidence, because
// the next person to have the obvious idea should find out here that it was tried.
//
// Measured on aarch64 with eight cores online: the full suite stopped at the seventh test with the
// booking in place, and passes without it.

// Put `thread` on `cpu`'s run queue.
//
// One lock, no allocation, no way to fail - which is what the intrusive queue above bought. Before
// it, this line was `cpu_sched(cpu).inner.lock().run_queue.push_back(..)` holding the lock across a
// push that could reallocate, and the two attempts to make that safe from outside (booking a slot on
// every core at creation, growing the deque outside the lock) are recorded there because both are
// worse than not needing to.
fn enqueue_on(cpu: usize, thread: Arc<Thread>) {
	cpu_sched(cpu).inner.lock().run_queue.push_back(thread);
}

// Allocate the per-core scheduler slots for `count` cores, sized by the MP
// response. Called once by smp::init before any core parks in its idle loop.
pub fn allocate(count: usize) {
	// ALLOC-OK: boot, one run queue per core before any thread exists
	let mut slots: Vec<CpuSched> = Vec::with_capacity(count);
	slots.resize_with(count, CpuSched::new);
	let leaked: &'static mut [CpuSched] = Vec::leak(slots);
	let prev = SCHED.swap(leaked.as_mut_ptr(), Ordering::Release);
	assert!(prev.is_null(), "scheduler slots allocated twice");
}

// The scheduler slot of core `cpu`. The table exists from SMP bring-up on; the
// scheduler is never entered before it (preemption is gated on init()).
fn cpu_sched(cpu: usize) -> &'static CpuSched {
	let base = SCHED.load(Ordering::Acquire);
	assert!(!base.is_null(), "scheduler slots not allocated");
	assert!(cpu < crate::smp::cpu_count(), "cpu id out of range");
	unsafe { &*base.add(cpu) }
}

// A thread blocked in `wait`, parked here (off every run queue) until the object
// it waits on becomes ready or its deadline passes. The Arc keeps the thread
// alive while blocked.
//
// The registry is split for scale: object waits live in WAIT_BUCKETS - an array
// of small per-bucket lists keyed by the object's koid - so waking an object
// locks and scans only that object's bucket, not every blocked thread in the
// system; timed waits additionally register in TIMED_WAITERS, the only list the
// deadline scan and min_deadline touch (most service waits carry no deadline, so
// the timed list stays short). A wake CLAIMS its thread with a Blocked -> Ready
// compare-exchange before enqueueing it, so concurrent wakes through different
// buckets (a wait_any waiter has one entry per object) enqueue the thread exactly
// once; the woken thread removes its own leftover entries when it resumes.
struct BucketWaiter {
	thread: Arc<Thread>,
	koid: u64,
}

// A PERSISTENT entry: this object's wakes are forwarded to that wait set.
//
// The difference from a `BucketWaiter` is its lifetime. A waiter is registered when a thread parks
// and taken out when it wakes; an observer is registered when a member joins a set and taken out
// when it leaves, so a service listening to sixty channels pays sixty registrations once rather
// than sixty per pass. That per-pass cost is what `MAX_CLIENTS` in StorageService is set around.
struct SetObserver {
	member_koid: u64,
	set_koid: u64,
}

// A blocked thread's deadline: an absolute LAPIC tick value. `periodic` marks a
// housekeeping wake (WAIT_PERIODIC): still woken when due, but invisible to
// min_deadline, so run_until_idle settles across it.
struct TimedWaiter {
	thread: Arc<Thread>,
	deadline: u64,
	periodic: bool,
}

const WAIT_BUCKET_COUNT: usize = 64;

static WAIT_BUCKETS: [SpinLock<Vec<BucketWaiter>>; WAIT_BUCKET_COUNT] = [const { SpinLock::new(Vec::new()) }; WAIT_BUCKET_COUNT];
// Bucketed the same way and by the MEMBER's koid, so a wake looks in one place for both kinds.
static SET_OBSERVERS: [SpinLock<Vec<SetObserver>>; WAIT_BUCKET_COUNT] = [const { SpinLock::new(Vec::new()) }; WAIT_BUCKET_COUNT];
// How many observers exist at all, so a wake can skip the lock when no wait set is in use.
//
// `wake_object` is the hottest path in the scheduler - every channel send, every event, every
// process transition goes through it - and taking a second bucket lock on all of them to look for
// observers that a system with no wait sets does not have is a cost paid by everybody for a feature
// nobody is using. It showed up as a cross-core wake taking milliseconds where it takes
// microseconds, which is the one thing the wake IPI exists to prevent.
static OBSERVER_COUNT: AtomicUsize = AtomicUsize::new(0);

// The most wait sets one object's wake will reach. Four is more than anything here needs - a channel
// belongs to one service's set - and it keeps the forwarding on the stack. Enforced at REGISTRATION
// (`register_set_observer`) as well as at delivery, so a set that was told it joined is a set that
// will be woken.
pub const MAX_SETS_PER_OBJECT: usize = 4;
static TIMED_WAITERS: SpinLock<Vec<TimedWaiter>> = SpinLock::new(Vec::new());

fn bucket_of(koid: u64) -> &'static SpinLock<Vec<BucketWaiter>> {
	&WAIT_BUCKETS[(koid % WAIT_BUCKET_COUNT as u64) as usize]
}

fn observers_of(koid: u64) -> &'static SpinLock<Vec<SetObserver>> {
	&SET_OBSERVERS[(koid % WAIT_BUCKET_COUNT as u64) as usize]
}

// Why a member's wakes could not be forwarded to a set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObserveError {
	// This object is already in `MAX_SETS_PER_OBJECT` sets.
	TooManySets,
	// No room to record the registration.
	NoMemory,
}

// Forward `member`'s wakes to `set` until told otherwise. Called by `WaitSet::add`.
//
// Fallible, and the caller must roll back on failure. It used to return `()`: a refused allocation
// printed a warning and returned, `WaitSet::add` - which called it after `members.push` - returned
// `Ok(())`, and `SYS_WAITSET_ADD` answered SUCCESS for a member whose wakes would never reach the
// set. A thread then calling `sys_waitset_wait` with no deadline parks on the set's koid with
// nothing left to rouse it. The readiness scan the old warning appealed to does not rescue that:
// the scan is what happens AFTER a wake, not instead of one.
//
// A warning plus success is the worst of the three answers - the caller cannot see it, cannot
// retry, and the fault presents as a service that is merely slow.
pub fn register_set_observer(member: u64, set: u64) -> Result<(), ObserveError> {
	let mut observers = observers_of(member).lock();
	// The ceiling is enforced HERE, where the registration is made, and not only where the wake is
	// delivered. `wake_object` copies at most `MAX_SETS_PER_OBJECT` set koids into a stack array
	// and warned that the rest would not be woken; nothing counted what was already registered for
	// this member, so five sets could each add the same channel, each be told it worked, and the
	// fifth wait forever. The limit exists for a good reason - it is what keeps the forwarding off
	// the heap, and the allocation it replaced measured 433,645 ns against a 188,821 ns baseline -
	// so the answer is to refuse the fifth `add`, not to raise the ceiling.
	if observers.iter().filter(|observer| observer.member_koid == member).count() >= MAX_SETS_PER_OBJECT {
		return Err(ObserveError::TooManySets);
	}
	if observers.try_reserve(1).is_err() {
		return Err(ObserveError::NoMemory);
	}
	observers.push(SetObserver { member_koid: member, set_koid: set });
	OBSERVER_COUNT.fetch_add(1, Ordering::AcqRel);
	Ok(())
}

pub fn unregister_set_observer(member: u64, set: u64) {
	let mut observers = observers_of(member).lock();
	let before = observers.len();
	observers.retain(|o: &SetObserver| !(o.member_koid == member && o.set_koid == set));
	OBSERVER_COUNT.fetch_sub(before - observers.len(), Ordering::AcqRel);
}

// No-deadline sentinel for `wait`.
pub const NO_DEADLINE: u64 = u64::MAX;

// The kernel address space shared by all kernel threads. Set once at init().
static KERNEL_AS: SpinLock<Option<Arc<AddressSpace>>> = SpinLock::new(None);

// The root resource Domain. Kernel threads are accounted here; it has no quotas,
// so existing behavior is unchanged. Bounded Domains are created explicitly.
static ROOT_DOMAIN: SpinLock<Option<Arc<Domain>>> = SpinLock::new(None);

// The kernel address space's CR3, cached for the scheduler hot path. The
// idle/bootstrap context runs on this; the scheduler restores it when a core goes
// idle so a dead process's page tables are freed while off their own CR3.
static KERNEL_CR3: AtomicU64 = AtomicU64::new(0);

// Whether the timer ISR may preempt. False until init() completes, so the timer
// fires (and counts ticks) before per-CPU state and the scheduler are ready
// without the preempt path touching either. Set once on the BSP at the end of
// init(), by which point init_smp() has set up per-CPU state on every core.
static PREEMPTION_ENABLED: AtomicBool = AtomicBool::new(false);

// Whether init() has completed: the per-CPU scheduler array is allocated, the kernel
// address space captured, and preemption armed. A secondary core spins on this before
// entering cpu_idle_loop, so it never indexes the scheduler before it exists (the x86
// APs are started after init(); the aarch64 secondaries come up before it).
pub fn is_initialized() -> bool {
	PREEMPTION_ENABLED.load(Ordering::Acquire)
}

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
	// Per-CPU state and the scheduler are now up: the timer ISR may preempt.
	PREEMPTION_ENABLED.store(true, Ordering::Release);
	// The timer tick and idle loop now drain the serial ring, so switch serial
	// transmit from synchronous (immediate boot logs) to the asynchronous ring.
	arch::serial::enable_async();
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
// A remote target core is kicked with a wake IPI, so a halted core picks the
// thread up immediately instead of on its next timer tick.
pub fn spawn_on(cpu: usize, entry: extern "C" fn(u64), arg: u64) -> Arc<Thread> {
	let process = Process::new(kernel_as(), root_domain()).expect("out of memory for a kernel thread's process");
	// A KERNEL thread. Nothing here has a caller that could carry a refusal back to
	// somebody who could act on it - these are boot-time and test-time spawns - so an
	// out-of-frames says so and stops. The userspace-reachable path is `thread_create`
	// below, and that one returns None.
	let thread = Thread::new(entry, arg, process).expect("out of memory for a kernel thread stack");
	// ALLOC-OK: the run queue holds one entry per RUNNABLE thread, bounded by the Domain thread quota.
	enqueue_on(cpu, thread.clone());
	if cpu != current_cpu_id() {
		arch::apic::send_wake_ipi(crate::smp::lapic_id(cpu));
	}
	thread
}

// Create a kernel thread on the current core, pre-seeded with a handle to
// `object` (delivered to the thread as its bootstrap-handle argument).
pub fn spawn_with_object(entry: extern "C" fn(u64), object: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> Arc<Thread> {
	let thread = prepare_with_object(entry, object, rights, badge);
	start_thread(&thread);
	thread
}

// Build a thread WITHOUT queueing it to run - the kernel-side twin of the userspace start gate
// (`process_prepare` / `process_release`), where a pipeline's stages must all exist before any
// of them runs. Split out of `spawn_with_object` rather than added beside it, so the two cannot
// drift in how a thread is constructed.
pub fn prepare_with_object(entry: extern "C" fn(u64), object: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> Arc<Thread> {
	let process = Process::new(kernel_as(), root_domain()).expect("out of memory for a kernel thread's process");
	let arg = process.install(object, rights, badge);
	Thread::new(entry, arg, process).expect("out of memory for a kernel thread stack")
}

// Release a prepared thread onto the run queue.
pub fn start_thread(thread: &Arc<Thread>) {
	// ALLOC-OK: the run queue holds one entry per RUNNABLE thread, bounded by the Domain thread quota.
	enqueue_on(current_cpu_id(), thread.clone());
}

// Create a kernel thread accounted to `domain` on the current core, enforcing the
// Domain's thread quota. Returns None (spawning nothing) if the Domain is at its
// thread cap - a clean refusal rather than a crash.
pub fn spawn_in(domain: Arc<Domain>, entry: extern "C" fn(u64), arg: u64) -> Option<Arc<Thread>> {
	let process = Process::new(kernel_as(), domain)?;
	let thread = Thread::new_in(entry, arg, process)?;
	// ALLOC-OK: the run queue holds one entry per RUNNABLE thread, bounded by the Domain thread quota.
	enqueue_on(current_cpu_id(), thread.clone());
	Some(thread)
}

// Create a new process with its own address space, accounted to `domain`. Returns
// None if no frame is available for the address space's top-level page table.
pub fn process_create(domain: Arc<Domain>) -> Option<Arc<Process>> {
	let address_space = AddressSpace::create()?;
	Process::new(address_space, domain)
}

// Create a thread in an existing `process` on the current core's run queue. The
// thread shares the process's address space and handle table with its siblings.
pub fn thread_create(process: Arc<Process>, entry: extern "C" fn(u64), arg: u64) -> Option<Arc<Thread>> {
	let thread = Thread::new(entry, arg, process)?;
	// ALLOC-OK: the run queue holds one entry per RUNNABLE thread, bounded by the Domain thread quota.
	enqueue_on(current_cpu_id(), thread.clone());
	Some(thread)
}

// Create a thread in `process` but leave it suspended - off every run queue - and
// enforce the process Domain's thread quota. The thread does not run until
// thread_start enqueues it. Returns None (charging nothing) if the Domain is at
// its thread cap. The userspace spawn path builds a process's initial thread this
// way so process_create / thread_create / thread_start stay separate, capability-
// gated steps.
pub fn thread_create_suspended(process: Arc<Process>, entry: extern "C" fn(u64), arg: u64) -> Option<Arc<Thread>> {
	Thread::new_in(entry, arg, process)
}

// Enqueue a previously-suspended thread onto the current core's run queue, exactly
// once. Returns false if the thread was already started, so a repeated call is a
// safe no-op rather than a double-enqueue.
pub fn thread_start(thread: Arc<Thread>) -> bool {
	if !thread.try_start() {
		return false;
	}
	thread.set_state(ThreadState::Ready);
	// ALLOC-OK: the run queue holds one entry per RUNNABLE thread, bounded by the Domain thread quota.
	enqueue_on(current_cpu_id(), thread);
	true
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
	cpu_sched(current_cpu_id()).inner.lock().current.clone()
}

// Terminate the calling thread. Never returns.
pub fn exit() -> ! {
	// If this is the last live thread of its process, the process has now terminated:
	// mark it so a holder of its handle waiting on the process-terminated signal wakes.
	// Scoped so the thread Arc is released before we retire - exit() never returns, and
	// holding the Arc across it would pin the thread and keep its slot from being
	// refunded.
	{
		if let Some(thread) = current_thread() {
			// Exactly one thread gets `true` here, whichever order they arrive in - see
			// `thread_exited`. This was a snapshot count of the OTHER live threads, which
			// two threads exiting together both read as non-zero.
			if thread.process().thread_exited() {
				thread.process().mark_exited();
			}
		}
	}
	reschedule(Disposition::Retire);
	// The scheduler always switches away from a retiring thread; reaching here
	// would mean it failed to, so halt rather than run on a corrupt stack.
	arch::halt_loop()
}

// Block the calling thread until woken: register it in the wait registry keyed on
// `koid` (the object whose readiness will wake it) with an absolute tick
// `deadline` (NO_DEADLINE for none), then deschedule. Returns once the thread has
// been woken by wake_object(koid) or check_deadlines() and rescheduled onto a
// core. The caller re-checks its wait condition after each return (a condition-
// variable loop), so spurious or early wakes are harmless.
//
// Holding the current-thread Arc across reschedule(Block) is safe: unlike exit()
// and the fault longjmp, reschedule(Block) RETURNS when the thread is woken, so
// the Arc's destructor still runs normally.
pub fn block_on(koid: u64, deadline: u64) {
	block_on_flagged(koid, deadline, false, || false);
}

// block_on with the periodic marker and a readiness re-check. `ready` is re-evaluated
// AFTER the thread has registered in the wait buckets, closing the classic wait/wake
// race: the caller checks readiness first (interrupts on), then calls this to block, so
// a wake landing in the window between that check and the registration below would scan
// the object's bucket without finding the not-yet-registered thread and be lost. By
// re-checking once registered, a readiness change (and its wake) in that window is
// caught here and the park is aborted. The periodic marker is a housekeeping deadline
// that never counts as pending progress (see TimedWaiter::periodic).
// Make room for one bucket registration, and one timed registration when there is a deadline.
//
// Answers false when the heap will not give it, which is the caller's cue to not park at all.
// Reserving is not the same as pushing: the capacity stays with the vector, so the push that
// follows - with interrupts masked - cannot allocate and cannot fail.
fn reserve_wait_slots(koid: u64, deadline: u64) -> bool {
	reserve_buckets(&[koid]) && (deadline == NO_DEADLINE || TIMED_WAITERS.lock().try_reserve(1).is_ok())
}

// Room for one registration per koid, counted PER BUCKET.
//
// `try_reserve(n)` is measured against the vector's own length, not against the calls before it: it
// means "have room for n more than you hold", so asking for one four times asks for the same one
// place four times. `block_on_any` did exactly that - one `try_reserve(1)` per koid - and then
// pushed once per koid, and `bucket_of` is `koid % WAIT_BUCKET_COUNT`. Two koids in one bucket and
// the second push allocates.
//
// Where it allocates is the point: after `disable_interrupts` and after `begin_park`, on the heap
// the reservation exists to keep out. And it is not contrived to reach - koids are handed out in
// sequence and there are 64 buckets, so a set of eight collides by accident about a third of the
// time.
fn reserve_buckets(koids: &[u64]) -> bool {
	let mut wanted: [usize; WAIT_BUCKET_COUNT] = [0; WAIT_BUCKET_COUNT];
	for &koid in koids {
		wanted[(koid % WAIT_BUCKET_COUNT as u64) as usize] += 1;
	}
	for (index, &count) in wanted.iter().enumerate() {
		if count != 0 && WAIT_BUCKETS[index].lock().try_reserve(count).is_err() {
			return false;
		}
	}
	true
}

pub fn block_on_flagged<F: Fn() -> bool>(koid: u64, deadline: u64, periodic: bool, ready: F) {
	let thread = match current_thread() {
		Some(t) => t,
		None => return,
	};
	// Interrupts stay masked from arming to the switch: a timer preemption between
	// the Blocked store and the park would requeue the thread as Ready and break a
	// waker's claim. reschedule re-disables and, having captured the masked state,
	// leaves interrupts off when the thread resumes; the original state is restored
	// after the cleanup below.
	// Room for the registration BEFORE anything is committed, and before interrupts go off.
	//
	// The pushes below are on the path a `wait` syscall takes, with interrupts masked and a thread
	// half-parked - and an infallible `push` on a short heap ABORTS the kernel. Reserving first
	// turns that into an early return: the caller's condition loop re-checks and calls again, which
	// is a spin rather than a dead machine. It also cannot fail later, because the capacity is
	// already there.
	if !reserve_wait_slots(koid, deadline) {
		return;
	}
	let saved_if = arch::interrupts_enabled();
	arch::disable_interrupts();
	thread.begin_park();
	// Register under the koid even when it is 0 (nothing will wake it by object):
	// the bucket entry's Arc is what keeps a blocked thread alive off every run
	// queue, deadline or not.
	// ALLOC-OK: one entry per BLOCKED thread, bounded by the Domain's thread quota.
	bucket_of(koid).lock().push(BucketWaiter { thread: thread.clone(), koid });
	if deadline != NO_DEADLINE {
		// ALLOC-OK: one entry per sleeping thread, bounded by the Domain's thread quota.
		TIMED_WAITERS.lock().push(TimedWaiter { thread: thread.clone(), deadline, periodic });
	}
	// Re-check now that we are registered. If the object became ready in the race
	// window AND we can reclaim ourselves (try_claim_wake wins the Blocked -> Ready
	// race), abort the park - the caller's condition loop re-checks and proceeds. If
	// try_claim_wake loses, a waker already claimed us and is enqueueing us (it spins
	// for our parked SP), so we must fall through and complete the park to release it.
	if ready() && thread.try_claim_wake() {
		thread.set_state(ThreadState::Running);
		bucket_of(koid).lock().retain(|w: &BucketWaiter| !Arc::ptr_eq(&w.thread, &thread));
		if deadline != NO_DEADLINE {
			TIMED_WAITERS.lock().retain(|w: &TimedWaiter| !Arc::ptr_eq(&w.thread, &thread));
		}
		if saved_if {
			arch::enable_interrupts();
		}
		return;
	}
	reschedule(Disposition::Block);
	// Woken and resumed: remove whatever entries this wait left behind (the waker
	// removed only the one it claimed through).
	bucket_of(koid).lock().retain(|w: &BucketWaiter| !Arc::ptr_eq(&w.thread, &thread));
	if deadline != NO_DEADLINE {
		TIMED_WAITERS.lock().retain(|w: &TimedWaiter| !Arc::ptr_eq(&w.thread, &thread));
	}
	if saved_if {
		arch::enable_interrupts();
	}
}

// Block the calling thread until ANY of `koids` becomes ready (or `deadline`
// passes): register it once per koid, so a wake on any of them returns it. The
// caller re-checks which object is actually ready after each wake (the wait_any
// condition loop), so an early or spurious wake just re-blocks.
pub fn block_on_any<F: Fn() -> bool>(koids: &[u64], deadline: u64, periodic: bool, ready: F) {
	let thread = match current_thread() {
		Some(t) => t,
		None => return,
	};
	// As in `block_on_flagged`: every bucket this will register in gets its room first, so no push
	// below can meet a short heap with interrupts off and a half-parked thread.
	if !reserve_buckets(koids) {
		return;
	}
	if deadline != NO_DEADLINE && TIMED_WAITERS.lock().try_reserve(1).is_err() {
		return;
	}
	let saved_if = arch::interrupts_enabled();
	arch::disable_interrupts();
	thread.begin_park();
	for &koid in koids {
		// ALLOC-OK: one entry per BLOCKED thread, bounded by the Domain's thread quota.
		bucket_of(koid).lock().push(BucketWaiter { thread: thread.clone(), koid });
	}
	if deadline != NO_DEADLINE {
		// ALLOC-OK: one entry per sleeping thread, bounded by the Domain's thread quota.
		TIMED_WAITERS.lock().push(TimedWaiter { thread: thread.clone(), deadline, periodic });
	}
	// Register-then-recheck, closing the wait/wake race across the whole set (see
	// block_on_flagged): if any object became ready in the window since the caller's
	// pre-check and we reclaim ourselves, abort the park.
	if ready() && thread.try_claim_wake() {
		thread.set_state(ThreadState::Running);
		for &koid in koids {
			bucket_of(koid).lock().retain(|w: &BucketWaiter| !Arc::ptr_eq(&w.thread, &thread));
		}
		if deadline != NO_DEADLINE {
			TIMED_WAITERS.lock().retain(|w: &TimedWaiter| !Arc::ptr_eq(&w.thread, &thread));
		}
		if saved_if {
			arch::enable_interrupts();
		}
		return;
	}
	reschedule(Disposition::Block);
	for &koid in koids {
		bucket_of(koid).lock().retain(|w: &BucketWaiter| !Arc::ptr_eq(&w.thread, &thread));
	}
	if deadline != NO_DEADLINE {
		TIMED_WAITERS.lock().retain(|w: &TimedWaiter| !Arc::ptr_eq(&w.thread, &thread));
	}
	if saved_if {
		arch::enable_interrupts();
	}
}

// Wake every thread blocked on object `koid`: claim each matching waiter in the
// object's bucket (the Blocked -> Ready compare-exchange - so a thread waiting on
// several objects at once is enqueued exactly once even when they fire together),
// remove the claimed entries, and enqueue the threads. Entries the thread left in
// other buckets are removed by the thread itself when it resumes.
// Claim and enqueue every thread waiting on `koid`, in bounded batches.
//
// The batch is what makes this allocation-free: waking is what a send, a close and a timer expiry
// all do, and building a vector of the claimed threads meant a short heap could abort the kernel on
// an ordinary event. Sixteen at a time also keeps the bucket lock held for a bounded stretch, which
// the previous shape did not.
fn drain_bucket_into_run_queue(koid: u64) {
	loop {
		let mut batch: [Option<Arc<Thread>>; 16] = [const { None }; 16];
		let taken = {
			let mut bucket = bucket_of(koid).lock();
			let mut taken = 0usize;
			bucket.retain(|w: &BucketWaiter| {
				if taken < batch.len() && w.koid == koid && w.thread.try_claim_wake() {
					batch[taken] = Some(w.thread.clone());
					taken += 1;
					return false;
				}
				true
			});
			taken
		};
		if taken == 0 {
			return;
		}
		for slot in batch.iter_mut().take(taken) {
			if let Some(thread) = slot.take() {
				enqueue(thread);
			}
		}
	}
}

pub fn wake_object(koid: u64) {
	// The sets watching this object, collected before anything is woken and NOT removed - an
	// observer outlives the wake, which is the whole difference between it and a waiter.
	// Into a fixed array, allocating NOTHING.
	//
	// This collected into a `Vec` - a kernel heap allocation on the hottest path in the scheduler,
	// taken on every wake the moment any wait set exists. It was measured rather than reasoned
	// about: a storage service with sixty-two clients answered a round trip in 189 us with no set,
	// and 425 us with a set merely POPULATED and the waiting still done the old way. The waiting
	// mechanism was never the cost; this allocation was.
	//
	// An object in more sets than this array holds keeps the sets past the limit un-woken, which is
	// a wait that only fires when something else happens - said out loud rather than silently,
	// because that is a fault that looks like a slow service.
	let mut sets: [u64; MAX_SETS_PER_OBJECT] = [0; MAX_SETS_PER_OBJECT];
	let mut set_count = 0usize;
	if OBSERVER_COUNT.load(Ordering::Acquire) != 0 {
		let observers = observers_of(koid).lock();
		for observer in observers.iter().filter(|o| o.member_koid == koid) {
			if set_count == sets.len() {
				crate::serial_println!("sched: WARNING: object {koid} is in more than {} wait sets; the rest will not be woken", sets.len());
				break;
			}
			sets[set_count] = observer.set_koid;
			set_count += 1;
		}
	}
	// Bounded batches, and no allocation on a wake path.
	//
	// It built a `Vec` of the threads it had claimed, under the bucket lock, with an infallible
	// `push`: waking is what a send, a close and a timer expiry all do, so a short heap turned an
	// ordinary event into a kernel abort. A fixed batch takes what it can, drops the lock, enqueues,
	// and comes back for more - which also shortens the time the bucket is held.
	drain_bucket_into_run_queue(koid);
	// One level, never a chain: a set may not contain a set, so a set's own wake reaches threads
	// only. `WaitSet::add` is where that is refused.
	for &set in sets.iter().take(set_count) {
		// Same shape, same reason: a set's wake enqueues through the bounded drain rather than
		// collecting into a vector under the lock.
		drain_bucket_into_run_queue(set);
	}
}

// Wake one specific thread if it is currently blocked: claim and enqueue it. A
// no-op if the thread is not blocked (already running or ready), so it cannot be
// double-enqueued. Signal delivery calls this for every thread of the target
// process, so a blocked thread wakes and observes the kill / stop / continue at
// its next scheduling point. The thread's registry entries are removed by the
// thread itself when it resumes.
pub fn wake_thread(thread: &Arc<Thread>) {
	if thread.try_claim_wake() {
		enqueue(thread.clone());
	}
}

// Wake every blocked thread whose deadline has passed (timed out). Called at the
// scheduler's idle points and by the timer path. Scans only the timed list -
// waits without a deadline (most service waits) never appear here.
pub fn check_deadlines() {
	let now = arch::apic::ticks();
	// Bounded batches, for the reason the bucket drain gives: this runs from the timer path, and a
	// vector built there could meet a short heap and abort the kernel on a tick.
	loop {
		let mut batch: [Option<Arc<Thread>>; 16] = [const { None }; 16];
		let taken = {
			let mut timed = TIMED_WAITERS.lock();
			let mut taken = 0usize;
			timed.retain(|w: &TimedWaiter| {
				if taken < batch.len() && w.deadline <= now && w.thread.try_claim_wake() {
					batch[taken] = Some(w.thread.clone());
					taken += 1;
					return false;
				}
				true
			});
			taken
		};
		if taken == 0 {
			return;
		}
		for slot in batch.iter_mut().take(taken) {
			if let Some(thread) = slot.take() {
				enqueue(thread);
			}
		}
	}
}

// The earliest finite deadline that represents pending PROGRESS - periodic
// housekeeping wakes (WAIT_PERIODIC) are excluded, so a service that ticks forever
// never keeps run_until_idle from settling. Expired periodic waits are still woken
// by check_deadlines wherever the scheduler runs it.
fn min_deadline() -> Option<u64> {
	TIMED_WAITERS.lock().iter().filter(|w: &&TimedWaiter| !w.periodic).map(|w: &TimedWaiter| w.deadline).min()
}

// Make a woken thread runnable again on the current core.
fn enqueue(thread: Arc<Thread>) {
	// A freshly claimed thread may still be completing its switch away: the block
	// path zeroes the saved stack pointer before parking and the context switch
	// writes the real value as its very first store. Wait for that store, so no
	// core can ever switch into a half-parked thread. Bounded: the blocker runs
	// its arm-to-switch sequence with interrupts masked, so it cannot stall.
	while thread.kstack_ptr_load() == 0 {
		core::hint::spin_loop();
	}
	thread.set_state(ThreadState::Ready);
	// The WAKER's run queue, which means a woken thread can resume on a different core
	// than it left. That is migration, and it is deliberate: it puts the thread where the
	// data that woke it is warm, and it needs no balancer.
	//
	// It is written down here because parts of this kernel were built as though migration
	// did not happen, and the difference matters for anything a thread carries per-CPU.
	// What has been checked to survive it:
	//
	//   - the saved stack pointer, published with a release above and read with an
	//     acquire, so a core resuming a thread sees a complete register frame;
	//   - the kernel stack, which is the thread's own mapped range and not per-CPU;
	//   - `KERNEL_RSP`, reloaded from the incoming thread on every switch rather than
	//     being a property of the core;
	//   - the interrupt state, restored by the guard that took it, which is now pinned to
	//     one CPU so it cannot be dropped on another.
	//
	// What still does NOT survive it is the TLB: an address space live on two cores has
	// no shootdown, so a thread migrating away from a core leaves translations behind it.
	// That is the open item in Phase 2, and it is the reason migration is not yet safe for
	// a process with threads on several cores rather than a reason to stop migrating.
	// ALLOC-OK: the run queue holds one entry per RUNNABLE thread, bounded by the Domain thread quota.
	enqueue_on(current_cpu_id(), thread);
}

// An optional hook the BSP runs while idle-spinning for the next timed wakeup. It
// keeps a polled input source (the serial console) responsive while the scheduler
// waits out a progress deadline. Set once at boot (a bare fn pointer stored as an
// integer).
static IDLE_HOOK: AtomicU64 = AtomicU64::new(0);

// Register the idle hook the BSP runs while spinning for the next deadline.
pub fn set_idle_hook(hook: fn()) {
	IDLE_HOOK.store(hook as usize as u64, Ordering::Release);
}

// Run the registered idle hook, if any.
fn run_idle_hook() {
	let raw = IDLE_HOOK.load(Ordering::Acquire);
	if raw != 0 {
		let hook: fn() = unsafe { core::mem::transmute::<usize, fn()>(raw as usize) };
		hook();
	}
}

// Run ready threads on the current core until the run queue drains, then return.
// Used by the bootstrap context to drive cooperative kernel threads to completion.
// If the queue drains while threads are blocked with a deadline, spin until the
// nearest PROGRESS deadline and wake them, so a timed wait completes; threads
// blocked with no deadline (waiting on an object nothing will signal here) or with
// only a PERIODIC deadline (a housekeeping tick, WAIT_PERIODIC) are left parked
// and this returns - the caller's standing loop re-enters, and each entry's
// check_deadlines wakes whatever housekeeping came due.
pub fn run_until_idle() {
	let cpu = current_cpu_id();
	loop {
		while !cpu_sched(cpu).inner.lock().run_queue.is_empty() {
			reschedule(Disposition::Requeue);
		}
		// Wake anything already past its deadline - a periodic wait does not count as
		// progress below, but it must still run when due.
		check_deadlines();
		if !cpu_sched(cpu).inner.lock().run_queue.is_empty() {
			continue;
		}
		match min_deadline() {
			Some(deadline) => {
				// Wait for the nearest deadline by HALTING between checks, not busy-spinning.
				// A spinning BSP pegs a host core at 100% AND - because the idle hook polls
				// the serial UART (an `inb` on the LSR) every pass - floods KVM with port-I/O
				// VM-exits that each grab the QEMU big lock, starving the device-emulation /
				// display-encode thread and making the framebuffer console feel laggy. Halting
				// yields the vCPU; the 100 Hz LAPIC timer (and any device IRQ) wakes us within
				// one tick to re-check the run queue, so an IRQ-woken driver (e.g. a virtio RX
				// completion) still runs promptly. The run-queue check drops its lock each pass
				// so the ISR that enqueues the woken thread can run between checks, the idle
				// hook runs each wake so the BSP keeps draining serial TX and polling serial
				// input, and check_deadlines runs each wake so a periodic wait due inside this
				// window still wakes on time.
				while arch::apic::ticks() < deadline && cpu_sched(cpu).inner.lock().run_queue.is_empty() {
					// ANSWER TLB SHOOTDOWNS HERE TOO, for the reason `cpu_idle_loop` gives and this
					// loop did not: a core that requested a shootdown waits for every other core to
					// acknowledge, and this one is sitting in a deadline wait. Relying on the wake
					// IPI's handler is not enough - `mem::tlb::shootdown` says so in its own comment,
					// and names the test that proved it - so the requester spun its two-hundred-
					// million-spin timeout per collision while the BSP halted here without ever
					// looking at its flag.
					//
					// That is an SMP-only deadlock, which is exactly what the evidence said: on
					// `--smp 1` the whole path completes, because `shootdown` returns immediately
					// when there is one core; on eight it hung, differently each run, wherever the
					// collision happened to land. `cpu_idle_loop` has serviced pending requests in
					// its own loop since that fix; this loop is the one that was missed.
					crate::mem::tlb::service_pending();
					run_idle_hook();
					arch::serial::drain_tx();
					check_deadlines();
					arch::idle_halt();
				}
				check_deadlines();
			}
			None => break,
		}
	}
	reap(cpu_sched(cpu));
}

// Idle loop for application processors: run any ready thread, otherwise HALT until
// the next interrupt and re-check. Each AP runs a periodic LAPIC timer (set up in
// arch::init_ap) only to wake it from the halt within one tick, so an idle core
// yields its physical CPU instead of busy-spinning - which, under virtualization,
// would steal host time from the cores doing real work and from the host's own device
// emulation. Work another core enqueues onto this core's run queue (rare - wakeups
// land on the waker's core, not here) is picked up at the next wake.
//
// APs deliberately do NOT touch the wait registry: in this cooperative model
// blocked threads and their deadlines are driven by run_until_idle on the BSP, so
// only the BSP wakes them. A waiter blocked on the BSP must not be stolen onto an
// AP's run queue. True per-core timed waits arrive with preemption.
pub fn cpu_idle_loop() -> ! {
	loop {
		reschedule(Disposition::Requeue);
		// Answer any TLB shootdown before settling. The interrupt handlers service these
		// too; this is the path for a core that was already awake and looping, and on
		// RISC-V it is the only one - its wake IPI has no handler of its own.
		crate::mem::tlb::service_pending();
		// An idle core has nothing better to do than push the serial ring to the wire.
		arch::serial::drain_tx();
		// Lost-wakeup-safe idle: mask interrupts, re-check the run queue under the mask,
		// and only wait if it is still empty. arch::idle_halt is entered with interrupts
		// masked and re-enables them across the wait (x86 `sti; hlt`, aarch64 / riscv WFI
		// wakes on a pending-but-masked interrupt), so a wake event - a cross-core IPI or
		// the timer - that arrives after this check is held pending and delivered by the
		// wait, rather than consumed (and lost) in the gap before it. Without the mask the
		// IPI could run its handler between the check and the wait, and the wait would
		// then sleep until the next tick despite the queued work.
		arch::disable_interrupts();
		if cpu_sched(current_cpu_id()).inner.lock().run_queue.is_empty() {
			arch::idle_halt();
		} else {
			arch::enable_interrupts();
		}
	}
}

// Drop a thread that exited on this core. Runs in the context switched to after
// the exit, so the dead thread's stack is guaranteed no longer in use.
fn reap(sched: &CpuSched) {
	let dead = sched.inner.lock().zombie.take();
	drop(dead);
}

// Preempt the running thread when its time slice expires, rotating to the next
// ready thread on this core. Called from the timer ISR (interrupts disabled, EOI
// already sent). A no-op in the idle context (no current thread) or when no other
// thread is ready, so a sole thread keeps running uninterrupted and the idle loop
// is never disturbed. The quantum is one timer tick (10 ms): a fair per-core round
// robin. Ring-0 and ring-3 threads alike reach here - a ring-3 interrupt frame
// lands on the thread's own kernel stack (per-thread TSS.RSP0), so the preemptive
// switch travels with the thread either way. `from_user` is true when the timer
// interrupted ring 3: user code holds no kernel locks, so a thread whose process
// was killed while it spun in ring 3 is retired right here - the one kill point a
// never-syscalling loop cannot dodge.
pub fn on_timer_preempt(from_user: bool) {
	if !PREEMPTION_ENABLED.load(Ordering::Relaxed) {
		return;
	}
	let sched = cpu_sched(current_cpu_id());
	if from_user {
		// No Arc is held across the never-returning Retire (the closure yields a
		// plain bool), and the lock guard drops at the end of the statement.
		let killed = sched.inner.lock().current.as_ref().is_some_and(|t| t.process().is_killed());
		if killed {
			reschedule(Disposition::Retire);
			// Retire switched away for good; this frame is never resumed.
			arch::halt_loop();
		}
	}
	{
		let inner = sched.inner.lock();
		if inner.current.is_none() || inner.run_queue.is_empty() {
			return;
		}
	}
	reschedule(Disposition::Requeue);
}

// Load `want_cr3` into CR3 unless it is already active. All kernel code and
// stacks live in the higher half, mapped identically in every address space, so
// switching the active address space mid-context-switch keeps the running code
// and both stacks mapped.
fn switch_address_space(want_cr3: u64) {
	if arch::context::read_cr3() != want_cr3 {
		// Refuse to load a CR3 whose kernel half has drifted from the kernel's own, because
		// the alternative is not an error - it is a triple fault. The very next instruction
		// fetch happens through these tables, so a missing kernel mapping faults at the
		// current instruction pointer, the handler needs that same mapping and faults again,
		// and the CPU resets with nothing on the wire. Panicking here names the address space
		// and the entry instead.
		let kernel_cr3 = KERNEL_CR3.load(Ordering::Acquire);
		if kernel_cr3 != 0
			&& want_cr3 != kernel_cr3
			&& let Some((index, theirs, ours)) = arch::paging::kernel_half_divergence(want_cr3, kernel_cr3)
		{
			panic!("address space {want_cr3:#x} diverges from the kernel mapping at PML4[{index}]: {theirs:#x} vs {ours:#x}");
		}
		unsafe { arch::context::write_cr3(want_cr3) };
	}
}

// Core scheduling step: pick the next ready thread and context-switch to it.
fn reschedule(disp: Disposition) {
	// The whole switch runs with interrupts disabled so the timer ISR cannot fire
	// between dropping the run-queue lock and completing switch_context (which would
	// corrupt the half-switched stack). The interrupt flag is not part of the saved
	// context, so capture it here and restore it when this thread is switched back
	// to. A ring-3 syscall runs with interrupts masked (FMASK); a thread preempted
	// by the timer captured resume_if = false and stays masked through the ISR tail,
	// after which iretq restores its real flag.
	let resume_if = arch::interrupts_enabled();
	arch::disable_interrupts();

	let sched = cpu_sched(current_cpu_id());
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
			// Track the incoming thread's parked syscall stack on this core, so a
			// ring-3 syscall it issues after resuming lands on its own kernel stack
			// even though cooperative services share the per-CPU block.
			let new_syscall_rsp = next.syscall_rsp_load();
			// AND WHICH STACK THIS CORE IS ABOUT TO BE ON, so the exception entry can refuse to
			// save a trap frame anywhere else. Published here, with the rest of the incoming
			// thread's per-CPU state and with interrupts already disabled: an exception taken
			// between this and `switch_context` would judge the outgoing stack pointer against the
			// incoming thread's bounds, so the two cannot be separated.
			let new_stack = next.kstack_region();
			guard.current = Some(next);
			drop(guard);
			arch::percpu::set_stack_bounds(new_stack.0, new_stack.1);
			arch::percpu::set_kernel_rsp(new_syscall_rsp);
			// Point TSS.RSP0 at the same parked position, so a ring-3 interrupt taken
			// while this thread runs lands on its own kernel stack (a zero value - a
			// thread that never entered ring 3 - leaves the slot alone; it cannot take
			// a ring-3 interrupt, and usermode::enter sets the slot itself).
			arch::percpu::set_rsp0(new_syscall_rsp);
			switch_address_space(new_cr3);
			unsafe { arch::context::switch_context(old_sp, new_sp) };
			// Resumed on this thread: restore the interrupt state it switched with.
			restore_interrupts(resume_if);
		}
		None => match prev {
			// Idle context with nothing to run: return to the idle loop.
			None => {
				drop(guard);
				restore_interrupts(resume_if);
			}
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
					// The idle context runs on this core's boot stack, which no Thread describes -
					// so the exception entry's stack check is told it does not know, rather than
					// left judging against the bounds of a thread that has just exited.
					arch::percpu::use_idle_stack();
					switch_address_space(KERNEL_CR3.load(Ordering::Acquire));
					unsafe { arch::context::switch_context(old_sp, new_sp) };
				}
				Disposition::Requeue => {
					// Sole runnable thread yielded (or was preempted): keep running
					// it, no switch.
					prev.set_state(ThreadState::Running);
					guard.current = Some(prev);
					drop(guard);
					restore_interrupts(resume_if);
				}
				Disposition::Block => {
					// Blocked with nothing else ready: save our SP and switch to
					// this core's idle context on the kernel address space. The
					// wait registry keeps us alive; we resume right here when woken
					// and rescheduled onto a core.
					let old_sp = prev.kstack_ptr_addr();
					guard.current = None;
					let new_sp = sched.idle_sp.load(Ordering::Acquire);
					drop(guard);
					// Same as the retire path: the idle stack is not a thread's, so the check is
					// disabled rather than aimed at the blocked thread's stack.
					arch::percpu::use_idle_stack();
					switch_address_space(KERNEL_CR3.load(Ordering::Acquire));
					unsafe { arch::context::switch_context(old_sp, new_sp) };
					// Woken and resumed: restore the interrupt state we blocked with.
					restore_interrupts(resume_if);
				}
			},
		},
	}
}

// Restore the interrupt flag captured at the start of a reschedule. Called after
// the run-queue guard has been dropped (the guard's own irq-safe drop leaves
// interrupts disabled, since reschedule disabled them up front).
fn restore_interrupts(resume_if: bool) {
	if resume_if {
		arch::enable_interrupts();
	} else {
		arch::disable_interrupts();
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
					// ALLOC-OK: the run queue holds one entry per RUNNABLE thread, bounded by the Domain thread quota, and a deque that has held its peak never reallocates again.
					inner.run_queue.push_back(prev);
				}
				Disposition::Block => {
					// State is already Blocked; keep the thread off the run queue and
					// the zombie slot. The wait registry holds the Arc that keeps it
					// alive, so dropping this one is fine.
				}
			}
			slot
		}
	}
}

#[cfg(test)]
mod tests;

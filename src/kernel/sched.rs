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
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

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

static SCHED: AtomicPtr<CpuSched> = AtomicPtr::new(core::ptr::null_mut());

// Allocate the per-core scheduler slots for `count` cores, sized by the MP
// response. Called once by smp::init before any core parks in its idle loop.
pub fn allocate(count: usize) {
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
static TIMED_WAITERS: SpinLock<Vec<TimedWaiter>> = SpinLock::new(Vec::new());

fn bucket_of(koid: u64) -> &'static SpinLock<Vec<BucketWaiter>> {
	&WAIT_BUCKETS[(koid % WAIT_BUCKET_COUNT as u64) as usize]
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
	let process = Process::new(kernel_as(), root_domain());
	let thread = Thread::new(entry, arg, process);
	cpu_sched(cpu).inner.lock().run_queue.push_back(thread.clone());
	if cpu != current_cpu_id() {
		arch::apic::send_wake_ipi(crate::smp::lapic_id(cpu));
	}
	thread
}

// Create a kernel thread on the current core, pre-seeded with a handle to
// `object` (delivered to the thread as its bootstrap-handle argument).
pub fn spawn_with_object(entry: extern "C" fn(u64), object: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> Arc<Thread> {
	let process = Process::new(kernel_as(), root_domain());
	let arg = process.install(object, rights, badge);
	let thread = Thread::new(entry, arg, process);
	cpu_sched(current_cpu_id()).inner.lock().run_queue.push_back(thread.clone());
	thread
}

// Create a kernel thread accounted to `domain` on the current core, enforcing the
// Domain's thread quota. Returns None (spawning nothing) if the Domain is at its
// thread cap - a clean refusal rather than a crash.
pub fn spawn_in(domain: Arc<Domain>, entry: extern "C" fn(u64), arg: u64) -> Option<Arc<Thread>> {
	let process = Process::new(kernel_as(), domain);
	let thread = Thread::new_in(entry, arg, process)?;
	cpu_sched(current_cpu_id()).inner.lock().run_queue.push_back(thread.clone());
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
	cpu_sched(current_cpu_id()).inner.lock().run_queue.push_back(thread.clone());
	thread
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
	cpu_sched(current_cpu_id()).inner.lock().run_queue.push_back(thread);
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
			let process = thread.process();
			let others = process.live_threads().iter().filter(|t: &&Arc<Thread>| !Arc::ptr_eq(t, &thread)).count();
			if others == 0 {
				process.mark_exited();
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
	block_on_flagged(koid, deadline, false);
}

// block_on with the periodic marker: a housekeeping deadline that never counts as
// pending progress (see TimedWaiter::periodic).
pub fn block_on_flagged(koid: u64, deadline: u64, periodic: bool) {
	let thread = match current_thread() {
		Some(t) => t,
		None => return,
	};
	// Interrupts stay masked from arming to the switch: a timer preemption between
	// the Blocked store and the park would requeue the thread as Ready and break a
	// waker's claim. reschedule re-disables and, having captured the masked state,
	// leaves interrupts off when the thread resumes; the original state is restored
	// after the cleanup below.
	let saved_if = arch::interrupts_enabled();
	arch::disable_interrupts();
	thread.begin_park();
	// Register under the koid even when it is 0 (nothing will wake it by object):
	// the bucket entry's Arc is what keeps a blocked thread alive off every run
	// queue, deadline or not.
	bucket_of(koid).lock().push(BucketWaiter { thread: thread.clone(), koid });
	if deadline != NO_DEADLINE {
		TIMED_WAITERS.lock().push(TimedWaiter { thread: thread.clone(), deadline, periodic });
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
pub fn block_on_any(koids: &[u64], deadline: u64, periodic: bool) {
	let thread = match current_thread() {
		Some(t) => t,
		None => return,
	};
	let saved_if = arch::interrupts_enabled();
	arch::disable_interrupts();
	thread.begin_park();
	for &koid in koids {
		bucket_of(koid).lock().push(BucketWaiter { thread: thread.clone(), koid });
	}
	if deadline != NO_DEADLINE {
		TIMED_WAITERS.lock().push(TimedWaiter { thread: thread.clone(), deadline, periodic });
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
pub fn wake_object(koid: u64) {
	let woken = {
		let mut bucket = bucket_of(koid).lock();
		let mut woken: Vec<Arc<Thread>> = Vec::new();
		bucket.retain(|w: &BucketWaiter| {
			if w.koid == koid && w.thread.try_claim_wake() {
				woken.push(w.thread.clone());
				return false;
			}
			true
		});
		woken
	};
	for thread in woken {
		enqueue(thread);
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
	let expired = {
		let mut timed = TIMED_WAITERS.lock();
		let mut expired: Vec<Arc<Thread>> = Vec::new();
		timed.retain(|w: &TimedWaiter| {
			if w.deadline <= now && w.thread.try_claim_wake() {
				expired.push(w.thread.clone());
				return false;
			}
			true
		});
		expired
	};
	for thread in expired {
		enqueue(thread);
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
	cpu_sched(current_cpu_id()).inner.lock().run_queue.push_back(thread);
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
// APs deliberately do NOT touch the wait registry: in this cooperative milestone
// blocked threads and their deadlines are driven by run_until_idle on the BSP, so
// only the BSP wakes them. A waiter blocked on the BSP must not be stolen onto an
// AP's run queue. True per-core timed waits arrive with preemption (M19).
pub fn cpu_idle_loop() -> ! {
	loop {
		reschedule(Disposition::Requeue);
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
			guard.current = Some(next);
			drop(guard);
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

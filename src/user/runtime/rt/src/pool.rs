//! WORKER THREADS: the one way a program in this system gets a second thread.
//!
//! A BOUNDED POOL OF WORKERS THAT LIVE AS LONG AS THE PROCESS, and nothing more general. A program
//! asks for a `Pool` of up to N workers and hands it work as items over slices it owns - `for_each`
//! runs a closure once per item, spread over the workers and the calling thread, and returns when
//! every one has run. That is the shape of every consumer this tree has (the software rasteriser's
//! tiles), and it is the shape that needs no thread to END while the program runs, which is what
//! keeps individual thread exit unnecessary - see EXIT below.
//!
//! WHAT A WORKER IS. A thread made with `SYS_THREAD_CREATE` on the process's own handle
//! (`SYS_PROCESS_SELF`), started once, on a stack of its own: a memory object charged to the
//! process's Domain like any other memory, with a READ-ONLY guard page mapped directly beneath it,
//! so a worker that runs off the bottom of its stack faults - which ends the process - instead of
//! writing into whatever happened to be mapped below. The only argument `SYS_THREAD_CREATE` carries
//! is one handle, moved into the new thread's first argument register, and that handle is the
//! channel the worker is rung on. The first thing it hears there is which slot is its own; every
//! order after that it reads from the slot.
//!
//! THE ORDERING CONTRACT IS STATED HERE AND NOT BORROWED. Work goes to a worker through its slot: the
//! order's words are written, the call is opened, and only then is the slot's mail posted, by a
//! sequentially consistent store the worker takes with a sequentially consistent swap and follows
//! with an acquire fence - so whatever the caller wrote before posting is visible to the worker that
//! took the mail. A parked worker is rung on its channel as well, which carries nothing but the
//! request to look. It comes back through the call's count of finished items, each finish a release
//! of what its item wrote and an acquire of every finish before it, and the caller's state or the
//! last finisher's one report - so the caller, past either, has seen every item. None of this leans
//! on the kernel's channel locks, which would stop holding the day a channel became lock-free.
//!
//! A CALL WAITS FOR ITS ITEMS AND NOT FOR EVERY WORKER IT WOKE. A worker whose core halted can be
//! slow to come back - under a hypervisor, far slower than the work it was woken for - and one that
//! arrives after the last item was claimed has nothing to do. So each worker's slot says which call
//! it may join, the pool closes the call once its items are done, and a worker that looks after the
//! close touches nothing of the caller's; the pool waits only for workers already inside.
//!
//! EXIT. The kernel latches the exit status of the FIRST thread to call `SYS_USER_EXIT`, so a worker
//! that ended on its own - with a status of its own, before the program had decided its answer -
//! would overwrite that answer. So a worker never ends on its own. A dropped pool gives its workers
//! back to the process for the next pool; `exit_with` sends every worker the program's own status
//! and each ends with exactly that, so whichever thread the kernel hears first, it hears the program.
//! A thread that ends individually and is joined needs a thread exit that claims no status - a
//! kernel change - and nothing in this tree asks for one.
//!
//! A CRASH ENDS THE PROCESS. A panic on a worker, a panic anywhere while workers exist, and an
//! `exit` called on a worker all KILL the process through its self handle: a worker that crashed
//! and quietly ended would leave the thread waiting for it waiting for ever. A process that never
//! made a worker behaves exactly as it did before this file existed.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering, fence};

use crate::{ERR_BAD_SYSCALL, ERR_INVALID, RIGHT_MAP, RIGHT_READ, Received, SIG_KILL, SYS_THREAD_CREATE, SYS_THREAD_START, SYS_THREAD_START_ON, SYS_USER_EXIT, channel, clock_ns, close, cpu_info, duplicate, map_object, memory_object_create, process_self, recv_blocking, send_blocking, signal, sys_is_err, syscall, unmap_object, yield_now};

/// The most workers one process may hold.
pub const MAX_WORKERS: usize = 64;

/// A worker's stack. A tile of a frame is the deepest thing a worker runs today, and it needs a
/// small fraction of this; the rest is room for the unoptimised builds.
const STACK_BYTES: u64 = 256 * 1024;
const PAGE: u64 = 4096;

/// How many times a guard page may land somewhere other than directly beneath its stack before the
/// worker is not made. See `stack`.
const GUARD_ATTEMPTS: usize = 8;

/// What a worker's channel carries: its slot, once, first; the end of the process; a ring to look
/// at its mail. And what a worker reports on its pool's channel: the last item done.
const SLOT: u8 = 1;
const EXIT: u8 = 2;
const DONE: u8 = 3;
const RING: u8 = 4;
/// `SLOT` or `EXIT`, then one word: the slot's index or the status.
const MESSAGE: usize = 9;

/// How long a worker that has just finished keeps asking for its next order before it parks, unless
/// its pool says otherwise (`Pool::linger`).
///
/// A PARKED WORKER IS A SLOW ONE TO GET BACK. Its core halts, and under a hypervisor - this system's
/// reference machine runs nested - the wake that ends the halt can take far longer than the work it
/// brings. A frame is several calls in a row with short gaps between them, so a worker that stays
/// on its core for a moment after each call takes the next one at once.
///
/// IT ASKS IN USER MODE. It polls its slot's mail with a pause between polls and makes a system
/// call only every `POLLS_PER_YIELD` of them - to read the clock and to yield, so anything else
/// made runnable on its core waits tens of microseconds and not a scheduling tick. Asking through
/// the channel instead - a receive and a yield per poll - was measured (2026-10-12, 32 workers on
/// the nested reference host) to slow everything else the machine was doing at the same moment:
/// the display service's present took 10-12 ms beside polling workers against 7 beside quiet ones.
pub const LINGER_NS: u64 = 2_000_000;

/// How many polls between two looks at the clock - each look a system call - and the yield after it.
const POLLS_PER_YIELD: u32 = 256;

/// The mail that ends a worker with the process's own status, which `leaving` stores first.
const MAIL_EXIT: u64 = u64::MAX;
static EXIT_STATUS: AtomicU64 = AtomicU64::new(0);

/// One worker, as the process knows it.
///
/// A CACHE LINE OF ITS OWN, because its worker polls it: slots sharing a line would have every post
/// to one invalidate the line its neighbour is polling.
#[repr(align(64))]
struct Slot {
	/// The channel end its orders are sent on. ZERO: there is no worker in this slot.
	orders: AtomicU64,
	/// Its stack, guard page included: `[low, high)`.
	low: AtomicU64,
	high: AtomicU64,
	/// Lent to a live pool.
	lent: AtomicBool,
	/// The call this worker may join, or zero. HERE AND NOT IN THE POOL, because a worker can wake to
	/// an order for a call that ended long ago - even for a pool that is gone - and a slot outlives
	/// every pool: what tells the worker the call is over must still be there when it looks.
	open: AtomicU64,
	/// Between taking an order and finishing with it. The pool waits for this to clear before the
	/// call's slices may be released.
	inside: AtomicBool,
	/// The call this worker is asked to join, or zero: posted after the order's words below and taken
	/// by the worker. `MAIL_EXIT` ends it with the process.
	mail: AtomicU64,
	/// The worker is about to block, or is blocked, on its channel, and a post must ring it.
	parked: AtomicBool,
	/// The order: the entry, the dispatch, the worker's lane, and how long it lingers after the call.
	entry: AtomicU64,
	data: AtomicU64,
	lane: AtomicU64,
	linger: AtomicU64,
}

impl Slot {
	const fn new() -> Slot {
		Slot { orders: AtomicU64::new(0), low: AtomicU64::new(0), high: AtomicU64::new(0), lent: AtomicBool::new(false), open: AtomicU64::new(0), inside: AtomicBool::new(false), mail: AtomicU64::new(0), parked: AtomicBool::new(false), entry: AtomicU64::new(0), data: AtomicU64::new(0), lane: AtomicU64::new(0), linger: AtomicU64::new(0) }
	}
}

/// The next call's number. Never zero, which is what a closed slot holds.
static CALLS: AtomicU64 = AtomicU64::new(1);

/// Where the caller of `for_each` is while the last items finish: still asking, parked on its report
/// channel, or past both because the last item is done.
const ASKING: u8 = 0;
const PARKED: u8 = 1;
const FINISHED: u8 = 2;

static SLOTS: [Slot; MAX_WORKERS] = [const { Slot::new() }; MAX_WORKERS];

/// How many slots hold a worker. Slots are filled in order and never emptied.
static WORKERS: AtomicUsize = AtomicUsize::new(0);

/// Held while a worker is being made, so two threads making pools cannot fill one slot twice.
static MAKING: AtomicBool = AtomicBool::new(false);

/// Next logical CPU to try, advanced only after an accepted placed start. `MAKING` serializes it.
/// Start at 1 to leave the usual boot CPU to the caller; the caller itself remains unplaced and
/// may migrate on a completion wake. Offline slots are retried without consuming another thread.
static NEXT_CPU: AtomicUsize = AtomicUsize::new(1);

/// Start one newly made worker on the next available logical CPU. The CPU inventory's values are
/// hardware identities, so only its returned slot count is used. A refused start leaves this same
/// suspended thread available for the next slot; authority/resource failures are not retried.
/// Older kernels without this additive operation keep the previous unplaced start behavior.
fn start_worker(thread: u64) -> bool {
	let count = cpu_info(&mut []);
	if count <= 0 {
		return !sys_is_err(unsafe { syscall(SYS_THREAD_START, thread, 0, 0, 0) });
	}
	let count = count as usize;
	let first = NEXT_CPU.load(Ordering::Relaxed) % count;
	for offset in 0..count {
		let cpu = (first + offset) % count;
		let started = unsafe { syscall(SYS_THREAD_START_ON, thread, cpu as u64, 0, 0) };
		if !sys_is_err(started) {
			NEXT_CPU.store((cpu + 1) % count, Ordering::Relaxed);
			return true;
		}
		match started as i64 {
			ERR_INVALID => continue,
			ERR_BAD_SYSCALL => return !sys_is_err(unsafe { syscall(SYS_THREAD_START, thread, 0, 0, 0) }),
			_ => return false,
		}
	}
	false
}

/// This process's handle to itself, asked for once.
static SELF: AtomicU64 = AtomicU64::new(0);

fn self_handle() -> Option<u64> {
	let held = SELF.load(Ordering::Acquire);
	if held != 0 {
		return Some(held);
	}
	let asked = process_self();
	if asked <= 0 {
		return None;
	}
	match SELF.compare_exchange(0, asked as u64, Ordering::AcqRel, Ordering::Acquire) {
		Ok(_) => Some(asked as u64),
		// Another thread asked first; this handle is a second name for the same authority.
		Err(held) => {
			close(asked as u64);
			Some(held)
		}
	}
}

/// End the whole process now, as killed.
///
/// THE YIELD IS THE KILL POINT. A signal on yourself terminates the process, and the thread that
/// sent it runs on until it next enters the scheduler, which a yield does - so this does not return
/// and does not spin.
pub fn die() -> ! {
	if let Some(me) = self_handle() {
		signal(me, SIG_KILL);
	}
	loop {
		yield_now();
	}
}

/// A stack with a read-only guard page directly beneath it: `(low, high)`, `low` the guard's base.
///
/// THE KERNEL PLACES A MAPPING AND THIS CANNOT CHOOSE WHERE, so the guard is mapped first and the
/// stack is accepted only if it landed directly above it. When it did not - the guard filled a hole
/// the stack did not fit - the guard STAYS, filling that hole, and the stack goes back for another
/// try. After `GUARD_ATTEMPTS` the worker is not made: a stack without a guard is not one this file
/// hands out.
fn stack() -> Option<(u64, u64)> {
	for _ in 0..GUARD_ATTEMPTS {
		let guard = memory_object_create(PAGE);
		if guard < 0 {
			return None;
		}
		// READ-ONLY BECAUSE THE HANDLE IS: the kernel maps a writable page only for a handle that
		// carries `WRITE`, so the guard's own mapping refuses the first push that reaches it.
		let read_only = duplicate(guard as u64, RIGHT_READ | RIGHT_MAP);
		close(guard as u64);
		if read_only < 0 {
			return None;
		}
		let guard_base = unsafe { map_object(read_only as u64) }?;
		let memory = memory_object_create(STACK_BYTES);
		if memory < 0 {
			return None;
		}
		let Some(base) = (unsafe { map_object(memory as u64) }) else {
			close(memory as u64);
			return None;
		};
		if base == guard_base + PAGE {
			return Some((guard_base, base + STACK_BYTES));
		}
		unmap_object(memory as u64);
		close(memory as u64);
	}
	None
}

unsafe extern "C" {
	fn liber_rt_worker_entry();
}

// THE ENTRY STUBS. The kernel starts a thread at `entry` with the stack pointer at the top of the
// stack it was given and the one handle in the first argument register; each stub makes that an
// ordinary call - aligned the way the architecture's ABI expects at a call, no frame to unwind into
// - and traps if the call ever returns, which `worker` does not. HIDDEN, so they add nothing to what
// a shared image exports.
#[cfg(all(target_arch = "x86_64", not(feature = "host-tests")))]
core::arch::global_asm!(".text", ".balign 16", ".global liber_rt_worker_entry", ".hidden liber_rt_worker_entry", "liber_rt_worker_entry:", "and rsp, -16", "xor ebp, ebp", "call {main}", "ud2", main = sym worker);

#[cfg(all(target_arch = "aarch64", not(feature = "host-tests")))]
core::arch::global_asm!(".text", ".balign 4", ".global liber_rt_worker_entry", ".hidden liber_rt_worker_entry", "liber_rt_worker_entry:", "mov x29, xzr", "mov x30, xzr", "bl {main}", "brk #0", main = sym worker);

#[cfg(all(target_arch = "riscv64", not(feature = "host-tests")))]
core::arch::global_asm!(".text", ".balign 4", ".global liber_rt_worker_entry", ".hidden liber_rt_worker_entry", "liber_rt_worker_entry:", "andi sp, sp, -16", "mv s0, zero", "mv ra, zero", "call {main}", "ebreak", main = sym worker);

fn word(bytes: &[u8], at: usize) -> u64 {
	let mut out = [0u8; 8];
	out.copy_from_slice(&bytes[at..at + 8]);
	u64::from_le_bytes(out)
}

/// End this worker with the program's own status, whichever thread the kernel hears first.
fn exit_with_status(status: u64) -> ! {
	unsafe { syscall(SYS_USER_EXIT, status, 0, 0, 0) };
	die();
}

/// This slot's mail, if any is waiting: the call it names - or, for `MAIL_EXIT`, the end.
fn take(slot: &Slot) -> Option<u64> {
	if slot.mail.load(Ordering::Relaxed) == 0 {
		return None;
	}
	match slot.mail.swap(0, Ordering::SeqCst) {
		0 => None,
		MAIL_EXIT => exit_with_status(EXIT_STATUS.load(Ordering::Acquire)),
		call => Some(call),
	}
}

/// Poll in user mode, a pause between polls, until `ready` answers or `until` passes on the clock -
/// which is read, and the core yielded, only every `POLLS_PER_YIELD` polls. `until` of zero polls not
/// at all. Answers what `ready` did, or `None` past `until`.
fn poll<R>(until: u64, mut ready: impl FnMut() -> Option<R>) -> Option<R> {
	let mut polls = 0u32;
	while until != 0 {
		if let Some(answer) = ready() {
			return Some(answer);
		}
		core::hint::spin_loop();
		polls += 1;
		if polls == POLLS_PER_YIELD {
			polls = 0;
			if clock_ns() >= until {
				break;
			}
			yield_now();
		}
	}
	None
}

/// The next call this worker is asked to join: its slot polled until `linger_until`, then waited for.
fn next_call(slot: &Slot, orders: u64, message: &mut [u8; MESSAGE], mut linger_until: u64) -> u64 {
	loop {
		if let Some(call) = poll(linger_until, || take(slot)) {
			return call;
		}
		// PARKED IS SAID BEFORE THE MAIL IS LOOKED AT, and a caller posts before it looks at this: one
		// of the two sees the other, so no call is posted to a worker that then sleeps through it.
		slot.parked.store(true, Ordering::SeqCst);
		if let Some(call) = take(slot) {
			slot.parked.store(false, Ordering::SeqCst);
			return call;
		}
		match recv_blocking(orders, message) {
			Received::Message { len, .. } => {
				slot.parked.store(false, Ordering::SeqCst);
				if len == MESSAGE && message[0] == EXIT {
					exit_with_status(word(message, 1));
				}
				// A RING SAYS ONLY "LOOK", and one rung for mail this worker had already taken finds
				// none: it parks again.
				if let Some(call) = take(slot) {
					return call;
				}
				linger_until = 0;
			}
			// NOBODY CLOSES A WORKER'S CHANNEL: the process is going, or something is badly wrong.
			Received::Closed => die(),
		}
	}
}

/// A worker, for the life of the process: learn its slot, then take an order, carry it out, and take
/// the next.
extern "C" fn worker(orders: u64) -> ! {
	let mut message = [0u8; MESSAGE];
	let slot = match recv_blocking(orders, &mut message) {
		Received::Message { len, .. } if len == MESSAGE && message[0] == SLOT => match SLOTS.get(word(&message, 1) as usize) {
			Some(slot) => slot,
			None => die(),
		},
		// THE PROGRAM ENDED WHILE THIS WORKER WAS BEING MADE.
		Received::Message { len, .. } if len == MESSAGE && message[0] == EXIT => exit_with_status(word(&message, 1)),
		_ => die(),
	};
	let mut linger_until = 0;
	loop {
		let call = next_call(slot, orders, &mut message, linger_until);
		let linger = slot.linger.load(Ordering::Relaxed);
		let mut reported = false;
		// IN BEFORE LOOKING, so a pool that closes the call after this load sees this worker inside and
		// waits for it, and one that closed it before is seen here as closed - which is also what makes
		// stale mail harmless: a call that closed before its worker looked is skipped whole.
		slot.inside.store(true, Ordering::SeqCst);
		if slot.open.load(Ordering::SeqCst) == call {
			fence(Ordering::Acquire);
			// SAFETY: an open call's words are the ones `Pool::for_each` wrote before posting it, whose
			// entry is a `participate` of the dispatch's own types, and the call is still open: the pool
			// does not let the dispatch go while this slot says it is inside.
			let entry = unsafe { core::mem::transmute::<u64, unsafe fn(*const (), usize) -> Option<u64>>(slot.entry.load(Ordering::Relaxed)) };
			// THE LAST ITEM'S FINISHER REPORTS, ONCE PER CALL AND ONLY TO A PARKED CALLER: the pool
			// waits for the items, not for every worker it woke.
			if let Some(report) = unsafe { entry(slot.data.load(Ordering::Relaxed) as *const (), slot.lane.load(Ordering::Relaxed) as usize) } {
				fence(Ordering::Release);
				if !send_blocking(report, &[DONE], 0) {
					die();
				}
				reported = true;
			}
		}
		slot.inside.store(false, Ordering::SeqCst);
		// THE WORKER THAT REPORTED PARKS AT ONCE. Its report woke the caller onto this core - a wake
		// lands on the waker's - and polling here would share the core with the caller for the rest of
		// its frame, and with whatever the caller wakes there next.
		linger_until = if reported { 0 } else { clock_ns().saturating_add(linger) };
	}
}

/// Make one more worker, in the next free slot. `false` when the process cannot.
fn make() -> bool {
	while MAKING.swap(true, Ordering::Acquire) {
		core::hint::spin_loop();
	}
	let made = (|| {
		let index = WORKERS.load(Ordering::Acquire);
		if index >= MAX_WORKERS {
			return false;
		}
		let Some(me) = self_handle() else { return false };
		let Some((low, high)) = stack() else { return false };
		let Some((command, orders)) = channel() else { return false };
		// ITS SLOT IS THE FIRST THING IT HEARS, queued before it can run, so no ring can come first.
		let mut message = [0u8; MESSAGE];
		message[0] = SLOT;
		message[1..9].copy_from_slice(&(index as u64).to_le_bytes());
		if !send_blocking(command, &message, 0) {
			close(command);
			close(orders);
			return false;
		}
		let thread = unsafe { syscall(SYS_THREAD_CREATE, me, liber_rt_worker_entry as *const () as u64, high, orders) };
		if sys_is_err(thread) {
			// The kernel gives a refused handle back where it was, so both ends are still ours.
			close(command);
			close(orders);
			return false;
		}
		let started = start_worker(thread);
		close(thread);
		if !started {
			close(command);
			return false;
		}
		let slot = &SLOTS[index];
		slot.low.store(low, Ordering::Relaxed);
		slot.high.store(high, Ordering::Relaxed);
		slot.orders.store(command, Ordering::Release);
		WORKERS.store(index + 1, Ordering::Release);
		true
	})();
	MAKING.store(false, Ordering::Release);
	made
}

/// Workers lent to one owner, and the channel they report on.
pub struct Pool {
	lent: [u8; MAX_WORKERS],
	count: usize,
	/// How long its workers keep asking for the next call after each one: `LINGER_NS` unless set.
	linger_ns: u64,
	/// Where a worker says it has finished, and where the owner hears it.
	report: u64,
	reports: u64,
}

impl Pool {
	/// A pool of up to `threads` workers BESIDES the calling thread: workers another pool gave back
	/// first, then new ones. It may hold fewer than asked - none, on a process that can make none -
	/// and it still works, because the calling thread does whatever the workers do not.
	pub fn new(threads: usize) -> Pool {
		let (report, reports) = channel().unwrap_or((0, 0));
		let mut pool = Pool { lent: [0; MAX_WORKERS], count: 0, linger_ns: LINGER_NS, report, reports };
		if report == 0 {
			return pool;
		}
		let wanted = threads.min(MAX_WORKERS);
		let mut index = 0;
		while pool.count < wanted && index < MAX_WORKERS {
			if index >= WORKERS.load(Ordering::Acquire) && !make() {
				break;
			}
			let slot = &SLOTS[index];
			if slot.orders.load(Ordering::Acquire) != 0 && !slot.lent.swap(true, Ordering::AcqRel) {
				pool.lent[pool.count] = index as u8;
				pool.count += 1;
			}
			index += 1;
		}
		pool
	}

	/// How many workers this pool holds, not counting the caller.
	pub fn threads(&self) -> usize {
		self.count
	}

	/// How long this pool's workers keep asking for its next call after each one before they park.
	///
	/// A PROGRAM THAT CALLS AGAIN AT A KNOWN RATE says so here: a frame loop whose calls are a frame
	/// apart keeps its workers awake across the gap and pays no wake at the frame's first call, at the
	/// price of the polling in between. Zero parks a worker as soon as it is done.
	pub fn linger(&mut self, ns: u64) {
		self.linger_ns = ns;
	}

	/// Run `work` once for every item, each with a lane of its own for the whole call: the caller
	/// takes lane 0, worker `k` takes lane `k + 1`, and items are claimed one at a time by whoever is
	/// free, so a slow item holds up only the participant that took it. Returns when every item has
	/// run - and NOT when every worker has answered; see the module's note on waiting.
	///
	/// NO MORE PARTICIPANTS THAN LANES: `lanes.len() - 1` workers at most are woken, and a call with
	/// no lanes runs nothing.
	pub fn for_each<L: Send, T: Send>(&self, lanes: &mut [L], items: &mut [T], work: &(dyn Fn(&mut L, &mut T) + Sync)) {
		if lanes.is_empty() {
			return;
		}
		// ONE ITEM IS THE CALLER'S OWN: waking a worker for it would only add the round trip.
		let helpers = if items.len() <= 1 { 0 } else { self.count.min(lanes.len() - 1).min(items.len() - 1) };
		let call = CALLS.fetch_add(1, Ordering::Relaxed);
		let dispatch = Dispatch { lanes: lanes.as_mut_ptr(), items: items.as_mut_ptr(), count: items.len(), next: AtomicUsize::new(0), done: AtomicUsize::new(0), caller: AtomicU8::new(if items.is_empty() { FINISHED } else { ASKING }), report: self.report, work };
		let data = &dispatch as *const Dispatch<'_, L, T> as *const ();
		let entry = participate::<L, T> as unsafe fn(*const (), usize) -> Option<u64> as usize as u64;
		// THE ORDER IS WRITTEN AND THE CALL OPEN BEFORE ANY MAIL GOES, so mail is never early for its
		// own call.
		for (helper, index) in self.lent[..helpers].iter().enumerate() {
			let slot = &SLOTS[*index as usize];
			slot.entry.store(entry, Ordering::Relaxed);
			slot.data.store(data as u64, Ordering::Relaxed);
			slot.lane.store(helper as u64 + 1, Ordering::Relaxed);
			slot.linger.store(self.linger_ns, Ordering::Relaxed);
			slot.open.store(call, Ordering::SeqCst);
		}
		// POSTED, THEN LOOKED AT: a worker found parked is rung, and one that parks after the post finds
		// the mail itself - see `next_call`. A WORKER THAT CANNOT BE RUNG IS SIMPLY NOT WAITED FOR: the
		// items are claimed, not assigned, so the participants that did start take its share.
		for index in &self.lent[..helpers] {
			let slot = &SLOTS[*index as usize];
			slot.mail.store(call, Ordering::SeqCst);
			if slot.parked.load(Ordering::SeqCst) {
				let _ = send_blocking(slot.orders.load(Ordering::Acquire), &[RING], 0);
			}
		}
		// SAFETY: lane 0 is the caller's alone - every worker was given a lane above it. The caller's own
		// finish of the last item asks for no report: it is not parked.
		let _ = unsafe { participate::<L, T>(data, 0) };
		// THE ITEMS ARE WAITED FOR, NOT THE WORKERS, and first by ASKING. A caller that parks is woken by
		// the last item's finisher - a halted core brought back, which nested under a hypervisor costs a
		// millisecond and more - and is moved onto that finisher's core besides. So it asks for as long
		// as the pool's workers would, and parks only past that; the finisher reports only to a caller
		// that did park, so a report is never sent that nobody takes.
		let finished = || (dispatch.caller.load(Ordering::Acquire) == FINISHED).then_some(());
		if poll(clock_ns().saturating_add(self.linger_ns), finished).is_none() && dispatch.caller.compare_exchange(ASKING, PARKED, Ordering::AcqRel, Ordering::Acquire).is_ok() {
			let mut reply = [0u8; 1];
			match recv_blocking(self.reports, &mut reply) {
				Received::Message { len: 1, .. } if reply[0] == DONE => {}
				// A report that will never come is a process that cannot finish this call; it ends
				// here rather than returning while a worker may still hold its slices.
				_ => die(),
			}
		}
		// CLOSE THE CALL, then wait out every worker that got inside before the close: one that
		// looks after it reads the call as over and touches nothing of the caller's. Mail nobody took
		// is taken back, so a worker that comes to it late is not woken for nothing.
		for index in &self.lent[..helpers] {
			let slot = &SLOTS[*index as usize];
			slot.open.store(0, Ordering::SeqCst);
			let _ = slot.mail.compare_exchange(call, 0, Ordering::SeqCst, Ordering::Relaxed);
		}
		for index in &self.lent[..helpers] {
			let slot = &SLOTS[*index as usize];
			let _ = poll(u64::MAX, || (!slot.inside.load(Ordering::SeqCst)).then_some(()));
		}
		fence(Ordering::Acquire);
	}
}

impl Drop for Pool {
	/// The workers go back to the process, parked, for the next pool - see EXIT at the top.
	fn drop(&mut self) {
		for index in &self.lent[..self.count] {
			SLOTS[*index as usize].lent.store(false, Ordering::Release);
		}
		if self.report != 0 {
			close(self.report);
			close(self.reports);
		}
	}
}

/// What every participant in one `for_each` reads: the lanes, the items, and which item is next.
struct Dispatch<'w, L, T> {
	lanes: *mut L,
	items: *mut T,
	count: usize,
	next: AtomicUsize,
	/// Items finished. The participant that takes it to `count` finished the call's last item.
	done: AtomicUsize,
	/// Whether the caller is asking, parked or finished; see `ASKING`.
	caller: AtomicU8,
	/// Where the last item's finisher reports, when that is not the caller.
	report: u64,
	work: &'w (dyn Fn(&mut L, &mut T) + Sync),
}

/// One participant's share of a dispatch: items claimed one at a time until none are left. Answers
/// where to report when this participant finished the call's last item and the caller had parked.
///
/// # Safety
/// `data` must point at a live `Dispatch<L, T>` with more than `lane` lanes, and no other
/// participant may be using lane `lane`. Each item index is claimed by exactly one `fetch_add`, so
/// the `&mut` made for it here is the only one.
unsafe fn participate<L, T>(data: *const (), lane: usize) -> Option<u64> {
	let dispatch = unsafe { &*(data as *const Dispatch<'_, L, T>) };
	let lane = unsafe { &mut *dispatch.lanes.add(lane) };
	let mut last = None;
	loop {
		let index = dispatch.next.fetch_add(1, Ordering::Relaxed);
		if index >= dispatch.count {
			break;
		}
		(dispatch.work)(lane, unsafe { &mut *dispatch.items.add(index) });
		// RELEASE FOR WHAT THIS ITEM WROTE, ACQUIRE FOR EVERY ITEM BEFORE IT: the finisher of the last
		// one has seen all of them, and passes them on - to a caller still asking through the state it
		// reads, to a parked one through the report it is woken by.
		if dispatch.done.fetch_add(1, Ordering::AcqRel) + 1 == dispatch.count && dispatch.caller.swap(FINISHED, Ordering::AcqRel) == PARKED {
			last = Some(dispatch.report);
		}
	}
	last
}

/// Whether the calling thread is one of this process's workers - asked of its own stack pointer,
/// which lies inside exactly one worker's stack or inside none.
fn on_worker() -> bool {
	let here: u64;
	#[cfg(target_arch = "x86_64")]
	unsafe {
		core::arch::asm!("mov {}, rsp", out(reg) here, options(nomem, nostack, preserves_flags))
	};
	#[cfg(target_arch = "aarch64")]
	unsafe {
		core::arch::asm!("mov {}, sp", out(reg) here, options(nomem, nostack, preserves_flags))
	};
	#[cfg(target_arch = "riscv64")]
	unsafe {
		core::arch::asm!("mv {}, sp", out(reg) here, options(nomem, nostack, preserves_flags))
	};
	SLOTS[..WORKERS.load(Ordering::Acquire)].iter().any(|slot| (slot.low.load(Ordering::Relaxed)..slot.high.load(Ordering::Relaxed)).contains(&here))
}

/// The process is ending with `status`: end every worker with the same status first.
///
/// CALLED FROM A WORKER, THIS KILLS THE PROCESS INSTEAD. A worker that ended the program would leave
/// the thread that handed it work waiting for its report, and the process would never finish.
pub(crate) fn leaving(status: u64) {
	let workers = WORKERS.load(Ordering::Acquire);
	if workers == 0 {
		return;
	}
	if on_worker() {
		die();
	}
	// A POLLING WORKER ENDS ON ITS MAIL, a parked one on the message below.
	EXIT_STATUS.store(status, Ordering::Release);
	for slot in &SLOTS[..workers] {
		slot.mail.store(MAIL_EXIT, Ordering::SeqCst);
	}
	let mut order = [0u8; MESSAGE];
	order[0] = EXIT;
	order[1..9].copy_from_slice(&status.to_le_bytes());
	fence(Ordering::Release);
	for slot in &SLOTS[..workers] {
		// A WORKER THAT CANNOT BE TOLD WOULD KEEP THE PROCESS ALIVE after its last ordinary thread
		// ended, so it ends as killed rather than as a process that never finishes.
		if !send_blocking(slot.orders.load(Ordering::Acquire), &order, 0) {
			die();
		}
	}
}

/// A panic is ending a thread: with workers in the process, the process goes with it.
pub(crate) fn crashed() {
	if WORKERS.load(Ordering::Acquire) != 0 {
		die();
	}
}

//! WORKER THREADS: the one way a program in this system gets a second thread.
//!
//! A BOUNDED POOL OF WORKERS THAT LIVE AS LONG AS THE PROCESS, and nothing more general. A program
//! asks for a `Pool` of up to N workers and hands it work as items over slices it owns - `for_each`
//! runs a closure once per item, spread over the workers and the calling thread, and returns when
//! every one has run. That is the shape of every consumer this tree has (the software rasteriser's
//! tiles), and it is the shape that needs no thread to END while the program runs, which is what
//! makes it possible without a kernel change - see EXIT below.
//!
//! WHAT A WORKER IS. A thread made with `SYS_THREAD_CREATE` on the process's own handle
//! (`SYS_PROCESS_SELF`), started once, on a stack of its own: a memory object charged to the
//! process's Domain like any other memory, with a READ-ONLY guard page mapped directly beneath it,
//! so a worker that runs off the bottom of its stack faults - which ends the process - instead of
//! writing into whatever happened to be mapped below. The only argument `SYS_THREAD_CREATE` carries
//! is one handle, moved into the new thread's first argument register, and that handle is the
//! channel the worker takes its orders from. Everything else it learns from the orders.
//!
//! THE ORDERING CONTRACT IS STATED HERE AND NOT BORROWED. Work goes to a worker as a message and
//! comes back as a message, with a release fence before every send and an acquire fence after every
//! receive: whatever a thread wrote before it sent is visible to the thread that received it. The
//! kernel's channel locks would give the same edge today; a contract that leaned on them would stop
//! holding the day a channel became lock-free, and nothing would say so.
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

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence};

use crate::{RIGHT_MAP, RIGHT_READ, Received, SIG_KILL, SYS_THREAD_CREATE, SYS_THREAD_START, SYS_USER_EXIT, channel, close, duplicate, map_object, memory_object_create, process_self, recv_blocking, send_blocking, signal, sys_is_err, syscall, unmap_object, yield_now};

/// The most workers one process may hold.
pub const MAX_WORKERS: usize = 64;

/// A worker's stack. A tile of a frame is the deepest thing a worker runs today, and it needs a
/// small fraction of this; the rest is room for the unoptimised builds.
const STACK_BYTES: u64 = 256 * 1024;
const PAGE: u64 = 4096;

/// How many times a guard page may land somewhere other than directly beneath its stack before the
/// worker is not made. See `stack`.
const GUARD_ATTEMPTS: usize = 8;

const RUN: u8 = 1;
const EXIT: u8 = 2;
const DONE: u8 = 3;
/// `RUN`, then the entry, the dispatch, the lane and the channel to report on: four words.
const ORDER: usize = 40;

/// One worker, as the process knows it.
struct Slot {
	/// The channel end its orders are sent on. ZERO: there is no worker in this slot.
	orders: AtomicU64,
	/// Its stack, guard page included: `[low, high)`.
	low: AtomicU64,
	high: AtomicU64,
	/// Lent to a live pool.
	lent: AtomicBool,
}

impl Slot {
	const fn new() -> Slot {
		Slot { orders: AtomicU64::new(0), low: AtomicU64::new(0), high: AtomicU64::new(0), lent: AtomicBool::new(false) }
	}
}

static SLOTS: [Slot; MAX_WORKERS] = [const { Slot::new() }; MAX_WORKERS];

/// How many slots hold a worker. Slots are filled in order and never emptied.
static WORKERS: AtomicUsize = AtomicUsize::new(0);

/// Held while a worker is being made, so two threads making pools cannot fill one slot twice.
static MAKING: AtomicBool = AtomicBool::new(false);

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

/// A worker, for the life of the process: take an order, carry it out, report, and take the next.
extern "C" fn worker(orders: u64) -> ! {
	let mut order = [0u8; ORDER];
	loop {
		let length = match recv_blocking(orders, &mut order) {
			Received::Message { len, .. } => len,
			// NOBODY CLOSES A WORKER'S ORDERS: the process is going, or something is badly wrong.
			Received::Closed => die(),
		};
		fence(Ordering::Acquire);
		match order[0] {
			RUN if length == ORDER => {
				// SAFETY: a `RUN` is only ever sent by `Pool::for_each`, whose entry is a
				// `participate` of the dispatch's own types and whose dispatch outlives every
				// participant - it waits for this worker's `DONE` before it returns.
				let entry = unsafe { core::mem::transmute::<u64, unsafe fn(*const (), usize)>(word(&order, 8)) };
				unsafe { entry(word(&order, 16) as *const (), word(&order, 24) as usize) };
				fence(Ordering::Release);
				if !send_blocking(word(&order, 32), &[DONE], 0) {
					die();
				}
			}
			EXIT if length == 9 => {
				// THE PROGRAM'S OWN STATUS, whichever thread the kernel hears first.
				unsafe { syscall(SYS_USER_EXIT, word(&order, 1), 0, 0, 0) };
				die();
			}
			_ => die(),
		}
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
		let thread = unsafe { syscall(SYS_THREAD_CREATE, me, liber_rt_worker_entry as *const () as u64, high, orders) };
		if sys_is_err(thread) {
			// The kernel gives a refused handle back where it was, so both ends are still ours.
			close(command);
			close(orders);
			return false;
		}
		let started = unsafe { syscall(SYS_THREAD_START, thread, 0, 0, 0) };
		close(thread);
		if sys_is_err(started) {
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
		let mut pool = Pool { lent: [0; MAX_WORKERS], count: 0, report, reports };
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

	/// Run `work` once for every item, each with a lane of its own for the whole call: the caller
	/// takes lane 0, worker `k` takes lane `k + 1`, and items are claimed one at a time by whoever is
	/// free, so a slow item holds up only the participant that took it. Returns when every item has
	/// run.
	///
	/// NO MORE PARTICIPANTS THAN LANES: `lanes.len() - 1` workers at most are woken, and a call with
	/// no lanes runs nothing.
	pub fn for_each<L: Send, T: Send>(&self, lanes: &mut [L], items: &mut [T], work: &(dyn Fn(&mut L, &mut T) + Sync)) {
		if lanes.is_empty() {
			return;
		}
		let helpers = self.count.min(lanes.len() - 1).min(items.len().saturating_sub(1));
		let dispatch = Dispatch { lanes: lanes.as_mut_ptr(), items: items.as_mut_ptr(), count: items.len(), next: AtomicUsize::new(0), work };
		let data = &dispatch as *const Dispatch<'_, L, T> as *const ();
		let entry = participate::<L, T> as unsafe fn(*const (), usize) as usize as u64;
		fence(Ordering::Release);
		let mut sent = 0;
		for (helper, index) in self.lent[..helpers].iter().enumerate() {
			let mut order = [0u8; ORDER];
			order[0] = RUN;
			order[8..16].copy_from_slice(&entry.to_le_bytes());
			order[16..24].copy_from_slice(&(data as u64).to_le_bytes());
			order[24..32].copy_from_slice(&(helper as u64 + 1).to_le_bytes());
			order[32..40].copy_from_slice(&self.report.to_le_bytes());
			// A WORKER THAT CANNOT BE REACHED IS SIMPLY NOT WAITED FOR: the items are claimed, not
			// assigned, so the participants that did start take its share.
			if send_blocking(SLOTS[*index as usize].orders.load(Ordering::Acquire), &order, 0) {
				sent += 1;
			}
		}
		// SAFETY: lane 0 is the caller's alone - every worker was given a lane above it.
		unsafe { participate::<L, T>(data, 0) };
		for _ in 0..sent {
			let mut reply = [0u8; 1];
			match recv_blocking(self.reports, &mut reply) {
				Received::Message { len: 1, .. } if reply[0] == DONE => {}
				// A worker that will never report is a process that cannot finish this call; it ends
				// here rather than returning while a worker may still hold the caller's slices.
				_ => die(),
			}
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
	work: &'w (dyn Fn(&mut L, &mut T) + Sync),
}

/// One participant's share of a dispatch: items claimed one at a time until none are left.
///
/// # Safety
/// `data` must point at a live `Dispatch<L, T>` with more than `lane` lanes, and no other
/// participant may be using lane `lane`. Each item index is claimed by exactly one `fetch_add`, so
/// the `&mut` made for it here is the only one.
unsafe fn participate<L, T>(data: *const (), lane: usize) {
	let dispatch = unsafe { &*(data as *const Dispatch<'_, L, T>) };
	let lane = unsafe { &mut *dispatch.lanes.add(lane) };
	loop {
		let index = dispatch.next.fetch_add(1, Ordering::Relaxed);
		if index >= dispatch.count {
			break;
		}
		(dispatch.work)(lane, unsafe { &mut *dispatch.items.add(index) });
	}
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
	let mut order = [0u8; 9];
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

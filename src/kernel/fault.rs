// Userspace fault handling: turn a ring-3 CPU fault into process termination.
//
// When ring-3 code faults (a bad pointer dereference, a privileged instruction,
// and so on) the CPU enters the matching exception handler on the per-CPU RSP0
// stack with interrupts masked and the kernel GS base still loaded (M8's
// no-swapgs design). The handler decides, from the saved code selector, whether
// the fault came from ring 3. If it did, it records the fault on the running
// process and longjmps back into the kernel thread that dropped to ring 3,
// reusing the same one-way return path as a clean SYS_USER_EXIT.
//
// The kernel thread resumes right after its `usermode::enter` call as if the
// excursion had returned, then unwinds normally; dropping the thread tears the
// process down and refunds its Domain. The kernel - and every other core - keeps
// running. A ring-0 fault is a real kernel bug and is left to halt loudly.

use crate::arch;
use crate::mem::frame::{self, PAGE_SIZE};
use crate::memlayout::USER_STACK_TOP;
use crate::object::KernelObject;
use crate::object::channel::{Channel, Message};
use crate::sched;
use crate::sync::SpinLock;
use alloc::sync::Arc;
use alloc::vec::Vec;

// Fault kinds recorded for a terminated process. Kept as plain u64 tags (rather
// than an enum) so a FaultInfo marshals cleanly across the syscall boundary.
pub const FAULT_PAGE: u64 = 1;
pub const FAULT_GENERAL_PROTECTION: u64 = 2;

// Page-fault error-code bit 0: set when the fault is a protection violation on a
// PRESENT page (never stack growth), clear when the page was simply not mapped.
const PF_PRESENT: u64 = 1;

// The hard floor of the stack span, in pages: the lowest page below the ceiling
// is never demand-mapped, so runaway recursion dies there instead of eating the
// machine page by page.
const STACK_GUARD_PAGES: u64 = 1;

// Try to satisfy a ring-3 page fault as stack growth: a not-present fault inside
// the faulting process's stack span - below USER_STACK_TOP, above the hard floor
// its Domain's per-thread stack ceiling (PROP_STACK_LIMIT) fixes - means the
// stack grew into the demand-paged region. Map a zeroed page there and return
// true: the exception handler then just returns and the faulting instruction
// retries. Anything else (a protection fault, an address outside the span, no
// memory left) returns false and the caller terminates the process as before.
// The faulting thread was in ring 3, so it holds no kernel locks - taking the
// frame-allocator and page-table locks here cannot deadlock against it.
pub fn grow_user_stack(address: u64, error_code: u64) -> bool {
	if error_code & PF_PRESENT != 0 {
		return false;
	}
	let Some(thread) = sched::current_thread() else {
		return false;
	};
	let process = thread.process();
	// The span the Domain policy grants: [top - ceiling, top), with the lowest
	// STACK_GUARD_PAGES never mapped. A ceiling larger than the address space
	// below the top is clamped so the floor cannot underflow.
	let ceiling = process.domain().account().stack().limit().min(USER_STACK_TOP);
	let floor = USER_STACK_TOP - ceiling + STACK_GUARD_PAGES * PAGE_SIZE;
	if address < floor || address >= USER_STACK_TOP {
		return false;
	}
	let Some(new_frame) = frame::allocate() else {
		return false;
	};
	let hhdm = crate::mem::hhdm_offset();
	unsafe {
		core::ptr::write_bytes((hhdm + new_frame) as *mut u8, 0, PAGE_SIZE as usize);
	}
	let page = address & !(PAGE_SIZE - 1);
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER | arch::paging::NO_EXECUTE;
	process.address_space().map(page, new_frame, flags);
	process.adopt_frames(alloc::vec![new_frame]);
	process.charge_stack(PAGE_SIZE);
	true
}

// A snapshot of the fault that terminated a process, readable back through
// SYS_FAULT_INFO_GET. `#[repr(C)]` and all-u64 so userspace can overlay it on a
// raw buffer without surprises.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct FaultInfo {
	pub kind: u64,
	pub error_code: u64,
	// Faulting address: CR2 for a page fault, 0 for a general-protection fault.
	pub address: u64,
	pub instruction_pointer: u64,
}

// The channel the kernel sends a crash record on when a userspace process faults.
// A supervisor (the future ServiceManager) registers the receiving peer; until
// one is set the notification is simply dropped. Each record is 16 bytes: the
// crashed process's koid and the fault kind, both u64 little-endian.
static CRASH_NOTIFY: SpinLock<Option<Arc<Channel>>> = SpinLock::new(None);

// Register the endpoint the kernel sends crash records on (the kernel-held sender
// of a channel whose peer the supervisor receives on). Replaces any previous one.
#[allow(dead_code)]
pub fn set_crash_notify(channel: Arc<Channel>) {
	*CRASH_NOTIFY.lock() = Some(channel);
}

// Clear the crash-notify registration.
#[allow(dead_code)]
pub fn clear_crash_notify() {
	*CRASH_NOTIFY.lock() = None;
}

// Send a crash record for process `koid` (fault `kind`) to the registered notify
// endpoint, if any. Best-effort: a full or closed channel drops the record, since
// the kernel must neither block nor fail on the fault path.
fn notify_crash(koid: u64, kind: u64) {
	let channel = CRASH_NOTIFY.lock().clone();
	if let Some(channel) = channel {
		let mut bytes: Vec<u8> = Vec::with_capacity(16);
		bytes.extend_from_slice(&koid.to_le_bytes());
		bytes.extend_from_slice(&kind.to_le_bytes());
		let _ = channel.send(Message::new(bytes, Vec::new(), 0));
	}
}

// SMAP/SMEP probe: the test suite arms a designated address, then deliberately
// dereferences (or jumps into) user memory from ring 0. The resulting ring-0 page
// fault is the EXPECTED refusal: the handler recognizes the armed address, records
// the fault's error code, and retires the probing kernel thread instead of halting
// the machine. The probe body must hold nothing that needs dropping across the
// faulting access (the handler exits the thread, abandoning its frames).
#[cfg(test)]
static SMAP_PROBE_ADDR: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static SMAP_PROBE_CODE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// Arm the probe for one expected ring-0 fault at `addr`.
#[cfg(test)]
pub fn arm_smap_probe(addr: u64) {
	SMAP_PROBE_CODE.store(0, core::sync::atomic::Ordering::SeqCst);
	SMAP_PROBE_ADDR.store(addr, core::sync::atomic::Ordering::SeqCst);
}

// Called from the ring-0 page-fault branch: true if this fault is the armed probe
// (recording its error code and disarming), in which case the handler retires the
// probing thread rather than halting.
#[cfg(test)]
pub fn smap_probe_trip(cr2: u64, error_code: u64) -> bool {
	use core::sync::atomic::Ordering;
	let armed = SMAP_PROBE_ADDR.load(Ordering::SeqCst);
	if armed == 0 || cr2 != armed {
		return false;
	}
	SMAP_PROBE_ADDR.store(0, Ordering::SeqCst);
	// Record error_code + 1 so a zero code is still distinguishable from "no hit".
	SMAP_PROBE_CODE.store(error_code + 1, Ordering::SeqCst);
	true
}

// The recorded probe fault: Some(error_code) once the armed access faulted.
#[cfg(test)]
pub fn smap_probe_hit() -> Option<u64> {
	let code = SMAP_PROBE_CODE.load(core::sync::atomic::Ordering::SeqCst);
	if code == 0 { None } else { Some(code - 1) }
}

// Record `info` on the current process and longjmp back to the kernel thread
// that entered ring 3. Called from the exception handlers for a ring-3 fault;
// never returns to its caller (the abandoned RSP0 exception frame is reclaimed
// from the TSS on the next ring transition).
//
// Nothing that needs dropping may be held across the longjmp: exit_to_kernel
// returns to the kernel thread with a raw `ret` that abandons this stack frame
// without running destructors, so a live Arc here would leak - pinning the thread
// and its process and leaking every resource they hold. So the thread reference is
// looked up, used, and explicitly dropped before the longjmp.
pub fn terminate_user(info: FaultInfo) -> ! {
	let have_thread = match sched::current_thread() {
		Some(thread) => {
			let process = thread.process();
			process.set_fault(info);
			let koid = process.header().koid();
			// Eagerly tear the crashed process's capabilities down - detaching its
			// IRQ, refunding its DMA and memory, and removing every handle - rather
			// than waiting for the thread to be reaped, so a supervisor can reclaim
			// and restart it at once. Then notify the registered supervisor.
			process.terminate();
			notify_crash(koid, info.kind);
			drop(thread);
			true
		}
		None => false,
	};
	if have_thread {
		// Unwind to enter's caller, exactly like a clean SYS_USER_EXIT.
		arch::usermode::exit_to_kernel();
	}
	// A ring-3 fault implies a thread drove the excursion, so reaching here should
	// be impossible; with no parked stack to longjmp to, fail loudly instead.
	crate::serial_println!("fault: ring-3 fault with no current thread, halting");
	arch::halt_loop()
}

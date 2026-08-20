// Userspace fault handling: turn a ring-3 CPU fault into process termination.
//
// When ring-3 code faults (a bad pointer dereference, a privileged instruction,
// and so on) the CPU enters the matching exception handler on the per-CPU RSP0
// stack with interrupts masked and the kernel GS base still loaded (a no-swapgs
// design). The handler decides, from the saved code selector, whether
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
// EVERY OTHER SYNCHRONOUS EXCEPTION A RING-3 INSTRUCTION CAN RAISE (KERN-ARCH-004).
//
// Page faults and #GP were the only two that looked at where the fault came from; a divide by zero,
// a `ud2`, a bound-range or alignment or SIMD fault took the fatal path whatever the privilege
// level. So any unprivileged process could stop every CPU with one instruction.
//
// `FAULT_EXCEPTION` is the catch-all for the vectors with no name of their own, and it carries the
// vector number in `FaultInfo::address` - which is otherwise CR2 for a page fault and 0 for a #GP -
// because "which exception" is the first thing a reader of a crash record wants and there is
// nowhere else in the record to put it without changing its shape.
pub const FAULT_DIVIDE: u64 = 3;
pub const FAULT_INVALID_OPCODE: u64 = 4;
pub const FAULT_BREAKPOINT: u64 = 5;
pub const FAULT_EXCEPTION: u64 = 6;

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
	// Out of frames for an intermediate page table: hand the frame back and refuse
	// the growth (the caller terminates the process) instead of panicking the kernel.
	if process.address_space().try_map(page, new_frame, flags).is_err() {
		// SAFETY: allocated a few lines above, never mapped (the map is what just failed),
		// and not handed to the process.
		// NEVER-MAPPED: the mapping is exactly what failed, so no page table on any core ever
		// referenced this frame.
		unsafe { frame::deallocate(new_frame) };
		// Unless somebody else already mapped it - which is the ordinary outcome when two threads
		// of one process fault on the same stack page at once. The loser used to return false and
		// the fault handler reads that as unhandled, so the process was killed for a page that had
		// just been provided for it. Nothing was wrong except the order the two faults arrived in.
		//
		// The permissions are checked as well as the presence: a mapping that is there but not
		// writable-user is not the growth this thread needed, and treating it as one would hand the
		// thread straight back into the same fault.
		// The fault is taken in the faulting thread's own address space, so this reads the
		// mapping that matters without switching anything.
		if arch::paging::translate_flags(page).is_some_and(|mapped| mapped & arch::paging::WRITABLE != 0 && mapped & arch::paging::USER != 0) {
			return true;
		}
		return false;
	}
	// FALLIBLY, and this is a page fault: `adopt_frames(alloc::vec![new_frame])` allocated a
	// one-element vector to record the frame, so a short heap aborted the kernel on a path ring 3
	// reaches by touching a guard page.
	if !process.try_adopt_frame(new_frame) {
		// Unmap what was just mapped and give the frame back, then report the fault unhandled: the
		// process is killed for a stack it cannot grow, which is a refusal rather than a halt.
		let _ = process.address_space().unmap(page);
		// THROUGH THE RETIREMENT DOOR, because this frame WAS in a page table.
		//
		// `unmap` is `arch::paging::unmap_page_in` and nothing else: the PTE is cleared and the
		// local core invalidates its own TLB. Another core running in the same address space keeps
		// its translation until a shootdown tells it to drop one, so `deallocate` here handed a
		// still-translated frame back to the allocator - a physical use-after-free reachable from
		// ring 3 by touching a guard page while the kernel heap is short, which is exactly the
		// condition the fallible adoption above was added for. `retire` is the rule this module
		// states for anything a page table ever pointed at, and this rollback was written without
		// it. The `deallocate` a few lines above is correct: that frame's mapping is what FAILED, so
		// no core ever had a translation for it.
		//
		// SAFETY: allocated in this function, mapped only into `page` in this address space, and
		// that mapping was just removed. Nothing else holds it - the adoption is what failed.
		unsafe { frame::retire(&[new_frame]) };
		return false;
	}
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
	// Faulting address: CR2 for a page fault, the exception vector for `FAULT_EXCEPTION`, and 0
	// for everything else.
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
	// ALLOC-OK: an `Option<Arc<Channel>>` out of the guard - a refcount bump. Nothing is copied,
	// which matters here more than elsewhere: this is the fault path.
	let channel = CRASH_NOTIFY.lock().clone();
	if let Some(channel) = channel {
		// FALLIBLY, on the FAULT path - which runs when a process has just died, a plausible moment
		// for the heap to be short, and where this function has already promised it "must neither
		// block nor fail". `Vec::with_capacity` aborts.
		let mut bytes: Vec<u8> = Vec::new();
		if bytes.try_reserve_exact(16).is_err() {
			return;
		}
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
			// The report belongs here rather than in each architecture's trap handler, and
			// for two reasons that are really one. Only x86_64 printed anything, so a
			// process could die on riscv64 and leave nothing in the log at all - a full
			// suite run there showed no fault line even where the deliberate crash tests
			// clearly faulted. And only here is the process in hand, so only here can the
			// message say which one it was: an instruction pointer alone is unattributable
			// when every EXEC image shares a load base.
			let kind = match info.kind {
				FAULT_PAGE => "page fault",
				FAULT_GENERAL_PROTECTION => "general protection fault",
				FAULT_DIVIDE => "divide error",
				FAULT_INVALID_OPCODE => "invalid opcode",
				FAULT_BREAKPOINT => "breakpoint",
				_ => "exception",
			};
			// BORROWED. This used to be `header().name()`, which clones the label into a fresh
			// `String` - an allocation taken while handling a ring-3 fault, which is the moment a
			// short heap is most likely and least survivable.
			process.header().with_name(|name| {
				crate::serial_println!("fault: ring-3 {} (code {:#x}) at {:#x}, addr {:#x} - terminating process koid={} ({})", kind, info.error_code, info.instruction_pointer, info.address, koid, name.unwrap_or("unnamed"));
			});
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

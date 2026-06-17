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
use crate::sched;

// Fault kinds recorded for a terminated process. Kept as plain u64 tags (rather
// than an enum) so a FaultInfo marshals cleanly across the syscall boundary.
pub const FAULT_PAGE: u64 = 1;
pub const FAULT_GENERAL_PROTECTION: u64 = 2;

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
			thread.process().set_fault(info);
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

// Portable thread bootstrap - the first Rust a freshly scheduled kernel thread runs.
//
// Each arch backend fabricates a new thread's initial stack so the first
// `switch_context` into it returns to that arch's `thread_trampoline` (in
// `arch::context`), which loads the entry pointer + argument into the argument
// registers and calls `thread_bootstrap` here. The bootstrap is identical on every
// architecture - only the trampoline asm around it (register names, frame layout) is
// arch-specific - so it lives in one place and every backend's trampoline links to
// this single `#[no_mangle]` symbol.

// First Rust code a freshly scheduled thread runs. Calls the thread entry with its
// argument; when the entry returns, the thread exits and never comes back.
#[unsafe(no_mangle)]
extern "C" fn thread_bootstrap(entry: u64, arg: u64) -> ! {
	// New threads start preemptible. The scheduler switches into a thread with
	// interrupts disabled (it disables them across every context switch); a thread
	// resumed from a yield gets its interrupt flag restored by the scheduler, but a
	// brand-new thread returns straight into the trampoline instead, so it must
	// enable interrupts itself to match.
	crate::arch::enable_interrupts();
	let entry_fn: extern "C" fn(u64) = unsafe { core::mem::transmute(entry) };
	entry_fn(arg);
	crate::sched::exit()
}

// Does a syscall made from a KERNEL thread give the caller its registers back?
//
// Nothing asserted that until now, and the evidence that it might not is specific: with probes
// inside `a_process_load_whose_image_goes_away_is_an_error_rather_than_a_dead_kernel`, the guest
// printed two clean lines from the spawned thread, then made its first syscall, and from that point
// every line was mangled - `0x100000001` came out as garbage, a counter that should have been in
// single digits printed as 1837, and the log degenerated into binary noise before the machine
// stopped. Corrupted formatting after a call is what a clobbered register file looks like from the
// outside.
//
// This is the smallest experiment that can tell. It does not touch the exception table, the ELF
// loader, the page tables or the scheduler - all of which were suspected in turn and cleared - and
// it does not need a fault to happen. It fills the callee-saved registers with values it chose,
// makes the most trivial syscall in the table, and reads them back.
//
// `SYS_DEBUG_NOOP` is the right call precisely because it does nothing: it returns its first
// argument and touches no object, no handle and no memory. If registers do not survive THAT, the
// damage is in the call path itself and not in anything a real syscall goes on to do.
//
// AArch64 only, and deliberately: `arch::syscall::invoke` is a plain function call here - it routes
// straight to `syscall_dispatch` with no `svc` and no trap frame - so what this checks is that the
// ordinary Rust/C ABI contract holds across it. x19-x28 are the callee-saved general registers; x18
// is the platform register and x29/x30 are the frame pointer and link register, so the block leaves
// all three alone. The asm saves and restores the ten it uses, because the compiler is entitled to
// have its own values in them.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// The pattern each register is loaded with. Distinct per register, so a failure says WHICH one moved
// rather than only that something did, and high enough that a stray small integer landing in one is
// obvious rather than plausible.
const PATTERN: [u64; 10] = [0xA19, 0xA20, 0xA21, 0xA22, 0xA23, 0xA24, 0xA25, 0xA26, 0xA27, 0xA28];

// The callee this test calls THROUGH, so the asm block has an ordinary `bl` target with the C ABI
// rather than an inlined body the compiler could reorder around the register loads.
extern "C" fn noop_syscall() {
	unsafe { crate::arch::syscall::invoke(crate::syscall::SYS_DEBUG_NOOP, 0, 0, 0, 0) };
}

crate::tagged_test!(a_syscall_from_a_kernel_thread_gives_the_caller_its_registers_back, [Kernel, Syscall, ArchAarch64], id = "kernel.arch.aarch64.a_syscall_from_a_kernel_thread_gives_the_caller_its_registers_back", covers = ["kernel"]);
fn a_syscall_from_a_kernel_thread_gives_the_caller_its_registers_back() {
	static RAN: AtomicBool = AtomicBool::new(false);
	static OBSERVED: [AtomicU64; 10] = [const { AtomicU64::new(0) }; 10];

	extern "C" fn body(_bootstrap: u64) {
		let mut seen = [0u64; 10];
		unsafe {
			// The three pointers are SPILLED before x19-x28 are touched, and that ordering is the
			// whole trick. `clobber_abi("C")` tells the compiler the call destroys every
			// caller-saved register, so the only registers left for it to put `pat`, `out` and
			// `call` in are the callee-saved ones - which is exactly the set this test overwrites.
			// The first version loaded the patterns straight over them and then dereferenced a
			// pattern as a pointer; it faulted at address 0x1c on the first run. Spilling first and
			// reloading through x9, a caller-saved scratch reloaded after the call, keeps every
			// pointer readable at the moment it is needed.
			core::arch::asm!(
				"sub sp, sp, #112",
				// The compiler may be using x19-x28; give them back exactly as found.
				"stp x19, x20, [sp, #0]",
				"stp x21, x22, [sp, #16]",
				"stp x23, x24, [sp, #32]",
				"stp x25, x26, [sp, #48]",
				"stp x27, x28, [sp, #64]",
				"str {pat}, [sp, #80]",
				"str {out}, [sp, #88]",
				"str {call}, [sp, #96]",
				// Load the patterns from the array rather than as immediates, so the values are the
				// ones this test named and not something an assembler had to round.
				"ldr x9, [sp, #80]",
				"ldp x19, x20, [x9, #0]",
				"ldp x21, x22, [x9, #16]",
				"ldp x23, x24, [x9, #32]",
				"ldp x25, x26, [x9, #48]",
				"ldp x27, x28, [x9, #64]",
				"ldr x9, [sp, #96]",
				"blr x9",
				// Whatever is in them NOW is the answer.
				"ldr x9, [sp, #88]",
				"stp x19, x20, [x9, #0]",
				"stp x21, x22, [x9, #16]",
				"stp x23, x24, [x9, #32]",
				"stp x25, x26, [x9, #48]",
				"stp x27, x28, [x9, #64]",
				"ldp x19, x20, [sp, #0]",
				"ldp x21, x22, [sp, #16]",
				"ldp x23, x24, [sp, #32]",
				"ldp x25, x26, [sp, #48]",
				"ldp x27, x28, [sp, #64]",
				"add sp, sp, #112",
				pat = in(reg) PATTERN.as_ptr(),
				out = in(reg) seen.as_mut_ptr(),
				call = in(reg) noop_syscall as extern "C" fn(),
				clobber_abi("C"),
			);
		}
		for (slot, value) in OBSERVED.iter().zip(seen) {
			slot.store(value, Ordering::SeqCst);
		}
		RAN.store(true, Ordering::SeqCst);
	}

	crate::sched::spawn(body, 0);
	crate::sched::run_until_idle();
	assert!(RAN.load(Ordering::SeqCst), "the probe thread ran at all");

	for (index, expected) in PATTERN.iter().enumerate() {
		let seen = OBSERVED[index].load(Ordering::SeqCst);
		assert_eq!(seen, *expected, "x{} did not survive a kernel-thread syscall: put in {expected:#x}, got {seen:#x} - the call path is clobbering a callee-saved register, which corrupts every caller that keeps anything in one", 19 + index);
	}
}

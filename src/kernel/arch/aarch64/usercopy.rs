// Copies to and from userspace that may FAULT, and say how far they got. See
// `crate::extable` for why they exist and `arch/x86_64/usercopy.rs` for the same routines on the
// architecture that has a string instruction to do it in one.
//
// A byte at a time, and the entries are per LOAD and per STORE: either can be the one that faults,
// because the user address is the source in one direction and the destination in the other. Both
// resume at the same exit, where `n` holds the count still to go.
//
// Byte-at-a-time is the slow shape and it is chosen for being the obviously correct one. A word loop
// would be eight times fewer iterations and needs an alignment prologue, an alignment epilogue and
// four table entries instead of two - worth doing if a measurement ever says the copy is the cost,
// and not worth guessing at first. What is NOT negotiable is that every instruction touching the
// user address is in the table: one that is not takes the kernel down on the fault this exists for.

use core::arch::asm;

// Copy `len` bytes from kernel memory at `src` to the user address `dst`. Returns how many arrived.
//
// SAFETY: `src` must be a readable kernel buffer of at least `len` bytes. `dst` is not required to
// be anything - being wrong about it is the case this exists for.
pub unsafe fn copy_to_user(dst: u64, src: *const u8, len: usize) -> usize {
	if len == 0 {
		return 0;
	}
	let mut remaining: usize = len;
	let mut to: u64 = dst;
	let mut from: *const u8 = src;
	let mut byte: u64;
	unsafe {
		asm!(
			"2:",
			"ldrb {byte:w}, [{from}], #1",
			"3:",
			"strb {byte:w}, [{to}], #1",
			"subs {n}, {n}, #1",
			"b.ne 2b",
			"4:",
			".pushsection .extable,\"a\",@progbits",
			".balign 8",
			".quad 2b",
			".quad 4b",
			".quad 3b",
			".quad 4b",
			".popsection",
			n = inout(reg) remaining,
			to = inout(reg) to,
			from = inout(reg) from,
			byte = out(reg) byte,
			options(nostack)
		);
	}
	let _ = (to, from, byte);
	len - remaining
}

// Copy `len` bytes from the user address `src` into kernel memory at `dst`. Returns how many were
// read.
//
// SAFETY: `dst` must be a writable kernel buffer of at least `len` bytes.
pub unsafe fn copy_from_user(dst: *mut u8, src: u64, len: usize) -> usize {
	if len == 0 {
		return 0;
	}
	let mut remaining: usize = len;
	let mut to: *mut u8 = dst;
	let mut from: u64 = src;
	let mut byte: u64;
	unsafe {
		asm!(
			"2:",
			"ldrb {byte:w}, [{from}], #1",
			"3:",
			"strb {byte:w}, [{to}], #1",
			"subs {n}, {n}, #1",
			"b.ne 2b",
			"4:",
			".pushsection .extable,\"a\",@progbits",
			".balign 8",
			".quad 2b",
			".quad 4b",
			".quad 3b",
			".quad 4b",
			".popsection",
			n = inout(reg) remaining,
			to = inout(reg) to,
			from = inout(reg) from,
			byte = out(reg) byte,
			options(nostack)
		);
	}
	let _ = (to, from, byte);
	len - remaining
}

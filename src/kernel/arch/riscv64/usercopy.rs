// Copies to and from userspace that may FAULT, and say how far they got. See
// `crate::extable` for why they exist, and `arch/aarch64/usercopy.rs` for the same byte loop and
// the same reason for choosing it.
//
// SSTATUS.SUM is set at boot here, so an S-mode load or store may reach a U-mapped page without
// bracketing. What it does NOT do is make the access safe: the page can go away between the check
// and the copy, and then the load faults in S-mode with no handler willing to resume it. That is
// what the table entries below are for.

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
			"lb {byte}, 0({from})",
			"3:",
			"sb {byte}, 0({to})",
			"addi {from}, {from}, 1",
			"addi {to}, {to}, 1",
			"addi {n}, {n}, -1",
			"bnez {n}, 2b",
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
			"lb {byte}, 0({from})",
			"3:",
			"sb {byte}, 0({to})",
			"addi {from}, {from}, 1",
			"addi {to}, {to}, 1",
			"addi {n}, {n}, -1",
			"bnez {n}, 2b",
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

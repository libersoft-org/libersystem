// Copies to and from userspace that may FAULT, and say how far they got.
//
// `user_buf_ok` validates a range and then the copy happens, and between those two moments another
// thread in the same process can unmap the page. The check cannot close that window - it is
// inherent to checking a thing and then using it - so the copy has to survive the fault instead.
//
// Every instruction below that touches a user address is registered in `.extable` with a fixup, so
// the page-fault handler resumes the copy at its exit instead of treating the fault as the kernel
// bug it would otherwise be. See `crate::extable`.
//
// `rep movsb` is the whole trick on this architecture. It is one instruction, so the table needs
// ONE entry per routine rather than one per load and store; and when it faults, RCX holds exactly
// the count still to go, which is the partial-copy answer the caller needs. The fixup target is the
// instruction after it - there is nothing to undo, because the bytes that were copied were copied.

use core::arch::asm;

// Copy `len` bytes from kernel memory at `src` to the user address `dst`.
//
// Returns how many bytes ARRIVED. Short means the destination stopped being writable partway, which
// is a userspace race and not a kernel fault; the caller decides what a partial write means to it.
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
	unsafe {
		asm!(
			"2:",
			"rep movsb byte ptr es:[rdi], byte ptr [rsi]",
			"3:",
			// The entry: this instruction may fault, and resumes at the next one. `.balign 8` keeps
			// the section a whole number of entries however the assembler lays the code out.
			".pushsection .extable,\"a\",@progbits",
			".balign 8",
			".quad 2b",
			".quad 3b",
			".popsection",
			inout("rcx") remaining,
			inout("rdi") to,
			inout("rsi") from,
			options(nostack)
		);
	}
	let _ = (to, from);
	len - remaining
}

// Copy `len` bytes from the user address `src` into kernel memory at `dst`.
//
// Returns how many bytes were READ. Short means the source stopped being readable partway.
//
// SAFETY: `dst` must be a writable kernel buffer of at least `len` bytes.
pub unsafe fn copy_from_user(dst: *mut u8, src: u64, len: usize) -> usize {
	if len == 0 {
		return 0;
	}
	let mut remaining: usize = len;
	let mut to: *mut u8 = dst;
	let mut from: u64 = src;
	unsafe {
		asm!(
			"2:",
			"rep movsb byte ptr es:[rdi], byte ptr [rsi]",
			"3:",
			".pushsection .extable,\"a\",@progbits",
			".balign 8",
			".quad 2b",
			".quad 3b",
			".popsection",
			inout("rcx") remaining,
			inout("rdi") to,
			inout("rsi") from,
			options(nostack)
		);
	}
	let _ = (to, from);
	len - remaining
}

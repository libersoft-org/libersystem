// Kernel thread context switching (riscv64).
//
// switch_context saves the callee-saved registers of the running thread onto its
// stack, stores the stack pointer into *old_sp, loads new_sp, restores the new
// thread's callee-saved registers, and returns into it. A brand-new thread is given
// a fabricated stack frame (see init_thread_stack) whose return address (ra) is
// thread_trampoline, so the first switch into it lands in Rust at thread_bootstrap
// (the shared arch::common::context symbol).
//
// The RISC-V callee-saved set: ra (the return address the switch resumes into), sp,
// s0..s11, and the callee-saved FP registers fs0..fs11 (the kernel is built with the
// D extension, so these are preserved across a switch). SSTATUS.FS is enabled at boot
// so the fsd / fld here do not trap.

#![allow(dead_code)]

use core::arch::{asm, global_asm};

unsafe extern "C" {
	// Save the current context to *old_sp and resume the context at new_sp.
	pub fn switch_context(old_sp: *mut u64, new_sp: u64);
	// Return target baked into a new thread's initial stack frame.
	fn thread_trampoline();
}

global_asm!(
	r#"
.text
.global switch_context
switch_context:
	// Frame: ra, s0..s11 (13 * 8), fs0..fs11 (12 * 8) = 200 B, padded to 208 (16-align).
	addi    sp, sp, -208
	sd      ra,   0*8(sp)
	sd      s0,   1*8(sp)
	sd      s1,   2*8(sp)
	sd      s2,   3*8(sp)
	sd      s3,   4*8(sp)
	sd      s4,   5*8(sp)
	sd      s5,   6*8(sp)
	sd      s6,   7*8(sp)
	sd      s7,   8*8(sp)
	sd      s8,   9*8(sp)
	sd      s9,  10*8(sp)
	sd      s10, 11*8(sp)
	sd      s11, 12*8(sp)
	fsd     fs0, 13*8(sp)
	fsd     fs1, 14*8(sp)
	fsd     fs2, 15*8(sp)
	fsd     fs3, 16*8(sp)
	fsd     fs4, 17*8(sp)
	fsd     fs5, 18*8(sp)
	fsd     fs6, 19*8(sp)
	fsd     fs7, 20*8(sp)
	fsd     fs8, 21*8(sp)
	fsd     fs9, 22*8(sp)
	fsd     fs10, 23*8(sp)
	fsd     fs11, 24*8(sp)
	sd      sp, 0(a0)            // *old_sp = sp
	mv      sp, a1              // load the incoming stack pointer
	ld      ra,   0*8(sp)
	ld      s0,   1*8(sp)
	ld      s1,   2*8(sp)
	ld      s2,   3*8(sp)
	ld      s3,   4*8(sp)
	ld      s4,   5*8(sp)
	ld      s5,   6*8(sp)
	ld      s6,   7*8(sp)
	ld      s7,   8*8(sp)
	ld      s8,   9*8(sp)
	ld      s9,  10*8(sp)
	ld      s10, 11*8(sp)
	ld      s11, 12*8(sp)
	fld     fs0, 13*8(sp)
	fld     fs1, 14*8(sp)
	fld     fs2, 15*8(sp)
	fld     fs3, 16*8(sp)
	fld     fs4, 17*8(sp)
	fld     fs5, 18*8(sp)
	fld     fs6, 19*8(sp)
	fld     fs7, 20*8(sp)
	fld     fs8, 21*8(sp)
	fld     fs9, 22*8(sp)
	fld     fs10, 23*8(sp)
	fld     fs11, 24*8(sp)
	addi    sp, sp, 208
	ret                         // return into the resumed thread (ra)

.global thread_trampoline
thread_trampoline:
	// The initial frame leaves the entry pointer in s0 and its argument in s1.
	mv      a0, s0
	mv      a1, s1
	call    thread_bootstrap
	ebreak
"#
);

// `thread_bootstrap` (the Rust entry the trampoline above calls) is the portable
// arch::common::context symbol - identical on every arch, so it lives there.

// Build the initial kernel stack for a new thread. The frame is laid out so the first
// switch_context into it restores entry into s0 and arg into s1, then returns (ra)
// into thread_trampoline. Returns the initial saved stack pointer.
pub fn init_thread_stack(stack: &mut [u8], entry: extern "C" fn(u64), arg: u64) -> u64 {
	let base = stack.as_mut_ptr() as u64;
	// 16-byte align the top so the trampoline runs with ABI-correct alignment.
	let top = (base + stack.len() as u64) & !0xf;
	let sp = top - 208;
	let trampoline: unsafe extern "C" fn() = thread_trampoline;
	unsafe {
		let frame = sp as *mut u64;
		frame.add(0).write(trampoline as usize as u64); // ra -> thread_trampoline
		frame.add(1).write(entry as usize as u64); // restored into s0
		frame.add(2).write(arg); // restored into s1
		for i in 3..26 {
			frame.add(i).write(0); // s2..s11, fs0..fs11, pad
		}
	}
	sp
}

// Read the active address-space token: the Sv39 root table's physical address (the
// portable "cr3" the scheduler saves/restores per process). SATP.PPN << 12.
pub fn read_cr3() -> u64 {
	let satp: u64;
	unsafe {
		asm!("csrr {}, satp", out(reg) satp, options(nomem, nostack, preserves_flags));
	}
	(satp & 0xFFF_FFFF_FFFF) << 12
}

// Switch the active address space to the Sv39 root at `root_phys` (an Sv39 root the
// kernel built) and flush the TLB. The kernel half of every address space is
// identical (the shared high-half megapages), so kernel code and stacks stay mapped.
//
// SAFETY: `root_phys` must be a valid Sv39 root whose high half maps the kernel.
pub unsafe fn write_cr3(root_phys: u64) {
	let satp = (8u64 << 60) | (root_phys >> 12); // mode 8 = Sv39
	unsafe {
		asm!(
			"csrw satp, {satp}",
			"sfence.vma",
			satp = in(reg) satp,
			options(nostack, preserves_flags),
		);
	}
}

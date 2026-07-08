// riscv64 U-mode (userspace) entry / return + the embedded ring-3 probe programs.
//
// `riscv64_enter_umode(entry, user_sp, arg, resume_slot)` saves the kernel's
// callee-saved registers onto the current (per-thread) kernel stack, parks that
// resume stack pointer in `*resume_slot` (the calling thread's syscall_rsp slot),
// points SSCRATCH at the kernel trap stack (so a trap from U-mode switches stacks in
// `__trap_entry`), sets SEPC / SSTATUS for a return to U-mode with interrupts enabled,
// and `sret`s down to U-mode with a0 = arg. It does not return here; when the U-mode
// program makes SYS_USER_EXIT, `riscv64_trap` calls `exit_to_kernel`, which reloads
// the parked block and `ret`s - unwinding straight back into `enter`. The resume
// state is per-thread (on each thread's own stack, addressed by its own slot), so
// several U-mode threads can be mid-excursion at once.
//
// S-mode interrupts are masked (SSTATUS.SIE = 0) across the transition so a timer
// interrupt cannot fire in the window where SSCRATCH is armed but the hart is still
// in S-mode; `sret` restores SIE from SPIE = 1, so U-mode itself is preemptible.

#![allow(dead_code)]

use core::arch::global_asm;

pub const FAULT_PROBE_ADDR: u64 = 0x0dea_d000;

global_asm!(
	r#"
.section .text, "ax"
.global riscv64_enter_umode
riscv64_enter_umode:                // a0=entry a1=user_sp a2=arg a3=resume_slot
	csrci   sstatus, 2              // clear SSTATUS.SIE (no S-mode IRQ during the switch)
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
	sd      sp, 0(a3)               // *resume_slot = kernel resume sp
	csrw    sscratch, sp            // kernel trap sp for the U-mode trap path
	csrr    t0, sstatus
	li      t1, 0x100
	not     t1, t1
	and     t0, t0, t1              // SSTATUS.SPP = 0 -> sret returns to U-mode
	ori     t0, t0, 0x20           // SSTATUS.SPIE = 1 -> SIE = 1 after sret
	csrw    sstatus, t0
	csrw    sepc, a0               // U-mode entry pc
	mv      sp, a1                 // U-mode stack pointer
	mv      a0, a2                 // arg -> a0
	sret

.global riscv64_exit_umode
riscv64_exit_umode:                 // a0 = parked resume sp
	mv      sp, a0
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
	ret
"#
);

unsafe extern "C" {
	fn riscv64_enter_umode(entry: u64, user_sp: u64, arg: u64, resume_slot: *mut u64);
	fn riscv64_exit_umode(resume_sp: u64) -> !;
}

// Drop to U-mode at `entry` with sp = `user_stack` and a0 = `arg`. The call "returns"
// here when the U-mode program makes SYS_USER_EXIT. The resume state is parked in the
// calling thread's syscall_rsp slot, so concurrent U-mode threads do not clobber one
// another. S-mode interrupts are re-enabled on return (the trap that unwound us here
// left them masked).
pub unsafe fn enter(entry: u64, user_stack: u64, arg: u64) {
	let slot = match crate::sched::current_thread() {
		Some(thread) => thread.syscall_rsp_addr(),
		None => return,
	};
	unsafe { riscv64_enter_umode(entry, user_stack, arg, slot) }
	super::enable_interrupts();
	if let Some(thread) = crate::sched::current_thread() {
		thread.set_syscall_rsp(0);
	}
}

// Unwind from a U-mode syscall back to the kernel that called `enter`, using the
// current thread's parked resume pointer.
pub fn exit_to_kernel() -> ! {
	let resume = crate::sched::current_thread().map_or(0, |thread| thread.syscall_rsp_load());
	unsafe { riscv64_exit_umode(resume) }
}

// The embedded ring-3 probe programs the kernel test suite runs in U-mode (mirrors the
// x86_64 / aarch64 usermode probes). Each returns its position-independent RV64GC
// instruction bytes, copied into a USER page before entering U-mode.
pub fn program_bytes() -> &'static [u8] {
	as_bytes(&PROGRAM_BASIC)
}
pub fn program_fault_bytes() -> &'static [u8] {
	as_bytes(&PROGRAM_FAULT)
}
pub fn program_yield_bytes() -> &'static [u8] {
	as_bytes(&PROGRAM_YIELD)
}
pub fn program_nx_bytes() -> &'static [u8] {
	as_bytes(&PROGRAM_NX)
}
pub fn program_stack_probe_bytes() -> &'static [u8] {
	as_bytes(&PROGRAM_STACK_PROBE)
}
pub fn program_spin_bytes() -> &'static [u8] {
	as_bytes(&PROGRAM_SPIN)
}

// Reinterpret a program's 32-bit instruction words as the little-endian byte slice the
// test harness copies into a USER page (RISC-V is little-endian, so the words are
// already in instruction-fetch order).
fn as_bytes(words: &'static [u32]) -> &'static [u8] {
	unsafe { core::slice::from_raw_parts(words.as_ptr() as *const u8, core::mem::size_of_val(words)) }
}

// RV64 register ABI numbers used below.
const ZERO: u32 = 0;
const SP: u32 = 2;
const T0: u32 = 5;
const S1: u32 = 9;
const A0: u32 = 10;
const A1: u32 = 11;
const A2: u32 = 12;
const A3: u32 = 13;
const A7: u32 = 17;

// RV32I/RV64I instruction encoders (const, so the syscall numbers and immediates bake
// in at compile time). The syscall ABI: a7 = number, a0..a3 = arguments, a0 = result.
const fn addi(rd: u32, rs1: u32, imm: i32) -> u32 {
	(((imm as u32) & 0xfff) << 20) | (rs1 << 15) | (rd << 7) | 0x13
}
const fn li(rd: u32, imm: i32) -> u32 {
	addi(rd, ZERO, imm)
}
const fn mv(rd: u32, rs: u32) -> u32 {
	addi(rd, rs, 0)
}
const fn lui(rd: u32, imm20: u32) -> u32 {
	((imm20 & 0xf_ffff) << 12) | (rd << 7) | 0x37
}
const fn store(rs2: u32, rs1: u32, off: i32, funct3: u32) -> u32 {
	let o = off as u32;
	((o >> 5 & 0x7f) << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | ((o & 0x1f) << 7) | 0x23
}
const fn sb(rs2: u32, rs1: u32, off: i32) -> u32 {
	store(rs2, rs1, off, 0b000)
}
const fn sd(rs2: u32, rs1: u32, off: i32) -> u32 {
	store(rs2, rs1, off, 0b011)
}
const fn ld(rd: u32, rs1: u32, off: i32) -> u32 {
	(((off as u32) & 0xfff) << 20) | (rs1 << 15) | (0b011 << 12) | (rd << 7) | 0x03
}
const fn jr(rs1: u32) -> u32 {
	(rs1 << 15) | 0x67 // jalr x0, rs1, 0
}
const fn branch(rs1: u32, rs2: u32, off: i32, funct3: u32) -> u32 {
	let o = off as u32;
	((o >> 12 & 1) << 31) | ((o >> 5 & 0x3f) << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | ((o >> 1 & 0xf) << 8) | ((o >> 11 & 1) << 7) | 0x63
}
const fn bne(rs1: u32, rs2: u32, off: i32) -> u32 {
	branch(rs1, rs2, off, 0b001)
}
const fn beq(rs1: u32, rs2: u32, off: i32) -> u32 {
	branch(rs1, rs2, off, 0b000)
}
const ECALL: u32 = 0x0000_0073;
const J_SELF: u32 = 0x0000_006f; // jal x0, 0 -> spin in place

use crate::syscall::{SYS_CHANNEL_SEND, SYS_DEBUG_WRITE, SYS_USER_EXIT, SYS_YIELD};

// Basic ring-3 probe: SYS_CHANNEL_SEND(a0 = handle, "OK", 2, 0), SYS_DEBUG_WRITE('U'),
// SYS_USER_EXIT. a0 arrives as the bootstrap Channel handle.
static PROGRAM_BASIC: [u32; 19] = [
	mv(S1, A0),        // s1 = handle
	addi(SP, SP, -16), // scratch for "OK"
	li(T0, 0x4f),      // 'O'
	sb(T0, SP, 0),
	li(T0, 0x4b), // 'K'
	sb(T0, SP, 1),
	mv(A0, S1), // a0 = handle
	mv(A1, SP), // a1 = ptr
	li(A2, 2),  // len 2
	li(A3, 0),  // xfer 0
	li(A7, SYS_CHANNEL_SEND as i32),
	ECALL,
	li(A0, 0x55), // 'U'
	li(A1, 0),    // len 0 (single-byte debug write)
	li(A7, SYS_DEBUG_WRITE as i32),
	ECALL,
	li(A7, SYS_USER_EXIT as i32),
	ECALL,
	J_SELF,
];

// Cooperative-yield probe: save the handle, SYS_YIELD x3 (so two instances on one core
// interleave), then send "OK" and exit.
static PROGRAM_YIELD: [u32; 22] = [
	mv(S1, A0),
	li(A7, SYS_YIELD as i32),
	ECALL,
	li(A7, SYS_YIELD as i32),
	ECALL,
	li(A7, SYS_YIELD as i32),
	ECALL,
	addi(SP, SP, -16),
	li(T0, 0x4f),
	sb(T0, SP, 0),
	li(T0, 0x4b),
	sb(T0, SP, 1),
	mv(A0, S1),
	mv(A1, SP),
	li(A2, 2),
	li(A3, 0),
	li(A7, SYS_CHANNEL_SEND as i32),
	ECALL,
	li(A7, SYS_USER_EXIT as i32),
	ECALL,
	J_SELF,
	J_SELF,
];

// Fault probe: store to FAULT_PROBE_ADDR (unmapped) to raise a store page fault from
// U-mode. FAULT_PROBE_ADDR = 0x0dead000, so `lui` alone materializes it (low 12 = 0).
static PROGRAM_FAULT: [u32; 3] = [
	lui(A0, 0x0dead), // a0 = 0x0dead000
	sd(A0, A0, 0),    // [a0] = a0 -> store page fault
	J_SELF,
];

// No-execute probe: jump into the writable, no-execute stack page. The instruction
// fetch there faults (W^X) before a byte executes.
static PROGRAM_NX: [u32; 3] = [
	addi(A0, SP, -64), // a0 = sp - 64 (inside the stack page)
	jr(A0),            // fetch from a NO_EXECUTE page -> instruction page fault
	J_SELF,
];

// Stack-growth probe: a0 = page count. Store one qword per page walking DOWN from the
// entry stack pointer, then exit cleanly (or fault at the Domain's stack floor). RV64
// addi cannot encode -4096, so each step subtracts 2048 twice.
static PROGRAM_STACK_PROBE: [u32; 8] = [
	mv(A1, SP),          // a1 = sp
	addi(A1, A1, -2048), // a1 -= 4096 (two steps)
	addi(A1, A1, -2048),
	sd(A1, A1, 0),      // touch the page
	addi(A0, A0, -1),   // count--
	bne(A0, ZERO, -16), // loop back 4 insns while a0 != 0
	li(A7, SYS_USER_EXIT as i32),
	ECALL,
];

// CPU-bound spinner: a0 = shared data page. [a0] is a stop flag another thread raises
// through the frame's kernel mapping, [a0 + 8] a counter this loop bumps so an observer
// sees it running. It makes no syscall until the flag is set.
static PROGRAM_SPIN: [u32; 7] = [
	ld(A1, A0, 8),      // a1 = [a0 + 8]
	addi(A1, A1, 1),    // a1 += 1
	sd(A1, A0, 8),      // [a0 + 8] = a1
	ld(A2, A0, 0),      // a2 = [a0] (stop flag)
	beq(A2, ZERO, -16), // loop back 4 insns while the flag is 0
	li(A7, SYS_USER_EXIT as i32),
	ECALL,
];

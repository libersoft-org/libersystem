// riscv64 traps - the S-mode trap vector (STVEC) and fault decode (M117).
//
// RISC-V has ONE trap entry (STVEC, direct mode): every exception and delegated
// interrupt enters `__trap_entry` in S-mode with the cause in SCAUSE, the faulting
// address / bad instruction in STVAL, and the return PC in SEPC. The entry saves the
// integer register frame + SEPC + SSTATUS, calls `riscv64_trap`, restores, and
// `sret`s back. OpenSBI delegates the S-mode-relevant traps to us via medeleg (page
// faults, breakpoint, U-mode ecall); M-mode keeps the rest (S-mode ecall = SBI).
//
// The fault split the kernel wants: a U-mode (userspace) fault terminates only the
// faulting process (the resumable stack-growth fault + terminate_user land with the
// usermode path in a later increment); an S-mode (kernel) fault is a bug and halts.
//
// Frame layout (34 u64 slots, 272 bytes): [0]=x0(unused) [1]=ra [2]=sp [3]=gp
// [4]=tp [5..31]=x5..x31 [32]=sepc [33]=sstatus.

use core::arch::global_asm;

global_asm!(
	r#"
.section .text.trap, "ax"
.balign 4
.global __trap_entry
__trap_entry:
	addi    sp, sp, -272
	sd      x1,  1*8(sp)
	addi    t0, sp, 272             // the pre-trap sp
	sd      t0,  2*8(sp)
	sd      x3,  3*8(sp)
	sd      x4,  4*8(sp)
	sd      x5,  5*8(sp)
	sd      x6,  6*8(sp)
	sd      x7,  7*8(sp)
	sd      x8,  8*8(sp)
	sd      x9,  9*8(sp)
	sd      x10, 10*8(sp)
	sd      x11, 11*8(sp)
	sd      x12, 12*8(sp)
	sd      x13, 13*8(sp)
	sd      x14, 14*8(sp)
	sd      x15, 15*8(sp)
	sd      x16, 16*8(sp)
	sd      x17, 17*8(sp)
	sd      x18, 18*8(sp)
	sd      x19, 19*8(sp)
	sd      x20, 20*8(sp)
	sd      x21, 21*8(sp)
	sd      x22, 22*8(sp)
	sd      x23, 23*8(sp)
	sd      x24, 24*8(sp)
	sd      x25, 25*8(sp)
	sd      x26, 26*8(sp)
	sd      x27, 27*8(sp)
	sd      x28, 28*8(sp)
	sd      x29, 29*8(sp)
	sd      x30, 30*8(sp)
	sd      x31, 31*8(sp)
	csrr    t0, sepc
	sd      t0, 32*8(sp)
	csrr    t1, sstatus
	sd      t1, 33*8(sp)
	csrr    a0, scause
	csrr    a1, stval
	mv      a2, sp
	call    riscv64_trap
	ld      t0, 32*8(sp)
	csrw    sepc, t0
	ld      t1, 33*8(sp)
	csrw    sstatus, t1
	ld      x1,  1*8(sp)
	ld      x3,  3*8(sp)
	ld      x4,  4*8(sp)
	ld      x5,  5*8(sp)
	ld      x6,  6*8(sp)
	ld      x7,  7*8(sp)
	ld      x8,  8*8(sp)
	ld      x9,  9*8(sp)
	ld      x10, 10*8(sp)
	ld      x11, 11*8(sp)
	ld      x12, 12*8(sp)
	ld      x13, 13*8(sp)
	ld      x14, 14*8(sp)
	ld      x15, 15*8(sp)
	ld      x16, 16*8(sp)
	ld      x17, 17*8(sp)
	ld      x18, 18*8(sp)
	ld      x19, 19*8(sp)
	ld      x20, 20*8(sp)
	ld      x21, 21*8(sp)
	ld      x22, 22*8(sp)
	ld      x23, 23*8(sp)
	ld      x24, 24*8(sp)
	ld      x25, 25*8(sp)
	ld      x26, 26*8(sp)
	ld      x27, 27*8(sp)
	ld      x28, 28*8(sp)
	ld      x29, 29*8(sp)
	ld      x30, 30*8(sp)
	ld      x31, 31*8(sp)
	ld      x2,  2*8(sp)             // restore sp last (base for the loads above)
	sret
"#
);

unsafe extern "C" {
	fn __trap_entry();
}

// SCAUSE bit 63 = interrupt (vs exception); the low bits are the cause code.
const CAUSE_INTERRUPT: u64 = 1 << 63;
// Exception cause codes.
const EXC_BREAKPOINT: u64 = 3;
const EXC_ECALL_U: u64 = 8;
const EXC_INSTR_PAGE_FAULT: u64 = 12;
const EXC_LOAD_PAGE_FAULT: u64 = 13;
const EXC_STORE_PAGE_FAULT: u64 = 15;
// SSTATUS.SPP (bit 8): the privilege the trap came from (1 = S-mode, 0 = U-mode).
const SSTATUS_SPP: u64 = 1 << 8;
// Frame slot indices.
const FRAME_SEPC: usize = 32;
const FRAME_SSTATUS: usize = 33;

// Point STVEC at `__trap_entry` in direct mode (low 2 bits = 0), and clear SSCRATCH
// (the S-mode convention; the U-mode trap path will use it later). Call once per hart.
pub fn init() {
	let stvec = __trap_entry as usize as u64;
	unsafe {
		core::arch::asm!(
			"csrw stvec, {0}",
			"csrw sscratch, zero",
			in(reg) stvec,
			options(nostack, preserves_flags),
		);
	}
}

// The common trap handler, called from `__trap_entry` with the cause, the trap value,
// and a pointer to the saved register frame.
#[unsafe(no_mangle)]
extern "C" fn riscv64_trap(scause: u64, stval: u64, frame: *mut u64) {
	if scause & CAUSE_INTERRUPT != 0 {
		let code = scause & 0xff;
		// S-mode timer interrupt (code 5): advance the tick + re-arm, then let the
		// scheduler preempt the running thread. External (PLIC) interrupts are wired
		// in a later increment.
		if code == 5 {
			super::apic::on_timer_tick();
			let from_user = unsafe { *frame.add(FRAME_SSTATUS) } & SSTATUS_SPP == 0;
			crate::sched::on_timer_preempt(from_user);
		}
		return;
	}
	let code = scause & 0xff;

	// Breakpoint (ebreak): a resumable trap - advance SEPC past the instruction
	// (2 bytes if compressed, else 4) and return via sret.
	if code == EXC_BREAKPOINT {
		unsafe {
			let sepc = *frame.add(FRAME_SEPC);
			let half = core::ptr::read_volatile(sepc as *const u16);
			let len = if half & 3 == 3 { 4 } else { 2 };
			*frame.add(FRAME_SEPC) = sepc + len;
		}
		return;
	}

	let sepc = unsafe { *frame.add(FRAME_SEPC) };
	let sstatus = unsafe { *frame.add(FRAME_SSTATUS) };
	let from_user = sstatus & SSTATUS_SPP == 0;

	// A U-mode fault terminates only the faulting process (the ecall syscall path +
	// the resumable stack-growth fault + terminate_user land with the usermode
	// increment). No U-mode exists yet, so this is currently unreachable.
	if from_user {
		let _ = (EXC_ECALL_U, EXC_INSTR_PAGE_FAULT, EXC_STORE_PAGE_FAULT);
		crate::serial_println!("riscv64: U-mode trap (usermode increment) scause={scause:#x} stval={stval:#x} sepc={sepc:#x}");
		super::halt_loop()
	}

	// An S-mode fault reaching here is a kernel bug: report it and halt.
	let cause = match code {
		1 => "instruction access fault",
		2 => "illegal instruction",
		5 => "load access fault",
		7 => "store access fault",
		EXC_INSTR_PAGE_FAULT => "instruction page fault",
		EXC_LOAD_PAGE_FAULT => "load page fault",
		EXC_STORE_PAGE_FAULT => "store page fault",
		_ => "trap",
	};
	crate::serial_println!("riscv64 S-MODE TRAP: {cause} (scause={scause:#x}) stval={stval:#x} sepc={sepc:#x}");
	crate::serial_println!("riscv64: unhandled kernel trap - halting");
	super::halt_loop()
}

// THE riscv64 PER-CORE RESUME PATH: a hart that lost its context comes back into the kernel through the entry its
// secondaries boot through - the SBI HSM's, in S-mode with the MMU off, at a physical address, `a1` the opaque value the
// call was given - and takes back what it saved.
//
// ONE OPERATION, `save_and_leave`. The callee-saved registers - integer and floating-point - with the stack pointer,
// `gp`, `tp` (the per-CPU block) and `ra`, and the supervisor state this kernel set on the hart - `sstatus`, `sie`,
// `stvec`, `sscratch`, `satp`, `scounteren` and, where Sstc is used, `stimecmp` - go into this hart's record, and `leave`
// is called with it. A `leave` that RETURNS left the context in place - a retentive wait, a firmware that refused, a
// non-retentive suspend the firmware ended as a retentive one - and its answer is `save_and_leave`'s. A hart that comes
// back through `riscv64_resume_start` instead - after a non-retentive HART_SUSPEND, or after it stopped itself and
// another hart started it again - adopts the boot page tables there (the stub runs at its physical address, as
// `riscv64_secondary_start` does), is put back in the higher half by `riscv64_resume_high` - its own page tables first,
// then the trap vector, the scratch register, the counters, the status with its FP state, the FP registers, the compare
// register, the interrupt enables, and the registers and the stack - and `save_and_leave` returns `RESUMED`. What is
// not a register is put back by `restored`, in Rust, after it: the hart's IMSIC interrupt file - its delivery, its
// threshold and its enables - and its tick.
//
// THE RECORD IS THE HART'S OWN, indexed by its logical id: nothing else writes it, and the opaque value names it, so
// the stub needs no lookup.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

// `save_and_leave`'s answer when the hart came back through the entry.
pub const RESUMED: i64 = 1;

// The record's words, as the assembly below addresses them (`WORD * 8`).
const RA: usize = 0;
const SP: usize = 1;
const GP: usize = 2;
const TP: usize = 3;
// s0..s11 at 4..16, fs0..fs11 at 16..28.
const FCSR: usize = 28;
const SSTATUS: usize = 29;
const SIE: usize = 30;
const STVEC: usize = 31;
const SSCRATCH: usize = 32;
const SATP: usize = 33;
const SCOUNTEREN: usize = 34;
const STIMECMP: usize = 35;
// Whether `stimecmp` is this kernel's: Sstc, written by the Rust half before the save.
const HAS_SSTC: usize = 36;
// The IMSIC file, saved and put back by the Rust half.
const EIDELIVERY: usize = 37;
const EITHRESHOLD: usize = 38;
const EIE0: usize = 39;
const WORDS: usize = 40;

#[repr(C, align(16))]
pub struct Record([u64; WORDS]);

static mut RECORDS: [Record; crate::smp::MAX_CPUS] = [const { Record([0; WORDS]) }; crate::smp::MAX_CPUS];

// How many times a hart came back through the entry - for the suite, which tells a state that lost the context from one
// the firmware ended as a wait.
static RESUMES: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
pub fn resumes() -> u64 {
	RESUMES.load(Ordering::Relaxed)
}

global_asm!(
	r#"
.section .text, "ax"
.global riscv64_save_and_leave
// riscv64_save_and_leave(record a0, leave a1, arg a2) -> a0: `leave(record, arg)`'s answer, or 1 through the entry.
riscv64_save_and_leave:
	sd      ra,   {RA}*8(a0)
	sd      sp,   {SP}*8(a0)
	sd      gp,   {GP}*8(a0)
	sd      tp,   {TP}*8(a0)
	sd      s0,   4*8(a0)
	sd      s1,   5*8(a0)
	sd      s2,   6*8(a0)
	sd      s3,   7*8(a0)
	sd      s4,   8*8(a0)
	sd      s5,   9*8(a0)
	sd      s6,  10*8(a0)
	sd      s7,  11*8(a0)
	sd      s8,  12*8(a0)
	sd      s9,  13*8(a0)
	sd      s10, 14*8(a0)
	sd      s11, 15*8(a0)
	fsd     fs0, 16*8(a0)
	fsd     fs1, 17*8(a0)
	fsd     fs2, 18*8(a0)
	fsd     fs3, 19*8(a0)
	fsd     fs4, 20*8(a0)
	fsd     fs5, 21*8(a0)
	fsd     fs6, 22*8(a0)
	fsd     fs7, 23*8(a0)
	fsd     fs8, 24*8(a0)
	fsd     fs9, 25*8(a0)
	fsd     fs10, 26*8(a0)
	fsd     fs11, 27*8(a0)
	frcsr   t0
	sd      t0, {FCSR}*8(a0)
	csrr    t0, sstatus
	sd      t0, {SSTATUS}*8(a0)
	csrr    t0, sie
	sd      t0, {SIE}*8(a0)
	csrr    t0, stvec
	sd      t0, {STVEC}*8(a0)
	csrr    t0, sscratch
	sd      t0, {SSCRATCH}*8(a0)
	csrr    t0, satp
	sd      t0, {SATP}*8(a0)
	csrr    t0, scounteren
	sd      t0, {SCOUNTEREN}*8(a0)
	ld      t1, {HAS_SSTC}*8(a0)
	beqz    t1, 1f
	csrr    t0, 0x14d
	sd      t0, {STIMECMP}*8(a0)
1:
	// `leave(record, arg)`, s0 holding the record across it - s0 itself is in the record already.
	mv      s0, a0
	mv      t2, a1
	mv      a1, a2
	jalr    ra, t2, 0
	// IT RETURNED: the context was never lost. Every callee-saved register is as the call left it but these two.
	ld      ra, {RA}*8(s0)
	ld      s0, 4*8(s0)
	ret

.global riscv64_resume_high
// FROM THE STUB, on the boot page tables in the higher half, a1 the record: the hart put back as it was saved.
riscv64_resume_high:
	mv      a0, a1
	ld      t0, {SATP}*8(a0)
	sfence.vma
	csrw    satp, t0
	sfence.vma
	ld      t0, {STVEC}*8(a0)
	csrw    stvec, t0
	ld      t0, {SSCRATCH}*8(a0)
	csrw    sscratch, t0
	ld      t0, {SCOUNTEREN}*8(a0)
	csrw    scounteren, t0
	// THE STATUS BEFORE THE FP REGISTERS: its FS field is what lets them be loaded. Its SIE is the save's - clear.
	ld      t0, {SSTATUS}*8(a0)
	csrw    sstatus, t0
	ld      t0, {FCSR}*8(a0)
	fscsr   t0
	fld     fs0, 16*8(a0)
	fld     fs1, 17*8(a0)
	fld     fs2, 18*8(a0)
	fld     fs3, 19*8(a0)
	fld     fs4, 20*8(a0)
	fld     fs5, 21*8(a0)
	fld     fs6, 22*8(a0)
	fld     fs7, 23*8(a0)
	fld     fs8, 24*8(a0)
	fld     fs9, 25*8(a0)
	fld     fs10, 26*8(a0)
	fld     fs11, 27*8(a0)
	ld      t1, {HAS_SSTC}*8(a0)
	beqz    t1, 1f
	ld      t0, {STIMECMP}*8(a0)
	csrw    0x14d, t0
1:
	ld      t0, {SIE}*8(a0)
	csrw    sie, t0
	ld      ra, {RA}*8(a0)
	ld      sp, {SP}*8(a0)
	ld      gp, {GP}*8(a0)
	ld      tp, {TP}*8(a0)
	ld      s0,   4*8(a0)
	ld      s1,   5*8(a0)
	ld      s2,   6*8(a0)
	ld      s3,   7*8(a0)
	ld      s4,   8*8(a0)
	ld      s5,   9*8(a0)
	ld      s6,  10*8(a0)
	ld      s7,  11*8(a0)
	ld      s8,  12*8(a0)
	ld      s9,  13*8(a0)
	ld      s10, 14*8(a0)
	ld      s11, 15*8(a0)
	li      a0, 1
	ret

.balign 8
.global riscv64_resume_start
// THE ENTRY, at its physical address with the MMU off - a0 the hart id, a1 the record: the boot page tables adopted, as
// a secondary's start does, and on into the higher half. IN `.text` AND NOT `.text.boot`: the identity window keeps
// execute over the kernel's read-only text alone once the harts are up (`paging::harden_direct_map`), and this runs a
// few instructions there with translation on. Its addresses are words beside it, read PC-relative: the code runs at
// its physical address and its virtual one alike.
riscv64_resume_start:
	lla     t0, 7f
	ld      t1, 0(t0)               // the boot page tables, physical
	srli    t1, t1, 12
	li      t2, 8
	slli    t2, t2, 60
	or      t1, t1, t2
	sfence.vma
	csrw    satp, t1
	sfence.vma
	ld      t0, 8(t0)               // `riscv64_resume_high`, virtual
	jr      t0
.balign 8
7:
	.dword  __boot_tables
	.dword  riscv64_resume_high

.balign 4
.global riscv64_replace_trampoline
// A HIBERNATION'S REPLACEMENT, from the boot page tables at its identity address - a0 the parameters' physical address:
// translation off, every page of the image copied to its frame, and the image's resume entry with the image's boot
// record, a0 the hart id. Position-independent, and no stack: the memory under it is being replaced. Its own page is
// the image's too, which holds the same instructions - the system image's digest is what says it is the same kernel.
riscv64_replace_trampoline:
	csrw    satp, zero
	sfence.vma
	ld      t0, 0(a0)               // pages left
	li      t1, 0                   // the list page's index
2:
	beqz    t0, 5f
	srli    t2, t1, 9
	slli    t2, t2, 3
	add     t2, t2, a0
	ld      t2, 64(t2)              // its directory page
	andi    t3, t1, 511
	slli    t3, t3, 3
	add     t2, t2, t3
	ld      t2, 0(t2)               // the list page
	li      t3, 256                 // pairs in it: 256, or what is left
	bgeu    t0, t3, 1f
	mv      t3, t0
1:
	sub     t0, t0, t3
3:
	beqz    t3, 4f
	ld      t4, 0(t2)               // the frame the page goes to
	ld      t5, 8(t2)               // the frame holding it
	li      t6, 512
6:
	ld      a1, 0(t5)
	sd      a1, 0(t4)
	addi    t4, t4, 8
	addi    t5, t5, 8
	addi    t6, t6, -1
	bnez    t6, 6b
	addi    t2, t2, 16
	addi    t3, t3, -1
	j       3b
4:
	addi    t1, t1, 1
	j       2b
5:
	fence.i
	ld      t0, 16(a0)              // the image's resume entry
	ld      a1, 24(a0)              // the image's boot record
	ld      a0, 32(a0)              // this hart's id
	jr      t0

.section .data, "aw"
.balign 8
.global riscv64_resume_entry
riscv64_resume_entry:
	.quad riscv64_resume_start
	.quad riscv64_replace_trampoline
	.quad __boot_tables
"#,
	RA = const RA,
	SP = const SP,
	GP = const GP,
	TP = const TP,
	FCSR = const FCSR,
	SSTATUS = const SSTATUS,
	SIE = const SIE,
	STVEC = const STVEC,
	SSCRATCH = const SSCRATCH,
	SATP = const SATP,
	SCOUNTEREN = const SCOUNTEREN,
	STIMECMP = const STIMECMP,
	HAS_SSTC = const HAS_SSTC,
);

unsafe extern "C" {
	fn riscv64_save_and_leave(record: *mut Record, leave: extern "C" fn(*mut Record, u64) -> i64, arg: u64) -> i64;
	// The entry's physical address, which the linker put in a word of its own; the trampoline's and the boot page
	// tables' in the two words after it.
	static riscv64_resume_entry: [u64; 3];
}

// THE ADDRESS OF CORE `cpu`'S RECORD - what a hibernation image's context names for its boot core.
pub fn record_address(cpu: usize) -> u64 {
	// An address, never read through here.
	(unsafe { &raw const RECORDS[cpu] }) as u64
}

// THE ENTRY THE FIRMWARE IS GIVEN: the stub's physical address.
pub fn entry() -> u64 {
	// SAFETY: words the linker filled, never written. The stub is linked in the kernel's half; its physical address is
	// its virtual one less the offset.
	unsafe { core::ptr::read_volatile(&raw const riscv64_resume_entry)[0] & !super::paging::KERNEL_VA_OFFSET }
}

// A HIBERNATION'S TRAMPOLINE, and the boot page tables it is jumped to on: physical addresses, which are the identity
// addresses those tables map them at.
pub fn trampoline() -> u64 {
	// SAFETY: as `entry`.
	unsafe { core::ptr::read_volatile(&raw const riscv64_resume_entry)[1] & !super::paging::KERNEL_VA_OFFSET }
}

pub fn boot_tables() -> u64 {
	// SAFETY: as `entry`.
	unsafe { core::ptr::read_volatile(&raw const riscv64_resume_entry)[2] }
}

// THE ONE OPERATION - see the head of this file. Called with interrupts masked; answers `leave`'s answer, or `RESUMED`
// once the hart is back through the entry and `restored` has put back what is not a register.
pub fn save_and_leave(leave: extern "C" fn(*mut Record, u64) -> i64, arg: u64) -> i64 {
	let cpu = crate::sched::current_cpu_id();
	// SAFETY: this hart's own record, written by this hart alone, with its interrupts masked.
	let record = unsafe { &raw mut RECORDS[cpu] };
	let (delivery, threshold, enables) = super::imsic::save_hart();
	// SAFETY: as above; the words the assembly does not write.
	unsafe {
		(*record).0[HAS_SSTC] = u64::from(super::apic::has_sstc());
		(*record).0[EIDELIVERY] = delivery;
		(*record).0[EITHRESHOLD] = threshold;
		(*record).0[EIE0] = enables;
	}
	// SAFETY: the record is this hart's and outlives the call; `leave` is an `extern "C"` function that either returns
	// or never comes back but through the entry.
	let answer = unsafe { riscv64_save_and_leave(record, leave, arg) };
	if answer == RESUMED {
		// SAFETY: as above.
		let saved = unsafe { ((*record).0[EIDELIVERY], (*record).0[EITHRESHOLD], (*record).0[EIE0]) };
		restored(saved);
	}
	answer
}

// WHAT IS NOT A REGISTER, put back on a hart that came back through the entry: its interrupt file, and its tick - the
// firmware that started it again owes it no compare value.
fn restored(saved: (u64, u64, u64)) {
	RESUMES.fetch_add(1, Ordering::Relaxed);
	super::imsic::restore_hart(saved);
	super::apic::timer_periodic();
}

// ------------------------------------------------------------------ the SBI's HSM calls

const HSM: usize = 0x48534D;

// A NON-RETENTIVE HART_SUSPEND (FID 3) as `leave`: the suspend type, the entry and the record. Answers the SBI's error;
// a success that returns is the firmware having kept the context.
pub extern "C" fn suspend_leave(record: *mut Record, suspend_type: u64) -> i64 {
	let error: i64;
	// SAFETY: the firmware either returns here with the hart's state kept, or resumes it at the entry with `a1` the
	// record.
	unsafe {
		core::arch::asm!("ecall", in("a7") HSM, in("a6") 3usize, inout("a0") suspend_type => error, inout("a1") entry() => _, in("a2") record as u64, options(nostack));
	}
	error
}

// HART_STOP (FID 1) as `leave`: this hart stopped, for another to start again through the entry with its record. Answers
// the SBI's error, since a stop that returns did not happen.
pub extern "C" fn off_leave(_record: *mut Record, _arg: u64) -> i64 {
	let error: i64;
	// SAFETY: a stop either does not return or refuses.
	unsafe {
		core::arch::asm!("ecall", in("a7") HSM, in("a6") 1usize, lateout("a0") error, lateout("a1") _, options(nostack));
	}
	error
}

// THIS CORE OFF, for good: a held core a restore's replacement turned off, which only the image's kernel turns on again.
pub fn off_now() -> ! {
	let answer = off_leave(core::ptr::null_mut(), 0);
	crate::serial_println!("resume: core {} could not turn itself off ({answer}) - it halts instead", crate::sched::current_cpu_id());
	super::halt_loop()
}

// HART_START (FID 0) of a hart that stopped through `stop_leave`, at the entry with its record. Answers the SBI's error.
pub fn start_saved(cpu: usize) -> i64 {
	let hart = crate::smp::lapic_id(cpu);
	// The address of the stopped hart's record, which its entry reads; nothing is read through it here.
	let record = unsafe { &raw mut RECORDS[cpu] } as u64;
	let error: i64;
	// SAFETY: an SBI call that starts another hart; it returns here.
	unsafe {
		core::arch::asm!("ecall", in("a7") HSM, in("a6") 0usize, inout("a0") hart => error, inout("a1") entry() => _, in("a2") record, options(nostack));
	}
	error
}

// Whether a hart is stopped, as HART_GET_STATUS (FID 2) answers: STOPPED is 1.
pub fn stopped(cpu: usize) -> bool {
	let hart = crate::smp::lapic_id(cpu);
	let (error, status): (i64, i64);
	// SAFETY: a read of the firmware's view of a hart.
	unsafe {
		core::arch::asm!("ecall", in("a7") HSM, in("a6") 2usize, inout("a0") hart => error, lateout("a1") status, options(nostack));
	}
	error == 0 && status == 1
}

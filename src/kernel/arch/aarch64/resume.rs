// THE aarch64 PER-CORE RESUME PATH: a core that lost its context comes back into the kernel through the entry its
// secondaries boot through - PSCI's, at EL1 with the MMU off, at a physical address, `x0` the context id the call was
// given - and takes back what it saved.
//
// ONE OPERATION, `save_and_leave`. The callee-saved registers - x19..x30 and d8..d15 - with the stack pointer, the
// floating-point control and status, and the EL1 state this kernel set on the core - the translation registers, the
// system control register, the vector base, the thread registers of both levels, `SP_EL0`, the FP enable, the timer's
// control and compare, the counter's EL0 access, the debug and context-id registers and the interrupt mask - go into
// this core's record, and `leave` is called with it. A `leave` that RETURNS left the context in place - a retention
// state, a firmware that refused, or a power-down state the firmware ended as a wait, which QEMU's PSCI always does -
// and its answer is `save_and_leave`'s. A core that comes back through `aarch64_resume_start` instead - after a
// power-down CPU_SUSPEND, or after it turned itself off and another core turned it on again - adopts the boot page
// tables there with its caches on (the stub runs at its physical address, as `aarch64_secondary_start` does), is put
// back in the higher half by `aarch64_resume_high` - its own translation first, then everything else, the stack and the
// registers last - and `save_and_leave` returns `RESUMED`. What is not a register is put back by `restored`, in Rust,
// after it: the core's interrupt controller interface - its redistributor woken and its SGIs and PPIs enabled on
// GICv3, its banked CPU interface on GICv2 - and its tick.
//
// THE RECORD IS THE CORE'S OWN, indexed by its logical id: nothing else writes it, and the context id names it, so the
// stub needs no lookup.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

// `save_and_leave`'s answer when the core came back through the entry.
pub const RESUMED: i64 = 1;

// The record's byte offsets, as the assembly below addresses them.
// x19..x28 at 0..80 and d8..d15 at 104..168, written out where they are used.
const FP_LR: usize = 80;
const SP: usize = 96;
const FPCR: usize = 168;
const TTBR: usize = 184;
const TCR_MAIR: usize = 200;
const SCTLR_VBAR: usize = 216;
const TPIDR: usize = 232;
const TPIDRRO_SPEL0: usize = 248;
const CPACR_CNTKCTL: usize = 264;
const DAIF_CONTEXTIDR: usize = 280;
const MDSCR: usize = 296;
const CNTP: usize = 304;
const BYTES: usize = 320;

#[repr(C, align(16))]
pub struct Record([u8; BYTES]);

static mut RECORDS: [Record; crate::smp::MAX_CPUS] = [const { Record([0; BYTES]) }; crate::smp::MAX_CPUS];

// How many times a core came back through the entry - for the suite, which tells a core that lost its context from one
// whose firmware ended the state as a wait.
static RESUMES: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
pub fn resumes() -> u64 {
	RESUMES.load(Ordering::Relaxed)
}

global_asm!(
	r#"
.text
.global aarch64_save_and_leave
// aarch64_save_and_leave(record x0, leave x1, arg x2) -> x0: `leave(record, arg)`'s answer, or 1 through the entry.
aarch64_save_and_leave:
	stp     x19, x20, [x0, #0]
	stp     x21, x22, [x0, #16]
	stp     x23, x24, [x0, #32]
	stp     x25, x26, [x0, #48]
	stp     x27, x28, [x0, #64]
	stp     x29, x30, [x0, #{FP_LR}]
	mov     x9, sp
	str     x9, [x0, #{SP}]
	stp     d8, d9, [x0, #104]
	stp     d10, d11, [x0, #120]
	stp     d12, d13, [x0, #136]
	stp     d14, d15, [x0, #152]
	mrs     x9, fpcr
	mrs     x10, fpsr
	stp     x9, x10, [x0, #{FPCR}]
	mrs     x9, ttbr0_el1
	mrs     x10, ttbr1_el1
	stp     x9, x10, [x0, #{TTBR}]
	mrs     x9, tcr_el1
	mrs     x10, mair_el1
	stp     x9, x10, [x0, #{TCR_MAIR}]
	mrs     x9, sctlr_el1
	mrs     x10, vbar_el1
	stp     x9, x10, [x0, #{SCTLR_VBAR}]
	mrs     x9, tpidr_el1
	mrs     x10, tpidr_el0
	stp     x9, x10, [x0, #{TPIDR}]
	mrs     x9, tpidrro_el0
	mrs     x10, sp_el0
	stp     x9, x10, [x0, #{TPIDRRO_SPEL0}]
	mrs     x9, cpacr_el1
	mrs     x10, cntkctl_el1
	stp     x9, x10, [x0, #{CPACR_CNTKCTL}]
	mrs     x9, daif
	mrs     x10, contextidr_el1
	stp     x9, x10, [x0, #{DAIF_CONTEXTIDR}]
	mrs     x9, mdscr_el1
	str     x9, [x0, #{MDSCR}]
	mrs     x9, cntp_ctl_el0
	mrs     x10, cntp_cval_el0
	stp     x9, x10, [x0, #{CNTP}]
	// THE STORES REACH MEMORY before the core can lose its caches: the entry reads them with the MMU just on.
	dsb     sy
	// `leave(record, arg)`, x19 holding the record across it - x19 itself is in the record already.
	mov     x19, x0
	mov     x9, x1
	mov     x1, x2
	blr     x9
	// IT RETURNED: the context was never lost. Every callee-saved register is as the call left it but these two.
	ldr     x30, [x19, #88]
	ldr     x19, [x19, #0]
	ret

.global aarch64_resume_high
// FROM THE STUB, on the boot page tables in the higher half, x0 the record: the core put back as it was saved.
aarch64_resume_high:
	ldp     x9, x10, [x0, #{TCR_MAIR}]
	msr     tcr_el1, x9
	msr     mair_el1, x10
	ldp     x9, x10, [x0, #{TTBR}]
	msr     ttbr0_el1, x9
	msr     ttbr1_el1, x10
	isb
	dsb     sy
	tlbi    vmalle1
	dsb     sy
	isb
	ldp     x9, x10, [x0, #{SCTLR_VBAR}]
	msr     sctlr_el1, x9
	msr     vbar_el1, x10
	isb
	ldp     x9, x10, [x0, #{TPIDR}]
	msr     tpidr_el1, x9
	msr     tpidr_el0, x10
	ldp     x9, x10, [x0, #{TPIDRRO_SPEL0}]
	msr     tpidrro_el0, x9
	msr     sp_el0, x10
	ldp     x9, x10, [x0, #{CPACR_CNTKCTL}]
	msr     cpacr_el1, x9
	msr     cntkctl_el1, x10
	isb
	ldp     x9, x10, [x0, #{FPCR}]
	msr     fpcr, x9
	msr     fpsr, x10
	ldp     d8, d9, [x0, #104]
	ldp     d10, d11, [x0, #120]
	ldp     d12, d13, [x0, #136]
	ldp     d14, d15, [x0, #152]
	ldr     x9, [x0, #{MDSCR}]
	msr     mdscr_el1, x9
	ldp     x9, x10, [x0, #{CNTP}]
	msr     cntp_cval_el0, x10
	msr     cntp_ctl_el0, x9
	ldp     x9, x10, [x0, #{DAIF_CONTEXTIDR}]
	msr     contextidr_el1, x10
	msr     daif, x9
	isb
	ldr     x9, [x0, #{SP}]
	mov     sp, x9
	ldp     x29, x30, [x0, #{FP_LR}]
	ldp     x27, x28, [x0, #64]
	ldp     x25, x26, [x0, #48]
	ldp     x23, x24, [x0, #32]
	ldp     x21, x22, [x0, #16]
	ldp     x19, x20, [x0, #0]
	mov     x0, #1
	ret

.balign 8
.global aarch64_resume_start
// THE ENTRY, at its physical address with the MMU off - x0 the record: the boot page tables adopted with the caches on,
// as a secondary's start does, and on into the higher half. IN `.text` AND NOT `.text.boot`: once every core is up the
// direct map - whose gigabytes the boot tables' low half shares - is execute-never everywhere but over the kernel's
// read-only text (`paging::harden_direct_map`), and this runs a few instructions there with translation on. Its
// addresses are words beside it, read PC-relative: it runs at its physical address and its virtual one alike. NO
// SET/WAY INVALIDATION, which the secondary's start does on caches PSCI leaves UNKNOWN at a first power-on: a core
// resuming from a power-down state finds its caches as the firmware's power-down left them, cleaned, and invalidating by
// set/way here could only throw away a line it still owns.
aarch64_resume_start:
	mov     x19, x0
	ic      iallu
	dsb     sy
	isb
	ldr     x20, 7f                 // the boot page tables, physical
	add     x21, x20, #4096         // L0_LOW  (TTBR0, low identity)
	add     x22, x20, #8192         // L0_HIGH (TTBR1, higher half)
	mov     x0, #0xFF00
	msr     mair_el1, x0
	mrs     x0, id_aa64mmfr0_el1
	and     x0, x0, #0x7
	lsl     x0, x0, #32
	movz    x1, #0x3510
	movk    x1, #0xB510, lsl #16
	orr     x0, x0, x1
	msr     tcr_el1, x0
	msr     ttbr0_el1, x21
	msr     ttbr1_el1, x22
	dsb     sy
	tlbi    vmalle1
	dsb     sy
	isb
	mrs     x0, sctlr_el1
	orr     x0, x0, #1             // M: translation on
	orr     x0, x0, #0x4           // C: data and unified caches
	orr     x0, x0, #0x1000        // I: instruction caches
	msr     sctlr_el1, x0
	isb
	ldr     x4, 8f                  // `aarch64_resume_high`, virtual
	mov     x0, x19
	br      x4
.balign 8
7:
	.quad   __boot_tables
8:
	.quad   aarch64_resume_high

.balign 4
.global aarch64_replace_trampoline
// A HIBERNATION'S REPLACEMENT, at its identity address on the boot tables' low half - x0 the parameters' physical address:
// every dirty line cleaned to memory, translation and the data cache off, every page of the image copied to its frame,
// and the image's resume entry with the image's boot record. Position-independent, and no stack: the memory under it is
// being replaced. Its own page is the image's too, which holds the same instructions - the system image's digest is
// what says it is the same kernel.
aarch64_replace_trampoline:
	mov     x19, x0
	// CLEAN AND INVALIDATE BY SET/WAY, every level to the point of coherency: the image's pages were written through the
	// cache, and the copy reads memory.
	mrs     x0, clidr_el1
	and     w3, w0, #0x07000000
	lsr     w3, w3, #23
	cbz     w3, 5f
	mov     w10, #0
1:
	add     w2, w10, w10, lsr #1
	lsr     w1, w0, w2
	and     w1, w1, #0x7
	cmp     w1, #2
	b.lt    4f
	msr     csselr_el1, x10
	isb
	mrs     x1, ccsidr_el1
	and     w2, w1, #7
	add     w2, w2, #4
	mov     w4, #0x3ff
	and     w4, w4, w1, lsr #3
	clz     w5, w4
	mov     w7, #0x7fff
	and     w7, w7, w1, lsr #13
2:
	mov     w9, w4
3:
	lsl     w6, w9, w5
	orr     w11, w10, w6
	lsl     w6, w7, w2
	orr     w11, w11, w6
	dc      cisw, x11
	subs    w9, w9, #1
	b.ge    3b
	subs    w7, w7, #1
	b.ge    2b
4:
	add     w10, w10, #2
	cmp     w3, w10
	b.gt    1b
5:
	mov     x0, #0
	msr     csselr_el1, x0
	dsb     sy
	isb
	// TRANSLATION AND THE DATA CACHE OFF: from here every access is physical and goes to memory.
	mrs     x0, sctlr_el1
	bic     x0, x0, #1
	bic     x0, x0, #4
	msr     sctlr_el1, x0
	isb
	ic      iallu
	dsb     sy
	isb
	ldr     x8, [x19, #0]           // pages left
	mov     x9, #0                  // the list page's index
6:
	cbz     x8, 10f
	lsr     x10, x9, #9
	add     x10, x19, x10, lsl #3
	ldr     x10, [x10, #64]         // its directory page
	and     x11, x9, #511
	ldr     x10, [x10, x11, lsl #3] // the list page
	mov     x11, #256               // pairs in it: 256, or what is left
	cmp     x8, x11
	csel    x11, x8, x11, lo
	sub     x8, x8, x11
7:
	cbz     x11, 9f
	ldp     x12, x13, [x10]         // the frame the page goes to, the frame holding it
	mov     x14, #256
8:
	ldp     x15, x16, [x13], #16
	stp     x15, x16, [x12], #16
	subs    x14, x14, #1
	b.ne    8b
	add     x10, x10, #16
	sub     x11, x11, #1
	b       7b
9:
	add     x9, x9, #1
	b       6b
10:
	dsb     sy
	ic      iallu
	dsb     sy
	isb
	ldr     x1, [x19, #16]          // the image's resume entry
	ldr     x0, [x19, #24]          // the image's boot record
	br      x1

.section .data, "aw"
.balign 8
.global aarch64_resume_entry
aarch64_resume_entry:
	.quad aarch64_resume_start
	.quad aarch64_replace_trampoline
	.quad __boot_tables
"#,
	FP_LR = const FP_LR,
	SP = const SP,
	FPCR = const FPCR,
	TTBR = const TTBR,
	TCR_MAIR = const TCR_MAIR,
	SCTLR_VBAR = const SCTLR_VBAR,
	TPIDR = const TPIDR,
	TPIDRRO_SPEL0 = const TPIDRRO_SPEL0,
	CPACR_CNTKCTL = const CPACR_CNTKCTL,
	DAIF_CONTEXTIDR = const DAIF_CONTEXTIDR,
	MDSCR = const MDSCR,
	CNTP = const CNTP,
);

unsafe extern "C" {
	fn aarch64_save_and_leave(record: *mut Record, leave: extern "C" fn(*mut Record, u64) -> i64, arg: u64) -> i64;
	// The entry's physical address, which the linker put in a word of its own; the trampoline's and the boot page
	// tables' in the two words after it.
	static aarch64_resume_entry: [u64; 3];
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
	unsafe { core::ptr::read_volatile(&raw const aarch64_resume_entry)[0] & !super::paging::KERNEL_VA_OFFSET }
}

// A HIBERNATION'S TRAMPOLINE, and the boot page tables it is jumped to on: physical addresses, which are the identity
// addresses the tables' low half maps them at.
pub fn trampoline() -> u64 {
	// SAFETY: as `entry`.
	unsafe { core::ptr::read_volatile(&raw const aarch64_resume_entry)[1] & !super::paging::KERNEL_VA_OFFSET }
}

pub fn boot_tables() -> u64 {
	// SAFETY: as `entry`.
	unsafe { core::ptr::read_volatile(&raw const aarch64_resume_entry)[2] }
}

// THE ONE OPERATION - see the head of this file. Called with interrupts masked; answers `leave`'s answer, or `RESUMED`
// once the core is back through the entry and `restored` has put back what is not a register.
pub fn save_and_leave(leave: extern "C" fn(*mut Record, u64) -> i64, arg: u64) -> i64 {
	let cpu = crate::sched::current_cpu_id();
	// SAFETY: this core's own record, written by this core alone, with its interrupts masked; it outlives the call, and
	// `leave` either returns or never comes back but through the entry.
	let answer = unsafe { aarch64_save_and_leave(&raw mut RECORDS[cpu], leave, arg) };
	if answer == RESUMED {
		restored();
	}
	answer
}

// WHAT IS NOT A REGISTER, put back on a core that came back through the entry: its interrupt controller interface and
// its tick - `gic::init_secondary` is the per-core half every core's start runs, and it is what a core that lost its
// power needs again.
fn restored() {
	RESUMES.fetch_add(1, Ordering::Relaxed);
	if !super::gic::init_secondary() {
		crate::serial_println!("resume: core {}'s interrupt interface could not be established again", crate::sched::current_cpu_id());
	}
}

// ------------------------------------------------------------------ PSCI's calls

// A POWER-DOWN CPU_SUSPEND as `leave`: the power state, the entry and the record. Answers PSCI's status; a success that
// returns is the firmware having kept the context.
pub extern "C" fn suspend_leave(record: *mut Record, power_state: u64) -> i64 {
	super::psci::cpu_suspend_to(power_state as u32, entry(), record as u64)
}

// CPU_OFF as `leave`: this core off, for another to turn on again through the entry with its record. Answers PSCI's
// status, since an off that returns did not happen.
pub extern "C" fn off_leave(_record: *mut Record, _arg: u64) -> i64 {
	super::psci::cpu_off()
}

// THIS CORE OFF, for good: a held core a restore's replacement turned off, which only the image's kernel turns on again.
pub fn off_now() -> ! {
	let answer = off_leave(core::ptr::null_mut(), 0);
	crate::serial_println!("resume: core {} could not turn itself off ({answer}) - it halts instead", crate::sched::current_cpu_id());
	super::halt_loop()
}

// CPU_ON of a core that turned off through `off_leave`, at the entry with its record. Answers PSCI's status.
pub fn start_saved(cpu: usize) -> i64 {
	// The address of the core's record, which its entry reads; nothing is read through it here.
	let record = unsafe { &raw mut RECORDS[cpu] } as u64;
	super::psci::cpu_on_to(crate::smp::lapic_id(cpu), entry(), record)
}

// Whether a core is off, as AFFINITY_INFO answers: OFF is 1.
pub fn stopped(cpu: usize) -> bool {
	super::psci::affinity_off(crate::smp::lapic_id(cpu))
}

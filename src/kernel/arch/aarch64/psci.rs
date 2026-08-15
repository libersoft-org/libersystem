// aarch64 SMP bring-up via PSCI CPU_ON.
//
// QEMU's `virt` machine implements PSCI (Power State Coordination Interface) and,
// for the default non-secure, non-virtualized configuration, expects the call via
// HVC from EL1 (QEMU emulates the PSCI service even without a real EL2). Secondary
// cores reset held in a PSCI-parked state; CPU_ON releases one at a physical entry
// point with the MMU off. `aarch64_secondary_start` is a low, position-independent
// stub (like the primary `_start`): it turns the MMU on with the boot core's page
// tables (TTBR0 = low identity, TTBR1 = the higher half, built once by the primary
// at `__boot_tables`), then branches into the higher half.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// PSCI CPU_ON, SMC64 calling convention (function id 0xC400_0003).
const PSCI_CPU_ON: u64 = 0xC400_0003;

// Matches the per-CPU pool size in `percpu`.
const MAX_CPUS: usize = 8;
const SEC_STACK_SIZE: u64 = 16384;

// Per-core boot stacks for the secondaries (indexed by cpu id).
#[unsafe(no_mangle)]
static mut SEC_STACKS: [[u8; SEC_STACK_SIZE as usize]; MAX_CPUS] = [[0; SEC_STACK_SIZE as usize]; MAX_CPUS];

// Count of secondaries that have come online, and their reported MPIDRs.
static SMP_ONLINE: AtomicU32 = AtomicU32::new(0);
static SEC_MPIDR: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

global_asm!(
	r#"
.section .data.boot, "a"
.balign 8
.Ls_main:  .quad aarch64_secondary_main
.Ls_stack: .quad SEC_STACKS

.section .text.boot, "ax"
.global aarch64_secondary_start
aarch64_secondary_start:
	// x0 = context_id = cpu id (passed by PSCI CPU_ON). MMU is off; PC is the low
	// physical entry. Adopt the boot page tables the primary already built.
	mov     x19, x0
	adrp    x20, __boot_tables
	add     x20, x20, :lo12:__boot_tables
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
	orr     x0, x0, #1             // enable the MMU (SCTLR_EL1.M)
	msr     sctlr_el1, x0
	isb
	// Per-core higher-half stack: SEC_STACKS[cpu_id] top.
	adrp    x0, .Ls_stack
	ldr     x5, [x0, :lo12:.Ls_stack]
	mov     x6, #16384
	madd    x5, x19, x6, x5
	add     x5, x5, x6
	mov     sp, x5
	// Branch into the higher half: aarch64_secondary_main(cpu_id).
	adrp    x0, .Ls_main
	ldr     x4, [x0, :lo12:.Ls_main]
	mov     x0, x19
	br      x4
0:
	wfe
	b       0b

.section .data, "aw"
.balign 8
.global aarch64_secondary_entry
aarch64_secondary_entry:
	.quad aarch64_secondary_start
"#
);

// Which conduit, if any, answers a PSCI call on this machine - what the loader recorded in the
// hand-off. `PSCI_NONE` for a kernel that was dropped from EL2 with nothing left below it.
//
// READ FROM THE HAND-OFF DIRECTLY, not from the published `BootInfo`, for the reason
// `loader_module_at` gives one file over: the secondaries are brought up BEFORE the kernel publishes
// one, and reading it there panics with "boot info read before it was published" - which is what the
// first version of this did.
//
// No hand-off (QEMU's `-kernel`, which enters with a raw DTB pointer) means the guest started at EL1
// under QEMU's own PSCI, which is HVC. That is the path every aarch64 test in this tree takes.
pub(crate) fn conduit(arg: u64) -> u32 {
	if arg == 0 {
		return bootproto::PSCI_NONE;
	}
	let magic = unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(arg) as *const u64) };
	if magic != bootproto::MAGIC {
		// A RAW DEVICE TREE, WHICH IS THE OTHER WAY THIS KERNEL BOOTS - and it answered `PSCI_HVC`
		// unconditionally.
		//
		// The loader learned to read `/psci/method` and the FADT, so a UEFI boot gets the conduit
		// the platform states. Booted directly by firmware with a DTB in `x0` there is no
		// `BootInfo`, and this fell back to the same guess the loader had just stopped making: the
		// identical machine, with `method = "smc"` in its own device tree, executed `hvc #0`.
		//
		// The tree is right there and `Fdt::psci_conduit` already reads it - the same function the
		// loader calls, so the two boot paths cannot answer differently about one platform. A tree
		// that states nothing gets `PSCI_NONE`, which boots single-core with a reason rather than
		// faulting on the first secondary.
		return match fdt::Fdt::new(arg, super::paging::phys_to_virt).psci_conduit() {
			Some(fdt::PsciConduit::Smc) => bootproto::PSCI_SMC,
			Some(fdt::PsciConduit::Hvc) => bootproto::PSCI_HVC,
			None => bootproto::PSCI_NONE,
		};
	}
	let bi = super::paging::phys_to_virt(arg) as *const bootproto::BootInfo;
	unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*bi).psci_conduit)) }
}

// Issue a PSCI CPU_ON on the conduit the loader said exists. Returns the PSCI status (0 = SUCCESS).
//
// THE CONDUIT IS NOT A PROPERTY OF THE ARCHITECTURE. It is a fact about whatever runs below this
// kernel, and this issued a bare `hvc #0` on the assumption that something always does. Under
// `virtualization=on` the guest owns EL2, so the `hvc` landed in the firmware's EL2 vectors - which
// outlive `ExitBootServices` - and the boot died there with fifteen lines of kernel output behind it
// and no explanation. `BootInfo::psci_conduit` is the loader saying what it left behind.
fn cpu_on(conduit: u32, target_mpidr: u64, entry: u64, context_id: u64) -> i64 {
	let ret: i64;
	// TWO INSTRUCTIONS, ONE CALLING CONVENTION. The SMC arm used to be a printed refusal, because
	// nothing in this tree could set `PSCI_SMC` - the loader inferred `PSCI_HVC` from the exception
	// level. Now that the loader reads the conduit off the platform, `PSCI_SMC` is a real answer on
	// most server-class AArch64, where EL2 belongs to a hypervisor and PSCI lives in EL3 firmware.
	// The register conventions are identical, so the difference is the instruction and nothing else.
	unsafe {
		if conduit == bootproto::PSCI_SMC {
			core::arch::asm!(
				"smc #0",
				inout("x0") PSCI_CPU_ON => ret,
				in("x1") target_mpidr,
				in("x2") entry,
				in("x3") context_id,
				options(nostack),
			);
		} else {
			core::arch::asm!(
				"hvc #0",
				inout("x0") PSCI_CPU_ON => ret,
				in("x1") target_mpidr,
				in("x2") entry,
				in("x3") context_id,
				options(nostack),
			);
		}
	}
	ret
}

// First Rust code a secondary core runs (MMU on, stack + vectors set). It brings
// up its per-CPU block and local GIC/timer, records itself online, then idles.
#[unsafe(no_mangle)]
extern "C" fn aarch64_secondary_main(cpu_id: u64) -> ! {
	// Install the shared EL1 exception vectors on this core (VBAR_EL1 resets to 0).
	super::exceptions::init_vectors();
	// Enable FP/SIMD on this core (CPACR_EL1 resets with FP trapped).
	super::enable_fp();
	let mpidr: u64;
	unsafe {
		core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
	}
	super::percpu::init(cpu_id as usize, mpidr as u32);
	// AND THE STACK THIS CORE IS STANDING ON, which nothing described until now.
	//
	// `SEC_STACKS` is a static array of 16 KiB slices with NO GUARD PAGES between them, so a core
	// that runs off the bottom of its slice walks into the previous core's - mapped memory, no
	// fault, silent. What it overwrites there is another core's saved state, and the first thing
	// that core does with a corrupted stack pointer is take an exception on it.
	//
	// The scheduler sets these bounds for every thread it runs and clears them for the idle
	// context, which left the one stack in the system with no guard page also being the one with no
	// check. This closes that: from here on this core's idle and interrupt work is bounded too, and
	// an exception taken on a stack pointer outside the slice is REPORTED rather than turned into a
	// runaway.
	//
	// `record_idle_stack` rather than `set_stack_bounds`: this IS this core's idle stack, so it has
	// to be the value the scheduler restores every time it leaves a thread. Setting only the live
	// pair made the bounds survive exactly until the first context switch back to idle, which is a
	// mistake worth leaving named - the check then covers everything except the moment it is for.
	unsafe {
		let base = (&raw const SEC_STACKS[cpu_id as usize]) as u64;
		super::percpu::record_idle_stack(base, SEC_STACK_SIZE as usize);
	}
	super::gic::init_secondary();
	SEC_MPIDR[cpu_id as usize].store(mpidr, Ordering::Relaxed);
	SMP_ONLINE.fetch_add(1, Ordering::Release);
	// Also count this core in the portable online tally the scheduler and tests read.
	crate::smp::mark_online();
	super::enable_interrupts();
	// The BSP brings the scheduler up (allocate + init) after waking us: spin until it
	// has, then park in the scheduler idle loop so threads can be scheduled onto this
	// core (and the wake IPI bounces us out of the halt to pick them up). Before it is
	// ready the timer IRQ is a no-op (on_timer_preempt is gated on the same flag).
	while !crate::sched::is_initialized() {
		super::idle_halt();
	}
	crate::sched::cpu_idle_loop()
}

// Wake every secondary core (cpu ids 1..cpu_count) via PSCI CPU_ON and wait for
// them to report in. On QEMU virt the MPIDR affinity of cpu N is simply N.
pub fn bring_up_secondaries(cpu_count: u32, boot_arg: u64) {
	if cpu_count <= 1 {
		return;
	}
	// NO CONDUIT, NO SECONDARIES - and a line saying so, rather than a fault into somebody else's
	// exception vectors. A single-core boot is a working system; a machine that dies bringing up its
	// second core is not, and the difference used to be one instruction the kernel had no way to know
	// was unanswered.
	let conduit = conduit(boot_arg);
	match conduit {
		bootproto::PSCI_HVC | bootproto::PSCI_SMC => {}
		_ => {
			crate::serial_println!("aarch64: SMP - no PSCI conduit below this kernel; running on one core");
			return;
		}
	}

	// The secondary entry is the low, physical `.text.boot` stub address; PSCI
	// releases each core there with the MMU off, and the stub adopts the boot
	// page tables the primary already built. High kernel code cannot `adrp` the
	// low symbol directly, so its address is read from a linker-filled data word.
	let entry = unsafe { aarch64_secondary_entry };
	let want = (cpu_count - 1).min((MAX_CPUS - 1) as u32);
	for cpu_id in 1..=want as u64 {
		let status = cpu_on(conduit, cpu_id, entry, cpu_id);
		if status != 0 {
			crate::serial_println!("aarch64: CPU_ON core {cpu_id} failed (PSCI {status})");
		}
	}

	// Wait for the secondaries to come online.
	let mut spins: u64 = 0;
	while SMP_ONLINE.load(Ordering::Acquire) < want && spins < 2_000_000_000 {
		core::hint::spin_loop();
		spins += 1;
	}

	let online = SMP_ONLINE.load(Ordering::Acquire);
	crate::serial_println!("aarch64: SMP - {}/{} secondary cores online", online, want);
	for cpu_id in 1..=want as usize {
		let mpidr = SEC_MPIDR[cpu_id].load(Ordering::Relaxed);
		crate::serial_println!("aarch64:   core {} up (mpidr={:#x})", cpu_id, mpidr & 0xff_ffff);
	}
}

// Declared so the assembly entry point is referenced from Rust.
unsafe extern "C" {
	// The low physical address of the secondary boot stub, filled in by the linker.
	static aarch64_secondary_entry: u64;
}

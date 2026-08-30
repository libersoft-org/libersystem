// aarch64 GIC interrupt controller (v2 and v3 cores) + ARM generic timer.
//
// TWO CONTROLLERS, ONE TIMER AND ONE WAKE IPI. A GICv2 has a memory-mapped CPU interface and a
// per-core enable set inside the distributor; a GICv3 replaces the CPU interface with SYSTEM
// REGISTERS and moves the per-core banked registers into a REDISTRIBUTOR, one frame per core, found
// by matching this core's affinity rather than by indexing a compiled address. Everything above
// them - the timer PPI, the wake SGI, the tick, preemption - is the same on both, so the split is
// exactly the register access and nothing else.
//
// The EL1 physical timer (CNTP_*) raises PPI 14 = INTID 30, which the controller forwards to this
// core.
//
// This is the periodic-tick bring-up: enable the distributor + CPU interface,
// unmask the timer PPI, arm CNTP for a 100 Hz tick, and count ticks in the IRQ
// handler (`handle_irq`, called from the exception vectors). It becomes the
// backing for the portable `apic` (tick) + `tsc` (cycle clock) contract when the
// port routes through the portable scheduler.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// WHERE THE CONTROLLER IS, PUBLISHED BY THE CALLER THAT READ THE MACHINE. These were constants
// naming QEMU's `virt` machine, so this driver could only run where that machine put them - and a
// pass on it proved nothing about a tree describing anything else. `init` takes both addresses from
// the boot prologue, which read them from the device tree and checked they lie inside the direct
// map before handing them over.
static GICD_BASE: AtomicU64 = AtomicU64::new(0);
static GICC_BASE: AtomicU64 = AtomicU64::new(0);
// The GICv3 redistributor region, and which core's frame each is - resolved per core at bring-up.
static GICR_BASE: AtomicU64 = AtomicU64::new(0);
static GICR_SIZE: AtomicU64 = AtomicU64::new(0);
// 2 for a memory-mapped CPU interface, 3 for the system-register one. Read on every interrupt, so
// it is a plain relaxed load of a value written once before any interrupt is enabled.
static VERSION: AtomicU32 = AtomicU32::new(0);

fn v3() -> bool {
	VERSION.load(Ordering::Relaxed) >= 3
}

const GICD_CTLR: usize = 0x000; // distributor control
const GICD_ISENABLER: usize = 0x100; // set-enable (1 bit per INTID)
const GICD_IPRIORITYR: usize = 0x400; // priority (1 byte per INTID)
const GICD_ITARGETSR: usize = 0x800; // CPU targets (1 byte per INTID, SPIs only)
const GICD_ICFGR: usize = 0xc00; // trigger config (2 bits per INTID)

// GICv3 distributor additions. Affinity routing (ARE) changes what the per-SPI registers mean:
// ITARGETSR is gone and IROUTER - one 64-bit register per SPI, holding an affinity - replaces it.
const GICD_TYPER: usize = 0x004; // ITLinesNumber in bits 4:0
const GICD_IGROUPR: usize = 0x080; // group select (1 bit per INTID; 1 = group 1)
const GICD_IROUTER: usize = 0x6000; // 8 bytes per INTID, SPIs only (ARE)
const GICD_CTLR_ARE_NS: u32 = 1 << 4;
const GICD_CTLR_GRP1NS: u32 = 1 << 1;
const GICD_CTLR_RWP: u32 = 1 << 31; // a write is still being taken

// The GICv3 redistributor. Each core owns a pair of 64 KiB frames: RD_base carries the frame's
// identity and its sleep state, SGI_base the banked SGI/PPI registers a GICv2 kept in the
// distributor.
const GICR_STRIDE: u64 = 0x2_0000;
const GICR_TYPER: usize = 0x008; // affinity in bits 63:32, Last in bit 4
const GICR_WAKER: usize = 0x014;
const GICR_WAKER_SLEEP: u32 = 1 << 1; // ProcessorSleep
const GICR_WAKER_CHILDREN: u32 = 1 << 2; // ChildrenAsleep
const GICR_SGI_FRAME: usize = 0x1_0000;
const GICR_IGROUPR0: usize = GICR_SGI_FRAME + 0x080;
const GICR_ISENABLER0: usize = GICR_SGI_FRAME + 0x100;
const GICR_IPRIORITYR: usize = GICR_SGI_FRAME + 0x400;
const GICR_CTLR: usize = 0x000; // bit 0 EnableLPIs
const GICR_PROPBASER: usize = 0x070;
const GICR_PENDBASER: usize = 0x078;

const GICC_CTLR: usize = 0x000; // CPU interface control
const GICC_PMR: usize = 0x004; // priority mask
const GICC_IAR: usize = 0x00c; // interrupt acknowledge (read the pending INTID)
const GICC_EOIR: usize = 0x010; // end of interrupt

// THE EL1 PHYSICAL TIMER'S INTID, READ FROM THE MACHINE.
//
// This was `const TIMER_INTID: u32 = 30` with a comment naming QEMU `virt` - a claim about one
// machine, enabled on every machine, on a port whose whole discovery story is that addresses come
// from the tree. A machine naming another PPI had 30 armed anyway, which is an interrupt it never
// described; a machine naming none had 30 armed too.
//
// Published by the boot path from `BootInfo::timer_intid` before the controller is brought up. Zero
// means the tree named none, and `init` refuses rather than defaulting - see `set_timer_intid`.
static TIMER_INTID_CELL: AtomicU32 = AtomicU32::new(0);

// Record what the tree said the timer's interrupt is. Called once, before `init`.
pub fn set_timer_intid(intid: u32) {
	TIMER_INTID_CELL.store(intid, Ordering::Release);
}

// The same, for the IRQ inventory, which reports what this machine's timer is rather than a constant.
pub fn timer_intid_for_report() -> u32 {
	timer_intid()
}

fn timer_intid() -> u32 {
	TIMER_INTID_CELL.load(Ordering::Acquire)
}

// 100 Hz tick (the shared scheduler-tick policy).
use crate::arch::common::time::TICK_HZ;

// COUNTER-GATED, NOT ONE-PER-CORE (KERN-ARCH-007). Every core takes its own CNTP interrupt, and
// this was `TICKS.fetch_add(1)` in the handler - so the monotonic clock advanced once per core per
// period and time ran at the core count times `TICK_HZ`. See `arch::common::time::TickClock`.
static CLOCK: crate::arch::common::time::TickClock = crate::arch::common::time::TickClock::new();
static INTERVAL: AtomicU64 = AtomicU64::new(0); // timer down-count per tick

#[inline]
fn gicd(off: usize) -> *mut u32 {
	super::paging::phys_to_virt(GICD_BASE.load(Ordering::Relaxed) + off as u64) as *mut u32
}

// This core's redistributor frame, found by AFFINITY rather than by index.
//
// A redistributor frame states which core it belongs to in GICR_TYPER, and the region holds them
// back to back, terminated by the frame whose Last bit is set. Indexing the region by logical CPU id
// would be the same dense assumption the secondary bring-up had: nothing says frame N belongs to the
// core this kernel calls N, and on a machine whose cores are not a flat 0..N it does not.
fn this_redistributor() -> Option<u64> {
	let base = GICR_BASE.load(Ordering::Relaxed);
	let size = GICR_SIZE.load(Ordering::Relaxed);
	if base == 0 {
		return None;
	}
	let mpidr: u64;
	unsafe {
		core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
	}
	let want = (mpidr & 0x00ff_ffff) | ((mpidr >> 32) & 0xff) << 24;
	let mut offset = 0u64;
	while offset + GICR_STRIDE <= size {
		let frame = base + offset;
		let typer = unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(frame + GICR_TYPER as u64) as *const u64) };
		if (typer >> 32) as u32 as u64 == want {
			return Some(frame);
		}
		if typer & (1 << 4) != 0 {
			break; // the region's last frame, and none of them was this core's
		}
		offset += GICR_STRIDE;
	}
	None
}

#[inline]
fn gicr(frame: u64, off: usize) -> *mut u32 {
	super::paging::phys_to_virt(frame + off as u64) as *mut u32
}

// Wait for the distributor to finish taking a write that changes routing.
fn gicd_wait() {
	let mut spins = 0u32;
	while unsafe { core::ptr::read_volatile(gicd(GICD_CTLR)) } & GICD_CTLR_RWP != 0 && spins < 1_000_000 {
		core::hint::spin_loop();
		spins += 1;
	}
}

#[inline]
fn gicc(off: usize) -> *mut u32 {
	super::paging::phys_to_virt(GICC_BASE.load(Ordering::Relaxed) + off as u64) as *mut u32
}

// The generic-timer counter frequency (Hz), from CNTFRQ_EL0.
fn cntfrq() -> u64 {
	let f: u64;
	unsafe {
		core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack, preserves_flags));
	}
	f
}

// Arm the EL1 physical timer to fire one interval from now.
fn arm_timer(interval: u64) {
	unsafe {
		core::arch::asm!("msr cntp_tval_el0, {}", in(reg) interval, options(nomem, nostack, preserves_flags));
	}
}

// Bring up the GIC and start the periodic timer on the boot core: enable the
// (global) distributor, then this core's CPU interface + timer. Interrupts stay
// masked (DAIF.I) until the caller enables them.
// Bring the controller up at the addresses the machine description named.
//
// Both are published before the first access rather than read per access: every other function here
// reaches the controller through `gicd`/`gicc`, and a base that could change under them would be a
// controller that moves while it is being programmed.
pub fn init(version: u8, distributor: u64, second: u64, second_size: u64) {
	VERSION.store(version as u32, Ordering::Relaxed);
	GICD_BASE.store(distributor, Ordering::Relaxed);
	if version >= 3 {
		// The second `reg` range of a GICv3 is the REDISTRIBUTOR region, not a CPU interface: the
		// same two ranges mean different things on the two controllers, which is why the version is
		// read from the machine rather than assumed.
		GICR_BASE.store(second, Ordering::Relaxed);
		GICR_SIZE.store(second_size, Ordering::Relaxed);
		init_distributor_v3();
	} else {
		GICC_BASE.store(second, Ordering::Relaxed);
		unsafe {
			// Distributor on (global - the boot core does this once).
			core::ptr::write_volatile(gicd(GICD_CTLR), 1);
		}
	}
	init_cpu_local();
}

// The GICv3 distributor: affinity routing on, group 1 enabled, and every SPI in group 1.
//
// THE GROUP IS NOT A DETAIL. A GICv3 delivers group 0 interrupts as FIQ and group 1 as IRQ, and
// every one of these registers RESETS TO GROUP 0. A kernel that enables an interrupt without moving
// it to group 1 has armed something its IRQ vector will never see - the interrupt is delivered, to a
// FIQ handler this port does not install.
fn init_distributor_v3() {
	unsafe {
		core::ptr::write_volatile(gicd(GICD_CTLR), GICD_CTLR_ARE_NS | GICD_CTLR_GRP1NS);
		gicd_wait();
		// ITLinesNumber: SPIs run 32..=32*(N+1)-1, so word 1 upward covers them.
		let lines = core::ptr::read_volatile(gicd(GICD_TYPER)) & 0x1f;
		for word in 1..=lines as usize {
			core::ptr::write_volatile(gicd(GICD_IGROUPR + word * 4), u32::MAX);
		}
		gicd_wait();
	}
}

// Bring up a secondary core's CPU interface + timer (the distributor is already on). The CPU
// interface, the redistributor frame and the SGI/PPI enable bits are all per core, so each core must
// run this itself.
//
// ANSWERS WHETHER THIS CORE CAN TAKE INTERRUPTS AT ALL, so its caller can refuse to count it online.
pub fn init_secondary() -> bool {
	init_cpu_local()
}

// Per-core setup: the CPU interface on, the timer PPI and the SGIs unmasked, CNTP armed.
fn init_cpu_local() -> bool {
	if v3() {
		return init_cpu_local_v3();
	}
	{
		unsafe {
			// Allow all priorities through (PMR high) and enable the CPU interface.
			core::ptr::write_volatile(gicc(GICC_PMR), 0xf0);
			core::ptr::write_volatile(gicc(GICC_CTLR), 1);
			// Enable the 16 SGIs (INTID 0..15, banked per core) so the cross-core wake IPI
			// (SGI 0) is delivered and bounces this core out of WFI.
			core::ptr::write_volatile(gicd(GICD_ISENABLER), 0x0000_ffff);
			// Unmask the timer PPI (INTID 30 -> ISENABLER0 bit 30; banked per core).
			let reg = gicd(GICD_ISENABLER + (timer_intid() as usize / 32) * 4);
			core::ptr::write_volatile(reg, 1 << (timer_intid() % 32));
		}
	}
	arm_local_timer();
	// GICv2's per-core state is banked registers on a fixed CPU interface: there is no per-core
	// frame that can be absent, so this path cannot fail the way v3's can.
	true
}

// The GICv3 per-core half: wake this core's redistributor, put its SGIs and PPIs in group 1, enable
// them, and turn on the system-register CPU interface.
fn init_cpu_local_v3() -> bool {
	let Some(frame) = this_redistributor() else {
		// A core whose redistributor the region does not describe cannot be given interrupts at
		// all - not the timer that drives its preemption, not the SGI that wakes it.
		//
		// SAYING SO WAS THE WHOLE OF WHAT THIS DID, AND IT LET THE CORE RUN. It then joined the
		// online set, the scheduler placed threads on it, and those threads were never preempted and
		// never woken by an IPI - a core that is counted as usable and takes no interrupts is worse
		// than one that is absent, because nothing downstream can tell. The size check on the GICR
		// region proves there are enough BYTES for the described cores; it cannot prove the runtime
		// `GICR_TYPER` affinity chain reaches each of them, and this is where that contradiction
		// shows up.
		//
		// `false` here is what keeps the core out of the online count. It parks instead.
		crate::serial_println!("gic: this core has no redistributor frame in the region the machine described - it takes no interrupts and is not brought online");
		return false;
	};
	unsafe {
		// OUT OF SLEEP FIRST. A redistributor resets asleep and forwards nothing while it is;
		// ChildrenAsleep clears when it is really awake, and the enables below mean nothing before
		// that.
		let waker = gicr(frame, GICR_WAKER);
		core::ptr::write_volatile(waker, core::ptr::read_volatile(waker) & !GICR_WAKER_SLEEP);
		let mut spins = 0u32;
		while core::ptr::read_volatile(waker) & GICR_WAKER_CHILDREN != 0 && spins < 1_000_000 {
			core::hint::spin_loop();
			spins += 1;
		}
		// SGIs and PPIs into group 1, so they arrive as IRQ rather than FIQ.
		core::ptr::write_volatile(gicr(frame, GICR_IGROUPR0), u32::MAX);
		// A priority the CPU interface's mask lets through (PMR is 0xf0 below).
		for intid in 0..32usize {
			core::ptr::write_volatile(gicr(frame, GICR_IPRIORITYR + intid) as *mut u8, 0xa0);
		}
		// The 16 SGIs (the wake IPI is SGI 0) and the timer PPI.
		core::ptr::write_volatile(gicr(frame, GICR_ISENABLER0), 0x0000_ffff | (1 << timer_intid()));

		// THE CPU INTERFACE IS SYSTEM REGISTERS, AND SRE ENABLES THEM. Written by encoding rather
		// than by name so this assembles without depending on the toolchain's register-name table.
		// ICC_SRE_EL1 (S3_0_C12_C12_5): SRE bit 0.
		let mut sre: u64;
		core::arch::asm!("mrs {}, S3_0_C12_C12_5", out(reg) sre, options(nomem, nostack, preserves_flags));
		sre |= 1;
		core::arch::asm!("msr S3_0_C12_C12_5, {}", "isb", in(reg) sre, options(nomem, nostack, preserves_flags));
		// ICC_PMR_EL1 (S3_0_C4_C6_0): let every priority below 0xf0 through.
		core::arch::asm!("msr S3_0_C4_C6_0, {}", in(reg) 0xf0u64, options(nomem, nostack, preserves_flags));
		// ICC_IGRPEN1_EL1 (S3_0_C12_C12_7): group 1 delivery on.
		core::arch::asm!("msr S3_0_C12_C12_7, {}", "isb", in(reg) 1u64, options(nomem, nostack, preserves_flags));
	}
	true
}

// Arm CNTP for a TICK_HZ tick on this core and enable it (ENABLE=1, IMASK=0).
fn arm_local_timer() {
	let interval = cntfrq() / TICK_HZ as u64;
	INTERVAL.store(interval, Ordering::Relaxed);
	arm_timer(interval);
	unsafe {
		core::arch::asm!("msr cntp_ctl_el0, {}", in(reg) 1u64, options(nomem, nostack, preserves_flags));
	}
}

// Acknowledge and dispatch a pending interrupt (called from the IRQ vector).
// `from_user` is true when the interrupt was taken from EL0, so a preemptive
// switch knows the interrupted context was userspace.
pub fn handle_irq(from_user: bool) {
	// A GICv2 reads its acknowledge register over MMIO and a GICv3 out of ICC_IAR1_EL1
	// (S3_0_C12_C12_0). The INTID is 10 bits on the first and 24 on the second.
	let iar: u32 = if v3() {
		let value: u64;
		unsafe { core::arch::asm!("mrs {}, S3_0_C12_C12_0", out(reg) value, options(nomem, nostack, preserves_flags)) };
		value as u32
	} else {
		unsafe { core::ptr::read_volatile(gicc(GICC_IAR)) }
	};
	let intid = if v3() { iar & 0xff_ffff } else { iar & 0x3ff };
	// SGI 0 is the wake IPI, and it now also carries the TLB shootdown - see
	// `mem::tlb`. Servicing it before the dispatch below keeps the answer prompt.
	if intid == 0 {
		crate::mem::tlb::service_pending();
	}
	if intid == timer_intid() {
		// Re-arm for the next tick (clears the timer's level-asserted condition). EVERY core does
		// this - it is what drives preemption on that core - and only the shared clock below is
		// gated, so the rate is the machine's and not the core count's.
		arm_timer(INTERVAL.load(Ordering::Relaxed));
		CLOCK.advance(super::tsc::now(), super::tsc::hz());
	} else {
		// A device MSI - a GICv2m SPI or an ITS LPI: wake the bound userspace driver, if any.
		super::interrupts::dispatch_msi(intid);
	}
	// End of interrupt for any real INTID. 1020..1023 are the special values - spurious, and the
	// ones a secure view uses - and everything else is an interrupt that was taken and must be
	// completed, INCLUDING AN LPI. This read `intid < 1020`, which is true of every SPI a GICv2m
	// frame can raise and false of every LPI an ITS can: the first device MSI on a GICv3 was
	// acknowledged and never completed, so the interface's running priority stayed at that LPI's
	// forever and nothing at the same priority - which is every other device vector - was ever
	// delivered again.
	if !(1020..=1023).contains(&intid) {
		unsafe {
			if v3() {
				core::arch::asm!("msr S3_0_C12_C12_1, {}", in(reg) iar as u64, options(nomem, nostack, preserves_flags));
			} else {
				core::ptr::write_volatile(gicc(GICC_EOIR), iar);
			}
		}
	}
	// The periodic timer tick drives preemption: rotate to the next ready thread on
	// this core (a no-op until the scheduler is up and only if another is ready).
	// EOI is already sent above, matching the x86 timer-ISR order.
	if intid == timer_intid() {
		crate::sched::on_timer_preempt(from_user);
	}
}

// Send a software-generated interrupt (SGI `id`, 0..15) to core `cpu` - the cross-core
// wake IPI. GICD_SGIR selects the target with a per-core bit in the target list; the
// delivery itself is the message (it bounces the target out of WFI so its idle loop
// re-checks its run queue), and gic::handle_irq just EOIs it (SGIs are INTID 0..15).
// Send SGI `id` to one core, addressed by its GICv2 CPU-INTERFACE NUMBER.
//
// THAT IS NOT AN MPIDR, AND THE DIFFERENCE IS THE REFUSAL BELOW. GICD_SGIR's target list is eight
// bits, one per CPU interface, so this controller can only address cores 0-7 and only by interface
// number. What the caller holds is an MPIDR affinity, and the two coincide only on a machine whose
// cores are a flat 0..N - which QEMU virt is, and which is why this worked while carrying `cpu &
// 0xff`. On a machine with a non-zero Aff1 that mask picks interface 0: the IPI goes to the BOOT
// CORE instead of the target, and nothing says so. A GICv3 profile addresses cores by affinity and
// is where such a machine belongs; here the honest answer is to name the limit and not send.
pub fn send_sgi(cpu: u64, id: u32) {
	if v3() {
		// A GICv3 ADDRESSES CORES BY AFFINITY, which is what the caller has been holding all along.
		// ICC_SGI1R_EL1 (S3_0_C12_C11_5) names Aff3:Aff2:Aff1 and then a sixteen-bit TARGET LIST
		// selecting cores within that group by Aff0 - so the eight-interface limit below is a GICv2
		// property this controller does not have, and only Aff0 must fit the list.
		let aff0 = cpu & 0xff;
		if aff0 >= 16 {
			crate::serial_println!("gic: SGI {id} to core {cpu:#x} - a target list selects 16 cores by affinity 0, and this core's is {aff0}");
			return;
		}
		let value = ((cpu >> 32) & 0xff) << 48 | ((cpu >> 16) & 0xff) << 32 | ((id as u64) & 0xf) << 24 | ((cpu >> 8) & 0xff) << 16 | (1u64 << aff0);
		unsafe {
			core::arch::asm!("msr S3_0_C12_C11_5, {}", "isb", in(reg) value, options(nomem, nostack, preserves_flags));
		}
		return;
	}
	const GICD_SGIR: usize = 0xf00;
	const INTERFACES: u64 = 8;
	if cpu >= INTERFACES {
		crate::serial_println!("gic: SGI {id} to core {cpu:#x} - GICv2 addresses {INTERFACES} CPU interfaces by number, so this core is unreachable from here");
		return;
	}
	unsafe {
		core::ptr::write_volatile(gicd(GICD_SGIR), (1 << (16 + cpu)) | (id & 0xf));
	}
}

// Configure a shared peripheral interrupt (SPI) as an edge-triggered MSI routed to
// the boot core, and enable it - the GIC-distributor side of a GICv2m MSI vector (the
// frame and the device's MSI-X table are programmed in arch::interrupts). SPIs are
// INTID 32.., so the byte-per-INTID target/priority registers are writable for them.
pub fn enable_msi_spi(spi: u32) {
	let spi = spi as usize;
	unsafe {
		// Route to the boot core and give it a priority below the CPU-interface mask (PMR 0xf0) so
		// it is delivered. WHICH REGISTER SAYS "THE BOOT CORE" DEPENDS ON THE CONTROLLER: a GICv2
		// names a CPU interface in a byte, and a GICv3 with affinity routing on names an AFFINITY in
		// a 64-bit IROUTER - ITARGETSR reads as zero there and writing it routes nothing.
		if v3() {
			let mpidr: u64;
			core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
			core::ptr::write_volatile(super::paging::phys_to_virt(GICD_BASE.load(Ordering::Relaxed) + (GICD_IROUTER + spi * 8) as u64) as *mut u64, mpidr & 0x0000_00ff_00ff_ffff);
			// And into group 1, for the reason `init_distributor_v3` states.
			let word = gicd(GICD_IGROUPR + (spi / 32) * 4);
			core::ptr::write_volatile(word, core::ptr::read_volatile(word) | 1 << (spi % 32));
		} else {
			core::ptr::write_volatile(gicd(GICD_ITARGETSR + spi) as *mut u8, 0x01);
		}
		core::ptr::write_volatile(gicd(GICD_IPRIORITYR + spi) as *mut u8, 0xa0);
		// Edge-triggered: ICFGR holds 2 bits per INTID; the high bit selects edge.
		let icfgr = gicd(GICD_ICFGR + (spi / 16) * 4);
		let shift = (spi % 16) * 2;
		let cfg = (core::ptr::read_volatile(icfgr) & !(0b11 << shift)) | (0b10 << shift);
		core::ptr::write_volatile(icfgr, cfg);
		// Enable the SPI.
		core::ptr::write_volatile(gicd(GICD_ISENABLER + (spi / 32) * 4), 1 << (spi % 32));
	}
}

// This core's redistributor frame, for a caller that has to name it to something else - the ITS
// targets a collection at a redistributor ADDRESS.
pub fn redistributor() -> Option<u64> {
	this_redistributor()
}

// The redistributor's own processor number, which is what an ITS with PTA clear names a core by.
//
// NOT THE LOGICAL CPU ID AND NOT THE AFFINITY. It is a third numbering, the controller's own, and
// GICR_TYPER is the only thing that states it.
pub fn processor_number(frame: u64) -> u32 {
	let typer = unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(frame + GICR_TYPER as u64) as *const u64) };
	(typer >> 8 & 0xffff) as u32
}

// Turn on LPI delivery at one redistributor, against the configuration table the ITS owns.
//
// ONCE ONLY, AND BEFORE ANYTHING ELSE READS THEM. `GICR_PROPBASER` and `GICR_PENDBASER` become
// read-only the moment `EnableLPIs` is set, and the bit cannot be cleared again - so a redistributor
// gets one chance at this, which is why it is a step the ITS bring-up drives rather than something
// the per-core path guesses at.
pub fn enable_lpis(frame: u64, config: u64, id_bits: u64, pending: u64) -> bool {
	unsafe {
		let ctlr = gicr(frame, GICR_CTLR);
		if core::ptr::read_volatile(ctlr) & 1 != 0 {
			crate::serial_println!("gic: this redistributor already has LPIs enabled - its tables cannot be replaced");
			return false;
		}
		// Inner-cacheable write-back (5 in bits 9:7), inner-shareable (1 in bits 11:10).
		let prop = (config & 0x000f_ffff_ffff_f000) | 5 << 7 | 1 << 10 | id_bits;
		core::ptr::write_volatile(gicr(frame, GICR_PROPBASER) as *mut u64, prop);
		// The same, plus PTZ: this kernel zeroed the table, so the redistributor need not.
		let pend = (pending & 0x000f_ffff_ffff_0000) | 5 << 7 | 1 << 10 | 1 << 62;
		core::ptr::write_volatile(gicr(frame, GICR_PENDBASER) as *mut u64, pend);
		core::arch::asm!("dsb ish", options(nostack, preserves_flags));
		core::ptr::write_volatile(ctlr, core::ptr::read_volatile(ctlr) | 1);
	}
	true
}

// Ticks counted since the timer started (the monotonic tick, 100 Hz).
pub fn ticks() -> u64 {
	CLOCK.ticks()
}

// The generic-timer frequency (Hz), for the boot log.
pub fn timer_hz() -> u64 {
	cntfrq()
}

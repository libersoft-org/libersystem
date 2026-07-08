// aarch64 GICv2 interrupt controller + ARM generic timer (M116).
//
// QEMU's `virt` machine (with gic-version=2) maps the GIC distributor (GICD) at
// 0x0800_0000 and the CPU interface (GICC) at 0x0801_0000. The EL1 physical timer
// (CNTP_*) raises PPI 14 = INTID 30, which the distributor forwards to this core.
//
// This is the periodic-tick bring-up: enable the distributor + CPU interface,
// unmask the timer PPI, arm CNTP for a 100 Hz tick, and count ticks in the IRQ
// handler (`handle_irq`, called from the exception vectors). It becomes the
// backing for the portable `apic` (tick) + `tsc` (cycle clock) contract when the
// port routes through the portable scheduler.

use core::sync::atomic::{AtomicU64, Ordering};

const GICD_BASE: usize = 0x0800_0000; // distributor
const GICC_BASE: usize = 0x0801_0000; // CPU interface

const GICD_CTLR: usize = 0x000; // distributor control
const GICD_ISENABLER: usize = 0x100; // set-enable (1 bit per INTID)
const GICD_IPRIORITYR: usize = 0x400; // priority (1 byte per INTID)
const GICD_ITARGETSR: usize = 0x800; // CPU targets (1 byte per INTID, SPIs only)
const GICD_ICFGR: usize = 0xc00; // trigger config (2 bits per INTID)

const GICC_CTLR: usize = 0x000; // CPU interface control
const GICC_PMR: usize = 0x004; // priority mask
const GICC_IAR: usize = 0x00c; // interrupt acknowledge (read the pending INTID)
const GICC_EOIR: usize = 0x010; // end of interrupt

// The EL1 physical timer interrupt on QEMU virt (PPI 14 -> INTID 30).
const TIMER_INTID: u32 = 30;

// 100 Hz tick (the shared scheduler-tick policy).
use crate::arch::common::time::TICK_HZ;

static TICKS: AtomicU64 = AtomicU64::new(0);
static INTERVAL: AtomicU64 = AtomicU64::new(0); // timer down-count per tick

#[inline]
fn gicd(off: usize) -> *mut u32 {
	super::paging::phys_to_virt((GICD_BASE + off) as u64) as *mut u32
}
#[inline]
fn gicc(off: usize) -> *mut u32 {
	super::paging::phys_to_virt((GICC_BASE + off) as u64) as *mut u32
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
pub fn init() {
	unsafe {
		// Distributor on (global - the boot core does this once).
		core::ptr::write_volatile(gicd(GICD_CTLR), 1);
	}
	init_cpu_local();
}

// Bring up a secondary core's GIC CPU interface + timer (the distributor is
// already on). The GICv2 CPU interface and the SGI/PPI enable bits are banked
// per core, so each core must run this itself.
pub fn init_secondary() {
	init_cpu_local();
}

// Per-core GIC + timer setup: CPU interface on, unmask the timer PPI, arm CNTP.
fn init_cpu_local() {
	unsafe {
		// Allow all priorities through (PMR high) and enable the CPU interface.
		core::ptr::write_volatile(gicc(GICC_PMR), 0xf0);
		core::ptr::write_volatile(gicc(GICC_CTLR), 1);
		// Enable the 16 SGIs (INTID 0..15, banked per core) so the cross-core wake IPI
		// (SGI 0) is delivered and bounces this core out of WFI.
		core::ptr::write_volatile(gicd(GICD_ISENABLER), 0x0000_ffff);
		// Unmask the timer PPI (INTID 30 -> ISENABLER0 bit 30; banked per core).
		let reg = gicd(GICD_ISENABLER + (TIMER_INTID as usize / 32) * 4);
		core::ptr::write_volatile(reg, 1 << (TIMER_INTID % 32));

		// Arm CNTP for a TICK_HZ tick and enable it (ENABLE=1, IMASK=0).
		let interval = cntfrq() / TICK_HZ as u64;
		INTERVAL.store(interval, Ordering::Relaxed);
		arm_timer(interval);
		core::arch::asm!("msr cntp_ctl_el0, {}", in(reg) 1u64, options(nomem, nostack, preserves_flags));
	}
}

// Acknowledge and dispatch a pending interrupt (called from the IRQ vector).
// `from_user` is true when the interrupt was taken from EL0, so a preemptive
// switch knows the interrupted context was userspace.
pub fn handle_irq(from_user: bool) {
	let iar = unsafe { core::ptr::read_volatile(gicc(GICC_IAR)) };
	let intid = iar & 0x3ff;
	if intid == TIMER_INTID {
		// Re-arm for the next tick (clears the timer's level-asserted condition).
		arm_timer(INTERVAL.load(Ordering::Relaxed));
		TICKS.fetch_add(1, Ordering::Relaxed);
	} else {
		// A device MSI (GICv2m SPI): wake the bound userspace driver, if any.
		super::interrupts::dispatch_msi(intid);
	}
	// End of interrupt for any real INTID (1020..1023 are special / spurious).
	if intid < 1020 {
		unsafe {
			core::ptr::write_volatile(gicc(GICC_EOIR), iar);
		}
	}
	// The periodic timer tick drives preemption: rotate to the next ready thread on
	// this core (a no-op until the scheduler is up and only if another is ready).
	// EOI is already sent above, matching the x86 timer-ISR order.
	if intid == TIMER_INTID {
		crate::sched::on_timer_preempt(from_user);
	}
}

// Send a software-generated interrupt (SGI `id`, 0..15) to core `cpu` - the cross-core
// wake IPI. GICD_SGIR selects the target with a per-core bit in the target list; the
// delivery itself is the message (it bounces the target out of WFI so its idle loop
// re-checks its run queue), and gic::handle_irq just EOIs it (SGIs are INTID 0..15).
pub fn send_sgi(cpu: u32, id: u32) {
	const GICD_SGIR: usize = 0xf00;
	unsafe {
		core::ptr::write_volatile(gicd(GICD_SGIR), (1 << (16 + (cpu & 0xff))) | (id & 0xf));
	}
}

// Configure a shared peripheral interrupt (SPI) as an edge-triggered MSI routed to
// the boot core, and enable it - the GIC-distributor side of a GICv2m MSI vector (the
// frame and the device's MSI-X table are programmed in arch::interrupts). SPIs are
// INTID 32.., so the byte-per-INTID target/priority registers are writable for them.
pub fn enable_msi_spi(spi: u32) {
	let spi = spi as usize;
	unsafe {
		// Route to the boot core (CPU 0) and give it a priority below the CPU-interface
		// mask (PMR 0xf0) so it is delivered.
		core::ptr::write_volatile(gicd(GICD_ITARGETSR + spi) as *mut u8, 0x01);
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

// Ticks counted since the timer started (the monotonic tick, 100 Hz).
pub fn ticks() -> u64 {
	TICKS.load(Ordering::Relaxed)
}

// The generic-timer frequency (Hz), for the boot log.
pub fn timer_hz() -> u64 {
	cntfrq()
}

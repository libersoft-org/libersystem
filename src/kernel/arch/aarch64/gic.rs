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

const GICC_CTLR: usize = 0x000; // CPU interface control
const GICC_PMR: usize = 0x004; // priority mask
const GICC_IAR: usize = 0x00c; // interrupt acknowledge (read the pending INTID)
const GICC_EOIR: usize = 0x010; // end of interrupt

// The EL1 physical timer interrupt on QEMU virt (PPI 14 -> INTID 30).
const TIMER_INTID: u32 = 30;

// 100 Hz tick.
const TICK_HZ: u64 = 100;

static TICKS: AtomicU64 = AtomicU64::new(0);
static INTERVAL: AtomicU64 = AtomicU64::new(0); // timer down-count per tick

#[inline]
fn gicd(off: usize) -> *mut u32 {
	(GICD_BASE + off) as *mut u32
}
#[inline]
fn gicc(off: usize) -> *mut u32 {
	(GICC_BASE + off) as *mut u32
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

// Bring up the GIC and start the periodic timer. Interrupts stay masked (DAIF.I)
// until the caller enables them.
pub fn init() {
	unsafe {
		// Distributor + CPU interface on; allow all priorities through (PMR high).
		core::ptr::write_volatile(gicd(GICD_CTLR), 1);
		core::ptr::write_volatile(gicc(GICC_PMR), 0xf0);
		core::ptr::write_volatile(gicc(GICC_CTLR), 1);
		// Unmask the timer PPI (INTID 30 -> ISENABLER0 bit 30).
		let reg = gicd(GICD_ISENABLER + (TIMER_INTID as usize / 32) * 4);
		core::ptr::write_volatile(reg, 1 << (TIMER_INTID % 32));

		// Arm CNTP for a TICK_HZ tick and enable it (ENABLE=1, IMASK=0).
		let interval = cntfrq() / TICK_HZ;
		INTERVAL.store(interval, Ordering::Relaxed);
		arm_timer(interval);
		core::arch::asm!("msr cntp_ctl_el0, {}", in(reg) 1u64, options(nomem, nostack, preserves_flags));
	}
}

// Acknowledge and dispatch a pending interrupt (called from the IRQ vector).
pub fn handle_irq() {
	let iar = unsafe { core::ptr::read_volatile(gicc(GICC_IAR)) };
	let intid = iar & 0x3ff;
	if intid == TIMER_INTID {
		// Re-arm for the next tick (clears the timer's level-asserted condition).
		arm_timer(INTERVAL.load(Ordering::Relaxed));
		TICKS.fetch_add(1, Ordering::Relaxed);
	}
	// End of interrupt for any real INTID (1020..1023 are special / spurious).
	if intid < 1020 {
		unsafe {
			core::ptr::write_volatile(gicc(GICC_EOIR), iar);
		}
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

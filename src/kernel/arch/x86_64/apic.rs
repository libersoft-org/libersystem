// Local APIC: software enable, end-of-interrupt, and a periodic timer.
//
// We use the LAPIC in xAPIC (MMIO) mode and drive a periodic timer interrupt.
// The timer is calibrated against the legacy PIT (channel 2, polled) so one tick
// is a real wall-clock interval rather than an arbitrary count.
//
// The LAPIC MMIO page is mapped explicitly as uncacheable (Limine's HHDM does
// not cover it). Each core's LAPIC lives at the same physical address, so the
// single mapping serves every core.

#![allow(dead_code)]

use super::port::outb;
use super::{msr, paging, pit};
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

// IA32_APIC_BASE model-specific register.
const IA32_APIC_BASE_MSR: u32 = 0x1b;
const APIC_BASE_ENABLE: u64 = 1 << 11; // global enable bit
const APIC_BASE_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

// LAPIC register offsets (bytes) within the MMIO page.
const REG_ID: u32 = 0x20; // local APIC id (in bits 24-31)
const REG_EOI: u32 = 0xb0;
const REG_SVR: u32 = 0xf0; // spurious interrupt vector register
const REG_LVT_TIMER: u32 = 0x320;
const REG_TIMER_INITIAL: u32 = 0x380;
const REG_TIMER_CURRENT: u32 = 0x390;
const REG_TIMER_DIVIDE: u32 = 0x3e0;

const SVR_ENABLE: u32 = 1 << 8; // APIC software enable
const LVT_TIMER_PERIODIC: u32 = 1 << 17;
const LVT_MASKED: u32 = 1 << 16;
const TIMER_DIVIDE_16: u32 = 0x3;

// Desired periodic tick rate.
const TIMER_HZ: u32 = 100;

// Virtual address where the LAPIC MMIO page is mapped (its own dedicated page,
// since Limine's HHDM does not cover the LAPIC MMIO region).
const LAPIC_VIRT: u64 = 0xffff_f100_0000_0000;

// Virtual (mapped) address of the LAPIC MMIO page, published once during init.
static LAPIC_BASE: AtomicUsize = AtomicUsize::new(0);

// Monotonic timer tick counter.
static TICKS: AtomicU64 = AtomicU64::new(0);

// The bootstrap processor's local APIC id, captured at init. Used only as a fallback
// for advancing the clock before the TSC is calibrated; once the TSC is up, any core
// advances the monotonic tick counter (see advance_ticks), so no single core owns the
// clock and one core stalling cannot freeze time.
static BSP_APIC_ID: AtomicU32 = AtomicU32::new(0);

// The LAPIC timer count for one TIMER_HZ period, calibrated once on the BSP (via the
// PIT) and reused to program each AP's timer - every core's LAPIC runs at the same
// frequency, so the BSP's calibration applies to all of them.
static TIMER_INITIAL: AtomicU32 = AtomicU32::new(0);

// One TIMER_HZ period expressed in TSC cycles (TSC frequency / TIMER_HZ), seeded
// lazily on the first tick once the TSC is calibrated. Zero until then. Gates how
// often any core may advance the monotonic tick counter, so the clock runs at exactly
// TIMER_HZ regardless of how many cores take periodic ticks.
static CYCLES_PER_TICK: AtomicU64 = AtomicU64::new(0);

// The TSC value at which the monotonic tick counter was last advanced. Whichever
// core's timer fires a full period past this point claims every period elapsed
// since it (via a CAS) and bumps TICKS by that many, so the clock keeps advancing
// at TIMER_HZ even if one core stalls - and a stall of the whole guest is caught
// up in one jump, not replayed tick by tick.
static LAST_TICK_TSC: AtomicU64 = AtomicU64::new(0);

fn read(reg: u32) -> u32 {
	let base = LAPIC_BASE.load(Ordering::Relaxed);
	unsafe { ((base + reg as usize) as *const u32).read_volatile() }
}

fn write(reg: u32, value: u32) {
	let base = LAPIC_BASE.load(Ordering::Relaxed);
	unsafe { ((base + reg as usize) as *mut u32).write_volatile(value) };
}

// This core's local APIC id (bits 24-31 of the ID register). An MMIO read, valid in
// any context (no per-CPU GS base needed, unlike the percpu cpu id), so the timer ISR
// can use it to tell the BSP from an AP even when it interrupted ring-3 code.
fn local_apic_id() -> u32 {
	read(REG_ID) >> 24
}

// Signal end-of-interrupt to the LAPIC. Must be called once per delivered
// interrupt so further interrupts of equal or lower priority can be delivered.
pub fn eoi() {
	write(REG_EOI, 0);
}

// Number of timer ticks since the timer started.
pub fn ticks() -> u64 {
	TICKS.load(Ordering::Relaxed)
}

pub fn init() {
	disable_pic();

	// Globally enable the LAPIC and map its MMIO page (uncacheable). The HHDM
	// does not cover MMIO, so we map the page explicitly with our own paging.
	let base_msr = msr::read(IA32_APIC_BASE_MSR);
	let phys = base_msr & APIC_BASE_ADDR_MASK;
	msr::write(IA32_APIC_BASE_MSR, base_msr | APIC_BASE_ENABLE);
	paging::map_page(LAPIC_VIRT, phys, paging::WRITABLE | paging::NO_CACHE | paging::NO_EXECUTE);
	LAPIC_BASE.store(LAPIC_VIRT as usize, Ordering::Relaxed);

	// Software-enable the APIC and set the spurious-interrupt vector.
	write(REG_SVR, SVR_ENABLE | super::interrupts::SPURIOUS_VECTOR as u32);

	// Remember which core is the BSP, so only its periodic timer advances the global
	// tick counter (APs run the same timer just to wake from the idle HALT).
	BSP_APIC_ID.store(local_apic_id(), Ordering::Relaxed);

	// Start the periodic timer. Its IDT gate (the preemptive `timer` stub) is
	// installed by interrupts::init; here we only program the LAPIC LVT and count.
	start_timer();
}

// Per-core LAPIC bring-up for an application processor. The LAPIC is already
// globally enabled and its MMIO page mapped by the BSP's init(); each core only
// software-enables its own LAPIC and sets the spurious vector. Each AP also starts
// its own periodic timer (reusing the BSP's calibration), not to keep time - only the
// BSP advances the tick counter - but to wake the core from the idle HALT within one
// tick so it can re-check its run queue instead of busy-spinning.
pub fn init_ap() {
	let base_msr = msr::read(IA32_APIC_BASE_MSR);
	msr::write(IA32_APIC_BASE_MSR, base_msr | APIC_BASE_ENABLE);
	write(REG_SVR, SVR_ENABLE | super::interrupts::SPURIOUS_VECTOR as u32);
	write(REG_TIMER_DIVIDE, TIMER_DIVIDE_16);
	write(REG_LVT_TIMER, LVT_TIMER_PERIODIC | super::interrupts::TIMER_VECTOR as u32);
	write(REG_TIMER_INITIAL, TIMER_INITIAL.load(Ordering::Relaxed));
}

// Advance the monotonic tick counter and service the serial ring. Called from the
// timer ISR on every core. The clock is gated by elapsed TSC time rather than owned
// by one core, so the rate stays at TIMER_HZ and time keeps flowing even if a single
// core stalls and stops taking its own timer interrupt (see advance_ticks).
pub(super) fn on_timer_tick() {
	advance_ticks();
	// Service the asynchronous serial transmit ring in the background, so debug
	// output never busy-waits on the UART in the writer's context (e.g. on the
	// console render thread). try_lock-guarded, so it never spins in this ISR.
	super::serial::drain_tx();
}

// Advance the global monotonic tick counter at TIMER_HZ. Any core's timer may drive
// it, but a tick only counts once a full TSC period has elapsed since the last one,
// so the rate is independent of the core count - and because it is not tied to one
// core, the clock survives any single core stalling (the original design counted only
// the BSP's tick, so a stalled BSP froze the clock and hung every tick-based wait).
fn advance_ticks() {
	let cpt = CYCLES_PER_TICK.load(Ordering::Acquire);
	if cpt == 0 {
		seed_clock();
		return;
	}
	let now = super::tsc::now();
	let last = LAST_TICK_TSC.load(Ordering::Relaxed);
	// Signed distance: a racing core may re-anchor past our `now` reading (or a
	// core's TSC may sit slightly behind its peers), making the unsigned difference
	// wrap to an enormous value - read that as "the anchor is ahead, nothing due"
	// instead of claiming an astronomic backlog.
	let elapsed = now.wrapping_sub(last) as i64;
	if elapsed < cpt as i64 {
		return;
	}
	// Claim every full period since the anchor in ONE step: exactly one core wins
	// the CAS and advances the clock by the whole backlog. Catching up one tick per
	// interrupt instead would replay a stall's backlog at the ISR rate times the
	// core count - time would run fast in bursts whenever the host starves the
	// vCPUs, and every relative deadline (the caret blink, timeouts) would fire in
	// rapid succession. A single jump keeps the long-term rate exact and delivers
	// at most one expiry per deadline.
	let periods = elapsed as u64 / cpt;
	if LAST_TICK_TSC.compare_exchange(last, last.wrapping_add(periods * cpt), Ordering::AcqRel, Ordering::Relaxed).is_ok() {
		TICKS.fetch_add(periods, Ordering::Relaxed);
	}
}

// Anchor the TSC-gated clock on the first tick after the TSC is calibrated. Stores
// the anchor before publishing the period (release/acquire), so any core that later
// observes a non-zero CYCLES_PER_TICK also sees the anchor and never measures an
// interval against a zero baseline. Until the TSC is up (hz == 0) the BSP advances
// the clock, so early ticks before calibration still move time forward.
fn seed_clock() {
	let hz = super::tsc::hz();
	if hz == 0 {
		if local_apic_id() == BSP_APIC_ID.load(Ordering::Relaxed) {
			TICKS.fetch_add(1, Ordering::Relaxed);
		}
		return;
	}
	LAST_TICK_TSC.store(super::tsc::now(), Ordering::Relaxed);
	CYCLES_PER_TICK.store(hz / TIMER_HZ as u64, Ordering::Release);
}

fn start_timer() {
	let initial = calibrate();
	TIMER_INITIAL.store(initial, Ordering::Relaxed);
	write(REG_TIMER_DIVIDE, TIMER_DIVIDE_16);
	write(REG_LVT_TIMER, LVT_TIMER_PERIODIC | super::interrupts::TIMER_VECTOR as u32);
	write(REG_TIMER_INITIAL, initial);
}

// Measure how many LAPIC timer ticks elapse in one timer period (1 / TIMER_HZ),
// using the PIT channel 2 one-shot as the reference clock.
fn calibrate() -> u32 {
	let pit_count = (pit::FREQ / TIMER_HZ) as u16;
	unsafe {
		pit::arm_one_shot(pit_count);

		// Run the LAPIC timer down from the maximum while the PIT counts.
		write(REG_TIMER_DIVIDE, TIMER_DIVIDE_16);
		write(REG_TIMER_INITIAL, 0xffff_ffff);

		pit::wait_terminal();

		let elapsed = 0xffff_ffff - read(REG_TIMER_CURRENT);
		write(REG_LVT_TIMER, LVT_MASKED);
		elapsed
	}
}

// Remap both 8259 PICs away from the exception range and mask every line, so no
// legacy IRQ is ever delivered: the LAPIC is our only interrupt source.
fn disable_pic() {
	const PIC1_CMD: u16 = 0x20;
	const PIC1_DATA: u16 = 0x21;
	const PIC2_CMD: u16 = 0xa0;
	const PIC2_DATA: u16 = 0xa1;
	unsafe {
		outb(PIC1_CMD, 0x11); // ICW1: begin init, expect ICW4
		outb(PIC2_CMD, 0x11);
		outb(PIC1_DATA, 0x20); // ICW2: master vector offset 0x20
		outb(PIC2_DATA, 0x28); // ICW2: slave vector offset 0x28
		outb(PIC1_DATA, 0x04); // ICW3: slave on IRQ2
		outb(PIC2_DATA, 0x02); // ICW3: slave identity
		outb(PIC1_DATA, 0x01); // ICW4: 8086 mode
		outb(PIC2_DATA, 0x01);
		outb(PIC1_DATA, 0xff); // mask all IRQs
		outb(PIC2_DATA, 0xff);
	}
}

// Local APIC: software enable, end-of-interrupt, and a periodic timer.
//
// We use the LAPIC in xAPIC (MMIO) mode and drive a periodic timer interrupt.
// The timer is calibrated against the legacy PIT (channel 2, polled) so one tick
// is a real wall-clock interval rather than an arbitrary count.
//
// The LAPIC MMIO page is mapped explicitly as uncacheable (the loader's HHDM does
// not cover it). Each core's LAPIC lives at the same physical address, so the
// single mapping serves every core.

use super::port::outb;
use super::{msr, paging, pit};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

// IA32_APIC_BASE model-specific register.
const IA32_APIC_BASE_MSR: u32 = 0x1b;
const APIC_BASE_ENABLE: u64 = 1 << 11; // global enable bit
const APIC_BASE_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

// LAPIC register offsets (bytes) within the MMIO page.
const REG_ID: u32 = 0x20; // local APIC id (in bits 24-31)
const REG_EOI: u32 = 0xb0;
const REG_SVR: u32 = 0xf0; // spurious interrupt vector register
const REG_ICR_LOW: u32 = 0x300; // interrupt command register (write low dword sends)
const REG_ICR_HIGH: u32 = 0x310; // destination LAPIC id (in bits 24-31)
const REG_LVT_TIMER: u32 = 0x320;
const REG_TIMER_INITIAL: u32 = 0x380;
const REG_TIMER_CURRENT: u32 = 0x390;
const REG_TIMER_DIVIDE: u32 = 0x3e0;

const SVR_ENABLE: u32 = 1 << 8; // APIC software enable
const LVT_TIMER_PERIODIC: u32 = 1 << 17;
// TSC-deadline mode (LVT timer bits 18:17 = 10b): the timer fires once when the TSC reaches the value
// written to IA32_TSC_DEADLINE, and writing zero disarms it.
const LVT_TIMER_TSC_DEADLINE: u32 = 2 << 17;
const IA32_TSC_DEADLINE: u32 = 0x6e0;
const LVT_MASKED: u32 = 1 << 16;
const TIMER_DIVIDE_16: u32 = 0x3;

// Desired periodic tick rate (the shared scheduler-tick policy).
use crate::arch::common::time::TICK_HZ as TIMER_HZ;

// Virtual address where the LAPIC MMIO page is mapped (its own dedicated page,
// since the loader's HHDM does not cover the LAPIC MMIO region).
const LAPIC_VIRT: u64 = 0xffff_f100_0000_0000;

// Virtual (mapped) address of the LAPIC MMIO page, published once during init.
static LAPIC_BASE: AtomicUsize = AtomicUsize::new(0);

// The LAPIC timer count for one TIMER_HZ period, calibrated once on the BSP (via the
// PIT) and reused to program each AP's timer - every core's LAPIC runs at the same
// frequency, so the BSP's calibration applies to all of them.
static TIMER_INITIAL: AtomicU32 = AtomicU32::new(0);

// WHETHER THIS CPU OFFERS THE TSC-DEADLINE TIMER (CPUID.01H:ECX[24]), read once on the BSP. An idle core's
// one-shot is a deadline on the very counter the clock is computed from where it does, and a LAPIC count
// re-armed in steps where it does not - which is what `-cpu host,-tsc-deadline` runs, since KVM with the
// in-kernel local APIC always offers the deadline mode.
static TSC_DEADLINE: AtomicBool = AtomicBool::new(false);

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

// This core's local APIC id, for SMP bring-up (identifying the BSP and, later,
// targeting each application processor's wake sequence).
pub fn local_id() -> u32 {
	local_apic_id()
}

// Signal end-of-interrupt to the LAPIC. Must be called once per delivered
// interrupt so further interrupts of equal or lower priority can be delivered.
pub fn eoi() {
	write(REG_EOI, 0);
}

// Send the wake IPI to `dest_lapic`: a fixed-delivery, edge-triggered interrupt on
// the wake vector, whose only job is to bounce the target core out of its idle HALT
// so it re-checks its run queue immediately instead of on its next timer tick.
// xAPIC ICR: wait out any in-flight send (delivery-status bit 12), write the
// destination to the high dword, then the low dword (vector + fixed delivery +
// physical destination) - the low write sends.
pub fn send_wake_ipi(dest_lapic: u64) {
	// THE PORTABLE TABLE IS WIDER THAN THIS FIELD, and this is where that is checked. An APIC id
	// is 32 bits and the xAPIC ICR destination is the top byte of one; the ids here come from the
	// MADT, which cannot express more, so a wider value means the table was written by something
	// that is not the MADT walk. Dropping the IPI wakes the target one timer tick later; sending it
	// to `dest & 0xff` wakes the WRONG CORE, which is a silent scheduling fault.
	let Ok(dest_lapic) = u32::try_from(dest_lapic) else {
		crate::serial_println!("apic: wake IPI to controller id {dest_lapic:#x}, which is wider than an APIC id - not sent");
		return;
	};
	while read(REG_ICR_LOW) & (1 << 12) != 0 {
		core::hint::spin_loop();
	}
	write(REG_ICR_HIGH, dest_lapic << 24);
	write(REG_ICR_LOW, super::interrupts::WAKE_VECTOR as u32);
}

// Send an INIT IPI to `dest_lapic` (physical destination, assert, edge), the first
// step of the INIT-SIPI-SIPI application-processor bring-up sequence. Waits out any
// in-flight send before and after (delivery-status bit 12).
pub fn send_init(dest_lapic: u32) {
	while read(REG_ICR_LOW) & (1 << 12) != 0 {
		core::hint::spin_loop();
	}
	write(REG_ICR_HIGH, dest_lapic << 24);
	// INIT (delivery mode 101), assert, edge, physical destination.
	write(REG_ICR_LOW, 0x0000_4500);
	while read(REG_ICR_LOW) & (1 << 12) != 0 {
		core::hint::spin_loop();
	}
}

// Send a STARTUP IPI (SIPI) to `dest_lapic` naming the real-mode trampoline page by
// `vector` (the page's physical address >> 12), the second/third step of AP bring-up.
pub fn send_startup(dest_lapic: u32, vector: u8) {
	while read(REG_ICR_LOW) & (1 << 12) != 0 {
		core::hint::spin_loop();
	}
	write(REG_ICR_HIGH, dest_lapic << 24);
	// STARTUP (delivery mode 110), assert, edge, physical destination + vector.
	write(REG_ICR_LOW, 0x0000_4600 | vector as u32);
	while read(REG_ICR_LOW) & (1 << 12) != 0 {
		core::hint::spin_loop();
	}
}

// Number of timer ticks since the timer started.
//
// A TEST build adds a skew the harness controls. Two bounds in the storage service - an idle
// stream and a listing nobody reads - are enforced by a deadline, and a deadline cannot be reached
// from a test that only pumps the scheduler: a hundred thousand passes advance this counter by a
// few hundred ticks, so a thirty-second bound is about a million pumps away. Both were built and
// removed for want of this.
pub fn ticks() -> u64 {
	let base = crate::arch::common::time::CLOCK.ticks(super::tsc::now());
	#[cfg(test)]
	{
		base + crate::tests::clock_skew()
	}
	#[cfg(not(test))]
	{
		base
	}
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

	// The one-shot this core's idle halts will use.
	TSC_DEADLINE.store(core::arch::x86_64::__cpuid(1).ecx & (1 << 24) != 0, Ordering::Relaxed);

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

// THE BOOT CORE'S LAPIC AFTER S3, which reset it with the machine: the legacy PIC masked again - the firmware's
// resume may have programmed it - and the LAPIC enabled as an application processor's is, on the calibration and
// the timer mode boot measured.
pub fn resume_boot_core() {
	disable_pic();
	init_ap();
}

// Service the serial ring on a timer interrupt. Called from the timer ISR on every core. The clock is
// not moved here any more - it is computed from the TSC when read (`arch::common::time::CLOCK`) - so a
// core whose timer is a one-shot, or silent, stops nobody's time.
pub(super) fn on_timer_tick() {
	// Service the asynchronous serial transmit ring in the background, so debug
	// output never busy-waits on the UART in the writer's context (e.g. on the
	// console render thread). try_lock-guarded, so it never spins in this ISR.
	super::serial::drain_tx();
}

// Whether idle halts arm the TSC-deadline timer (false: a LAPIC one-shot count), for the boot's report.
pub fn tsc_deadline() -> bool {
	TSC_DEADLINE.load(Ordering::Relaxed)
}

// THIS CORE'S TIMER AS A ONE-SHOT for a halt: it fires once at tick `deadline` - never, for `None` or a tick
// too far away to express - instead of the periodic tick. False when the clock cannot convert a tick yet
// (it is not anchored), and the periodic tick then stays.
//
// The TSC-deadline timer where the CPU offers it, which is a deadline on the counter the clock is computed
// from, converted through the sleep offset. Otherwise a LAPIC ONE-SHOT COUNT: `TIMER_INITIAL` counts to a
// tick, and a wait longer than 32 bits of count ends early and is simply halted again - re-armed in steps.
pub fn timer_one_shot(deadline: Option<u64>) -> bool {
	let clock = &crate::arch::common::time::CLOCK;
	if !clock.anchored() {
		return false;
	}
	timer_at_counter(deadline.and_then(|tick| clock.counter_at(tick)))
}

// THIS CORE'S TIMER AS A ONE-SHOT AT A RAW COUNTER READING - never, for `None`. The sleep entry's timed wake, which is
// a deadline on the counter while the clock is held and no tick converts; `timer_one_shot` for a tick. False when the
// clock has no rate yet.
pub fn timer_at_counter(target: Option<u64>) -> bool {
	let clock = &crate::arch::common::time::CLOCK;
	if !clock.anchored() {
		return false;
	}
	if TSC_DEADLINE.load(Ordering::Relaxed) {
		write(REG_LVT_TIMER, LVT_TIMER_TSC_DEADLINE | super::interrupts::TIMER_VECTOR as u32);
		// THE MODE BEFORE THE DEADLINE: an MSR write is not ordered after an MMIO write by itself, and a
		// deadline written while the LVT is still periodic is ignored.
		core::sync::atomic::fence(Ordering::SeqCst);
		// Zero disarms, so a deadline at counter zero is the next cycle rather than none.
		msr::write(IA32_TSC_DEADLINE, target.map_or(0, |counter| counter.max(1)));
		return true;
	}
	let count: u32 = match target {
		// A zero initial count stops the one-shot: no timer at all.
		None => 0,
		Some(counter) => {
			let cycles = counter.wrapping_sub(super::tsc::now()) as i64;
			if cycles <= 0 {
				1
			} else {
				let per_tick = clock.cycles_per_tick().max(1) as u128;
				(cycles as u128 * TIMER_INITIAL.load(Ordering::Relaxed) as u128 / per_tick).clamp(1, u32::MAX as u128) as u32
			}
		}
	};
	write(REG_LVT_TIMER, super::interrupts::TIMER_VECTOR as u32);
	write(REG_TIMER_INITIAL, count);
	true
}

// THIS CORE'S PERIODIC TICK, back after a halt: the time slice and every other duty a busy core's tick
// carries.
pub fn timer_periodic() {
	if TSC_DEADLINE.load(Ordering::Relaxed) {
		// Disarmed first, so a deadline the halt did not reach cannot fire into the periodic mode.
		msr::write(IA32_TSC_DEADLINE, 0);
	}
	write(REG_LVT_TIMER, LVT_TIMER_PERIODIC | super::interrupts::TIMER_VECTOR as u32);
	write(REG_TIMER_INITIAL, TIMER_INITIAL.load(Ordering::Relaxed));
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

#[cfg(test)]
mod tests;

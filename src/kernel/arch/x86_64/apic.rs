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

use core::arch::asm;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::msr;
use super::paging;

// IA32_APIC_BASE model-specific register.
const IA32_APIC_BASE_MSR: u32 = 0x1b;
const APIC_BASE_ENABLE: u64 = 1 << 11; // global enable bit
const APIC_BASE_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

// LAPIC register offsets (bytes) within the MMIO page.
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

// PIT runs at a fixed 1.193182 MHz.
const PIT_FREQ: u32 = 1_193_182;

// Virtual address where the LAPIC MMIO page is mapped (its own dedicated page,
// since Limine's HHDM does not cover the LAPIC MMIO region).
const LAPIC_VIRT: u64 = 0xffff_f100_0000_0000;

// Virtual (mapped) address of the LAPIC MMIO page, published once during init.
static LAPIC_BASE: AtomicUsize = AtomicUsize::new(0);

// Monotonic timer tick counter.
static TICKS: AtomicU64 = AtomicU64::new(0);

#[inline]
unsafe fn outb(port: u16, value: u8) {
	asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
	let value: u8;
	asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
	value
}

fn rdmsr(msr: u32) -> u64 {
	msr::read(msr)
}

fn wrmsr(msr: u32, value: u64) {
	msr::write(msr, value);
}

fn read(reg: u32) -> u32 {
	let base = LAPIC_BASE.load(Ordering::Relaxed);
	unsafe { ((base + reg as usize) as *const u32).read_volatile() }
}

fn write(reg: u32, value: u32) {
	let base = LAPIC_BASE.load(Ordering::Relaxed);
	unsafe { ((base + reg as usize) as *mut u32).write_volatile(value) };
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
	let base_msr = rdmsr(IA32_APIC_BASE_MSR);
	let phys = base_msr & APIC_BASE_ADDR_MASK;
	wrmsr(IA32_APIC_BASE_MSR, base_msr | APIC_BASE_ENABLE);
	paging::map_page(LAPIC_VIRT, phys, paging::WRITABLE | paging::NO_CACHE);
	LAPIC_BASE.store(LAPIC_VIRT as usize, Ordering::Relaxed);

	// Software-enable the APIC and set the spurious-interrupt vector.
	write(REG_SVR, SVR_ENABLE | super::interrupts::SPURIOUS_VECTOR as u32);

	// Count ticks on the timer vector, then start the periodic timer.
	super::interrupts::register(super::interrupts::TIMER_VECTOR, tick);
	start_timer();
}

// Per-core LAPIC bring-up for an application processor. The LAPIC is already
// globally enabled and its MMIO page mapped by the BSP's init(); each core only
// software-enables its own LAPIC and sets the spurious vector. APs do not start
// the periodic timer in M3 (the PIT calibration path is BSP-only).
pub fn init_ap() {
	let base_msr = rdmsr(IA32_APIC_BASE_MSR);
	wrmsr(IA32_APIC_BASE_MSR, base_msr | APIC_BASE_ENABLE);
	write(REG_SVR, SVR_ENABLE | super::interrupts::SPURIOUS_VECTOR as u32);
}

fn tick(_vector: u8) {
	TICKS.fetch_add(1, Ordering::Relaxed);
}

fn start_timer() {
	let initial = calibrate();
	write(REG_TIMER_DIVIDE, TIMER_DIVIDE_16);
	write(REG_LVT_TIMER, LVT_TIMER_PERIODIC | super::interrupts::TIMER_VECTOR as u32);
	write(REG_TIMER_INITIAL, initial);
}

// Measure how many LAPIC timer ticks elapse in one timer period (1 / TIMER_HZ),
// using the PIT channel 2 one-shot as the reference clock.
fn calibrate() -> u32 {
	let pit_count = (PIT_FREQ / TIMER_HZ) as u16;
	unsafe {
		// Enable the channel-2 gate, disable the speaker output.
		outb(0x61, (inb(0x61) & 0xfc) | 0x01);

		// Channel 2, lobyte/hibyte access, mode 0 (interrupt on terminal count).
		outb(0x43, 0b1011_0000);
		outb(0x42, (pit_count & 0xff) as u8);
		outb(0x42, (pit_count >> 8) as u8);

		// Toggle the gate low->high to (re)start the count from the loaded value.
		let gate = inb(0x61) & 0xfe;
		outb(0x61, gate);
		outb(0x61, gate | 0x01);

		// Run the LAPIC timer down from the maximum while the PIT counts.
		write(REG_TIMER_DIVIDE, TIMER_DIVIDE_16);
		write(REG_TIMER_INITIAL, 0xffff_ffff);

		// Wait for the PIT channel-2 output (port 0x61 bit 5) to go high.
		while inb(0x61) & 0x20 == 0 {
			core::hint::spin_loop();
		}

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

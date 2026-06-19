// Time Stamp Counter: a fine-grained cycle clock for latency measurement.
//
// The LAPIC tick counter (apic::ticks) only advances at TIMER_HZ (100 Hz, i.e.
// every 10 ms), far too coarse to time an IPC round-trip that takes hundreds of
// nanoseconds. The TSC increments every CPU cycle, giving the resolution we
// need. Its frequency is calibrated once against the PIT so a raw cycle count
// can be reported as nanoseconds.

#![allow(dead_code)]

use super::port::{inb, outb};
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

// The PIT runs at a fixed 1.193182 MHz.
const PIT_FREQ: u32 = 1_193_182;

// Calibrated TSC frequency in Hz (cycles per second); 0 until init() runs.
static TSC_HZ: AtomicU64 = AtomicU64::new(0);

// Read the time-stamp counter. The leading lfence keeps the read from being
// reordered ahead of the work being measured; omitting the nomem option makes
// the compiler treat this as a memory access, so it will not hoist loads or
// stores across it either - the timed region stays put.
#[inline]
pub fn now() -> u64 {
	let lo: u32;
	let hi: u32;
	unsafe {
		asm!("lfence", "rdtsc", out("eax") lo, out("edx") hi, options(nostack, preserves_flags));
	}
	((hi as u64) << 32) | lo as u64
}

// Calibrate the TSC frequency against a 10 ms PIT channel-2 one-shot window.
// This polls the PIT output bit rather than taking an interrupt, so it must run
// with interrupts disabled (an interrupt could delay observing the terminal
// count and inflate the cycle total). It uses the same reference clock as
// apic::calibrate(); the two run sequentially on the BSP, never concurrently.
pub fn init() {
	const WINDOW_HZ: u32 = 100; // a 10 ms measurement window
	let pit_count = (PIT_FREQ / WINDOW_HZ) as u16;
	let cycles = unsafe {
		// Enable the channel-2 gate, disable the speaker output.
		outb(0x61, (inb(0x61) & 0xfc) | 0x01);
		// Channel 2, lobyte/hibyte access, mode 0 (interrupt on terminal count).
		outb(0x43, 0b1011_0000);
		outb(0x42, (pit_count & 0xff) as u8);
		outb(0x42, (pit_count >> 8) as u8);
		// Toggle the gate low->high to start the count from the loaded value.
		let gate = inb(0x61) & 0xfe;
		outb(0x61, gate);
		outb(0x61, gate | 0x01);

		let start = now();
		// Wait for the PIT channel-2 output (port 0x61 bit 5) to go high.
		while inb(0x61) & 0x20 == 0 {
			core::hint::spin_loop();
		}
		now().wrapping_sub(start)
	};
	// cycles elapsed in 1/WINDOW_HZ seconds, so cycles * WINDOW_HZ = cycles/sec.
	TSC_HZ.store(cycles.wrapping_mul(WINDOW_HZ as u64), Ordering::Relaxed);
}

// The calibrated TSC frequency in Hz, or 0 if init() has not run.
pub fn hz() -> u64 {
	TSC_HZ.load(Ordering::Relaxed)
}

// Convert a TSC cycle count to nanoseconds using the calibrated frequency.
// Returns 0 if the TSC has not been calibrated.
pub fn cycles_to_ns(cycles: u64) -> u64 {
	let hz = TSC_HZ.load(Ordering::Relaxed);
	if hz == 0 {
		return 0;
	}
	// ns = cycles * 1e9 / hz; the intermediate uses u128 to avoid overflow.
	((cycles as u128 * 1_000_000_000) / hz as u128) as u64
}

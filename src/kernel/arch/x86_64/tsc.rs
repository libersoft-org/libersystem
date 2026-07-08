// Time Stamp Counter: a fine-grained cycle clock for latency measurement.
//
// The LAPIC tick counter (apic::ticks) only advances at TIMER_HZ (100 Hz, i.e.
// every 10 ms), far too coarse to time an IPC round-trip that takes hundreds of
// nanoseconds. The TSC increments every CPU cycle, giving the resolution we
// need. Its frequency is calibrated once against the PIT so a raw cycle count
// can be reported as nanoseconds.

#![allow(dead_code)]

use super::pit;
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

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
	let pit_count = (pit::FREQ / WINDOW_HZ) as u16;
	let cycles = unsafe {
		pit::arm_one_shot(pit_count);

		let start = now();
		pit::wait_terminal();
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
	crate::arch::common::time::cycles_to_ns(cycles, TSC_HZ.load(Ordering::Relaxed))
}

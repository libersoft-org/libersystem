// Legacy 8254 PIT (Programmable Interval Timer) channel 2, used as a polled
// reference clock to calibrate the LAPIC timer (apic) and the TSC (tsc).
//
// Channel 2 is special: its gate is software-controlled via port 0x61 and its
// output level is readable there (bit 5), unlike channels 0/1. That lets us run
// a one-shot countdown and poll for the terminal count without taking an
// interrupt - the shared mechanism both calibrations rely on. The two callers
// run sequentially on the BSP with interrupts disabled, never concurrently.

use super::port::{inb, outb};

// The PIT input clock: a fixed 1.193182 MHz.
pub(crate) const FREQ: u32 = 1_193_182;

// Arm PIT channel 2 as a one-shot that counts down `pit_count` input ticks
// (mode 0, interrupt on terminal count). Enables the channel-2 gate, disables the
// speaker, loads the count, and toggles the gate low->high to start. The caller
// begins its own measurement immediately after, then calls `wait_terminal`.
// Interrupts must be disabled so polling the output is not delayed.
pub(crate) unsafe fn arm_one_shot(pit_count: u16) {
	unsafe {
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
	}
}

// Busy-wait until the channel-2 output (port 0x61 bit 5) goes high, i.e. the
// one-shot armed by `arm_one_shot` reached its terminal count.
pub(crate) unsafe fn wait_terminal() {
	unsafe {
		while inb(0x61) & 0x20 == 0 {
			core::hint::spin_loop();
		}
	}
}

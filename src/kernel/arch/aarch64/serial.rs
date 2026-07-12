// PL011 UART - the console on QEMU's `virt` machine (UART0 at 0x0900_0000).
//
// This is a minimal polled driver, enough for the bring-up: transmit a byte
// (wait while the TX FIFO is full, then write UARTDR) and read one (if the RX
// FIFO is not empty). The kernel runs in the higher half, so the device's MMIO is
// reached through the physical direct map (`phys_to_virt`). Interrupt-driven RX +
// the async TX ring come later with the GIC.

use super::paging::phys_to_virt;
use core::fmt::{self, Write};

// UART0 on QEMU virt.
const UART_BASE: u64 = 0x0900_0000;
const UARTDR: u64 = 0x00; // data register
const UARTFR: u64 = 0x18; // flag register
const FR_RXFE: u32 = 1 << 4; // receive FIFO empty
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full

#[inline]
fn reg(off: u64) -> *mut u32 {
	phys_to_virt(UART_BASE + off) as *mut u32
}

pub fn init() {
	// QEMU's PL011 is usable out of reset (the firmware/ROM left it enabled); no
	// baud or line-control programming is needed to transmit.
}

fn put_byte(b: u8) {
	unsafe {
		while core::ptr::read_volatile(reg(UARTFR)) & FR_TXFF != 0 {
			core::hint::spin_loop();
		}
		core::ptr::write_volatile(reg(UARTDR), b as u32);
	}
}

pub fn write_bytes(bytes: &[u8]) -> usize {
	for &b in bytes {
		if b == b'\n' {
			put_byte(b'\r');
		}
		put_byte(b);
	}
	bytes.len()
}

pub fn read_byte() -> Option<u8> {
	unsafe { if core::ptr::read_volatile(reg(UARTFR)) & FR_RXFE != 0 { None } else { Some(core::ptr::read_volatile(reg(UARTDR)) as u8) } }
}

// The interrupt / async-TX surface (used by the portable console path); these
// become real once the GIC is up. Polled transmit needs none of them.
pub fn enable_rx_irq() {}

pub fn enable_async() {}

pub fn drain_tx() {}

pub fn flush_sync() {}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		write_bytes(s.as_bytes());
		Ok(())
	}
}

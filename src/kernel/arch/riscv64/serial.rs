// riscv64 serial console: the NS16550 UART at 0x1000_0000 on QEMU's `virt` machine.
//
// THE SAME DEVICE THE FIRMWARE USES, and that is the whole point. This was the SBI legacy console
// (EID 0x01 `console_putchar`), which is the obvious choice - OpenSBI is already there, no driver
// is needed - and which on this machine writes to nothing. Measured: a full test-kernel boot made
// 9256 `console_putchar` ecalls and not one byte reached the serial log, while the loader's own
// output through the same UART did. Every riscv64 line the kernel has ever printed has been lost,
// including the ones a boot failure would print, which is why "riscv64 hangs after
// ExitBootServices with no kernel output" was diagnosed for a day as a hang. It was not: the
// kernel was running, with a console nobody could hear.
//
// A polled driver, like the aarch64 PL011 beside it: wait for the holding register to empty, write
// the byte. The kernel runs in the higher half, so the MMIO is reached through the physical direct
// map (`phys_to_virt`) - which the boot stub installs before it branches here, so this is usable
// from the kernel's first line.
//
// The base is QEMU `virt`'s, stated rather than discovered, exactly as the aarch64 port states
// UART0's. The device tree carries it (`/soc/serial@10000000`, `ns16550a`) and reading it from
// there is what a second riscv64 machine would need; nothing else in this port is portable to one
// yet, and a console that lies about its own generality is worse than one that does not claim any.

use super::paging::phys_to_virt;
use core::fmt::{self, Write};

// The NS16550 on QEMU virt. Byte-wide registers, no shift.
const UART_BASE: u64 = 0x1000_0000;
const RBR_THR: u64 = 0x00; // receive buffer (read) / transmit holding (write)
const LSR: u64 = 0x05; // line status
#[cfg(not(test))]
const LSR_DATA_READY: u8 = 1 << 0;
const LSR_THR_EMPTY: u8 = 1 << 5;

#[inline]
fn reg(off: u64) -> *mut u8 {
	phys_to_virt(UART_BASE + off) as *mut u8
}

// Nothing to program: the firmware left the line configured, and this port only ever runs after
// firmware. Divisor and line-control setup belongs with a machine that boots the kernel cold.
pub fn init() {}

pub fn enable_async() {}

pub fn drain_tx() {}

pub fn flush_sync() {}

fn put_byte(b: u8) {
	unsafe {
		while core::ptr::read_volatile(reg(LSR)) & LSR_THR_EMPTY == 0 {
			core::hint::spin_loop();
		}
		core::ptr::write_volatile(reg(RBR_THR), b);
	}
}

// Write `bytes` to the console, returning the count written. A newline is sent as CR LF, because
// the far end is a terminal and the firmware that printed before this did the same.
pub fn write_bytes(bytes: &[u8]) -> usize {
	for &b in bytes {
		if b == b'\n' {
			put_byte(b'\r');
		}
		put_byte(b);
	}
	bytes.len()
}

// Read one input byte if available (polled).
#[cfg(not(test))]
pub fn read_byte() -> Option<u8> {
	unsafe {
		if core::ptr::read_volatile(reg(LSR)) & LSR_DATA_READY == 0 {
			return None;
		}
		Some(core::ptr::read_volatile(reg(RBR_THR)))
	}
}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		write_bytes(s.as_bytes());
		Ok(())
	}
}

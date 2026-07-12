// riscv64 serial console over SBI (bring-up).
//
// OpenSBI (the M-mode firmware QEMU's `virt` machine boots) exposes a console the
// kernel reaches through an `ecall` (a trap into M-mode). This uses the legacy SBI
// console calls - console_putchar (EID 0x01) and console_getchar (EID 0x02) - which
// OpenSBI always provides, so the kernel has a serial console with no device driver
// during early bring-up. A native NS16550 driver (QEMU virt at 0x1000_0000) can
// replace it later for interrupt-driven RX; for now the SBI console is polled.

use core::fmt::{self, Write};

// Write one byte to the SBI console (legacy EID 0x01, console_putchar).
fn sbi_putchar(byte: u8) {
	unsafe {
		core::arch::asm!(
			"ecall",
			in("a7") 1usize, // EID: legacy console_putchar
			in("a0") byte as usize,
			lateout("a0") _,
			options(nostack, preserves_flags),
		);
	}
}

// Read one byte from the SBI console (legacy EID 0x02, console_getchar), or None
// when the input queue is empty (SBI returns -1).
fn sbi_getchar() -> Option<u8> {
	let ret: isize;
	unsafe {
		core::arch::asm!(
			"ecall",
			in("a7") 2usize, // EID: legacy console_getchar
			lateout("a0") ret,
			options(nostack, preserves_flags),
		);
	}
	if ret < 0 { None } else { Some(ret as u8) }
}

// The SBI console is always up; nothing to initialize.
pub fn init() {}

pub fn enable_rx_irq() {}

pub fn enable_async() {}

pub fn drain_tx() {}

pub fn flush_sync() {}

// Write `bytes` to the console, returning the count written (always all of them -
// the SBI call cannot partially fail here).
pub fn write_bytes(bytes: &[u8]) -> usize {
	for &b in bytes {
		sbi_putchar(b);
	}
	bytes.len()
}

// Read one input byte if available (polled).
pub fn read_byte() -> Option<u8> {
	sbi_getchar()
}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		write_bytes(s.as_bytes());
		Ok(())
	}
}

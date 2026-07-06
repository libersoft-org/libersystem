// COM1 serial output for loader diagnostics. The loader logs to the same 16550
// UART (I/O port 0x3F8) the kernel and the QEMU test harness use, so its progress
// lines appear in the boot serial log. Output only - the loader never reads.

use core::arch::asm;

const COM1: u16 = 0x3F8;

// Write a byte to an I/O port.
unsafe fn outb(port: u16, val: u8) {
	unsafe { asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags)) };
}

// Read a byte from an I/O port.
unsafe fn inb(port: u16) -> u8 {
	let val: u8;
	unsafe { asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags)) };
	val
}

// Program the UART: 115200 8N1, FIFO on, interrupts off. Idempotent.
pub fn init() {
	unsafe {
		outb(COM1 + 1, 0x00); // interrupts off
		outb(COM1 + 3, 0x80); // DLAB on
		outb(COM1 + 0, 0x01); // divisor low (115200)
		outb(COM1 + 1, 0x00); // divisor high
		outb(COM1 + 3, 0x03); // 8N1, DLAB off
		outb(COM1 + 2, 0xC7); // FIFO enable + clear, 14-byte threshold
		outb(COM1 + 4, 0x0B); // DTR/RTS/OUT2
	}
}

// Transmit one byte, waiting for the holding register to drain.
fn write_byte(byte: u8) {
	unsafe {
		while inb(COM1 + 5) & 0x20 == 0 {}
		outb(COM1, byte);
	}
}

// Write a string, expanding newlines to CRLF so serial terminals advance cleanly.
pub fn write_str(s: &str) {
	for byte in s.bytes() {
		if byte == b'\n' {
			write_byte(b'\r');
		}
		write_byte(byte);
	}
}

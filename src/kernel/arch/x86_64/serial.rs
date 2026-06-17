// COM1 serial driver (16550 UART)

use core::arch::asm;
use core::fmt::{self, Write};

const COM1: u16 = 0x3F8;

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

// UART init: 38400 baud, 8N1, FIFO enabled
pub fn init() {
	unsafe {
		outb(COM1 + 1, 0x00);
		outb(COM1 + 3, 0x80);
		outb(COM1 + 0, 0x03);
		outb(COM1 + 1, 0x00);
		outb(COM1 + 3, 0x03);
		outb(COM1 + 2, 0xC7);
		outb(COM1 + 4, 0x0B);
	}
}

fn transmit_empty() -> bool {
	unsafe { (inb(COM1 + 5) & 0x20) != 0 }
}

fn write_byte(byte: u8) {
	while !transmit_empty() {}
	unsafe {
		outb(COM1, byte);
	}
}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		for byte in s.bytes() {
			if byte == b'\n' {
				write_byte(b'\r');
			}
			write_byte(byte);
		}
		Ok(())
	}
}

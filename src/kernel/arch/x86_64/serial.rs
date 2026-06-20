// COM1 serial driver (16550 UART)

use super::port::{inb, outb};
use core::fmt::{self, Write};

const COM1: u16 = 0x3F8;

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

// True if the UART has a received byte waiting (Line Status Register, DR bit).
#[cfg(not(test))]
fn data_ready() -> bool {
	unsafe { (inb(COM1 + 5) & 0x01) != 0 }
}

// Read one received byte without waiting: Some(byte) if one is buffered, else
// None. Lets a poller (the serial CLI) check for input without blocking.
#[cfg(not(test))]
pub fn read_byte() -> Option<u8> {
	if data_ready() { Some(unsafe { inb(COM1) }) } else { None }
}

// Read one received byte, spinning until one arrives. Interrupts stay enabled
// while spinning, so the timer and other cores keep running.
#[cfg(not(test))]
pub fn read_byte_blocking() -> u8 {
	loop {
		if let Some(byte) = read_byte() {
			return byte;
		}
		core::hint::spin_loop();
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

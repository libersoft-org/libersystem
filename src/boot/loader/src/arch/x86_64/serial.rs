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

// How many times the fallback transmit polls the line-status register before giving the byte up.
//
// THE WAIT WAS UNBOUNDED. `while inb(COM1 + 5) & 0x20 == 0 {}` on a machine with no UART at 0x3F8 -
// which is every machine UEFI actually promises anything about, since this address is QEMU's and
// nothing else's - reads a floating bus forever. And this runs after the firmware console has been
// released, so the symptom is a machine that stops with no output at all, in the one place where
// the loader's whole job is to be able to say what happened. A bounded spin drops the byte instead,
// which loses a diagnostic; the alternative loses the machine.
//
// Large enough that a real UART draining at 115200 - about 87 us per byte - is never cut off: this
// is several milliseconds of polling on any plausible core.
const TRANSMIT_SPINS: u32 = 1_000_000;

// Transmit one byte, waiting for the holding register to drain.
//
// THE FIRMWARE'S CONSOLE FIRST. The address below is QEMU's, and UEFI promises nothing about a
// device region at an address this loader made up - see `crate::console`.
pub fn write_byte(byte: u8) {
	if crate::console::write_byte(byte) {
		return;
	}
	unsafe {
		let mut spins = TRANSMIT_SPINS;
		while inb(COM1 + 5) & 0x20 == 0 {
			spins -= 1;
			if spins == 0 {
				return;
			}
			core::hint::spin_loop();
		}
		outb(COM1, byte);
	}
}

// Write a string, expanding newlines to CRLF so serial terminals advance cleanly.
pub fn write_str(s: &str) {
	if crate::console::write_str(s) {
		return;
	}
	for byte in s.bytes() {
		if byte == b'\n' {
			write_byte(b'\r');
		}
		write_byte(byte);
	}
}

// A 64-bit value as `0x...` hex, for a diagnostic that has to name an ADDRESS.
//
// The loader has no formatter - it is pre-ExitBootServices code with no allocator - so a warning
// that wanted to say which physical range failed had no way to say it, and said "an MMIO range"
// instead. A warning nobody can act on is a warning that costs the reader time and gives nothing.
pub fn write_hex(value: u64) {
	write_str("0x");
	let mut started = false;
	for shift in (0..16).rev() {
		let nibble = ((value >> (shift * 4)) & 0xf) as u8;
		if nibble == 0 && !started && shift != 0 {
			continue;
		}
		started = true;
		write_byte(if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 });
	}
}

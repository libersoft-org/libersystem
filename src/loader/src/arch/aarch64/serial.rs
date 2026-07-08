// PL011 UART output for loader diagnostics on aarch64. QEMU's `virt` machine (and
// the AAVMF firmware that boots on it) puts UART0 at 0x0900_0000; UEFI identity-maps
// device memory, so the loader reaches the data/flag registers directly at their
// physical addresses. Output only - the loader never reads.

const UART_BASE: u64 = 0x0900_0000;
const UARTDR: u64 = 0x00; // data register
const UARTFR: u64 = 0x18; // flag register
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full

#[inline]
fn reg(off: u64) -> *mut u32 {
	(UART_BASE + off) as *mut u32
}

// The firmware left the PL011 enabled (it printed its own banner over it), so no
// baud / line-control programming is needed to transmit.
pub fn init() {}

// Transmit one byte, waiting while the transmit FIFO is full.
pub fn write_byte(byte: u8) {
	unsafe {
		while core::ptr::read_volatile(reg(UARTFR)) & FR_TXFF != 0 {
			core::hint::spin_loop();
		}
		core::ptr::write_volatile(reg(UARTDR), byte as u32);
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

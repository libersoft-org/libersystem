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
//
// THE FIRMWARE'S CONSOLE FIRST. The address below is QEMU's, and UEFI promises nothing about a
// device region at an address this loader made up - see `crate::console`.
pub fn write_byte(byte: u8) {
	if crate::console::write_byte(byte) {
		return;
	}
	unsafe {
		while core::ptr::read_volatile(reg(UARTFR)) & FR_TXFF != 0 {
			core::hint::spin_loop();
		}
		core::ptr::write_volatile(reg(UARTDR), byte as u32);
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

// NS16550A UART output for loader diagnostics on riscv64. QEMU's `virt` machine puts
// the 16550 UART at 0x1000_0000; U-Boot (which boots the loader as an EFI application)
// identity-maps device memory, so the loader reaches the transmit-holding and
// line-status registers directly at their physical addresses. Output only - the
// loader never reads a character.

const UART_BASE: u64 = 0x1000_0000;
const THR: u64 = 0x00; // transmit holding register (write)
const LSR: u64 = 0x05; // line status register
const LSR_THRE: u8 = 1 << 5; // transmit holding register empty

#[inline]
fn reg(off: u64) -> *mut u8 {
	(UART_BASE + off) as *mut u8
}

// U-Boot left the UART initialised (it printed its own banner over it), so no
// baud / line-control programming is needed to transmit.
pub fn init() {}

// Transmit one byte, waiting while the transmit holding register is not yet empty.
pub fn write_byte(byte: u8) {
	unsafe {
		while core::ptr::read_volatile(reg(LSR)) & LSR_THRE == 0 {
			core::hint::spin_loop();
		}
		core::ptr::write_volatile(reg(THR), byte);
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

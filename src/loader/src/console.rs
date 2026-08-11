// The loader's diagnostic output, while the firmware is still there to do it properly.
//
// Every architecture backend carries a UART driver at a FIXED PHYSICAL ADDRESS - PL011 at
// 0x0900_0000 on aarch64, a 16550 at 0x1000_0000 on riscv64 - and `efi_main` starts printing almost
// immediately. Those are QEMU's addresses. UEFI promises an identity map for RAM; it promises
// nothing about a device region at an address the loader made up, so on a machine that is not
// `virt` the first diagnostic line is a store to whatever happens to live there. That is the wrong
// way round: while boot services exist, the firmware's own console is the console, and it works on
// every machine because the firmware wrote the driver for the machine it is running on.
//
// So: output goes to `ConOut` until `ExitBootServices`, and only after that to the built-in UART,
// which from then on is what it always was - a `qemu-virt` fallback, now named as one.

use core::ptr;

use crate::uefi::{self, SimpleTextOutput};

// The firmware's console, or null before it is known and after boot services end.
static mut CON_OUT: *mut SimpleTextOutput = ptr::null_mut();

// Remember the firmware console. Called from `efi_main` before anything is printed.
pub(crate) fn adopt(system_table: *mut uefi::SystemTable) {
	unsafe { CON_OUT = (*system_table).con_out };
}

// Give it up. After `ExitBootServices` the firmware's console is a pointer to memory the loader no
// longer owns, so calling it is worse than printing nothing.
pub(crate) fn release() {
	unsafe { CON_OUT = ptr::null_mut() };
}

// Write a string through the firmware console. False when there is none, which is the signal to
// fall back to the built-in UART.
//
// `OutputString` takes NUL-terminated UTF-16, so this fills a small buffer and flushes it - no
// allocation, because this runs before the heap exists and after it is gone. Newlines are expanded
// to CRLF for the same reason the UART path does it: a terminal that does not do it itself.
pub(crate) fn write_str(s: &str) -> bool {
	let con_out = unsafe { CON_OUT };
	if con_out.is_null() {
		return false;
	}
	let mut buf = [0u16; 64];
	let mut n = 0;
	for unit in s.encode_utf16() {
		if unit == u16::from(b'\n') {
			buf[n] = u16::from(b'\r');
			n += 1;
		}
		buf[n] = unit;
		n += 1;
		// Two units can be added per pass, and one slot is the terminator.
		if n + 2 >= buf.len() {
			buf[n] = 0;
			unsafe { ((*con_out).output_string)(con_out, buf.as_ptr()) };
			n = 0;
		}
	}
	buf[n] = 0;
	unsafe { ((*con_out).output_string)(con_out, buf.as_ptr()) };
	true
}

// One byte, for the callers that build their output a character at a time.
pub(crate) fn write_byte(byte: u8) -> bool {
	let mut one = [0u8; 1];
	one[0] = byte;
	match core::str::from_utf8(&one) {
		Ok(s) => write_str(s),
		// Not a character on its own; the UART path takes it.
		Err(_) => false,
	}
}

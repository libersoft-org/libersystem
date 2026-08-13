// Loader diagnostics on riscv64.
//
// THE FIRMWARE'S CONSOLE FIRST, and after `ExitBootServices` the console the MACHINE named.
//
// This file used to carry a fixed physical address - a 16550 at 0x1000_0000, QEMU's `virt` - and store to it
// directly. UEFI promises an identity map for RAM and nothing at all about a device region at an
// address the loader made up, so on a machine that is not `virt` every post-`ExitBootServices` line
// went into whatever lives there. P02M0129 carried that as an open finding for three rounds because
// closing it needed a device-tree reader the loader did not have.
//
// It has one now (`fdt`), and an SPCR reader for machines with no device tree (`uefi::acpi`), and
// `crate::console` drives whichever they named. A machine that names neither gets SILENCE here,
// which is the answer this file could not give before.

// The firmware console while it exists; nothing to initialise afterwards, because the driver and
// its address both come from the machine.
pub fn init() {}

// Transmit one byte: the firmware console, then the discovered one, then nothing.
pub fn write_byte(byte: u8) {
	if crate::console::write_byte(byte) {
		return;
	}
	let _ = crate::console::write_byte_post_ebs(byte);
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

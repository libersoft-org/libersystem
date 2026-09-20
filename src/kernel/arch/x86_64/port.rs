// Programmed I/O port access (the x86 IN/OUT instructions). The single home for
// the byte/word port helpers shared by the arch modules (serial, apic, tsc, and
// the reset/power-off paths), so the inline-asm sequence lives in exactly one
// place instead of being copied per module.

use core::arch::asm;

// Write a byte to an I/O port.
#[inline]
pub(crate) unsafe fn outb(port: u16, value: u8) {
	unsafe {
		asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
	}
}

// Write a word (16 bits) to an I/O port.
#[inline]
pub(crate) unsafe fn outw(port: u16, value: u16) {
	unsafe {
		asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
	}
}

// Write a long (32 bits) to an I/O port.
#[inline]
pub(crate) unsafe fn outl(port: u16, value: u32) {
	unsafe {
		asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
	}
}

// Read a byte from an I/O port.
#[inline]
pub(crate) unsafe fn inb(port: u16) -> u8 {
	unsafe {
		let value: u8;
		asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
		value
	}
}

// Read a word (16 bits) from an I/O port.
//
// NOT IN A TEST BUILD: its one caller is the ACPI SCI path, which programs a real PM1 register block
// and is `cfg(not(test))` for that reason. Warnings are errors here, so an unused `inw` stops the
// test kernel compiling and takes `test.sh` with it.
#[cfg(not(test))]
#[inline]
pub(crate) unsafe fn inw(port: u16) -> u16 {
	unsafe {
		let value: u16;
		asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack, preserves_flags));
		value
	}
}

// Read a long (32 bits) from an I/O port.
#[inline]
pub(crate) unsafe fn inl(port: u16) -> u32 {
	unsafe {
		let value: u32;
		asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack, preserves_flags));
		value
	}
}

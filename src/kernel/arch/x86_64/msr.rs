// Model-specific register access.

use core::arch::asm;

pub fn read(msr: u32) -> u64 {
	let (low, high): (u32, u32);
	unsafe { asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags)) };
	((high as u64) << 32) | low as u64
}

pub fn write(msr: u32, value: u64) {
	let low = value as u32;
	let high = (value >> 32) as u32;
	unsafe { asm!("wrmsr", in("ecx") msr, in("eax") low, in("edx") high, options(nomem, nostack, preserves_flags)) };
}

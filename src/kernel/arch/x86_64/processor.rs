// THE x86_64 HALF OF PROCESSOR POWER: what this CPU offers an idle state - MONITOR/MWAIT, the invariant TSC the clock is
// computed from, the always-running APIC timer (ARAT) - the entries themselves, and the access to the registers a table
// names: port I/O, and memory mapped uncached into a window of the kernel's own. What is entered and written is
// `crate::processor`'s to decide.

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

use super::paging;
use super::port::{inb, inl, inw, outb, outl, outw};

pub const ARCH: procpower::Arch = procpower::Arch::X86_64;

// THE WINDOW processor registers in memory are mapped into: one page each, uncached, for as long as the kernel runs -
// beside the LAPIC (0xffff_f100), the IOAPIC (f200), the MSI-X tables (f300), ECAM (f400) and the declared registers
// (f500), in a top-level slot of its own.
const REGISTER_VIRT: u64 = 0xffff_f600_0000_0000;
const REGISTER_PAGES: u64 = 64;
// The physical page each slot maps, plus one; zero for a free slot.
static SLOTS: [AtomicU64; REGISTER_PAGES as usize] = [const { AtomicU64::new(0) }; REGISTER_PAGES as usize];

// THE WINDOW'S TOP-LEVEL ENTRY, reserved at boot while the kernel's is the only address space: a register mapped later,
// from ProcessorPowerService's syscall in its own address space, then lands in tables every address space shares - and
// the walk never has to add a kernel-half entry a space created earlier would lack.
pub fn reserve_window() {
	paging::reserve_kernel_top_level(REGISTER_VIRT, REGISTER_PAGES * 0x1000);
}

// What this CPU offers, read from CPUID: leaf 1's MONITOR/MWAIT, leaf 0x80000007's invariant TSC, leaf 6's ARAT.
pub fn cpu() -> procpower::Cpu {
	let mwait = core::arch::x86_64::__cpuid(1).ecx & (1 << 3) != 0;
	let invariant_counter = core::arch::x86_64::__cpuid(0x8000_0000).eax >= 0x8000_0007 && core::arch::x86_64::__cpuid(0x8000_0007).edx & (1 << 8) != 0;
	let timer_always_running = core::arch::x86_64::__cpuid(0).eax >= 6 && core::arch::x86_64::__cpuid(6).eax & (1 << 2) != 0;
	// A core that loses its context resumes only through S3's path, which is the whole machine's: no per-core resume.
	procpower::Cpu { mwait, invariant_counter, timer_always_running, context_resume: false }
}

// THE VIRTUAL ADDRESS of a register in memory: its page mapped uncached into the window, once - a page another register
// already brought in is shared. Refused when the window is full.
pub fn map_register(phys: u64) -> Result<u64, &'static str> {
	let page = phys & !0xfff;
	for (at, slot) in SLOTS.iter().enumerate() {
		let held = slot.load(Ordering::Acquire);
		if held == page + 1 {
			return Ok(REGISTER_VIRT + at as u64 * 0x1000 + (phys & 0xfff));
		}
	}
	for (at, slot) in SLOTS.iter().enumerate() {
		if slot.compare_exchange(0, page + 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
			let virt = REGISTER_VIRT + at as u64 * 0x1000;
			paging::map_page(virt, page, paging::WRITABLE | paging::NO_CACHE | paging::NO_EXECUTE);
			return Ok(virt + (phys & 0xfff));
		}
	}
	Err("the processor window is full")
}

// A REGISTER'S PAGE GIVEN BACK, once no table names any register on it.
pub fn unmap_register(phys: u64) {
	let page = phys & !0xfff;
	for (at, slot) in SLOTS.iter().enumerate() {
		if slot.compare_exchange(page + 1, 0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
			let _ = paging::unmap_page(REGISTER_VIRT + at as u64 * 0x1000);
			unsafe { asm!("invlpg [{}]", in(reg) REGISTER_VIRT + at as u64 * 0x1000, options(nostack, preserves_flags)) };
		}
	}
}

// A port read or write of the register's width.
pub fn port_read(port: u16, bytes: u8) -> u64 {
	// SAFETY: a port the processor table names, admitted and installed in the reserved set under processor power.
	unsafe {
		match bytes {
			1 => u64::from(inb(port)),
			2 => u64::from(inw(port)),
			_ => u64::from(inl(port)),
		}
	}
}

pub fn port_write(port: u16, bytes: u8, value: u64) {
	// SAFETY: as `port_read`.
	unsafe {
		match bytes {
			1 => outb(port, value as u8),
			2 => outw(port, value as u16),
			_ => outl(port, value as u32),
		}
	}
}

// WHETHER THE CPU OFFERS MONITOR/MWAIT, read from CPUID once: the idle path asks at every entry, and in a virtual machine
// CPUID is an exit.
pub fn offers_mwait() -> bool {
	static OFFERED: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
	match OFFERED.load(Ordering::Relaxed) {
		0 => {
			let mwait = cpu().mwait;
			OFFERED.store(if mwait { 2 } else { 1 }, Ordering::Relaxed);
			mwait
		}
		known => known == 2,
	}
}

// AN IDLE ENTRY THAT RETURNED BY ITSELF: the interrupt that ended it pending under the mask is taken now - the mask
// lifted - rather than halted past; with none pending, the entry did not wait (an emulated register), and the halt does.
pub fn after_register_entry() {
	if super::apic::interrupt_pending() {
		// SAFETY: the idle path's own unmask, as the halt's `sti` is.
		unsafe { asm!("sti", "nop", options(nomem, nostack, preserves_flags)) };
	} else {
		super::idle_halt();
	}
}

// A FIRMWARE'S SUSPEND (PSCI, the SBI) is refused at install on this architecture; were one reached, the halt stands in.
pub fn firmware_suspend(_parameter: u32) {
	super::idle_halt();
}

// MWAIT WITH THE STATE'S HINT, entered with interrupts masked: MONITOR on a line of this core's own, then MWAIT with
// ECX bit 0 set, so an interrupt pending under the mask ends it - the halt's own form - and the mask is lifted after.
pub fn mwait(hint: u32) {
	static LINE: AtomicU64 = AtomicU64::new(0);
	// SAFETY: MONITOR on a static the kernel owns; MWAIT with interrupt-break enabled; offered (`cpu().mwait`), which the
	// caller checked.
	unsafe {
		asm!("monitor", in("rax") &LINE as *const AtomicU64 as u64, in("ecx") 0u32, in("edx") 0u32, options(nostack, preserves_flags));
		asm!("mwait", in("eax") hint, in("ecx") 1u32, options(nostack, preserves_flags));
		asm!("sti", options(nomem, nostack, preserves_flags));
	}
}

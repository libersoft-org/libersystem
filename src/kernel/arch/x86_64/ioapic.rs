// I/O APIC: external-interrupt controller, kept fully masked.
//
// Devices deliver their interrupts as per-device MSI-X messages straight to a LAPIC
// (see arch::interrupts), so the kernel routes nothing through the I/O APIC. We still
// map it and mask every redirection entry at boot, so a stray legacy INTx line can
// never reach a CPU - every device interrupt source is either an MSI-X vector or a
// pin the kernel disables (arch::pci::set_intx_disabled).
//
// The MMIO page (the loader's HHDM does not cover it) is mapped uncacheable, like the
// LAPIC's. A single I/O APIC at the standard PC base covers our QEMU q35 target; a
// fuller implementation would enumerate them (and their GSI bases) from the ACPI
// MADT.

use core::sync::atomic::{AtomicUsize, Ordering};

use super::paging;

// The I/O APIC's fixed physical base on the PC platform (QEMU q35 included).
const IOAPIC_PHYS: u64 = 0xFEC0_0000;
const IOAPIC_VIRT: u64 = 0xffff_f200_0000_0000;

// Indirect MMIO access: write a register index to IOREGSEL, then read/write IOWIN.
const IOREGSEL: usize = 0x00;
const IOWIN: usize = 0x10;

const REG_VERSION: u32 = 0x01;
const REG_REDTBL: u32 = 0x10; // GSI n: low dword at REG_REDTBL + 2n, high at +1

// Redirection-entry low-dword bit we use: every entry is left masked.
const MASKED: u32 = 1 << 16;

// Virtual base of the mapped MMIO page (0 until init maps it).
static BASE: AtomicUsize = AtomicUsize::new(0);

fn read(index: u32) -> u32 {
	let base = BASE.load(Ordering::Relaxed);
	unsafe {
		((base + IOREGSEL) as *mut u32).write_volatile(index);
		((base + IOWIN) as *const u32).read_volatile()
	}
}

fn write(index: u32, value: u32) {
	let base = BASE.load(Ordering::Relaxed);
	unsafe {
		((base + IOREGSEL) as *mut u32).write_volatile(index);
		((base + IOWIN) as *mut u32).write_volatile(value);
	}
}

// Map the I/O APIC MMIO page and mask every redirection entry, so no device can
// raise an interrupt until a driver routes and unmasks its GSI.
pub fn init() {
	paging::map_page(IOAPIC_VIRT, IOAPIC_PHYS, paging::WRITABLE | paging::NO_CACHE | paging::NO_EXECUTE);
	BASE.store(IOAPIC_VIRT as usize, Ordering::Relaxed);
	let count = ((read(REG_VERSION) >> 16) & 0xff) + 1;
	for gsi in 0..count {
		mask(gsi);
	}
}

// Mask `gsi`'s redirection entry, leaving its routing intact (used at init to silence
// every entry, since the kernel takes all device interrupts via MSI-X).
// Route a GSI to `vector` on the core with LAPIC id `dest_lapic` and unmask it: fixed delivery,
// physical destination, edge-triggered, active-high (the ISA defaults - the serial UART's legacy
// IRQ is the one line the kernel routes). The boot tail routes it, so a test build never asks.
#[cfg(not(test))]
// Route `gsi` to `vector` on one core, addressed by the redirection entry's PHYSICAL DESTINATION -
// eight bits at 63:56, which is an xAPIC id and nothing wider. A machine with more than 256 cores
// needs the interrupt-remapping path to address the rest; until then the honest answer for an
// unaddressable core is to say so rather than to write `id & 0xff` and route the IRQ to whichever
// core that happens to name.
pub fn route(gsi: u32, vector: u8, dest_lapic: u64) {
	let Ok(dest_lapic) = u8::try_from(dest_lapic) else {
		crate::serial_println!("ioapic: GSI {gsi} cannot be routed to controller id {dest_lapic:#x} - the redirection entry addresses 8 bits");
		return;
	};
	write(REG_REDTBL + 2 * gsi + 1, (dest_lapic as u32) << 24);
	write(REG_REDTBL + 2 * gsi, vector as u32);
}

pub fn mask(gsi: u32) {
	if BASE.load(Ordering::Relaxed) == 0 {
		return;
	}
	let lo = REG_REDTBL + 2 * gsi;
	write(lo, read(lo) | MASKED);
}

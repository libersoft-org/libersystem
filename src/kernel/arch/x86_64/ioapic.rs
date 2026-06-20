// I/O APIC: routes external (device) interrupts to CPU vectors.
//
// The LAPIC handles a core's local interrupts (the periodic timer, IPIs); external
// device IRQs instead arrive at the I/O APIC, which we program so each device's GSI
// is delivered to a vector in the IDT's device-IRQ window (interrupts::IRQ_BASE..).
// Until a driver acquires its device's interrupt every redirection entry stays
// masked, so a device can never interrupt a kernel that has not opted in - the same
// "nothing happens until a capability is handed out" rule the object model follows.
//
// The MMIO page (Limine's HHDM does not cover it) is mapped uncacheable, like the
// LAPIC's. A single I/O APIC at the standard PC base covers our QEMU q35 target; a
// fuller implementation would enumerate them (and their GSI bases) from the ACPI
// MADT.

#![allow(dead_code)]

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

// Redirection-entry low-dword bits (delivery = fixed, dest mode = physical are 0).
const POLARITY_ACTIVE_LOW: u32 = 1 << 13;
const TRIGGER_LEVEL: u32 = 1 << 15;
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
	paging::map_page(IOAPIC_VIRT, IOAPIC_PHYS, paging::WRITABLE | paging::NO_CACHE);
	BASE.store(IOAPIC_VIRT as usize, Ordering::Relaxed);
	let count = ((read(REG_VERSION) >> 16) & 0xff) + 1;
	for gsi in 0..count {
		mask(gsi);
	}
}

// Mask `gsi`'s redirection entry, leaving its routing intact (used on init and when
// a driver releases its interrupt, so a gone driver's device cannot storm us).
pub fn mask(gsi: u32) {
	if BASE.load(Ordering::Relaxed) == 0 {
		return;
	}
	let lo = REG_REDTBL + 2 * gsi;
	write(lo, read(lo) | MASKED);
}

// Clear `gsi`'s mask bit, re-arming a routed entry (used to ack a serviced IRQ).
pub fn unmask(gsi: u32) {
	if BASE.load(Ordering::Relaxed) == 0 {
		return;
	}
	let lo = REG_REDTBL + 2 * gsi;
	write(lo, read(lo) & !MASKED);
}

// Route `gsi` to `vector` on the LAPIC `dest`, and unmask it. PCI INTx is level-
// triggered active-low; an ISA IRQ is edge-triggered active-high (level = false).
pub fn route(gsi: u32, vector: u8, dest: u8, level_active_low: bool) {
	if BASE.load(Ordering::Relaxed) == 0 {
		return;
	}
	let lo = REG_REDTBL + 2 * gsi;
	let mut value = vector as u32;
	if level_active_low {
		value |= TRIGGER_LEVEL | POLARITY_ACTIVE_LOW;
	}
	// High dword: destination LAPIC id in bits 56..63 (bits 24..31 of the high reg).
	// Program the high dword (still masked) before the low dword that unmasks it.
	write(lo + 1, (dest as u32) << 24);
	write(lo, value);
}

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

// THE TWO BITS THAT SAY WHAT KIND OF LINE THIS IS, and they are not decoration.
//
// A redirection entry defaults to the ISA convention - edge-triggered, active-high - and that is
// right for exactly one of the three lines this kernel routes. A PCI INTx line and the ACPI SCI are
// both LEVEL-triggered and ACTIVE-LOW, and configuring either as an edge is not a cosmetic mismatch:
// an edge entry latches the transition, so a source that asserts and STAYS asserted is delivered
// once and then never again until it deasserts. On a shared line - which INTx is by design, and the
// SCI is by definition - the second device to assert while the first is still asserted produces no
// edge at all, and its interrupt does not exist.
// NOT IN A TEST BUILD, for the same reason `route` is not: they exist to be written into a
// redirection entry, `route` is the only thing that writes one, and a test build has no I/O APIC to
// program. Without this the test kernel does not compile at all - warnings are errors here - and
// `test.sh` cannot run, which is a worse failure than the dead code it is complaining about.
#[cfg(not(test))]
const ACTIVE_LOW: u32 = 1 << 13;
#[cfg(not(test))]
const LEVEL_TRIGGERED: u32 = 1 << 15;

/// How a line is asserted and delivered, which the caller knows and this module does not.
#[cfg(not(test))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	/// The ISA convention: edge-triggered, active-high. The 16550's legacy line.
	IsaEdge,
	/// Level-triggered, active-low: a PCI INTx pin, and the ACPI SCI.
	LevelLow,
}

#[cfg(not(test))]
impl Kind {
	fn bits(self) -> u32 {
		match self {
			Kind::IsaEdge => 0,
			Kind::LevelLow => ACTIVE_LOW | LEVEL_TRIGGERED,
		}
	}
}

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
// physical destination, and the trigger and polarity the CALLER states. The boot tail routes them,
// so a test build never asks.
#[cfg(not(test))]
// Route `gsi` to `vector` on one core, addressed by the redirection entry's PHYSICAL DESTINATION -
// eight bits at 63:56, which is an xAPIC id and nothing wider. A machine with more than 256 cores
// needs the interrupt-remapping path to address the rest; until then the honest answer for an
// unaddressable core is to say so rather than to write `id & 0xff` and route the IRQ to whichever
// core that happens to name.
//
// THE KIND IS AN ARGUMENT BECAUSE ONLY THE CALLER KNOWS IT. This wrote the ISA defaults for every
// line, which was right for the UART and wrong for the two lines added after it - see `Kind`.
pub fn route(gsi: u32, vector: u8, dest_lapic: u64, kind: Kind) {
	let Ok(dest_lapic) = u8::try_from(dest_lapic) else {
		crate::serial_println!("ioapic: GSI {gsi} cannot be routed to controller id {dest_lapic:#x} - the redirection entry addresses 8 bits");
		return;
	};
	// THE HIGH DWORD FIRST, WHILE THE ENTRY IS STILL MASKED. Writing the low dword unmasks the line,
	// and an entry unmasked before its destination is written delivers its first interrupt to
	// whichever core the previous contents named.
	write(REG_REDTBL + 2 * gsi + 1, (dest_lapic as u32) << 24);
	write(REG_REDTBL + 2 * gsi, vector as u32 | kind.bits());
}

pub fn mask(gsi: u32) {
	if BASE.load(Ordering::Relaxed) == 0 {
		return;
	}
	let lo = REG_REDTBL + 2 * gsi;
	write(lo, read(lo) | MASKED);
}

// I/O APICs: the external-interrupt controllers, every one the MADT names, each entry masked at boot.
//
// Devices deliver their interrupts as per-device MSI-X messages straight to a LAPIC (see arch::interrupts),
// so the kernel routes through the I/O APICs only the lines that are WIRED: its own - the console UART, the
// SCI, a hot-plug slot's INTx - and a platform device's claimed line. Every redirection entry is masked at
// boot, so a stray legacy INTx line can never reach a CPU.
//
// EVERY CONTROLLER, BY ITS GSI RANGE. This drove one I/O APIC at the fixed PC base and never read the MADT's
// entries, so a Global System Interrupt past the first controller's 24 pins was an entry of a controller
// that was never mapped. The MADT names each controller's window and the first GSI it answers for; a line is
// routed through the controller whose range holds it. A machine whose MADT names none - or a boot with no
// RSDP - gets the one controller at the PC base, which is what every PC has.
//
// The MMIO pages (the loader's HHDM does not cover them) are mapped uncacheable, like the LAPIC's.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use super::paging;

// The I/O APIC's fixed physical base on the PC platform (QEMU q35 included): the fallback.
const IOAPIC_PHYS: u64 = 0xFEC0_0000;
// Where the controllers' windows are mapped, one page each.
const IOAPIC_VIRT: u64 = 0xffff_f200_0000_0000;
// How many controllers this kernel drives. A PC has one; a large server a handful.
const MAX_IOAPICS: usize = 8;

// Indirect MMIO access: write a register index to IOREGSEL, then read/write IOWIN.
const IOREGSEL: usize = 0x00;
const IOWIN: usize = 0x10;

const REG_VERSION: u32 = 0x01;
const REG_REDTBL: u32 = 0x10; // pin n: low dword at REG_REDTBL + 2n, high at +1

const MASKED: u32 = 1 << 16;

// THE TWO BITS THAT SAY WHAT KIND OF LINE THIS IS, and they are not decoration.
//
// A redirection entry defaults to the ISA convention - edge-triggered, active-high - and that is right for
// exactly one kind of line. A PCI INTx line and the ACPI SCI are both LEVEL-triggered and ACTIVE-LOW, and
// configuring either as an edge is not a cosmetic mismatch: an edge entry latches the transition, so a source
// that asserts and STAYS asserted is delivered once and then never again until it deasserts. On a shared
// line - which INTx is by design, and the SCI is by definition - the second device to assert while the first
// is still asserted produces no edge at all, and its interrupt does not exist.
const ACTIVE_LOW: u32 = 1 << 13;
const LEVEL_TRIGGERED: u32 = 1 << 15;

#[cfg(not(test))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
	IsaEdge,
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

// Each controller: its mapped window (zero for an unused slot), the first GSI it answers for, and how many.
static BASES: [AtomicUsize; MAX_IOAPICS] = [const { AtomicUsize::new(0) }; MAX_IOAPICS];
static GSI_BASES: [AtomicU32; MAX_IOAPICS] = [const { AtomicU32::new(0) }; MAX_IOAPICS];
static PINS: [AtomicU32; MAX_IOAPICS] = [const { AtomicU32::new(0) }; MAX_IOAPICS];

fn read(base: usize, index: u32) -> u32 {
	unsafe {
		((base + IOREGSEL) as *mut u32).write_volatile(index);
		((base + IOWIN) as *const u32).read_volatile()
	}
}

fn write(base: usize, index: u32, value: u32) {
	unsafe {
		((base + IOREGSEL) as *mut u32).write_volatile(index);
		((base + IOWIN) as *mut u32).write_volatile(value);
	}
}

// Map controller `slot` at `phys`, answering from `gsi_base`, and mask every entry it has.
fn adopt(slot: usize, phys: u64, gsi_base: u32) {
	let virt = IOAPIC_VIRT + slot as u64 * 0x1000;
	paging::map_page(virt, phys & !0xfff, paging::WRITABLE | paging::NO_CACHE | paging::NO_EXECUTE);
	let base = (virt + (phys & 0xfff)) as usize;
	let pins = ((read(base, REG_VERSION) >> 16) & 0xff) + 1;
	for pin in 0..pins {
		let lo = REG_REDTBL + 2 * pin;
		write(base, lo, read(base, lo) | MASKED);
	}
	GSI_BASES[slot].store(gsi_base, Ordering::Relaxed);
	PINS[slot].store(pins, Ordering::Relaxed);
	BASES[slot].store(base, Ordering::Release);
}

// Map every controller the MADT names - or the one at the PC base when it names none - and mask them all.
pub fn init() {
	let madt = crate::smp::acpi_table(crate::boot_info().rsdp, b"APIC").and_then(|bytes| acpi::Madt::new(bytes).ok());
	let mut adopted = 0usize;
	if let Some(madt) = madt {
		for io_apic in madt.io_apics() {
			if adopted == MAX_IOAPICS {
				crate::serial_println!("ioapic: the MADT names more than {MAX_IOAPICS} I/O APICs; the rest are not driven");
				break;
			}
			adopt(adopted, u64::from(io_apic.address), io_apic.gsi_base);
			adopted += 1;
		}
	}
	if adopted == 0 {
		adopt(0, IOAPIC_PHYS, 0);
	}
}

// The controller whose range holds `gsi`, and the pin it is on it.
fn locate(gsi: u32) -> Option<(usize, u32)> {
	for slot in 0..MAX_IOAPICS {
		let base = BASES[slot].load(Ordering::Acquire);
		if base == 0 {
			continue;
		}
		let first = GSI_BASES[slot].load(Ordering::Relaxed);
		let pins = PINS[slot].load(Ordering::Relaxed);
		if gsi >= first && gsi - first < pins {
			return Some((base, gsi - first));
		}
	}
	None
}

// Whether any controller answers for `gsi`.
pub fn handles(gsi: u32) -> bool {
	locate(gsi).is_some()
}

// Write `gsi`'s redirection entry: `vector` on the core with LAPIC id `dest_lapic`, the given kind bits,
// unmasked. False when no controller answers for it or the core cannot be addressed.
//
// THE HIGH DWORD FIRST, WHILE THE ENTRY IS STILL MASKED. Writing the low dword unmasks the line, and an entry
// unmasked before its destination is written delivers its first interrupt to whichever core the previous
// contents named. And the destination is the entry's PHYSICAL DESTINATION - eight bits at 63:56, an xAPIC id
// and nothing wider: a core past 255 needs interrupt remapping, and the honest answer until then is to say so
// rather than route to whichever core `id & 0xff` names.
fn program(gsi: u32, vector: u8, dest_lapic: u64, bits: u32) -> bool {
	let Some((base, pin)) = locate(gsi) else {
		crate::serial_println!("ioapic: GSI {gsi} is on no I/O APIC this kernel drives");
		return false;
	};
	let Ok(dest_lapic) = u8::try_from(dest_lapic) else {
		crate::serial_println!("ioapic: GSI {gsi} cannot be routed to controller id {dest_lapic:#x} - the redirection entry addresses 8 bits");
		return false;
	};
	write(base, REG_REDTBL + 2 * pin + 1, (dest_lapic as u32) << 24);
	write(base, REG_REDTBL + 2 * pin, vector as u32 | bits);
	true
}

// Route one of the kernel's own lines. The boot tail routes them, so a test build never asks.
#[cfg(not(test))]
pub fn route(gsi: u32, vector: u8, dest_lapic: u64, kind: Kind) {
	program(gsi, vector, dest_lapic, kind.bits());
}

// ROUTE A CLAIMED LINE with the trigger and polarity its description states.
pub fn route_line(gsi: u32, vector: u8, dest_lapic: u64, level: bool, active_low: bool) -> bool {
	program(gsi, vector, dest_lapic, if level { LEVEL_TRIGGERED } else { 0 } | if active_low { ACTIVE_LOW } else { 0 })
}

// Mask `gsi`'s redirection entry, leaving its routing intact.
pub fn mask(gsi: u32) {
	if let Some((base, pin)) = locate(gsi) {
		let lo = REG_REDTBL + 2 * pin;
		write(base, lo, read(base, lo) | MASKED);
	}
}

// Unmask it again: what a driver's acknowledgement does for a level line masked when it fired.
pub fn unmask(gsi: u32) {
	if let Some((base, pin)) = locate(gsi) {
		let lo = REG_REDTBL + 2 * pin;
		write(base, lo, read(base, lo) & !MASKED);
	}
}

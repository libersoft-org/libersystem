// DECLARED REGISTERS: the few registers a claim holder may read and write ONE AT A TIME, AT EXACTLY THEIR WIDTH,
// through the kernel - because the driver cannot reach them any other way and a mapping would hand over more.
//
// A ring-3 driver cannot reach configuration space: the mechanism is the kernel's (x86's CF8/CFC pair is in the
// reserved set; an ECAM window is the kernel's mapping). A few devices are ARMED through it - the i6300esb
// watchdog answers its configuration registers 0x60 only as a WORD and 0x68 only as a BYTE, and a dword access
// falls through and does nothing - and a few chipset registers live in a page the kernel will never map for a
// claimant, because the same page holds registers nobody else may touch: the ICH9's GCS, whose page holds the
// interrupt routing. So a row DECLARES them - a space, an offset, a width and a write mask - and the claim's
// `RESOURCE_KIND_REGISTERS` capability reaches exactly those, by index: a read or a write at the declared width, a
// write touching only the bits of its mask. The release revokes the capability and WRITES NOTHING.
//
// TWO SOURCES OF DECLARATIONS: the rows below, keyed by a PCI function's (vendor, device) - recorded at the boot
// scan - and a platform row's firmware-described system-memory registers - a WDAT's - recorded at its publication.

use alloc::vec::Vec;

use crate::sync::SpinLock;

// Where a declared register lives, resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
	// The claimed function's configuration space.
	Config { bus: u8, dev: u8, func: u8, offset: u16 },
	// A chipset or firmware register in memory, through the kernel's own uncached mapping.
	Memory { phys: u64, virt: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Register {
	pub space: Space,
	pub width: u8,
	// The bits a write may change; the rest keep what the register holds.
	pub mask: u32,
}

// A REGISTER A ROW DECLARES, before it is resolved against a function.
#[derive(Clone, Copy, Debug)]
pub enum Declaration {
	// The claimed function's configuration register.
	Config { offset: u16, width: u8, mask: u32 },
	// A chipset register in memory at an offset from a base the claimed function's configuration register
	// holds (`base_register`, `base_mask`), present only while its `enable` bits are set there.
	ChipsetMemory { base_register: u16, base_mask: u32, enable: u32, offset: u32, width: u8, mask: u32 },
}

// A ROW OF THE (vendor, device) TABLE: a function the kernel resolves more of than its class says - the BAR it
// hands the claim (checked to share its page with no other function's BAR), the registers it declares, and a table
// whose presence means the row is NOT APPLIED and the function's claim is refused - an ACPI WDAT, which drives the
// same timer the row's registers would.
pub struct Row {
	pub name: &'static str,
	pub vendor: u16,
	pub device: u16,
	pub bar: Option<usize>,
	pub registers: &'static [Declaration],
	pub suppressed_by: Option<[u8; 4]>,
}

// THE i6300ESB: BAR 0 (the stage preloads and the reload register behind an unlock sequence), and its two arming
// registers - 0x60 (reboot enable, clock scale, stage-one interrupt type) as a WORD, 0x68 (enable, lock, free-run)
// as a BYTE.
// THE ICH9 LPC BRIDGE: GCS at the root-complex base (configuration 0xF0, bits 31:14, enabled by bit 0) + 0x3410,
// whose No-Reboot bit (5) alone is writable - the TCO's own registers are the LPC/TCO derivation's port range.
pub const ROWS: &[Row] = &[
	Row { name: "i6300esb", vendor: 0x8086, device: 0x25ab, bar: Some(0), registers: &[Declaration::Config { offset: 0x60, width: 2, mask: 0xFFFF }, Declaration::Config { offset: 0x68, width: 1, mask: 0xFF }], suppressed_by: None },
	Row { name: "ich9-lpc", vendor: 0x8086, device: 0x2918, bar: None, registers: &[Declaration::ChipsetMemory { base_register: 0xF0, base_mask: 0xFFFF_C000, enable: 1, offset: 0x3410, width: 4, mask: 1 << 5 }], suppressed_by: Some(*b"WDAT") },
];

pub fn row_for(vendor: u16, device: u16) -> Option<&'static Row> {
	ROWS.iter().find(|row| row.vendor == vendor && row.device == device)
}

// Whether a row is not applied on this machine: the table that suppresses it is present.
pub fn suppressed(row: &Row) -> bool {
	row.suppressed_by.is_some_and(|signature| crate::smp::acpi_table(crate::boot_info().rsdp, &signature).is_some())
}

// The most registers one row carries.
pub const MAX_REGISTERS: usize = 16;

// Each device row's resolved registers, by its index in the device table.
static SETS: SpinLock<Vec<(usize, Vec<Register>)>> = SpinLock::new(Vec::new());

// RESOLVE A ROW'S DECLARATIONS against the function at `bus:dev.func`: configuration registers as they are, a
// chipset memory register at the base its function's configuration register holds - refused, with a line, when the
// base is not enabled or the window cannot map it. Answers the registers that resolved.
pub fn resolve(row: &Row, bus: u8, dev: u8, func: u8) -> Vec<Register> {
	let mut out = Vec::new();
	for declaration in row.registers {
		let register = match *declaration {
			Declaration::Config { offset, width, mask } => Some(Register { space: Space::Config { bus, dev, func, offset }, width, mask }),
			Declaration::ChipsetMemory { base_register, base_mask, enable, offset, width, mask } => {
				let raw = crate::arch::pci::config_read_exact(bus, dev, func, base_register, 4).unwrap_or(0);
				let base = u64::from(raw & base_mask);
				if raw & enable != enable || base == 0 {
					crate::serial_println!("device: {bus:02x}:{dev:02x}.{func} ({}) declares a register at a base its configuration does not enable - not declared", row.name);
					None
				} else {
					let phys = base + u64::from(offset);
					match crate::arch::pci::map_declared(phys) {
						Some(virt) => Some(Register { space: Space::Memory { phys, virt }, width, mask }),
						None => {
							crate::serial_println!("device: {bus:02x}:{dev:02x}.{func} ({}) declares a register at {phys:#x} this kernel cannot map - not declared", row.name);
							None
						}
					}
				}
			}
		};
		if let Some(register) = register {
			// ALLOC-OK: boot, bounded by the row's declarations.
			out.push(register);
		}
	}
	out
}

// A system-memory register a firmware table names, mapped for the row that describes it: `None` when the window
// cannot map it.
pub fn memory(phys: u64, width: u8) -> Option<Register> {
	if !matches!(width, 1 | 2 | 4) || phys % u64::from(width) != 0 {
		return None;
	}
	let virt = crate::arch::pci::map_declared(phys)?;
	Some(Register { space: Space::Memory { phys, virt }, width, mask: u32::MAX })
}

// Record a device row's registers. At most `MAX_REGISTERS` are kept.
pub fn record(index: usize, mut registers: Vec<Register>) {
	if registers.is_empty() {
		return;
	}
	registers.truncate(MAX_REGISTERS);
	let mut sets = SETS.lock();
	sets.retain(|(held, _)| *held != index);
	// ALLOC-OK: once per described device.
	sets.push((index, registers));
}

// How many registers the row at `index` declares.
pub fn count(index: usize) -> usize {
	SETS.lock().iter().find(|(held, _)| *held == index).map_or(0, |(_, registers)| registers.len())
}

fn register(index: usize, which: usize) -> Option<Register> {
	SETS.lock().iter().find(|(held, _)| *held == index).and_then(|(_, registers)| registers.get(which).copied())
}

fn access(register: Register, value: Option<u32>) -> Option<u32> {
	match register.space {
		Space::Config { bus, dev, func, offset } => match value {
			None => crate::arch::pci::config_read_exact(bus, dev, func, offset, register.width),
			Some(value) => crate::arch::pci::config_write_exact(bus, dev, func, offset, register.width, value).then_some(value),
		},
		// SAFETY: the kernel's own uncached mapping of the register's page, at an address aligned to its width.
		Space::Memory { virt, .. } => unsafe {
			match (register.width, value) {
				(1, None) => Some(core::ptr::read_volatile(virt as *const u8) as u32),
				(2, None) => Some(core::ptr::read_volatile(virt as *const u16) as u32),
				(4, None) => Some(core::ptr::read_volatile(virt as *const u32)),
				(1, Some(value)) => {
					core::ptr::write_volatile(virt as *mut u8, value as u8);
					Some(value)
				}
				(2, Some(value)) => {
					core::ptr::write_volatile(virt as *mut u16, value as u16);
					Some(value)
				}
				(4, Some(value)) => {
					core::ptr::write_volatile(virt as *mut u32, value);
					Some(value)
				}
				_ => None,
			}
		},
	}
}

// READ declared register `which` of the row at `index`, at its width.
pub fn read(index: usize, which: usize) -> Option<u32> {
	access(register(index, which)?, None)
}

// WRITE it: only the bits of its mask change - a register whose mask is not the whole width is read first, at the
// same width, so the bits outside the mask are written back as they were.
pub fn write(index: usize, which: usize, value: u32) -> bool {
	let Some(register) = register(index, which) else { return false };
	let full = match register.width {
		1 => 0xFF,
		2 => 0xFFFF,
		_ => u32::MAX,
	};
	let mask = register.mask & full;
	let merged = if mask == full {
		value & full
	} else {
		let Some(current) = access(register, None) else { return false };
		(current & !mask) | (value & mask)
	};
	access(register, Some(merged)).is_some()
}

// PCI configuration-space access and bus enumeration (legacy CAM via I/O ports
// 0xCF8/0xCFC). The kernel scans the bus once at boot to discover devices - in
// particular the virtio devices QEMU exposes (PCI vendor 0x1AF4) - and records
// each one's identity, BARs, and interrupt line. DeviceManager later queries this
// table and is handed a DeviceMemory capability per device BAR, so it can map each
// virtio device to a userspace driver.
//
// Discovery has to live in the kernel: reading PCI config space needs the I/O-port
// instructions, which ring 3 cannot issue. Only bus 0 is scanned for now (where
// QEMU places the virtio endpoints on q35); recursive bridge enumeration is a
// later refinement.

#![allow(dead_code)]

use alloc::vec::Vec;

use super::port::{inl, outl};

// The PCI configuration mechanism #1 ports.
const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

// virtio's PCI vendor id (Red Hat / virtio).
pub const VIRTIO_VENDOR: u16 = 0x1AF4;

// Modern virtio-pci device ids are 0x1040 + the virtio device type.
const VIRTIO_MODERN_BASE: u16 = 0x1040;

// virtio device types (a subset; the ones QEMU gives us). The numeric values are
// the single source of truth in `abi`, shared with the userspace device services.
pub const VIRTIO_NET: u16 = abi::VIRTIO_TYPE_NET as u16;
pub const VIRTIO_BLK: u16 = abi::VIRTIO_TYPE_BLOCK as u16;
pub const VIRTIO_CONSOLE: u16 = abi::VIRTIO_TYPE_CONSOLE as u16;
pub const VIRTIO_RNG: u16 = abi::VIRTIO_TYPE_RNG as u16;

// Build the CONFIG_ADDRESS value selecting a device's config dword. `offset` is
// rounded down to a 4-byte boundary (the dword the field lives in).
fn address(bus: u8, dev: u8, func: u8, offset: u16) -> u32 {
	0x8000_0000 | (bus as u32) << 16 | (dev as u32) << 11 | (func as u32) << 8 | (offset as u32 & 0xFC)
}

// Read a 32-bit config-space dword.
pub fn config_read32(bus: u8, dev: u8, func: u8, offset: u16) -> u32 {
	unsafe {
		outl(CONFIG_ADDRESS, address(bus, dev, func, offset));
		inl(CONFIG_DATA)
	}
}

// Write a 32-bit config-space dword.
pub fn config_write32(bus: u8, dev: u8, func: u8, offset: u16, value: u32) {
	unsafe {
		outl(CONFIG_ADDRESS, address(bus, dev, func, offset));
		super::port::outl(CONFIG_DATA, value);
	}
}

// Read a 16-bit config-space field.
pub fn config_read16(bus: u8, dev: u8, func: u8, offset: u16) -> u16 {
	let dword = config_read32(bus, dev, func, offset & !3);
	(dword >> ((offset as u32 & 2) * 8)) as u16
}

// Read an 8-bit config-space field.
pub fn config_read8(bus: u8, dev: u8, func: u8, offset: u16) -> u8 {
	let dword = config_read32(bus, dev, func, offset & !3);
	(dword >> ((offset as u32 & 3) * 8)) as u8
}

// One discovered PCI function.
#[derive(Clone, Copy)]
pub struct PciDevice {
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	pub vendor: u16,
	pub device_id: u16,
	pub class: u8,
	pub subclass: u8,
	pub prog_if: u8,
	pub header_type: u8,
	// The six 32-bit base address registers (raw; only meaningful for header type 0).
	pub bars: [u32; 6],
	// The interrupt line (legacy INTx routing), 0xFF = none.
	pub irq_line: u8,
	// The interrupt pin this function drives: 1 = INTA .. 4 = INTD, 0 = none.
	pub irq_pin: u8,
}

impl PciDevice {
	// Whether this is a virtio device.
	pub fn is_virtio(&self) -> bool {
		self.vendor == VIRTIO_VENDOR
	}

	// The modern virtio device type (`device_id - 0x1040`), or None if this is not
	// a modern virtio-pci device.
	pub fn virtio_type(&self) -> Option<u16> {
		if self.is_virtio() && (VIRTIO_MODERN_BASE..VIRTIO_MODERN_BASE + 0x40).contains(&self.device_id) { Some(self.device_id - VIRTIO_MODERN_BASE) } else { None }
	}

	// The IOAPIC GSI this function's INTx pin is wired to, or None if it drives no
	// pin. The PCI Interrupt Line register (0x3C) holds the legacy 8259 IRQ, not a
	// GSI; under the IOAPIC the q35 (ICH9) chipset routes each slot's INTx pin to a
	// PIRQ link (PIRQ pin = (slot + pin_index) % 8) and the firmware maps PIRQ[A..H]
	// to GSI 16..23. So GSI = 16 + (slot + (pin - 1)) % 8.
	pub fn intx_gsi(&self) -> Option<u32> {
		if self.irq_pin == 0 {
			return None;
		}
		Some(16 + (self.dev as u32 + (self.irq_pin as u32 - 1)) % 8)
	}
}

// Read the full identity of one present function.
fn read_function(bus: u8, dev: u8, func: u8) -> PciDevice {
	let id = config_read32(bus, dev, func, 0x00);
	let class_reg = config_read32(bus, dev, func, 0x08);
	let mut bars = [0u32; 6];
	for (i, bar) in bars.iter_mut().enumerate() {
		*bar = config_read32(bus, dev, func, 0x10 + (i as u16) * 4);
	}
	PciDevice { bus, dev, func, vendor: id as u16, device_id: (id >> 16) as u16, prog_if: (class_reg >> 8) as u8, subclass: (class_reg >> 16) as u8, class: (class_reg >> 24) as u8, header_type: config_read8(bus, dev, func, 0x0E), bars, irq_line: config_read8(bus, dev, func, 0x3C), irq_pin: config_read8(bus, dev, func, 0x3D) }
}

// Enumerate every present function on bus 0. Multi-function devices (header-type
// bit 7) have all eight functions probed; absent slots (vendor 0xFFFF) are skipped.
pub fn scan() -> Vec<PciDevice> {
	let mut out: Vec<PciDevice> = Vec::new();
	let bus: u8 = 0;
	for dev in 0..32u8 {
		if config_read16(bus, dev, 0, 0x00) == 0xFFFF {
			continue;
		}
		let multifunction = config_read8(bus, dev, 0, 0x0E) & 0x80 != 0;
		let func_count: u8 = if multifunction { 8 } else { 1 };
		for func in 0..func_count {
			if config_read16(bus, dev, func, 0x00) == 0xFFFF {
				continue;
			}
			out.push(read_function(bus, dev, func));
		}
	}
	out
}

// The human name of a virtio device type, for the boot log.
pub fn virtio_type_name(virtio_type: u16) -> &'static str {
	match virtio_type {
		VIRTIO_NET => "net",
		VIRTIO_BLK => "blk",
		VIRTIO_CONSOLE => "console",
		VIRTIO_RNG => "rng",
		_ => "other",
	}
}

// PCI status register bit 4: a capability list is present (pointer at offset 0x34).
const STATUS_CAP_LIST: u16 = 1 << 4;
// Vendor-specific capability id; virtio describes its MMIO structures with these.
const CAP_ID_VENDOR: u8 = 0x09;

// virtio capability cfg_type values (which structure the capability points at).
const VIRTIO_CAP_COMMON: u8 = 1;
const VIRTIO_CAP_NOTIFY: u8 = 2;
const VIRTIO_CAP_ISR: u8 = 3;
const VIRTIO_CAP_DEVICE: u8 = 4;

// Decode a memory BAR's physical base, handling 64-bit BARs (which occupy two
// adjacent slots). Returns None for an I/O BAR or an out-of-range index.
pub fn bar_address(d: &PciDevice, bar_idx: usize) -> Option<u64> {
	let bar = *d.bars.get(bar_idx)?;
	if bar & 1 != 0 {
		return None; // an I/O-space BAR, not memory
	}
	let base_lo = (bar & 0xFFFF_FFF0) as u64;
	if (bar >> 1) & 3 == 2 {
		// 64-bit memory BAR: the high half lives in the next slot.
		let hi = *d.bars.get(bar_idx + 1)? as u64;
		Some(hi << 32 | base_lo)
	} else {
		Some(base_lo)
	}
}

// One located virtio configuration structure (resolved from a virtio PCI cap):
// which BAR it lives in, its byte offset within that BAR, and its length.
#[derive(Clone, Copy, Default)]
pub struct VirtioCap {
	pub bar: u8,
	pub offset: u32,
	pub length: u32,
	// For the notify capability only: the queue_notify_off multiplier.
	pub notify_multiplier: u32,
}

// A modern virtio-pci device with its MMIO layout resolved from its capabilities.
// `bar_phys`/`region_len` describe the physical MMIO window a driver maps (the BAR
// the common-config structure lives in); the per-structure offsets index into it.
#[derive(Clone, Copy)]
pub struct VirtioDevice {
	pub pci: PciDevice,
	pub virtio_type: u16,
	pub bar: u8,
	pub bar_phys: u64,
	pub region_len: u64,
	pub common: VirtioCap,
	pub notify: VirtioCap,
	pub isr: VirtioCap,
	pub device: VirtioCap,
}

// Walk a device's PCI capability list and resolve its virtio configuration
// structures. Returns None if it is not a modern virtio device, has no capability
// list, or is missing the required common/notify/ISR structures.
fn resolve_virtio(d: &PciDevice) -> Option<VirtioDevice> {
	let virtio_type = d.virtio_type()?;
	if config_read16(d.bus, d.dev, d.func, 0x06) & STATUS_CAP_LIST == 0 {
		return None;
	}
	let (mut common, mut notify, mut isr, mut device) = (None, None, None, None);
	let mut ptr: u16 = (config_read8(d.bus, d.dev, d.func, 0x34) & 0xFC) as u16;
	// Bound the walk so a malformed (cyclic) list cannot spin forever.
	for _ in 0..48 {
		if ptr == 0 {
			break;
		}
		let cap_id = config_read8(d.bus, d.dev, d.func, ptr);
		let next = (config_read8(d.bus, d.dev, d.func, ptr + 1) & 0xFC) as u16;
		if cap_id == CAP_ID_VENDOR {
			let cfg_type = config_read8(d.bus, d.dev, d.func, ptr + 3);
			let mut cap = VirtioCap { bar: config_read8(d.bus, d.dev, d.func, ptr + 4), offset: config_read32(d.bus, d.dev, d.func, ptr + 8), length: config_read32(d.bus, d.dev, d.func, ptr + 12), notify_multiplier: 0 };
			match cfg_type {
				VIRTIO_CAP_COMMON => common = Some(cap),
				VIRTIO_CAP_NOTIFY => {
					cap.notify_multiplier = config_read32(d.bus, d.dev, d.func, ptr + 16);
					notify = Some(cap);
				}
				VIRTIO_CAP_ISR => isr = Some(cap),
				VIRTIO_CAP_DEVICE => device = Some(cap),
				_ => {}
			}
		}
		ptr = next;
	}
	let common = common?;
	let notify = notify?;
	let isr = isr?;
	let device = device.unwrap_or_default();
	let bar = common.bar;
	let bar_phys = bar_address(d, bar as usize)?;
	// The window the driver maps is the BAR holding the common-config structure;
	// its length is the furthest end of any virtio structure in that same BAR,
	// rounded up to a page.
	let mut end: u64 = 0;
	for cap in [common, notify, isr, device] {
		if cap.bar == bar {
			end = end.max(cap.offset as u64 + cap.length as u64);
		}
	}
	let region_len = end.div_ceil(0x1000) * 0x1000;
	Some(VirtioDevice { pci: *d, virtio_type, bar, bar_phys, region_len, common, notify, isr, device })
}

// Scan the bus and resolve every modern virtio device's MMIO layout.
pub fn scan_virtio() -> Vec<VirtioDevice> {
	scan().iter().filter_map(resolve_virtio).collect()
}

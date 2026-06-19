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

// virtio device types (a subset; the ones QEMU gives us).
pub const VIRTIO_NET: u16 = 1;
pub const VIRTIO_BLK: u16 = 2;
pub const VIRTIO_CONSOLE: u16 = 3;
pub const VIRTIO_RNG: u16 = 4;

// Build the CONFIG_ADDRESS value selecting a device's config dword. `offset` is
// rounded down to a 4-byte boundary (the dword the field lives in).
fn address(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
	0x8000_0000 | (bus as u32) << 16 | (dev as u32) << 11 | (func as u32) << 8 | (offset as u32 & 0xFC)
}

// Read a 32-bit config-space dword.
pub fn config_read32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
	unsafe {
		outl(CONFIG_ADDRESS, address(bus, dev, func, offset));
		inl(CONFIG_DATA)
	}
}

// Write a 32-bit config-space dword.
pub fn config_write32(bus: u8, dev: u8, func: u8, offset: u8, value: u32) {
	unsafe {
		outl(CONFIG_ADDRESS, address(bus, dev, func, offset));
		super::port::outl(CONFIG_DATA, value);
	}
}

// Read a 16-bit config-space field.
pub fn config_read16(bus: u8, dev: u8, func: u8, offset: u8) -> u16 {
	let dword = config_read32(bus, dev, func, offset & !3);
	(dword >> ((offset as u32 & 2) * 8)) as u16
}

// Read an 8-bit config-space field.
pub fn config_read8(bus: u8, dev: u8, func: u8, offset: u8) -> u8 {
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
}

// Read the full identity of one present function.
fn read_function(bus: u8, dev: u8, func: u8) -> PciDevice {
	let id = config_read32(bus, dev, func, 0x00);
	let class_reg = config_read32(bus, dev, func, 0x08);
	let mut bars = [0u32; 6];
	for (i, bar) in bars.iter_mut().enumerate() {
		*bar = config_read32(bus, dev, func, 0x10 + (i as u8) * 4);
	}
	PciDevice { bus, dev, func, vendor: id as u16, device_id: (id >> 16) as u16, prog_if: (class_reg >> 8) as u8, subclass: (class_reg >> 16) as u8, class: (class_reg >> 24) as u8, header_type: config_read8(bus, dev, func, 0x0E), bars, irq_line: config_read8(bus, dev, func, 0x3C) }
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

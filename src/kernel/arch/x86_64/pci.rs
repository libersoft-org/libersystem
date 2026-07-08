// x86_64 PCI config-space access: the legacy configuration mechanism #1 (I/O ports
// 0xCF8/0xCFC). This is the ONLY architecture-specific part of PCI enumeration - the
// device tables, capability walk, BAR decoding and MSI-X resolution all live in
// `arch::common::pci`, generic over the `ConfigAccess` primitives implemented here.
// QEMU's q35 places the virtio endpoints and any xHCI controller on bus 0, and its
// firmware assigns the BARs, so this backend probes bus 0 only and needs no BAR
// allocator (`assign_bars` stays the common no-op).

#![allow(dead_code)]

use alloc::vec::Vec;

use super::port::{inl, outl};
use crate::arch::common::pci as common;

// The PCI surface every backend re-exports (the HAL contract); not every type is
// named directly in this backend's code.
#[allow(unused_imports)]
pub use common::{PciDevice, VirtioCap, VirtioDevice, XhciDevice, virtio_type_name};

// The PCI configuration mechanism #1 ports.
const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

// Build the CONFIG_ADDRESS value selecting a device's config dword. `offset` is
// rounded down to a 4-byte boundary (the dword the field lives in).
fn address(bus: u8, dev: u8, func: u8, offset: u16) -> u32 {
	0x8000_0000 | (bus as u32) << 16 | (dev as u32) << 11 | (func as u32) << 8 | (offset as u32 & 0xFC)
}

// The config-space access mechanism: dword reads/writes through the CF8/CFC ports.
// The byte/word reads and every enumeration routine come from `common` unchanged.
struct Access;

impl common::ConfigAccess for Access {
	// QEMU q35 exposes the virtio / xHCI endpoints on bus 0; recursive bridge
	// enumeration is a later refinement.
	const BUS_COUNT: u16 = 1;

	fn read32(bus: u8, dev: u8, func: u8, off: u16) -> u32 {
		unsafe {
			outl(CONFIG_ADDRESS, address(bus, dev, func, off));
			inl(CONFIG_DATA)
		}
	}

	fn write32(bus: u8, dev: u8, func: u8, off: u16, val: u32) {
		unsafe {
			outl(CONFIG_ADDRESS, address(bus, dev, func, off));
			outl(CONFIG_DATA, val);
		}
	}
}

// Enumerate every present function on bus 0.
pub fn scan() -> Vec<PciDevice> {
	common::scan::<Access>()
}

// Scan the bus and resolve every modern virtio device's MMIO layout.
pub fn scan_virtio() -> Vec<VirtioDevice> {
	common::scan_virtio::<Access>()
}

// Scan the bus and resolve every xHCI USB host controller's MMIO window.
pub fn scan_xhci() -> Vec<XhciDevice> {
	common::scan_xhci::<Access>()
}

// Decode a memory BAR's physical base (live), handling 64-bit BARs.
pub fn bar_address(d: &PciDevice, bar_idx: usize) -> Option<u64> {
	common::bar_address::<Access>(d, bar_idx)
}

// Measure a memory BAR's window size with the standard all-ones probe.
pub fn bar_size(d: &PciDevice, bar_idx: usize) -> Option<u64> {
	common::bar_size::<Access>(d, bar_idx)
}

// Set or clear a function's PCI command-register Interrupt Disable bit (bit 10).
pub fn set_intx_disabled(bus: u8, dev: u8, func: u8, disabled: bool) {
	common::set_intx_disabled::<Access>(bus, dev, func, disabled);
}

// Enable MSI-X on a device and ensure its memory space is decoded. `cap` is the
// MSI-X capability's config-space offset (from VirtioDevice::msix_cap).
pub fn msix_enable(bus: u8, dev: u8, func: u8, cap: u16) {
	common::msix_enable::<Access>(bus, dev, func, cap);
}

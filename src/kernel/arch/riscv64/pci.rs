// riscv64 PCI / PCIe config-space access: the ECAM MMIO window (M117). Like the other
// arches, the ONLY architecture-specific part is the config-space access mechanism -
// the device tables, capability walk, BAR decoding and virtio layout resolution all
// live in `arch::common::pci`, generic over the `ConfigAccess` primitives here.
//
// On QEMU's `virt` the PCIe ECAM base comes from the device tree ("pci@30000000", under
// /soc). The ECAM window and the 32-bit MMIO BAR window both sit below 8 GiB, so the
// boot high direct map already reaches them through `phys_to_virt`. There is no firmware
// to assign BARs, so this backend provides an MMIO-window allocator (from the pcie
// node's `ranges`: 0x4000_0000..0x8000_0000) and drives the common ECAM reassignment.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::arch::common::pci as common;

// The PCI surface every backend re-exports (the HAL contract); not every type is named
// directly in this backend's code.
#[allow(unused_imports)]
pub use common::{PciDevice, VirtioCap, VirtioDevice, XhciDevice, virtio_type_name};

// PCIe ECAM base (set from the device tree at boot) and the number of buses to probe.
static ECAM_BASE: AtomicUsize = AtomicUsize::new(0);
const ECAM_BUSES: u16 = 16;

// The PCIe 32-bit MMIO window on QEMU virt (from the pcie node's `ranges`): BARs are
// assigned out of it by a simple size-aligned bump. It sits below 8 GiB, so the boot
// high direct map already covers it.
const MMIO_WINDOW_BASE: u64 = 0x4000_0000;
const MMIO_WINDOW_END: u64 = 0x8000_0000;
static MMIO_NEXT: AtomicU64 = AtomicU64::new(MMIO_WINDOW_BASE);

// Record the ECAM base discovered in the device tree.
pub fn set_ecam_base(base: u64) {
	ECAM_BASE.store(base as usize, Ordering::Relaxed);
}

// Physical address of a config-space register for a given B/D/F.
fn cfg_phys(bus: u8, dev: u8, func: u8, off: u16) -> u64 {
	(ECAM_BASE.load(Ordering::Relaxed) + ((bus as usize) << 20) + ((dev as usize) << 15) + ((func as usize) << 12) + off as usize) as u64
}

// The config-space access mechanism: dword reads/writes through the ECAM MMIO window
// (reached via the physical direct map, since the kernel runs higher-half). The
// byte/word reads and every enumeration routine come from `common` unchanged.
struct Access;

impl common::ConfigAccess for Access {
	const BUS_COUNT: u16 = ECAM_BUSES;
	const MMIO_WINDOW_END: u64 = MMIO_WINDOW_END;

	fn read32(bus: u8, dev: u8, func: u8, off: u16) -> u32 {
		unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(cfg_phys(bus, dev, func, off)) as *const u32) }
	}

	fn write32(bus: u8, dev: u8, func: u8, off: u16, val: u32) {
		unsafe { core::ptr::write_volatile(super::paging::phys_to_virt(cfg_phys(bus, dev, func, off)) as *mut u32, val) }
	}

	// The bus is reachable only once the ECAM base is known (from the device tree).
	fn ready() -> bool {
		ECAM_BASE.load(Ordering::Relaxed) != 0
	}

	// No firmware programs the BARs on QEMU `virt`, so reassign them out of the 32-bit
	// MMIO window before a device's layout is resolved.
	fn assign_bars(d: &PciDevice) {
		common::assign_bars_ecam::<Self>(d);
	}

	// Allocate a size-aligned span from the PCIe MMIO window, or None if exhausted.
	fn alloc_mmio(size: u64) -> Option<u64> {
		let size = size.max(0x1000);
		loop {
			let cur = MMIO_NEXT.load(Ordering::Relaxed);
			let base = (cur + size - 1) & !(size - 1);
			if base + size > MMIO_WINDOW_END {
				return None;
			}
			if MMIO_NEXT.compare_exchange(cur, base + size, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
				return Some(base);
			}
		}
	}
}

// Enumerate every present function on the ECAM buses.
pub fn scan() -> Vec<PciDevice> {
	common::scan::<Access>()
}

// Scan the bus and resolve every virtio device's modern MMIO layout.
pub fn scan_virtio() -> Vec<VirtioDevice> {
	common::scan_virtio::<Access>()
}

// Scan the bus and resolve every xHCI USB host controller's MMIO window.
pub fn scan_xhci() -> Vec<XhciDevice> {
	common::scan_xhci::<Access>()
}

// Decode a memory BAR's assigned physical base (live), handling 64-bit BARs.
pub fn bar_address(d: &PciDevice, bar_idx: usize) -> Option<u64> {
	common::bar_address::<Access>(d, bar_idx)
}

// Probe a memory BAR's window size with the standard all-ones probe.
pub fn bar_size(d: &PciDevice, bar_idx: usize) -> Option<u64> {
	common::bar_size::<Access>(d, bar_idx)
}

// Set or clear a function's PCI command-register Interrupt Disable bit (bit 10).
pub fn set_intx_disabled(bus: u8, dev: u8, func: u8, disabled: bool) {
	common::set_intx_disabled::<Access>(bus, dev, func, disabled);
}

// Enable MSI-X on a device and ensure its memory space is decoded + bus mastering is on.
// `cap` is the MSI-X capability's config-space offset (from VirtioDevice::msix_cap).
pub fn msix_enable(bus: u8, dev: u8, func: u8, cap: u16) {
	common::msix_enable::<Access>(bus, dev, func, cap);
}

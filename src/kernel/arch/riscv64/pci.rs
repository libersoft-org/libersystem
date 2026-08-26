// riscv64 PCI / PCIe config-space access: the ECAM MMIO window. Like the other
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
pub use common::{PciDevice, VirtioDevice, XhciDevice};

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
	// The window is `ECAM_BUSES` wide, so that is also as far as a bridge can be followed: past it
	// the address is not config space at all, it is whatever the direct map holds next.
	const MAX_BUS: u8 = (ECAM_BUSES - 1) as u8;
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

	// Take a span the firmware already placed out of the window (KERN-ARCH-015).
	//
	// The window is handed out by a BUMP, so reserving means pushing the cursor past the span's
	// end - which also gives away whatever gap lies below it. That is the price of a bump
	// allocator and it is the safe direction: the alternative is handing the same addresses to a
	// second device. Spans outside the window are not its business.
	fn reserve_mmio(base: u64, size: u64) {
		let end = base.saturating_add(size);
		if end <= MMIO_WINDOW_BASE || base >= MMIO_WINDOW_END {
			return;
		}
		let mut cur = MMIO_NEXT.load(Ordering::Relaxed);
		while cur < end {
			match MMIO_NEXT.compare_exchange(cur, end, Ordering::AcqRel, Ordering::Relaxed) {
				Ok(_) => return,
				Err(now) => cur = now,
			}
		}
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

// Set or clear a function's PCI command-register Interrupt Disable bit (bit 10).
pub fn set_intx_disabled(bus: u8, dev: u8, func: u8, disabled: bool) {
	common::set_intx_disabled::<Access>(bus, dev, func, disabled);
}

// The device's PCI Interrupt Pin (config byte 0x3D): 0 = none, 1..4 = INTA..INTD.

// Turn bus mastering on or off for one function. The only caller is `device`, which knows whether a
// driver owns the device - see `arch::common::pci::set_bus_master`.
pub fn set_bus_master(bus: u8, dev: u8, func: u8, on: bool) {
	common::set_bus_master::<Access>(bus, dev, func, on);
}

// WHERE THIS PORT'S INTERRUPTS ARE WRITTEN, for a translated endpoint that named no doorbell of its
// own.
//
// A device's MSI is a memory write, so behind a translating IOMMU it needs a mapping like any other
// write. An endpoint that reports an MSI reserved region says where; one that offers no PROBE at
// all, or lists no such region, says nothing - and used to end up with no doorbell mapping and no
// interrupts, silently. This is the address that endpoint would have named.
pub fn msi_doorbell() -> Option<(u64, u64)> {
	// An IMSIC S-mode interrupt file, one 4 KiB page per hart at `base + hart * stride`. The first
	// hart's file is where this port's MSI addresses start; the span covers every hart's.
	crate::arch::imsic::msi_window()
}

// One function's memory BAR, resolved live from configuration space: its assigned base and its
// probed size.
//
// FOR A FUNCTION THIS KERNEL BINDS NO DRIVER TO. The device table admits resolved virtio and xHCI
// functions only, and the IOMMU fixture needs the PCI `edu` device - which is neither. Retaining a
// window for an arbitrary function is what that fixture needs and what this provides; it does not
// map anything or grant anything, it reads two registers.
#[cfg(test)]
pub fn function_bar(bus: u8, dev: u8, func: u8, index: usize) -> Option<(u64, u64)> {
	let device = common::probe_function::<Access>(bus, dev, func)?;
	let base = common::bar_address::<Access>(&device, index)?;
	let size = common::bar_size::<Access>(&device, index)?;
	if base == 0 || size == 0 { None } else { Some((base, size)) }
}

// One function's COMMAND register, read back - test-only, see `arch::common::pci::command`.
#[cfg(test)]
pub fn command(bus: u8, dev: u8, func: u8) -> u16 {
	common::command::<Access>(bus, dev, func)
}

// With QEMU's `virt,aia=aplic-imsic` the PCIe host bridge delivers MSI-X to the AIA
// IMSIC, so - like x86 and aarch64 - each device gets its own edge-triggered vector
// (its IMSIC EID) with no INTx line sharing. Ensure memory decode, then set the device's
// MSI-X enable bit + clear its function mask. Bus mastering follows ownership and is not set here.
pub fn msix_enable(bus: u8, dev: u8, func: u8, cap: u16) {
	common::enable_memory_space::<Access>(bus, dev, func);
	common::msix_enable::<Access>(bus, dev, func, cap);
}

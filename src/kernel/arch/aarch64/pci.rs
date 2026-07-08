// aarch64 PCI / PCIe (M116).
//
// PCI config space is a bus standard; only the ACCESS mechanism is arch-specific
// (x86 I/O ports vs aarch64 ECAM MMIO). On QEMU's `virt` the PCIe ECAM is the
// high-mem window (0x40_1000_0000, read from the device tree), which the boot map
// covers as Device memory (see paging::identity_map_device_gb). `scan` enumerates
// the bus; `scan_virtio` walks each virtio device's capability list to resolve its
// modern MMIO layout - and, because there is no firmware to do it, ASSIGNS the
// device's BARs out of the PCIe 32-bit MMIO window first.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

// PCIe ECAM base (set from the device tree at boot) and the number of buses to
// probe.
static ECAM_BASE: AtomicUsize = AtomicUsize::new(0);
const ECAM_BUSES: u8 = 16;

// The PCIe 32-bit MMIO window on QEMU virt (from the pcie node's `ranges`): BARs
// are assigned out of it by a simple size-aligned bump. It sits below 1 GB, so
// the boot identity map already covers it as Device memory.
const MMIO_WINDOW_BASE: u64 = 0x1000_0000;
const MMIO_WINDOW_END: u64 = 0x3eff_0000;
static MMIO_NEXT: AtomicU64 = AtomicU64::new(MMIO_WINDOW_BASE);

const VIRTIO_VENDOR: u16 = 0x1af4;
const VIRTIO_MODERN_BASE: u16 = 0x1040;

// The xHCI USB host controller PCI class triple (serial-bus / USB / xHCI programming
// interface), by which the controller is recognised regardless of vendor.
const CLASS_SERIAL_BUS: u8 = 0x0c;
const SUBCLASS_USB: u8 = 0x03;
const PROG_IF_XHCI: u8 = 0x30;

// PCI status register bit 4: a capability list is present (pointer at 0x34).
const STATUS_CAP_LIST: u16 = 1 << 4;
// Vendor-specific capability id; virtio describes its MMIO structures with these.
const CAP_ID_VENDOR: u8 = 0x09;
const MSIX_CAP_ID: u8 = 0x11;

// virtio capability cfg_type values (which structure the capability points at).
const VIRTIO_CAP_COMMON: u8 = 1;
const VIRTIO_CAP_NOTIFY: u8 = 2;
const VIRTIO_CAP_ISR: u8 = 3;
const VIRTIO_CAP_DEVICE: u8 = 4;

// Record the ECAM base discovered in the device tree.
pub fn set_ecam_base(base: u64) {
	ECAM_BASE.store(base as usize, Ordering::Relaxed);
}

// Virtual address of a config-space register for a given B/D/F (the ECAM MMIO is
// reached through the physical direct map, since the kernel runs higher-half).
fn cfg_addr(bus: u8, dev: u8, func: u8, off: usize) -> usize {
	let phys = ECAM_BASE.load(Ordering::Relaxed) + ((bus as usize) << 20) + ((dev as usize) << 15) + ((func as usize) << 12) + off;
	super::paging::phys_to_virt(phys as u64) as usize
}
fn cfg_read32(bus: u8, dev: u8, func: u8, off: usize) -> u32 {
	unsafe { core::ptr::read_volatile(cfg_addr(bus, dev, func, off) as *const u32) }
}
fn cfg_read16(bus: u8, dev: u8, func: u8, off: usize) -> u16 {
	unsafe { core::ptr::read_volatile(cfg_addr(bus, dev, func, off) as *const u16) }
}
fn cfg_read8(bus: u8, dev: u8, func: u8, off: usize) -> u8 {
	unsafe { core::ptr::read_volatile(cfg_addr(bus, dev, func, off) as *const u8) }
}
fn cfg_write32(bus: u8, dev: u8, func: u8, off: usize, val: u32) {
	unsafe { core::ptr::write_volatile(cfg_addr(bus, dev, func, off) as *mut u32, val) }
}
fn cfg_write16(bus: u8, dev: u8, func: u8, off: usize, val: u16) {
	unsafe { core::ptr::write_volatile(cfg_addr(bus, dev, func, off) as *mut u16, val) }
}

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
	pub bars: [u32; 6],
}

impl PciDevice {
	pub fn is_virtio(&self) -> bool {
		self.vendor == VIRTIO_VENDOR
	}

	// The virtio device type. Modern IDs encode it as device_id - 0x1040; the
	// transitional IDs QEMU virt exposes (0x1000 net, 0x1001 block, ...) are mapped
	// to the same type numbers so the modern capability path still applies.
	pub fn virtio_type(&self) -> Option<u16> {
		if !self.is_virtio() {
			return None;
		}
		if (VIRTIO_MODERN_BASE..VIRTIO_MODERN_BASE + 0x40).contains(&self.device_id) {
			return Some(self.device_id - VIRTIO_MODERN_BASE);
		}
		match self.device_id {
			0x1000 => Some(abi::VIRTIO_TYPE_NET as u16),
			0x1001 => Some(abi::VIRTIO_TYPE_BLOCK as u16),
			0x1003 => Some(abi::VIRTIO_TYPE_CONSOLE as u16),
			0x1005 => Some(abi::VIRTIO_TYPE_RNG as u16),
			_ => None,
		}
	}

	// Whether this function is an xHCI USB host controller (by its PCI class triple).
	pub fn is_xhci(&self) -> bool {
		self.class == CLASS_SERIAL_BUS && self.subclass == SUBCLASS_USB && self.prog_if == PROG_IF_XHCI
	}
}

#[derive(Clone, Copy, Default)]
pub struct VirtioCap {
	pub bar: u8,
	pub offset: u32,
	pub length: u32,
	pub notify_multiplier: u32,
}

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
	pub msix_cap: u16,
	pub msix_count: u16,
	pub msix_table_phys: u64,
}

#[derive(Clone, Copy)]
pub struct XhciDevice {
	pub pci: PciDevice,
	pub bar_phys: u64,
	pub bar_len: u64,
	pub msix_cap: u16,
	pub msix_count: u16,
	pub msix_table_phys: u64,
}

// Read the fixed header fields of one present function.
fn read_function(bus: u8, dev: u8, func: u8, vendor: u16) -> PciDevice {
	let mut bars = [0u32; 6];
	for (i, bar) in bars.iter_mut().enumerate() {
		*bar = cfg_read32(bus, dev, func, 0x10 + i * 4);
	}
	PciDevice { bus, dev, func, vendor, device_id: cfg_read16(bus, dev, func, 0x02), class: cfg_read8(bus, dev, func, 0x0b), subclass: cfg_read8(bus, dev, func, 0x0a), prog_if: cfg_read8(bus, dev, func, 0x09), header_type: cfg_read8(bus, dev, func, 0x0e), bars }
}

// Enumerate every present function on the ECAM bus.
pub fn scan() -> Vec<PciDevice> {
	let mut out = Vec::new();
	if ECAM_BASE.load(Ordering::Relaxed) == 0 {
		return out;
	}
	for bus in 0..ECAM_BUSES {
		for dev in 0..32u8 {
			if cfg_read16(bus, dev, 0, 0x00) == 0xffff {
				continue;
			}
			// Multi-function devices (header type bit 7) expose funcs 1..8.
			let funcs = if cfg_read8(bus, dev, 0, 0x0e) & 0x80 != 0 { 8 } else { 1 };
			for func in 0..funcs {
				let vendor = cfg_read16(bus, dev, func, 0x00);
				if vendor == 0xffff {
					continue;
				}
				out.push(read_function(bus, dev, func, vendor));
			}
		}
	}
	out
}

// The human name of a virtio device type, for the boot log.
pub fn virtio_type_name(virtio_type: u16) -> &'static str {
	match virtio_type as u32 {
		abi::VIRTIO_TYPE_NET => "net",
		abi::VIRTIO_TYPE_BLOCK => "blk",
		abi::VIRTIO_TYPE_CONSOLE => "console",
		abi::VIRTIO_TYPE_RNG => "rng",
		_ => "other",
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

// The raw value of one BAR slot, read live from config space.
fn bar_raw(d: &PciDevice, i: usize) -> u32 {
	cfg_read32(d.bus, d.dev, d.func, 0x10 + i * 4)
}

// Decode a memory BAR's assigned physical base (live), handling 64-bit BARs.
pub fn bar_address(d: &PciDevice, i: usize) -> Option<u64> {
	if i >= 6 {
		return None;
	}
	let bar = bar_raw(d, i);
	if bar & 1 != 0 {
		return None; // I/O-space BAR
	}
	let lo = (bar & 0xFFFF_FFF0) as u64;
	if (bar >> 1) & 3 == 2 { Some((bar_raw(d, i + 1) as u64) << 32 | lo) } else { Some(lo) }
}

// Assign this device's unprogrammed memory BARs out of the MMIO window, then
// enable memory-space decoding and bus-master in the command register (there is
// no firmware to do this on QEMU virt with `-kernel`). Idempotent: an already
// assigned BAR (nonzero base) is left as is.
fn assign_bars(d: &PciDevice) {
	let mut i = 0usize;
	while i < 6 {
		let off = 0x10 + i * 4;
		let bar = cfg_read32(d.bus, d.dev, d.func, off);
		if bar & 1 != 0 {
			i += 1; // I/O BAR - not used here
			continue;
		}
		let is64 = (bar >> 1) & 3 == 2;
		// Probe the size (write all-ones, read back the mask, restore).
		cfg_write32(d.bus, d.dev, d.func, off, 0xFFFF_FFFF);
		let mask_lo = cfg_read32(d.bus, d.dev, d.func, off);
		cfg_write32(d.bus, d.dev, d.func, off, bar);
		let (mask, cur, hi_bar) = if is64 {
			let bar_hi = cfg_read32(d.bus, d.dev, d.func, off + 4);
			cfg_write32(d.bus, d.dev, d.func, off + 4, 0xFFFF_FFFF);
			let mask_hi = cfg_read32(d.bus, d.dev, d.func, off + 4);
			cfg_write32(d.bus, d.dev, d.func, off + 4, bar_hi);
			(((mask_hi as u64) << 32) | (mask_lo & 0xFFFF_FFF0) as u64, ((bar_hi as u64) << 32) | (bar & 0xFFFF_FFF0) as u64, bar_hi)
		} else {
			((mask_lo & 0xFFFF_FFF0) as u64 | 0xFFFF_FFFF_0000_0000, (bar & 0xFFFF_FFF0) as u64, 0)
		};
		let size = (!mask).wrapping_add(1);
		let step = if is64 { 2 } else { 1 };
		if size == 0 || mask == !0u64 {
			i += step;
			continue;
		}
		if cur == 0 {
			if let Some(base) = alloc_mmio(size) {
				cfg_write32(d.bus, d.dev, d.func, off, (base as u32 & 0xFFFF_FFF0) | (bar & 0xF));
				if is64 {
					cfg_write32(d.bus, d.dev, d.func, off + 4, (base >> 32) as u32);
				}
			}
		}
		let _ = hi_bar;
		i += step;
	}
	// Command register (0x04): enable memory space (bit 1) + bus master (bit 2).
	let cmd = cfg_read16(d.bus, d.dev, d.func, 0x04);
	cfg_write16(d.bus, d.dev, d.func, 0x04, cmd | 0x6);
}

// Walk a device's capability list for its MSI-X capability: (config offset, table
// entry count, table physical address). (0, 0, 0) if absent.
fn resolve_msix(d: &PciDevice) -> (u16, u16, u64) {
	if cfg_read16(d.bus, d.dev, d.func, 0x06) & STATUS_CAP_LIST == 0 {
		return (0, 0, 0);
	}
	let mut ptr = (cfg_read8(d.bus, d.dev, d.func, 0x34) & 0xFC) as usize;
	for _ in 0..48 {
		if ptr == 0 {
			break;
		}
		let cap_id = cfg_read8(d.bus, d.dev, d.func, ptr);
		let next = (cfg_read8(d.bus, d.dev, d.func, ptr + 1) & 0xFC) as usize;
		if cap_id == MSIX_CAP_ID {
			let mc = cfg_read16(d.bus, d.dev, d.func, ptr + 2);
			let table = cfg_read32(d.bus, d.dev, d.func, ptr + 4);
			let bir = (table & 7) as usize;
			if let Some(base) = bar_address(d, bir) {
				return (ptr as u16, (mc & 0x7ff) + 1, base + (table & !7) as u64);
			}
		}
		ptr = next;
	}
	(0, 0, 0)
}

// Walk a device's capability list and resolve its virtio configuration
// structures. Returns None if it is not a virtio device or is missing the
// required common/notify/ISR structures.
fn resolve_virtio(d: &PciDevice) -> Option<VirtioDevice> {
	let virtio_type = d.virtio_type()?;
	if cfg_read16(d.bus, d.dev, d.func, 0x06) & STATUS_CAP_LIST == 0 {
		return None;
	}
	assign_bars(d);

	let (mut common, mut notify, mut isr, mut device) = (None, None, None, None);
	let mut ptr = (cfg_read8(d.bus, d.dev, d.func, 0x34) & 0xFC) as usize;
	for _ in 0..48 {
		if ptr == 0 {
			break;
		}
		let cap_id = cfg_read8(d.bus, d.dev, d.func, ptr);
		let next = (cfg_read8(d.bus, d.dev, d.func, ptr + 1) & 0xFC) as usize;
		if cap_id == CAP_ID_VENDOR {
			let cfg_type = cfg_read8(d.bus, d.dev, d.func, ptr + 3);
			let mut cap = VirtioCap { bar: cfg_read8(d.bus, d.dev, d.func, ptr + 4), offset: cfg_read32(d.bus, d.dev, d.func, ptr + 8), length: cfg_read32(d.bus, d.dev, d.func, ptr + 12), notify_multiplier: 0 };
			match cfg_type {
				VIRTIO_CAP_COMMON => common = Some(cap),
				VIRTIO_CAP_NOTIFY => {
					cap.notify_multiplier = cfg_read32(d.bus, d.dev, d.func, ptr + 16);
					notify = Some(cap);
				}
				VIRTIO_CAP_ISR => isr = Some(cap),
				VIRTIO_CAP_DEVICE => device = Some(cap),
				_ => {}
			}
		}
		ptr = next;
	}
	let (msix_cap, msix_count, msix_table_phys) = resolve_msix(d);
	let common = common?;
	let notify = notify?;
	let isr = isr?;
	let device = device.unwrap_or_default();
	let bar = common.bar;
	let bar_phys = bar_address(d, bar as usize)?;
	let mut end = 0u64;
	for cap in [common, notify, isr, device] {
		if cap.bar == bar {
			end = end.max(cap.offset as u64 + cap.length as u64);
		}
	}
	let region_len = end.div_ceil(0x1000) * 0x1000;
	Some(VirtioDevice { pci: *d, virtio_type, bar, bar_phys, region_len, common, notify, isr, device, msix_cap, msix_count, msix_table_phys })
}

// Scan the bus and resolve every virtio device's modern MMIO layout.
pub fn scan_virtio() -> Vec<VirtioDevice> {
	scan().iter().filter_map(resolve_virtio).collect()
}

// Probe a memory BAR's window size: write all-ones, read back the address mask,
// restore. The low half suffices (no device here has a window over 4 GB). Returns
// None for an I/O BAR or an out-of-range index. Used for xHCI, whose window size is
// not described anywhere else (virtio derives its window from the capability list).
fn bar_size(d: &PciDevice, bar_idx: usize) -> Option<u64> {
	if bar_idx >= 6 {
		return None;
	}
	let off = 0x10 + bar_idx * 4;
	let bar = cfg_read32(d.bus, d.dev, d.func, off);
	if bar & 1 != 0 {
		return None; // an I/O-space BAR, not memory
	}
	cfg_write32(d.bus, d.dev, d.func, off, 0xFFFF_FFFF);
	let mask = cfg_read32(d.bus, d.dev, d.func, off);
	cfg_write32(d.bus, d.dev, d.func, off, bar);
	let size = (!(mask & 0xFFFF_FFF0) as u64).wrapping_add(1);
	if size == 0 { None } else { Some(size) }
}

// Resolve an xHCI controller's MMIO window: BAR 0 holds the whole register file, so
// its assigned base + probed size is the window a driver maps. The BARs are assigned
// here first (no firmware does it on QEMU virt with `-kernel`, as for virtio). Returns
// None if the function is not an xHCI controller or BAR 0 is not a memory BAR.
fn resolve_xhci(d: &PciDevice) -> Option<XhciDevice> {
	if !d.is_xhci() {
		return None;
	}
	assign_bars(d);
	let bar_phys = bar_address(d, 0)?;
	let bar_len = bar_size(d, 0)?;
	let (msix_cap, msix_count, msix_table_phys) = resolve_msix(d);
	Some(XhciDevice { pci: *d, bar_phys, bar_len, msix_cap, msix_count, msix_table_phys })
}

// Scan the bus and resolve every xHCI USB host controller's MMIO window.
pub fn scan_xhci() -> Vec<XhciDevice> {
	scan().iter().filter_map(resolve_xhci).collect()
}

// Set or clear a function's PCI command-register Interrupt Disable bit (bit 10), which
// gates whether the device may assert its legacy INTx pin. The kernel takes every
// device interrupt via per-device MSI-X, so the pins stay disabled - a device whose
// driver does not service its interrupt cannot then storm a shared INTx line.
pub fn set_intx_disabled(bus: u8, dev: u8, func: u8, disabled: bool) {
	const INTX_DISABLE: u16 = 1 << 10;
	let command = cfg_read16(bus, dev, func, 0x04);
	let new_command = if disabled { command | INTX_DISABLE } else { command & !INTX_DISABLE };
	cfg_write16(bus, dev, func, 0x04, new_command);
}

// Enable MSI-X on a device (set the MSI-X Enable bit, clear the Function Mask) and make
// sure its memory space is decoded and it is a bus master, so the MSI-X table BAR
// responds and the device may issue the DMA memory write that GICv2m MSI delivery is.
// Called once the kernel has programmed the device's table entry. `cap` is the MSI-X
// capability's config-space offset (from VirtioDevice::msix_cap).
pub fn msix_enable(bus: u8, dev: u8, func: u8, cap: u16) {
	const MEMORY_SPACE: u16 = 1 << 1;
	const BUS_MASTER: u16 = 1 << 2;
	const MSIX_ENABLE: u16 = 1 << 15;
	const FUNCTION_MASK: u16 = 1 << 14;
	let command = cfg_read16(bus, dev, func, 0x04);
	cfg_write16(bus, dev, func, 0x04, command | MEMORY_SPACE | BUS_MASTER);
	// Message Control is the upper 16 bits of the dword at `cap` (cap_id/next are the
	// low 16): enable MSI-X, clear the function mask.
	let dword = cfg_read32(bus, dev, func, cap as usize);
	let mc = (((dword >> 16) as u16) | MSIX_ENABLE) & !FUNCTION_MASK;
	cfg_write32(bus, dev, func, cap as usize, (dword & 0x0000_ffff) | ((mc as u32) << 16));
}

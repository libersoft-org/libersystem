// Portable PCI / PCIe enumeration - shared by every architecture backend.
//
// PCI configuration space is a bus standard: the device identity, the BAR layout,
// the capability list, the virtio MMIO structure descriptors and the MSI-X capability
// are all architecture-independent. The ONLY arch-specific part is how a config-space
// dword is reached - x86 issues the I/O-port mechanism #1 (0xCF8/0xCFC), while ECAM
// platforms (aarch64, riscv64 on QEMU `virt`) map the config space as MMIO. Each
// backend therefore implements the tiny `ConfigAccess` trait (a `read32` / `write32`
// primitive, the bus count, and - where no firmware assigns BARs - a BAR allocator),
// and gets all of the enumeration below for free.
//
// Discovery has to live in the kernel: reading config space needs either the I/O-port
// instructions ring 3 cannot issue or the ECAM MMIO the kernel keeps mapped. The
// resolved device tables are handed to DeviceManager, which maps each BAR to a
// userspace driver via a DeviceMemory capability.

#![allow(dead_code)]

use alloc::vec::Vec;

// virtio's PCI vendor id (Red Hat / virtio).
pub const VIRTIO_VENDOR: u16 = 0x1AF4;

// Modern virtio-pci device ids are 0x1040 + the virtio device type.
const VIRTIO_MODERN_BASE: u16 = 0x1040;

// The PCI class triple of an xHCI USB host controller: Serial Bus Controller /
// USB Controller / xHCI programming interface. Any vendor's controller matches.
const CLASS_SERIAL_BUS: u8 = 0x0C;
const SUBCLASS_USB: u8 = 0x03;
const PROG_IF_XHCI: u8 = 0x30;

// PCI status register bit 4: a capability list is present (pointer at offset 0x34).
const STATUS_CAP_LIST: u16 = 1 << 4;
// Vendor-specific capability id; virtio describes its MMIO structures with these.
const CAP_ID_VENDOR: u8 = 0x09;
// MSI-X capability id. Message Control at +2 (bit 15 = MSI-X Enable, bit 14 =
// Function Mask, bits 10:0 = table size - 1); Table Offset/BIR at +4 (bits 2:0 =
// which BAR, bits 31:3 = byte offset into it).
const MSIX_CAP_ID: u8 = 0x11;

// virtio capability cfg_type values (which structure the capability points at).
const VIRTIO_CAP_COMMON: u8 = 1;
const VIRTIO_CAP_NOTIFY: u8 = 2;
const VIRTIO_CAP_ISR: u8 = 3;
const VIRTIO_CAP_DEVICE: u8 = 4;

// The command-register bits (config offset 0x04, low 16).
const CMD_MEMORY_SPACE: u16 = 1 << 1;
const CMD_BUS_MASTER: u16 = 1 << 2;
const CMD_INTX_DISABLE: u16 = 1 << 10;
// MSI-X Message Control bits (config offset cap+2, upper 16 of the dword at `cap`).
const MSIX_ENABLE: u16 = 1 << 15;
const MSIX_FUNCTION_MASK: u16 = 1 << 14;

// The arch-specific config-space access mechanism. A backend implements the two
// primitives (`read32` / `write32`) and, where relevant, the BAR allocator; the
// derived byte / word reads and every enumeration routine below are portable.
pub trait ConfigAccess {
	// How many buses to enumerate: legacy x86 CAM probes bus 0 only; an ECAM window
	// exposes several (16 on QEMU `virt`).
	const BUS_COUNT: u16;

	// The end of the low 32-bit MMIO window that `assign_bars` reassigns BARs into,
	// used to decide whether a firmware-placed BAR sits outside the mapped window.
	// Only consulted by ECAM platforms that override `assign_bars`; 0 otherwise.
	const MMIO_WINDOW_END: u64 = 0;

	// Read / write a 32-bit config-space dword for one bus/device/function.
	fn read32(bus: u8, dev: u8, func: u8, off: u16) -> u32;
	fn write32(bus: u8, dev: u8, func: u8, off: u16, val: u32);

	// A 16-bit config field, extracted from its enclosing dword. Works for both an
	// ECAM MMIO window and legacy port CAM (which only reads whole dwords), so no
	// backend needs its own sub-dword read.
	fn read16(bus: u8, dev: u8, func: u8, off: u16) -> u16 {
		(Self::read32(bus, dev, func, off & !3) >> ((off as u32 & 2) * 8)) as u16
	}

	// An 8-bit config field, extracted from its enclosing dword.
	fn read8(bus: u8, dev: u8, func: u8, off: u16) -> u8 {
		(Self::read32(bus, dev, func, off & !3) >> ((off as u32 & 3) * 8)) as u8
	}

	// Whether the config space is reachable yet (an ECAM backend gates this on the
	// ECAM base being discovered in the device tree). Default: always ready.
	fn ready() -> bool {
		true
	}

	// Assign a device's memory BARs. A no-op where firmware (or QEMU on x86) already
	// programmed them; ECAM platforms with no firmware override this to call
	// `assign_bars_ecam::<Self>` so the BARs land in the kernel-mapped low window.
	fn assign_bars(_d: &PciDevice) {}

	// Allocate a size-aligned span from the platform's low MMIO window, or None when
	// exhausted. Provided by ECAM platforms whose `assign_bars` reprograms BARs.
	fn alloc_mmio(_size: u64) -> Option<u64> {
		None
	}
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
}

impl PciDevice {
	// Whether this is a virtio device.
	pub fn is_virtio(&self) -> bool {
		self.vendor == VIRTIO_VENDOR
	}

	// Whether this is an xHCI USB host controller (by its PCI class triple, so any
	// vendor's controller matches - QEMU's qemu-xhci as well as real Intel/AMD parts).
	pub fn is_xhci(&self) -> bool {
		self.class == CLASS_SERIAL_BUS && self.subclass == SUBCLASS_USB && self.prog_if == PROG_IF_XHCI
	}

	// The virtio device type. Modern ids encode it as device_id - 0x1040; the
	// transitional ids QEMU can expose (0x1000 net, 0x1001 block, ...) map to the
	// same type numbers so the modern capability path still applies. None if this is
	// not a virtio device.
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
	// MSI-X (when present): the config-space offset of the MSI-X capability (0 = none),
	// the number of table entries, and the physical address of the MSI-X table. The
	// kernel programs table entry 0 and enables MSI-X for an interrupt-driven driver.
	pub msix_cap: u16,
	pub msix_count: u16,
	pub msix_table_phys: u64,
}

// An xHCI USB host controller with its MMIO window resolved: the physical base and
// probed size of BAR 0 (the capability registers start at its base; the operational,
// runtime, and doorbell registers follow at offsets the driver reads from them), plus
// its MSI-X capability for a per-device interrupt vector.
#[derive(Clone, Copy)]
pub struct XhciDevice {
	pub pci: PciDevice,
	pub bar_phys: u64,
	pub bar_len: u64,
	pub msix_cap: u16,
	pub msix_count: u16,
	pub msix_table_phys: u64,
}

// Read the full identity of one present function.
fn read_function<A: ConfigAccess>(bus: u8, dev: u8, func: u8) -> PciDevice {
	let mut bars = [0u32; 6];
	for (i, bar) in bars.iter_mut().enumerate() {
		*bar = A::read32(bus, dev, func, 0x10 + (i as u16) * 4);
	}
	PciDevice { bus, dev, func, vendor: A::read16(bus, dev, func, 0x00), device_id: A::read16(bus, dev, func, 0x02), class: A::read8(bus, dev, func, 0x0b), subclass: A::read8(bus, dev, func, 0x0a), prog_if: A::read8(bus, dev, func, 0x09), header_type: A::read8(bus, dev, func, 0x0e), bars }
}

// Enumerate every present function across the backend's buses. Multi-function
// devices (header-type bit 7) have all eight functions probed; absent slots
// (vendor 0xFFFF) are skipped.
pub fn scan<A: ConfigAccess>() -> Vec<PciDevice> {
	let mut out: Vec<PciDevice> = Vec::new();
	if !A::ready() {
		return out;
	}
	for bus in 0..A::BUS_COUNT {
		let bus = bus as u8;
		for dev in 0..32u8 {
			if A::read16(bus, dev, 0, 0x00) == 0xFFFF {
				continue;
			}
			let multifunction = A::read8(bus, dev, 0, 0x0e) & 0x80 != 0;
			let func_count: u8 = if multifunction { 8 } else { 1 };
			for func in 0..func_count {
				if A::read16(bus, dev, func, 0x00) == 0xFFFF {
					continue;
				}
				out.push(read_function::<A>(bus, dev, func));
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
		abi::VIRTIO_TYPE_GPU => "gpu",
		abi::VIRTIO_TYPE_SOUND => "snd",
		_ => "other",
	}
}

// Decode a memory BAR's assigned physical base (read live from config space, so it
// is correct after `assign_bars` reprograms it), handling 64-bit BARs (which occupy
// two adjacent slots). Returns None for an I/O BAR or an out-of-range index.
pub fn bar_address<A: ConfigAccess>(d: &PciDevice, bar_idx: usize) -> Option<u64> {
	if bar_idx >= 6 {
		return None;
	}
	let bar = A::read32(d.bus, d.dev, d.func, 0x10 + (bar_idx as u16) * 4);
	if bar & 1 != 0 {
		return None; // an I/O-space BAR, not memory
	}
	let base_lo = (bar & 0xFFFF_FFF0) as u64;
	if (bar >> 1) & 3 == 2 {
		// 64-bit memory BAR: the high half lives in the next slot.
		let hi = A::read32(d.bus, d.dev, d.func, 0x10 + (bar_idx as u16 + 1) * 4) as u64;
		Some(hi << 32 | base_lo)
	} else {
		Some(base_lo)
	}
}

// Measure a memory BAR's window size with the standard probe: write all-ones to the
// register, read back the address mask, and restore the original value (the low half
// suffices - no device here has a window over 4 GB). Needed for devices like xHCI
// whose window size is not described anywhere else; virtio derives its window from
// the capability list instead. Returns None for an I/O BAR or an out-of-range index.
pub fn bar_size<A: ConfigAccess>(d: &PciDevice, bar_idx: usize) -> Option<u64> {
	if bar_idx >= 6 {
		return None;
	}
	let off: u16 = 0x10 + (bar_idx as u16) * 4;
	let bar = A::read32(d.bus, d.dev, d.func, off);
	if bar & 1 != 0 {
		return None; // an I/O-space BAR, not memory
	}
	A::write32(d.bus, d.dev, d.func, off, 0xFFFF_FFFF);
	let mask = A::read32(d.bus, d.dev, d.func, off);
	A::write32(d.bus, d.dev, d.func, off, bar);
	let size = (!(mask & 0xFFFF_FFF0) as u64).wrapping_add(1);
	if size == 0 { None } else { Some(size) }
}

// Assign a device's memory BARs out of the low 32-bit MMIO window, then enable
// memory-space decoding and bus-master in the command register. For ECAM platforms
// with no firmware to program the BARs: a BAR is (re)assigned if it is unprogrammed
// (QEMU `virt` with `-kernel`) OR the firmware placed it outside the low window the
// kernel's boot stub maps (a UEFI boot may assign the 64-bit window at 512 GB, which
// the direct map does not cover) - so devices land in the mapped low window regardless
// of how the kernel was booted. Memory decode is turned off while the BARs move.
pub fn assign_bars_ecam<A: ConfigAccess>(d: &PciDevice) {
	// Disable memory-space decoding while the BARs move (the firmware may have enabled it).
	let cmd0 = A::read16(d.bus, d.dev, d.func, 0x04);
	write_command::<A>(d.bus, d.dev, d.func, cmd0 & !CMD_MEMORY_SPACE);
	let mut i = 0usize;
	while i < 6 {
		let off = 0x10 + (i as u16) * 4;
		let bar = A::read32(d.bus, d.dev, d.func, off);
		if bar & 1 != 0 {
			i += 1; // I/O BAR - not used here
			continue;
		}
		let is64 = (bar >> 1) & 3 == 2;
		// Probe the size (write all-ones, read back the mask, restore).
		A::write32(d.bus, d.dev, d.func, off, 0xFFFF_FFFF);
		let mask_lo = A::read32(d.bus, d.dev, d.func, off);
		A::write32(d.bus, d.dev, d.func, off, bar);
		let (mask, cur) = if is64 {
			let bar_hi = A::read32(d.bus, d.dev, d.func, off + 4);
			A::write32(d.bus, d.dev, d.func, off + 4, 0xFFFF_FFFF);
			let mask_hi = A::read32(d.bus, d.dev, d.func, off + 4);
			A::write32(d.bus, d.dev, d.func, off + 4, bar_hi);
			(((mask_hi as u64) << 32) | (mask_lo & 0xFFFF_FFF0) as u64, ((bar_hi as u64) << 32) | (bar & 0xFFFF_FFF0) as u64)
		} else {
			((mask_lo & 0xFFFF_FFF0) as u64 | 0xFFFF_FFFF_0000_0000, (bar & 0xFFFF_FFF0) as u64)
		};
		let size = (!mask).wrapping_add(1);
		let step = if is64 { 2 } else { 1 };
		if size == 0 || mask == !0u64 {
			i += step;
			continue;
		}
		if cur == 0 || cur >= A::MMIO_WINDOW_END {
			if let Some(base) = A::alloc_mmio(size) {
				A::write32(d.bus, d.dev, d.func, off, (base as u32 & 0xFFFF_FFF0) | (bar & 0xF));
				if is64 {
					A::write32(d.bus, d.dev, d.func, off + 4, (base >> 32) as u32);
				}
			}
		}
		i += step;
	}
	// Enable memory space (bit 1) + bus master (bit 2) for the device to respond + DMA.
	let cmd = A::read16(d.bus, d.dev, d.func, 0x04);
	write_command::<A>(d.bus, d.dev, d.func, cmd | CMD_MEMORY_SPACE | CMD_BUS_MASTER);
}

// Write the 16-bit command register (config offset 0x04) as a full dword with the
// status half (write-1-to-clear bits) forced to 0, so updating the command never
// clears a status bit. Shared by every command-register update below.
fn write_command<A: ConfigAccess>(bus: u8, dev: u8, func: u8, command: u16) {
	A::write32(bus, dev, func, 0x04, command as u32);
}

// Walk a device's PCI capability list and resolve its MSI-X capability: the
// capability's config-space offset (0 = none), the table entry count, and the
// physical address of the MSI-X table. Shared by the virtio and xHCI paths.
fn resolve_msix<A: ConfigAccess>(d: &PciDevice) -> (u16, u16, u64) {
	if A::read16(d.bus, d.dev, d.func, 0x06) & STATUS_CAP_LIST == 0 {
		return (0, 0, 0);
	}
	let mut ptr: u16 = (A::read8(d.bus, d.dev, d.func, 0x34) & 0xFC) as u16;
	// Bound the walk so a malformed (cyclic) list cannot spin forever.
	for _ in 0..48 {
		if ptr == 0 {
			break;
		}
		let cap_id = A::read8(d.bus, d.dev, d.func, ptr);
		let next = (A::read8(d.bus, d.dev, d.func, ptr + 1) & 0xFC) as u16;
		if cap_id == MSIX_CAP_ID {
			let mc = A::read16(d.bus, d.dev, d.func, ptr + 2);
			let table_off_bir = A::read32(d.bus, d.dev, d.func, ptr + 4);
			let bir = (table_off_bir & 7) as usize;
			let table_offset = (table_off_bir & !7) as u64;
			if let Some(base) = bar_address::<A>(d, bir) {
				return (ptr, (mc & 0x7ff) + 1, base + table_offset);
			}
		}
		ptr = next;
	}
	(0, 0, 0)
}

// Walk a device's PCI capability list and resolve its virtio configuration
// structures. Returns None if it is not a virtio device, has no capability list, or
// is missing the required common/notify/ISR structures. `assign_bars` runs first so
// ECAM platforms program the BARs the resolved offsets are relative to.
fn resolve_virtio<A: ConfigAccess>(d: &PciDevice) -> Option<VirtioDevice> {
	let virtio_type = d.virtio_type()?;
	if A::read16(d.bus, d.dev, d.func, 0x06) & STATUS_CAP_LIST == 0 {
		return None;
	}
	A::assign_bars(d);
	let (mut common, mut notify, mut isr, mut device) = (None, None, None, None);
	let mut ptr: u16 = (A::read8(d.bus, d.dev, d.func, 0x34) & 0xFC) as u16;
	// Bound the walk so a malformed (cyclic) list cannot spin forever.
	for _ in 0..48 {
		if ptr == 0 {
			break;
		}
		let cap_id = A::read8(d.bus, d.dev, d.func, ptr);
		let next = (A::read8(d.bus, d.dev, d.func, ptr + 1) & 0xFC) as u16;
		if cap_id == CAP_ID_VENDOR {
			let cfg_type = A::read8(d.bus, d.dev, d.func, ptr + 3);
			let mut cap = VirtioCap { bar: A::read8(d.bus, d.dev, d.func, ptr + 4), offset: A::read32(d.bus, d.dev, d.func, ptr + 8), length: A::read32(d.bus, d.dev, d.func, ptr + 12), notify_multiplier: 0 };
			match cfg_type {
				VIRTIO_CAP_COMMON => common = Some(cap),
				VIRTIO_CAP_NOTIFY => {
					cap.notify_multiplier = A::read32(d.bus, d.dev, d.func, ptr + 16);
					notify = Some(cap);
				}
				VIRTIO_CAP_ISR => isr = Some(cap),
				VIRTIO_CAP_DEVICE => device = Some(cap),
				_ => {}
			}
		}
		ptr = next;
	}
	let (msix_cap, msix_count, msix_table_phys) = resolve_msix::<A>(d);
	let common = common?;
	let notify = notify?;
	let isr = isr?;
	let device = device.unwrap_or_default();
	let bar = common.bar;
	let bar_phys = bar_address::<A>(d, bar as usize)?;
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
	Some(VirtioDevice { pci: *d, virtio_type, bar, bar_phys, region_len, common, notify, isr, device, msix_cap, msix_count, msix_table_phys })
}

// Scan the bus and resolve every modern virtio device's MMIO layout.
pub fn scan_virtio<A: ConfigAccess>() -> Vec<VirtioDevice> {
	scan::<A>().iter().filter_map(resolve_virtio::<A>).collect()
}

// Resolve an xHCI controller's MMIO window: BAR 0 holds the whole register file, so
// its (assigned) base + probed size is the window a driver maps. `assign_bars` runs
// first for ECAM platforms with no firmware. Returns None if the function is not an
// xHCI controller or BAR 0 is not a memory BAR.
fn resolve_xhci<A: ConfigAccess>(d: &PciDevice) -> Option<XhciDevice> {
	if !d.is_xhci() {
		return None;
	}
	A::assign_bars(d);
	let bar_phys = bar_address::<A>(d, 0)?;
	let bar_len = bar_size::<A>(d, 0)?;
	let (msix_cap, msix_count, msix_table_phys) = resolve_msix::<A>(d);
	Some(XhciDevice { pci: *d, bar_phys, bar_len, msix_cap, msix_count, msix_table_phys })
}

// Scan the bus and resolve every xHCI USB host controller's MMIO window.
pub fn scan_xhci<A: ConfigAccess>() -> Vec<XhciDevice> {
	scan::<A>().iter().filter_map(resolve_xhci::<A>).collect()
}

// Set or clear a function's PCI command-register Interrupt Disable bit (bit 10), which
// gates whether the device may assert its legacy INTx pin. Disabling it silences a
// device whose driver does not service its interrupt, so it cannot storm a shared INTx
// line (the kernel takes every device interrupt via per-device MSI-X). The status half
// is written as 0, so no write-1-to-clear status bit is touched.
pub fn set_intx_disabled<A: ConfigAccess>(bus: u8, dev: u8, func: u8, disabled: bool) {
	let command = A::read16(bus, dev, func, 0x04);
	let new_command = if disabled { command | CMD_INTX_DISABLE } else { command & !CMD_INTX_DISABLE };
	write_command::<A>(bus, dev, func, new_command);
}

// Enable MSI-X on a device (set the MSI-X Enable bit, clear the Function Mask) and
// make sure its memory space is decoded and it is a bus master, so the MSI-X table
// BAR responds and the device may issue the DMA memory write MSI delivery is. Called
// once the kernel has programmed the device's table entry. `cap` is the MSI-X
// capability's config-space offset (from VirtioDevice::msix_cap).
pub fn msix_enable<A: ConfigAccess>(bus: u8, dev: u8, func: u8, cap: u16) {
	let command = A::read16(bus, dev, func, 0x04);
	write_command::<A>(bus, dev, func, command | CMD_MEMORY_SPACE | CMD_BUS_MASTER);
	// Message Control is the upper 16 bits of the dword at `cap` (cap_id/next are the
	// low 16): enable MSI-X, clear the function mask.
	let dword = A::read32(bus, dev, func, cap);
	let mc = (((dword >> 16) as u16) | MSIX_ENABLE) & !MSIX_FUNCTION_MASK;
	A::write32(bus, dev, func, cap, (dword & 0x0000_ffff) | ((mc as u32) << 16));
}

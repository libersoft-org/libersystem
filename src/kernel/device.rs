// The system device table.
//
// The kernel scans the PCI bus once at boot (it alone can issue the I/O-port
// instructions PCI config space needs) and records each virtio device's MMIO
// layout here. DeviceManager queries this table over the device syscalls and is
// handed a DeviceMemory capability per device, so it can map each virtio device to
// a userspace driver and give that driver only its own device's MMIO window. The
// per-structure offsets travel as plain data (`device_info`) since a ring-3 driver
// cannot read PCI config space itself.

use alloc::vec::Vec;

use crate::sync::SpinLock;

// One discovered virtio device, resolved from its PCI capabilities.
pub struct DeviceEntry {
	pub virtio_type: u16,
	// Physical base + length of the MMIO BAR the driver maps.
	pub bar_phys: u64,
	pub bar_len: u64,
	// Byte offsets of the virtio structures within that BAR.
	pub common_offset: u32,
	pub notify_offset: u32,
	pub notify_multiplier: u32,
	pub isr_offset: u32,
	pub device_offset: u32,
	// MSI-X (when present): the config-space offset of the device's MSI-X capability
	// (0 = none) and the physical address of its MSI-X table. The kernel programs table
	// entry 0 and enables MSI-X so a driver gets its own per-device edge-triggered
	// vector instead of the shared INTx line above.
	pub msix_cap: u16,
	pub msix_table_phys: u64,
	// The device's PCI address, so the interrupt-acquire path can re-enable its INTx pin
	// (init disables every device's pin by default; see below).
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
}

static DEVICES: SpinLock<Vec<DeviceEntry>> = SpinLock::new(Vec::new());

// Populate the table from a PCI scan. Called once at boot, after the heap is up.
pub fn init() {
	let mut table = DEVICES.lock();
	table.clear();
	for v in crate::arch::pci::scan_virtio() {
		// Silence every device's legacy INTx pin: the kernel takes all device interrupts via
		// per-device MSI-X (input, net, snd) and the remaining drivers poll, so no driver uses
		// a shared INTx line. Disabling the pins keeps a stray assertion off the (fully masked)
		// I/O APIC by construction.
		crate::arch::pci::set_intx_disabled(v.pci.bus, v.pci.dev, v.pci.func, true);
		table.push(DeviceEntry { virtio_type: v.virtio_type, bar_phys: v.bar_phys, bar_len: v.region_len, common_offset: v.common.offset, notify_offset: v.notify.offset, notify_multiplier: v.notify.notify_multiplier, isr_offset: v.isr.offset, device_offset: v.device.offset, msix_cap: v.msix_cap, msix_table_phys: v.msix_table_phys, bus: v.pci.bus, dev: v.pci.dev, func: v.pci.func });
	}
}

// The number of discovered devices.
pub fn count() -> usize {
	DEVICES.lock().len()
}

// Run `f` against the device at `index`, returning None if it is out of range. The
// closure runs under the table lock, so callers must not block inside it.
pub fn with<R>(index: usize, f: impl FnOnce(&DeviceEntry) -> R) -> Option<R> {
	let table = DEVICES.lock();
	table.get(index).map(f)
}

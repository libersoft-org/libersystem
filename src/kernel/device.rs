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
	// The IOAPIC GSI this device's INTx pin is routed to (0 = no interrupt pin), so a
	// driver can acquire an Interrupt the kernel routes through the I/O APIC.
	pub irq: u8,
}

static DEVICES: SpinLock<Vec<DeviceEntry>> = SpinLock::new(Vec::new());

// Populate the table from a PCI scan. Called once at boot, after the heap is up.
pub fn init() {
	let mut table = DEVICES.lock();
	table.clear();
	for v in crate::arch::pci::scan_virtio() {
		table.push(DeviceEntry { virtio_type: v.virtio_type, bar_phys: v.bar_phys, bar_len: v.region_len, common_offset: v.common.offset, notify_offset: v.notify.offset, notify_multiplier: v.notify.notify_multiplier, isr_offset: v.isr.offset, device_offset: v.device.offset, irq: v.pci.intx_gsi().unwrap_or(0) as u8 });
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

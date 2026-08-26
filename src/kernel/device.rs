// The system device table.
//
// The kernel scans the PCI bus once at boot (it alone can issue the I/O-port
// instructions PCI config space needs) and records each discovered device's MMIO
// layout here - the virtio devices and any xHCI USB host controller. DeviceManager
// queries this table over the device syscalls and is handed a DeviceMemory
// capability per device, so it can map each device to a userspace driver and give
// that driver only its own device's MMIO window. The per-structure offsets travel
// as plain data (`device_info`) since a ring-3 driver cannot read PCI config space
// itself.

use alloc::vec::Vec;

use crate::sync::SpinLock;

// One discovered device, resolved from its PCI configuration.
pub struct DeviceEntry {
	pub device_type: u16,
	// Physical base + length of the MMIO BAR the driver maps.
	pub bar_phys: u64,
	pub bar_len: u64,
	// Byte offsets of the virtio structures within that BAR (zero for a non-virtio
	// device such as the xHCI controller, whose registers start at the BAR base).
	pub common_offset: u32,
	pub notify_offset: u32,
	pub notify_multiplier: u32,
	pub isr_offset: u32,
	// The optional device-specific structure. A length of zero is how "this device has none" is
	// said, because offset zero is also a legal offset for one that does (KERN-ARCH-014).
	pub device_offset: u32,
	pub device_len: u32,
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
	// The standards identity, carried from the same scan that resolved the BAR. It was resolved,
	// retained for `lspci`, and not passed to the one consumer that binds drivers by it.
	pub class: u8,
	pub subclass: u8,
	pub prog_if: u8,
}

static DEVICES: SpinLock<Vec<DeviceEntry>> = SpinLock::new(Vec::new());

// The full boot PCI scan - every present function, not just the virtio / xHCI ones
// drivers bind - retained so the bus stays inspectable at runtime. SYS_PCI_INFO
// reads it for `lspci`.
static PCI_FUNCTIONS: SpinLock<Vec<abi::PciInfo>> = SpinLock::new(Vec::new());

// Populate the table from a PCI scan. Called once at boot, after the heap is up.
pub fn init() {
	let mut functions = PCI_FUNCTIONS.lock();
	functions.clear();
	for p in crate::arch::pci::scan() {
		// NOBODY IS DRIVING ANYTHING YET, so nothing on this bus may write to memory.
		//
		// `assign_bars_ecam` clears the bit as it places the BARs, but only two of the three ports
		// place their own: on x86 the firmware placed them AND enabled bus mastering, and that path
		// never runs, so the bit arrived set and stayed set. This is the sweep that covers every
		// port, over every function the scan found - including the ones no driver will ever bind,
		// which are exactly the devices nobody would notice mastering the bus.
		//
		// BRIDGES ARE LEFT ALONE (header type 1): their bus-master bit forwards transactions from
		// everything behind them rather than granting the bridge anything of its own, and clearing
		// it here would silently cut off a device whose own driver had legitimately acquired it.
		if p.header_type & 0x7F == 0 {
			crate::arch::pci::set_bus_master(p.bus, p.dev, p.func, false);
		}
		// ALLOC-OK: the device inventory is built once at boot from what the bus reports.
		functions.push(abi::PciInfo { vendor: p.vendor, device: p.device_id, class: p.class, subclass: p.subclass, prog_if: p.prog_if, bus: p.bus, dev: p.dev, func: p.func, _pad: 0 });
	}
	drop(functions);
	let mut table = DEVICES.lock();
	table.clear();
	for v in crate::arch::pci::scan_virtio() {
		// Silence every device's legacy INTx pin: the kernel takes all device interrupts via
		// per-device MSI-X (input, net, snd) and the remaining drivers poll, so no driver uses
		// a shared INTx line. Disabling the pins keeps a stray assertion off the (fully masked)
		// I/O APIC by construction.
		crate::arch::pci::set_intx_disabled(v.pci.bus, v.pci.dev, v.pci.func, true);
		// ALLOC-OK: the device inventory is built once at boot from what the bus reports.
		table.push(DeviceEntry { device_type: v.virtio_type, bar_phys: v.bar_phys, bar_len: v.region_len, common_offset: v.common.offset, notify_offset: v.notify.offset, notify_multiplier: v.notify.notify_multiplier, isr_offset: v.isr.offset, device_offset: v.device.map_or(0, |cap| cap.offset), device_len: v.device.map_or(0, |cap| cap.length), msix_cap: v.msix_cap, msix_table_phys: v.msix_table_phys, bus: v.pci.bus, dev: v.pci.dev, func: v.pci.func, class: v.pci.class, subclass: v.pci.subclass, prog_if: v.pci.prog_if });
	}
	for x in crate::arch::pci::scan_xhci() {
		// The xHCI controller joins the same table: its whole register file lives in
		// BAR 0, so the virtio structure offsets are zero and the driver reads the
		// operational/runtime/doorbell offsets from the capability registers at the base.
		crate::arch::pci::set_intx_disabled(x.pci.bus, x.pci.dev, x.pci.func, true);
		// ALLOC-OK: the device inventory is built once at boot from what the bus reports.
		table.push(DeviceEntry { device_type: abi::DEVICE_TYPE_XHCI as u16, bar_phys: x.bar_phys, bar_len: x.bar_len, common_offset: 0, notify_offset: 0, notify_multiplier: 0, isr_offset: 0, device_offset: 0, device_len: 0, msix_cap: x.msix_cap, msix_table_phys: x.msix_table_phys, bus: x.pci.bus, dev: x.pci.dev, func: x.pci.func, class: x.pci.class, subclass: x.pci.subclass, prog_if: x.pci.prog_if });
	}
	// One ownership count per device, all zero: nothing is driving anything yet, and enumeration
	// left every device with bus mastering off.
	let mut owners = OWNERS.lock();
	owners.clear();
	// ALLOC-OK: sized once at boot from the table just built.
	owners.resize(table.len(), 0);
}

// The number of discovered devices.
pub fn count() -> usize {
	DEVICES.lock().len()
}

// The number of retained PCI functions.
pub fn pci_count() -> usize {
	PCI_FUNCTIONS.lock().len()
}

// One retained PCI function by index.
pub fn pci_get(index: usize) -> Option<abi::PciInfo> {
	PCI_FUNCTIONS.lock().get(index).copied()
}

// Run `f` against the device at `index`, returning None if it is out of range. The
// closure runs under the table lock, so callers must not block inside it.
pub fn with<R>(index: usize, f: impl FnOnce(&DeviceEntry) -> R) -> Option<R> {
	let table = DEVICES.lock();
	table.get(index).map(f)
}

// HOW MANY DRIVERS HOLD EACH DEVICE, and therefore whether it may write to memory.
//
// `sys_device_acquire` mints a FRESH `DeviceMemory` per call, so two acquisitions of one device are
// two independent objects with no refcount between them - which is why "nobody owns it" needed
// defining before bus mastering could follow ownership at all. One count per device-table index is
// enough, and the kernel already keys per-device bookkeeping this way in `dma_buffer::release_for`.
//
// Parallel to `DEVICES` and taken under the SAME lock as the config-space write below, so two
// acquisitions racing cannot leave the PCI bit disagreeing with the count.
static OWNERS: SpinLock<Vec<u32>> = SpinLock::new(Vec::new());

// A driver has taken the device at `index`: count it, and on the 0 -> 1 transition let the device
// master the bus. False when the index names no device, in which case NOTHING changed - a failed
// acquisition must not leave a device able to write to memory.
pub fn acquire_bus_master(index: usize) -> bool {
	let table = DEVICES.lock();
	let mut owners = OWNERS.lock();
	let Some(entry) = table.get(index) else { return false };
	let Some(count) = owners.get_mut(index) else { return false };
	// THE DMA THREAT MODEL IS DECIDED HERE, at the one moment a device gains the ability to reach
	// memory on its own. A driver that declared it needs translation does not master the bus without
	// it - and a refusal is a refusal: there is no fall-back to untranslated DMA, because falling
	// back is the failure the isolation claim names in as many words: it must never silently become
	// untranslated DMA.
	//
	// Everything in this tree is `trusted-untranslated` today, so what this call does at present is
	// RECORD the degraded state by name rather than change any outcome. That is the point of putting
	// it in before there is an IOMMU: when one arrives, the decision is already where it belongs.
	if crate::dma_policy::admit(entry.device_type, entry.bus, entry.dev, entry.func) == dma::BindDecision::Refused {
		return false;
	}
	if *count == 0 {
		// ATTACHED BEFORE IT CAN MASTER THE BUS, and refused if the attach does not confirm. The
		// window between "this device can reach memory" and "this device is translated" is the one
		// place untranslated DMA could happen under an enforcing profile, and the way to have no
		// such window is to do them in this order.
		if crate::iommu::present() && !crate::iommu::attach_for(index, entry.bus, entry.dev, entry.func) {
			return false;
		}
		crate::arch::pci::set_bus_master(entry.bus, entry.dev, entry.func, true);
	}
	*count += 1;
	true
}

// How many drivers hold the device at `index`.
//
// TEST-ONLY. Nothing in the kernel needs to ask - the count exists to drive the two transitions
// above - but a test that means to check the PCI bit against the kernel's own opinion of who is
// driving what has to be able to read both.
#[cfg(test)]
pub fn bus_master_owners(index: usize) -> u32 {
	OWNERS.lock().get(index).copied().unwrap_or(0)
}

// A driver has let the device at `index` go - the last `DeviceMemory` for it was dropped. On the
// 1 -> 0 transition the device stops mastering the bus, so a driver that CRASHED disables its own
// device without knowing the rule exists: its handle dies with its process.
pub fn release_bus_master(index: usize) {
	let table = DEVICES.lock();
	let mut owners = OWNERS.lock();
	let (Some(entry), Some(count)) = (table.get(index), owners.get_mut(index)) else { return };
	if *count == 0 {
		return;
	}
	*count -= 1;
	if *count == 0 {
		// BUS MASTERING GOES FIRST, then the translation. The reverse order would leave a window in
		// which the device can still master the bus and is no longer translated, which is the same
		// hole as the acquire path's, arrived at from the other side.
		crate::arch::pci::set_bus_master(entry.bus, entry.dev, entry.func, false);
		if crate::iommu::present() {
			crate::iommu::detach_for(index, entry.bus, entry.dev, entry.func);
		}
		// AND THE AUDIT RECORD GOES WITH IT. `admit` wrote the degraded row when this device was
		// asking to master the bus; it is not mastering it any more, and a list of "devices reaching
		// memory untranslated" that keeps a device which gave the bus back is a list reporting a
		// machine nobody is running.
		crate::dma_policy::forget_degraded(entry.bus, entry.dev, entry.func);
	}
}

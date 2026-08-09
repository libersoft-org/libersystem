use super::{scan, scan_xhci};

crate::tagged_test!(pci_scan_finds_virtio_devices, [Pci, Drivers, ArchX86_64], covers = ["kernel"]);
fn pci_scan_finds_virtio_devices() {
	// QEMU is launched (see qemu-run.sh) with virtio-blk, virtio-net, and a virtio
	// serial device on the PCI bus. The kernel's PCI scan must find them: at least
	// one device carrying virtio's PCI vendor id, and each such modern virtio device
	// must report a recognizable device type.
	let devices = scan();
	let virtio: alloc::vec::Vec<_> = devices.iter().filter(|device| device.is_virtio()).collect();
	assert!(!virtio.is_empty(), "the PCI scan should find the QEMU virtio devices");
	for device in &virtio {
		assert!(device.virtio_type().is_some(), "a modern virtio device should report a device type (id {:#06x})", device.device_id);
	}
}

crate::tagged_test!(pci_scan_finds_the_xhci_controller, [Pci, Drivers, Usb, ArchX86_64], covers = ["kernel"]);
fn pci_scan_finds_the_xhci_controller() {
	// QEMU is launched (see qemu-run.sh) with a qemu-xhci USB host controller. The
	// kernel's PCI scan must find it by its class triple (0x0C/0x03/0x30) and resolve
	// its MMIO window: a non-zero BAR 0 base and a probed BAR size (the sizing write-
	// all-ones round-trip), plus an MSI-X capability for its interrupt vector.
	let controllers = scan_xhci();
	assert!(!controllers.is_empty(), "the PCI scan should find the QEMU xHCI controller");
	for controller in &controllers {
		assert!(controller.bar_phys != 0, "the xHCI BAR 0 should have a physical base");
		assert!(controller.bar_len >= 0x1000, "the xHCI BAR 0 should be at least a page (probed {:#x})", controller.bar_len);
		assert!(controller.msix_cap != 0, "the xHCI controller should expose MSI-X");
	}
}

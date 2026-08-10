use super::{acquire_msi, bind_msi, dispatch_msi, irq_info, irq_info_len, is_bound, unbind};
use crate::mem::frame;
use crate::object::interrupt::Interrupt;

// The AArch64 counterpart of the x86 INTx tests: every device interrupt is MSI-X
// delivered through the GICv2m frame. There is no bindable wired vector.
crate::tagged_test!(gicv2m_msi_binds_and_dispatch_signals_the_driver, [Interrupt, Drivers, ArchAarch64], id = "kernel.arch.aarch64.interrupts.gicv2m_msi_binds_and_dispatch_signals_the_driver", covers = ["kernel"]);
fn gicv2m_msi_binds_and_dispatch_signals_the_driver() {
	// A frame stands in for a device's MSI-X table: acquire_msi programs entry 0 into it
	// (message address = the GICv2m frame's MSI_SETSPI_NS, message data = the SPI).
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	// Acquire a per-device MSI vector (a GICv2m SPI). `dest` (the x86 LAPIC target) is
	// unused on AArch64; `owner` is a fake discovered-device index.
	let vector = acquire_msi(table, 0, 3).expect("acquire_msi hands out a free SPI");
	// Bind a driver Interrupt to the vector; a second live bind is refused.
	let interrupt = Interrupt::new(vector);
	assert!(bind_msi(vector, &interrupt), "the first bind succeeds");
	assert!(is_bound(vector), "the vector reads as bound");
	let second_interrupt = Interrupt::new(vector);
	assert!(!bind_msi(vector, &second_interrupt), "a second live bind is refused");
	// Dispatching the SPI INTID - what gic::handle_irq does when the SPI fires - marks
	// the bound Interrupt pending (its wait readiness).
	assert!(!interrupt.is_pending(), "not pending before the SPI fires");
	assert!(dispatch_msi(vector as u32), "dispatch_msi claims its own SPI");
	assert!(interrupt.is_pending(), "dispatch signalled the bound Interrupt");
	// An INTID below the frame's SPI range (the SGI / PPI space) is not an MSI vector.
	assert!(!dispatch_msi(0), "INTID 0 is not one of the frame's MSI SPIs");
	// Unbinding frees the slot for re-use.
	unbind(vector);
	assert!(!is_bound(vector), "unbind drops the binding");
	unsafe { frame::deallocate(table) };
}

crate::tagged_test!(gicv2m_msi_inventory_reports_the_timer_and_msi_vectors, [Interrupt, Drivers, ArchAarch64], id = "kernel.arch.aarch64.interrupts.gicv2m_msi_inventory_reports_the_timer_and_msi_vectors", covers = ["kernel"]);
fn gicv2m_msi_inventory_reports_the_timer_and_msi_vectors() {
	// Index 0 of the AArch64 IRQ inventory (what `lsirq` reads) is the kernel's own EL1
	// physical-timer PPI (INTID 30), always in use and reported as a fixed vector - the
	// AArch64 analogue of x86's fixed timer entry.
	let timer = irq_info(0).expect("the inventory has a timer entry");
	assert_eq!(timer.kind, abi::IRQ_KIND_FIXED, "index 0 is the fixed timer PPI");
	assert_eq!(timer.vector, 30, "the AArch64 timer is the EL1 physical-timer PPI (INTID 30)");
	assert_eq!(timer.bound, 1, "the timer is always the kernel's own");
	// After the timer, each entry is a GICv2m MSI SPI. Acquiring one for a fake device
	// makes it appear in the inventory as an MSI vector owned by that device index.
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	let vector = acquire_msi(table, 0, 9).expect("acquire an MSI SPI");
	let mut seen = false;
	for index in 1..irq_info_len() {
		if let Some(info) = irq_info(index)
			&& info.vector == vector as u32
		{
			assert_eq!(info.kind, abi::IRQ_KIND_MSI, "an acquired vector reports as MSI");
			assert_eq!(info.device, 9, "the inventory records the owning device index");
			seen = true;
		}
	}
	assert!(seen, "the acquired MSI vector appears in the inventory");
	unbind(vector);
	unsafe { frame::deallocate(table) };
}

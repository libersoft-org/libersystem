use super::{acquire_msi, bind_msi, dispatch_msi, irq_info, irq_info_len, is_bound, unbind};
use crate::mem::frame;
use crate::object::interrupt::Interrupt;

// The RISC-V counterpart of the x86 INTx and aarch64 GICv2m interrupt tests: on QEMU
// virt with AIA/IMSIC, every device interrupt is an MSI-X-delivered EID pended in a
// hart's IMSIC S-file. There is no bindable wired vector.
crate::tagged_test!(imsic_msi_binds_and_dispatch_signals_the_driver, [Interrupt, Drivers, ArchRiscv64], id = "kernel.arch.riscv64.interrupts.imsic_msi_binds_and_dispatch_signals_the_driver", covers = ["kernel"]);
fn imsic_msi_binds_and_dispatch_signals_the_driver() {
	// A frame stands in for a device's MSI-X table: acquire_msi programs entry 0 into it
	// (message address = the acquiring hart's IMSIC S-file, message data = the EID).
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	// Acquire a per-device MSI vector (an IMSIC EID). `dest` (the x86 LAPIC target) is
	// unused on RISC-V; `owner` is a fake discovered-device index.
	let vector = acquire_msi(table, 0, 3).expect("acquire_msi hands out a free EID");
	// Bind a driver Interrupt to the vector; a second live bind is refused.
	let interrupt = Interrupt::new(vector);
	assert!(bind_msi(vector, &interrupt), "the first bind succeeds");
	assert!(is_bound(vector), "the vector reads as bound");
	let second_interrupt = Interrupt::new(vector);
	assert!(!bind_msi(vector, &second_interrupt), "a second live bind is refused");
	// Dispatching the EID - what imsic::handle_external does when the EID fires - marks the
	// bound Interrupt pending (its wait readiness).
	assert!(!interrupt.is_pending(), "not pending before the EID fires");
	assert!(dispatch_msi(vector as u32), "dispatch_msi claims its own EID");
	assert!(interrupt.is_pending(), "dispatch signalled the bound Interrupt");
	// EID 0 is "no interrupt" - outside the MSI window - so it dispatches to no one.
	assert!(!dispatch_msi(0), "EID 0 is not one of the device MSI EIDs");
	// Unbinding frees the slot for re-use.
	unbind(vector);
	assert!(!is_bound(vector), "unbind drops the binding");
	unsafe { frame::deallocate(table) };
}

crate::tagged_test!(imsic_msi_inventory_reports_the_timer_and_msi_vectors, [Interrupt, Drivers, ArchRiscv64], id = "kernel.arch.riscv64.interrupts.imsic_msi_inventory_reports_the_timer_and_msi_vectors", covers = ["kernel"]);
fn imsic_msi_inventory_reports_the_timer_and_msi_vectors() {
	// Index 0 of the RISC-V IRQ inventory (what `lsirq` reads) is the kernel's own S-mode
	// timer interrupt (SCAUSE code 5), always in use and reported as a fixed vector - the
	// RISC-V analogue of x86's fixed LAPIC-timer entry and aarch64's timer PPI.
	let timer = irq_info(0).expect("the inventory has a timer entry");
	assert_eq!(timer.kind, abi::IRQ_KIND_FIXED, "index 0 is the fixed S-mode timer");
	assert_eq!(timer.vector, 5, "the RISC-V timer is the S-mode timer interrupt (scause code 5)");
	assert_eq!(timer.bound, 1, "the timer is always the kernel's own");
	// After the timer, each entry is an IMSIC MSI EID. Acquiring one for a fake device
	// makes it appear in the inventory as an MSI vector owned by that device index.
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	let vector = acquire_msi(table, 0, 9).expect("acquire an MSI EID");
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

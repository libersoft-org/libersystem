use super::{acquire_msi, bind_msi, dispatch_msi, irq_info, irq_info_len, is_bound, unbind};
use crate::mem::frame;
use crate::object::interrupt::Interrupt;

// The AArch64 counterpart of the x86 INTx tests: every device interrupt is MSI-X
// delivered through the GICv2m frame. There is no bindable wired vector.
crate::tagged_test!(gicv2m_msi_binds_and_dispatch_signals_the_driver, [Interrupt, Drivers, ArchAarch64], id = "kernel.arch.aarch64.interrupts.gicv2m_msi_binds_and_dispatch_signals_the_driver", covers = ["kernel"]);
fn gicv2m_msi_binds_and_dispatch_signals_the_driver() {
	// AND ONLY WHERE THERE IS AN MSI BACKEND AT ALL. QEMU's `virt` has a GICv2m frame with GICv2 and
	// an ITS with `its=on`; a GICv3 machine with the ITS OFF has neither, so `acquire_msi` has no
	// vector to hand out and this test's premise - "every device interrupt is MSI-X delivered
	// through the GICv2m frame" - is simply false there. Saying so is the honest answer: the profile
	// exists to exercise the timer and IPI paths, and asking it an MSI question proves nothing about
	// either.
	if super::MSI_LEN.load(core::sync::atomic::Ordering::Relaxed) == 0 {
		crate::serial_println!("gicv2m: skipped - this machine has no MSI frame and no ITS, so there is no device interrupt to bind");
		return;
	}
	// A frame stands in for a device's MSI-X table: acquire_msi programs entry 0 into it
	// (message address = the GICv2m frame's MSI_SETSPI_NS, message data = the SPI).
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	// Acquire a per-device MSI vector (a GICv2m SPI). `dest` (the x86 LAPIC target) is
	// unused on AArch64; `owner` is a fake discovered-device index.
	let vector = acquire_msi(table, 0, 3).expect("acquire_msi hands out a free SPI");
	// Bind a driver Interrupt to the vector; a second live bind is refused.
	let interrupt = Interrupt::new(vector).expect("a test interrupt");
	assert!(bind_msi(vector, &interrupt), "the first bind succeeds");
	assert!(is_bound(vector), "the vector reads as bound");
	let second_interrupt = Interrupt::new(vector).expect("a test interrupt");
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
	//
	// THE TIMER HALF IS TRUE ON EVERY PROFILE and is asserted above; the MSI half needs a machine
	// that HAS MSIs. A GICv3 with the ITS off has neither a GICv2m frame nor an ITS, and the
	// inventory there is the timer and nothing else - which is the correct inventory for that
	// machine rather than a missing entry.
	if super::MSI_LEN.load(core::sync::atomic::Ordering::Relaxed) == 0 {
		crate::serial_println!("gicv2m: the timer entry is checked; this machine has no MSI backend, so there is no MSI entry to look for");
		return;
	}
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

crate::tagged_test!(a_gicv2m_spi_above_255_keeps_its_identity, [Interrupt, Drivers, ArchAarch64], id = "kernel.arch.aarch64.interrupts.a_gicv2m_spi_above_255_keeps_its_identity", covers = ["kernel"]);
fn a_gicv2m_spi_above_255_keeps_its_identity() {
	// KERN-ARCH-017. GICv2m's MSI_TYPER gives a TEN-BIT base SPI and count, and the backend
	// programmed the full `u32` into the device's message data - then returned it as a `u8`. On a
	// frame based at 256 or above the device's interrupt stayed armed under the real SPI while the
	// registry, the bind, the teardown and `lsirq` all named `spi & 0xff`: a different, possibly
	// another device's, identifier.
	//
	// QEMU virt's frame starts at SPI 80, so the range is stood up here instead. Every result is
	// taken BEFORE the frame's real range is put back, so a failing assertion cannot leave the
	// following tests looking at a machine that does not exist.
	//
	// AND ONLY ON A MACHINE WHOSE MSIs REALLY GO THROUGH A GICv2m FRAME. Under the GICv3/ITS
	// profile an MSI is an LPI: `acquire_msi` returns `LPI_BASE + slot`, the stood-up SPI range is
	// not consulted at all, and this test asserted 256 against 8192. That is not a defect in either
	// path - it is a test asking a GICv2m question of a machine that has no GICv2m, and the honest
	// answer is to say so rather than to widen the assertion until both answers pass.
	use core::sync::atomic::Ordering;
	if super::USING_ITS.load(Ordering::Relaxed) {
		crate::serial_println!("gicv2m: skipped - this machine's MSIs are LPIs through the ITS, so there is no SPI frame to identify");
		return;
	}
	let base = super::BASE_SPI.load(Ordering::Relaxed);
	let len = super::MSI_LEN.load(Ordering::Relaxed);
	super::BASE_SPI.store(256, Ordering::Relaxed);
	super::MSI_LEN.store(64, Ordering::Relaxed);
	// The whole path, not just the arithmetic: acquire a slot from the stood-up frame and see
	// which identifier comes back. Truncated, the first slot of a frame based at 256 answers 0 -
	// SGI 0, the cross-core wake IPI, which is emphatically not this device's interrupt.
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	let acquired = acquire_msi(table, 0, 7);
	if let Some(vector) = acquired {
		super::release_unused_msi(vector);
	}
	let first = super::spi_slot(256);
	let middle = super::spi_slot(300);
	let below = super::spi_slot(255);
	let past = super::spi_slot(320);
	let recognised = super::is_msi(300);
	let carried = Interrupt::new(300).map(|intr| intr.vector());
	super::BASE_SPI.store(base, Ordering::Relaxed);
	super::MSI_LEN.store(len, Ordering::Relaxed);

	// SAFETY: the frame allocated above, used only as a stand-in MSI-X table and never mapped.
	// NEVER-MAPPED: a plain frame written through the direct map by `program_msix_entry`.
	unsafe { frame::deallocate(table) };
	assert_eq!(acquired, Some(256), "the frame's first slot is SPI 256, not SPI 256 truncated to a byte");
	assert_eq!(first, Some(0), "the frame's first SPI is its first slot");
	assert_eq!(middle, Some(44), "and SPI 300 is slot 44, not slot 300 & 0xff");
	assert_eq!(below, None, "an INTID below the frame is not one of its MSIs");
	assert_eq!(past, None, "nor one past its count");
	assert!(recognised, "SPI 300 reads as an MSI vector");
	assert_eq!(carried, Some(300), "and the identifier survives the object that carries it");
}

use super::{acquire_msi, bind_msi, dispatch_msi, irq_info, irq_info_len, is_bound, is_quarantined, quarantine_for_test, release_msi_for_device, unbind};
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
	let interrupt = Interrupt::new(vector).expect("a test interrupt");
	assert!(bind_msi(vector, &interrupt), "the first bind succeeds");
	assert!(is_bound(vector), "the vector reads as bound");
	let second_interrupt = Interrupt::new(vector).expect("a test interrupt");
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

crate::tagged_test!(an_eid_is_disabled_in_the_file_that_owns_it, [Interrupt, Drivers, ArchRiscv64], id = "kernel.arch.riscv64.interrupts.an_eid_is_disabled_in_the_file_that_owns_it", covers = ["kernel"]);
fn an_eid_is_disabled_in_the_file_that_owns_it() {
	// An IMSIC enable bit lives in ONE hart's interrupt file and can only be written by that hart.
	// `acquire_msi` enables the EID here and programs the device to target here, so this hart is the
	// owner and the disable is local.
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	let vector = acquire_msi(table, 0, 11).expect("acquire_msi hands out a free EID");
	assert!(crate::arch::imsic::disable_eid_on_owner(vector), "the acquiring hart owns the EID, so it disables it itself");
	// And an EID nobody owns is already in the state the caller asked for, rather than a refusal.
	assert!(crate::arch::imsic::disable_eid_on_owner(vector), "an EID with no owner needs no cross-hart request");
	unbind(vector);
	unsafe { frame::deallocate(table) };
}

crate::tagged_test!(a_vector_whose_identity_stayed_armed_is_never_handed_out_again, [Interrupt, Drivers, ArchRiscv64], id = "kernel.arch.riscv64.interrupts.a_vector_whose_identity_stayed_armed_is_never_handed_out_again", covers = ["kernel"]);
fn a_vector_whose_identity_stayed_armed_is_never_handed_out_again() {
	// WHAT A TIMED-OUT CROSS-HART DISABLE COSTS, and what it must not cost. The owning hart never
	// answered, so the EID is still enabled in its file and a device may still be delivering to it.
	// Handing that identity to another driver would wake it from hardware it does not own; leaking
	// one vector is the smaller price, and it is visible.
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	let stranded = acquire_msi(table, 0, 12).expect("acquire_msi hands out a free EID");
	quarantine_for_test(stranded);
	assert!(is_quarantined(stranded), "the vector reads as out of circulation");
	assert!(!is_bound(stranded), "quarantine drops the binding");
	// NOT EVEN THE DEVICE CAN RELEASE IT. A quiesce is the device saying it stopped, which answers
	// the `retire` case; it says nothing about an enable bit this kernel could not clear.
	assert_eq!(release_msi_for_device(12), 0, "a device quiesce does not release a quarantined vector");
	let next = acquire_msi(table, 0, 13).expect("another EID is available");
	assert_ne!(next, stranded, "the quarantined EID is not handed out again");
	unbind(next);
	unsafe { frame::deallocate(table) };
}

// A MACHINE WHOSE IMSIC THIS KERNEL COULD NOT ADDRESS HANDS OUT NO VECTOR.
//
// `imsic::configure` refuses every layout this port cannot address and deliberately leaves the
// previous value alone, so a boot that READ a tree and refused what it described used to keep the
// compiled `qemu-virt-aia` address and start writing MSIs into it - a static descriptor selected by
// a boot which has a DT, which the architecture contract forbids in as many words. Naming the
// refusal, which it did, is much better than defaulting silently and is still hardcoded addresses on
// a machine that said otherwise. The boot now takes the MSI path out of service instead, and this is
// the half of that a test can drive: what `disarm` costs, and that it costs it before any address is
// programmed into a device's table.
crate::tagged_test!(a_machine_whose_imsic_this_kernel_refused_hands_out_no_msi_vector, [Interrupt, Drivers, ArchRiscv64], id = "kernel.arch.riscv64.interrupts.a_machine_whose_imsic_this_kernel_refused_hands_out_no_msi_vector", covers = ["kernel"]);
fn a_machine_whose_imsic_this_kernel_refused_hands_out_no_msi_vector() {
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	assert!(crate::arch::imsic::usable(), "this machine's IMSIC was accepted, which is what makes the refusal below a change");

	// THE REFUSAL IS DRIVEN, NOT ASSERTED. This set the flag directly, which proved that `usable()`
	// gates `acquire_msi` and nothing about the boundary the test is named for: that a TREE
	// describing a layout this port cannot address takes the MSI path out of service. Each of these
	// is a machine `configure` must refuse, and every one of them is a layout the AIA binding
	// permits and this kernel's `base + hart * 4096` arithmetic does not describe.
	let accepted = crate::arch::imsic::snapshot_for_test();
	for (what, info) in [
		("no supervisor IMSIC at all", {
			let mut b = accepted;
			b.base = 0;
			b
		}),
		("guest-indexed files", {
			let mut b = accepted;
			b.guest_index_bits = 1;
			b
		}),
		("group-indexed files", {
			let mut b = accepted;
			b.group_index_bits = 2;
			b
		}),
		("fewer identities than the window arms", {
			let mut b = accepted;
			b.num_ids = 8;
			b
		}),
		("files tied to no hart", {
			let mut b = accepted;
			b.hart_count = 0;
			b
		}),
		("a region smaller than the files it declares", {
			let mut b = accepted;
			b.size = 0x800;
			b
		}),
		("harts that are not their own file index", {
			let mut b = accepted;
			b.harts[0] = 9;
			b
		}),
		// AND FILES THE DIRECT MAP DOES NOT REACH. The refusal for this has always been the FIRST one
		// `configure_layout` makes after the zero-base check, and no case here drove it - the FDT
		// suite decodes a high address and never passes it through this boundary, so the one shape
		// whose consequence is a store into whatever `phys_to_virt` arithmetic produced was the one
		// shape nobody had watched be refused.
		("interrupt files outside the direct map", {
			let mut b = accepted;
			b.base = crate::mem::direct_map_ceiling_for_test().saturating_add(0x1000_0000);
			b
		}),
	] {
		let refused = crate::arch::imsic::configure_layout(&info);
		assert!(refused.is_err(), "{what} is a layout this port cannot address, and `configure` must say so rather than compute an address inside it");
		// AND THE REFUSAL LEAVES THE PREVIOUS VALUE ALONE, which is the half that makes the boot's
		// `disarm` necessary: without it the machine keeps the address it was given before.
		assert!(crate::arch::imsic::usable(), "a refusal on its own does not disarm - the boot path does, and that is what the next line drives");
		crate::arch::imsic::disarm();
		assert!(!crate::arch::imsic::usable(), "the boot's answer to a refusal is to take the MSI path out of service");
		assert!(acquire_msi(table, 0, 21).is_none(), "and a disarmed machine hands out no vector rather than programming the address it refused: {what}");
		crate::arch::imsic::set_usable_for_test(true);
		crate::arch::imsic::configure_layout(&accepted).expect("the machine this test runs on is still the one it started on");
	}

	crate::arch::imsic::set_usable_for_test(false);
	assert!(acquire_msi(table, 0, 21).is_none(), "a refused machine hands out no vector rather than programming the address it refused");

	// AND THE REGISTRY IS NOT LEFT HOLDING THE SLOT. A refusal that consumed an EID would run the
	// machine out of vectors on a path that never delivers one.
	crate::arch::imsic::set_usable_for_test(true);
	let vector = acquire_msi(table, 0, 21).expect("and the same acquire works once the machine is usable again");
	release_msi_for_device(21);
	assert!(!is_bound(vector));

	// SAFETY: allocated above and never handed to a device - no vector was ever programmed into it.
	unsafe { frame::deallocate(table) };
}

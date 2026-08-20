use super::register;
use core::sync::atomic::{AtomicBool, Ordering};

crate::tagged_test!(handler_registration_dispatch, [Interrupt, Kernel, ArchX86_64], id = "kernel.arch.x86_64.interrupts.handler_registration_dispatch", covers = ["kernel"]);
fn handler_registration_dispatch() {
	static FIRED: AtomicBool = AtomicBool::new(false);
	fn handler(_vector: u32) {
		FIRED.store(true, Ordering::SeqCst);
	}
	// Register on an unused device vector and trigger it with a software
	// interrupt: proves registration and dispatch wiring without a device.
	register(47, handler);
	unsafe { core::arch::asm!("int 0x2f", options(nomem, nostack)) };
	assert!(FIRED.load(Ordering::SeqCst));
}

crate::tagged_test!(an_msix_entry_is_mapped_whole_wherever_in_its_page_it_starts, [Kernel, Interrupt], id = "kernel.arch.x86_64.interrupts.an_msix_entry_is_mapped_whole_wherever_in_its_page_it_starts", covers = ["kernel"]);
fn an_msix_entry_is_mapped_whole_wherever_in_its_page_it_starts() {
	// KERN-ARCH-016. The backend mapped ONE page and wrote four dwords at the entry's offset into
	// it. An MSI-X table is 8-byte aligned, so a spec-valid device can put its entry at 0xff8 -
	// and then the message data and vector control dwords are written to an unmapped address,
	// while programming an interrupt, in the kernel.
	//
	// Asserted as arithmetic rather than by programming a device: the failure is a kernel page
	// fault, and QEMU will not place a table where it would happen.
	let mut offset = 0u64;
	while offset < 4096 {
		let pages = super::msix_pages_for_entry(offset);
		assert!((1..=2).contains(&pages), "an entry needs one page or two, not {pages}");
		assert!(offset + super::MSIX_ENTRY_BYTES <= pages * 4096, "an entry at {offset:#x} ends at {:#x}, past the {pages} page(s) mapped for it", offset + super::MSIX_ENTRY_BYTES);
		offset += 8; // the alignment the PCI capability's offset field actually guarantees
	}
	assert_eq!(super::msix_pages_for_entry(0xff0), 1, "the last entry that fits whole needs one page");
	assert_eq!(super::msix_pages_for_entry(0xff8), 2, "and the one eight bytes later does not");

	// AND ONE SLOT'S SECOND PAGE IS NOT THE NEXT SLOT'S FIRST. The mapping is per slot at a fixed
	// address, so a stride of one page would have a straddling entry map over its neighbour -
	// which `map_page` refuses, turning a working device into a failed one.
	for slot in 0..super::MSI_COUNT - 1 {
		assert!(super::msix_virt(slot + 1) - super::msix_virt(slot) >= 2 * 4096, "slot {slot} and the next overlap");
	}
	// And the whole window still fits the single page table `init` materialises for it, which is
	// what makes these mappings visible in every address space's shared kernel half.
	let top = super::msix_virt(super::MSI_COUNT - 1) + 2 * 4096;
	assert!(top - super::MSIX_VIRT_BASE <= 2 * 1024 * 1024, "the MSI-X window outgrew its page table");
}

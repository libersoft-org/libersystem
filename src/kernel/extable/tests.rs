use super::*;

crate::tagged_test!(the_exception_table_exists_and_is_small_enough_to_scan, [Kernel, Memory], covers = ["kernel"]);
fn the_exception_table_exists_and_is_small_enough_to_scan() {
	// Two claims this module makes about itself, both of which stop being true silently.
	//
	// The first is that the mechanism is IN the binary. `.extable` is emitted by inline assembly
	// inside the copy routines and gathered by the linker script; drop the `KEEP`, rename the
	// section, or let the routines be optimised out and the table is empty - and an empty table
	// turns every fixup into a kernel halt, which is exactly the behaviour this milestone replaced.
	// Nothing else would notice until a userspace race took the machine down.
	assert!(declared() > 0, "the exception table is empty: no instruction in this build declares that it may fault");

	// The second is that a LINEAR scan is the right shape. It is, at this size - the faulting
	// instructions are the copy loops themselves and there are a handful of them. The moment that
	// stops being true the lookup runs inside a fault handler on every kernel fault, and the answer
	// is to sort the table and binary-search it. This is the line that will say so.
	assert!(declared() <= 64, "the exception table has grown to {} entries; a linear scan in the fault handler is no longer the right shape - sort it and binary-search", declared());
}

crate::tagged_test!(a_copy_to_a_page_that_is_not_there_reports_rather_than_kills_the_kernel, [Kernel, Memory, Syscall], covers = ["kernel"]);
fn a_copy_to_a_page_that_is_not_there_reports_rather_than_kills_the_kernel() {
	// The whole milestone in one assertion: a kernel copy to a user address that is NOT mapped must
	// come back saying how far it got, and the machine must still be running to hear it.
	//
	// Without the fixup this does not fail, it HALTS - the ring-0 page fault falls through to the
	// halt loop and the suite stops dead at this line with no result. That is the shape of the
	// defect too, so an unfixed kernel and a broken fixup look identical from here, which is the
	// point: there is no way to write this test such that it merely goes red.
	//
	// A user-half address nothing has mapped. The kernel half would fault for a different reason
	// (and on x86_64 with SMAP the user half is exactly where the protection lives), so this has to
	// be an address userspace COULD have had and does not.
	//
	// Derived from `USER_VA_END` rather than written out, because the user half is not the same size
	// on every target: riscv64 runs Sv39 and its user half ends at 2^38, where the 2^46 address this
	// used to name is not merely unmapped but unmappable. That difference is invisible in a test
	// that only wants a fault - both answers fault - and it is exactly what broke the partial-copy
	// test below, which needs the address to be one the kernel can really map.
	const ABSENT: u64 = crate::memlayout::USER_VA_END / 2;
	let payload = [0xA5u8; 64];

	let caught_before = caught();
	let copied = crate::arch::paging::user_access(|| unsafe { crate::arch::usercopy::copy_to_user(ABSENT, payload.as_ptr(), payload.len()) });
	assert_eq!(copied, 0, "not one byte can have landed in a page that is not there");
	assert!(caught() > caught_before, "the fault was fixed up, which is the only way execution reached this line");

	// And the reverse direction, which is a different instruction and a different table entry: the
	// LOAD faults rather than the store.
	let mut into = [0u8; 64];
	let caught_before = caught();
	let read = crate::arch::paging::user_access(|| unsafe { crate::arch::usercopy::copy_from_user(into.as_mut_ptr(), ABSENT, into.len()) });
	assert_eq!(read, 0, "nothing can be read out of a page that is not there");
	assert!(caught() > caught_before, "and that fault was fixed up too");
	assert_eq!(into, [0u8; 64], "a refused read leaves the destination alone");

	// The kernel is still healthy: an ordinary copy between two kernel buffers still works, which
	// says the fixup returned to the right place rather than somewhere that happens not to crash.
	let mut ok = [0u8; 64];
	let n = unsafe { crate::arch::usercopy::copy_from_user(ok.as_mut_ptr(), payload.as_ptr() as u64, payload.len()) };
	assert_eq!(n, payload.len(), "a copy that faults nowhere copies everything");
	assert_eq!(ok, payload, "and copies the right bytes");
}

crate::tagged_test!(a_fault_on_a_kernel_address_is_never_rescued_however_the_pc_matches, [Kernel, Memory], covers = ["kernel"]);
fn a_fault_on_a_kernel_address_is_never_rescued_however_the_pc_matches() {
	// The condition that keeps a real kernel bug loud, which the PC match alone does not.
	//
	// The table's entries cover instructions that touch a KERNEL operand as well as a user one: the
	// aarch64 and riscv64 byte loops declare both their load and their store, and x86_64's
	// `rep movsb` reads and writes in one instruction. So a kernel bug that hands `copy_to_user` a
	// bad kernel source pointer faults at a PC that is genuinely in the table. Recovering it would
	// convert that bug into a short copy - a wrong answer, delivered quietly, to a caller with no
	// way to tell.
	//
	// Asserted on the lookup rather than by faulting, because the failure mode of getting this
	// wrong is a kernel that keeps running with a corrupted assumption, and there is no way to
	// arrange the real fault without also arranging the bug.
	let declared = declared();
	assert!(declared > 0, "there is a table to ask about");

	// Every address the table names is a real fixup, so a user-half fault at one is rescued.
	let entry = first_entry().expect("the table has at least one entry");
	assert!(fixup_for(entry, crate::memlayout::USER_VA_END / 2).is_some(), "a declared instruction faulting on a user address must be rescued - that is the whole mechanism");

	// The SAME instruction faulting on a kernel address is not.
	assert!(fixup_for(entry, crate::memlayout::USER_VA_END).is_none(), "the first address of the kernel half is not userspace's");
	assert!(fixup_for(entry, u64::MAX & !0xFFF).is_none(), "nor is the top of the address space");
	assert!(fixup_for(entry, 0xFFFF_8000_0000_0000).is_none(), "nor is a higher-half kernel pointer, which is what a kernel bug passes");
}

crate::tagged_test!(a_copy_that_loses_its_page_partway_reports_exactly_how_far_it_got, [Kernel, Memory, Syscall], covers = ["kernel"]);
fn a_copy_that_loses_its_page_partway_reports_exactly_how_far_it_got() {
	// The state a mid-copy unmap actually produces, arranged deterministically.
	//
	// M0149 asks for a test that unmaps the page DURING the copy, on the grounds that the defect is
	// a race. The race is real and the interleaving is not reproducible on a cooperative single-core
	// harness - this suite has retired tests that tried. What IS reproducible is the state the race
	// leaves behind: a copy running into a page that is not there, partway through, with bytes
	// already delivered behind it. Two adjacent user pages with only the first mapped put the copy
	// in exactly that state at exactly the boundary, every time.
	//
	// And it tests more than the wholly-absent case does. A copy that faults on its first byte only
	// proves the fixup runs; this proves the ACCOUNTING - that the count coming back is the bytes
	// that landed, not zero and not the length asked for. A caller that believed either would
	// truncate a message or read uninitialised memory.
	use crate::arch::paging::{PRESENT, USER, WRITABLE};
	use crate::mem::frame::PAGE_SIZE;

	// Inside the user half on EVERY target - see `ABSENT` above for why that is not automatic. Two
	// pages are needed, so the address is a page-pair below the halfway mark rather than at it.
	const AT: u64 = crate::memlayout::USER_VA_END / 2 - 2 * PAGE_SIZE;
	let frame = crate::mem::frame::allocate().expect("a frame for the mapped half");
	crate::arch::paging::map_page(AT, frame, PRESENT | WRITABLE | USER);
	// The page AFTER it is deliberately absent. Nothing to undo at the end.

	let payload = alloc::vec![0x5Au8; (PAGE_SIZE * 2) as usize];
	let caught_before = caught();
	let copied = crate::arch::paging::user_access(|| unsafe { crate::arch::usercopy::copy_to_user(AT, payload.as_ptr(), payload.len()) });

	assert_eq!(copied as u64, PAGE_SIZE, "the copy stops at the page boundary and says so - not zero, not the whole length");
	assert!(caught() > caught_before, "and it stopped by being fixed up, not by checking anything");

	// The bytes that ARE claimed really arrived. A fixup that reported the right number while
	// delivering nothing would pass every assertion above.
	let landed = crate::arch::paging::user_access(|| {
		let mut all = true;
		for offset in [0u64, 1, PAGE_SIZE / 2, PAGE_SIZE - 1] {
			all &= unsafe { core::ptr::read_volatile((AT + offset) as *const u8) } == 0x5A;
		}
		all
	});
	assert!(landed, "every byte the copy counted must be on the page");

	crate::arch::paging::unmap_page(AT);
	unsafe { crate::mem::frame::deallocate(frame) };
}

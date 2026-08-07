use super::AddressSpace;
use crate::{arch, mem};

// A user virtual address far enough from a low one to need its own intermediate page
// tables, and inside the user half on every port - riscv64's Sv39 half ends at 256 GiB,
// so this is 64 GiB rather than something comfortable on x86_64.
const FAR_USER_VA: u64 = 0x0000_0010_0000_0000;

fn user_flags() -> u64 {
	arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER | arch::paging::NO_EXECUTE
}

crate::tagged_test!(map_degrades_to_error_when_out_of_frames, [Memory]);
fn map_degrades_to_error_when_out_of_frames() {
	use mem::frame;
	// A userspace-triggered map must degrade, not panic, when the frame pool is
	// empty: the walk cannot allocate an intermediate page table and returns an
	// error the map syscalls turn into ERR_NO_MEMORY. A fresh address space has an
	// empty user half, so mapping a low (user) VA is guaranteed to need a new
	// intermediate table.
	let space = AddressSpace::create().expect("a fresh address space");
	let leaf = frame::allocate().expect("one frame to point the leaf at");
	// Drain the rest of the pool. Reserve the holding vector first so it never
	// grows (mapping a heap page) inside the drained window.
	let mut held: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
	held.reserve(frame::free_count() + 8);
	while let Some(frame) = frame::allocate() {
		held.push(frame);
	}
	let flags = arch::paging::PRESENT | arch::paging::WRITABLE | arch::paging::USER | arch::paging::NO_EXECUTE;
	let result = space.try_map(0x1_0000, leaf, flags);
	// Refill the pool before asserting, so a failed assertion never leaves it
	// drained. `leaf` stays ours until the end.
	for frame in held {
		unsafe { frame::deallocate(frame) };
	}
	assert!(result.is_err(), "an out-of-frames map must fail cleanly, not panic");
	// The failed map left nothing behind: the same VA maps fine now the pool is back.
	space.try_map(0x1_0000, leaf, flags).expect("the map succeeds once frames are available");
	space.unmap(0x1_0000);
	unsafe { frame::deallocate(leaf) };
}

crate::tagged_test!(each_address_space_has_its_own_user_window, [Memory]);
fn each_address_space_has_its_own_user_window() {
	// The user mmap window is per address space, and the sharpest way to say so is that
	// two spaces hand out the SAME address. A global pool cannot: it would give the second
	// caller the range after the first, because to it the two requests are neighbours in
	// one window. They are not neighbours in anything - each address is a number in a
	// different set of page tables.
	let first = AddressSpace::create().expect("a fresh address space");
	let second = AddressSpace::create().expect("a second fresh address space");
	let a = first.alloc_vrange(4 * mem::frame::PAGE_SIZE);
	let b = second.alloc_vrange(4 * mem::frame::PAGE_SIZE);
	assert_ne!(a, 0, "a fresh window must have room");
	assert_eq!(a, b, "two address spaces must hand out the same user address, not consecutive ones");
	assert_eq!(a, crate::memlayout::USER_MMAP_BASE, "and it must be the base of the window");

	// And they are independent in the other direction too: releasing in one does not put
	// the range into the other's free list, so the second space's next allocation still
	// comes off its own bump cursor rather than reusing what the first gave back.
	first.free_vrange(a, 4 * mem::frame::PAGE_SIZE);
	let c = second.alloc_vrange(4 * mem::frame::PAGE_SIZE);
	assert_eq!(c, b + 4 * mem::frame::PAGE_SIZE, "a free in one space must not feed an allocation in another");
	// And the first space did take its own range back.
	assert_eq!(first.alloc_vrange(4 * mem::frame::PAGE_SIZE), a, "the released range returns to the space that released it");
}

crate::tagged_test!(a_page_table_oom_rolls_back_and_leaves_earlier_mappings_alone, [Memory]);
fn a_page_table_oom_rolls_back_and_leaves_earlier_mappings_alone() {
	// The rollback that matters is not the one at the first level. Draining the pool tests
	// that: nothing can be allocated, so nothing is half-built. What has to hold is a failure
	// PART WAY DOWN - two levels created, the third refused - leaving the space exactly as it
	// was, including every mapping made before it.
	//
	// The number of levels differs by port (four on x86_64, four on aarch64, three on Sv39),
	// so this walks the budget upward instead of naming a depth: fail after k allocations for
	// k = 0, 1, 2, ... until the map finally succeeds, and after every refusal check that the
	// page mapped first is still mapped, still to its own frame.
	let space = AddressSpace::create().expect("a fresh address space");
	let first_frame = mem::frame::allocate().expect("a frame for the low page");
	let far_frame = mem::frame::allocate().expect("a frame for the far page");
	const LOW: u64 = 0x1_0000;
	space.try_map(LOW, first_frame, user_flags()).expect("the low page maps in an empty space");

	let mut refusals = 0;
	let mut budget = 0;
	loop {
		assert!(budget < 8, "no budget up to 8 let the far page map: the injection is not reaching the mapper");
		mem::frame::fail_allocations_after(budget);
		let result = space.try_map(FAR_USER_VA, far_frame, user_flags());
		mem::frame::stop_failing_allocations();
		if result.is_ok() {
			break;
		}
		refusals += 1;
		// Unmapping is how a mapping is read back here - there is no per-space translate - so
		// the low page is checked by taking it out and putting it straight back. A rollback
		// that freed a table it did not own would have taken this mapping with it.
		let recovered = space.unmap(LOW);
		assert_eq!(recovered, Some(first_frame), "a failed map at budget {budget} disturbed a mapping made before it");
		space.try_map(LOW, first_frame, user_flags()).expect("restoring the low page needs no new table");
		budget += 1;
	}
	assert!(refusals > 0, "the far page mapped with no allocations at all: nothing was injected");

	space.unmap(FAR_USER_VA);
	space.unmap(LOW);
	// SAFETY: both frames were allocated by this test and have just been unmapped from the
	// only address space they were ever in.
	unsafe {
		mem::frame::deallocate(first_frame);
		mem::frame::deallocate(far_frame);
	}
}

crate::tagged_test!(a_user_mapping_is_refused_outside_the_user_half, [Memory]);
fn a_user_mapping_is_refused_outside_the_user_half() {
	// The bound the ET_EXEC escalation ran through. A mapping carrying USER names memory
	// ring 3 may reach, and the kernel half is not that - whoever computed the address.
	let space = AddressSpace::create().expect("a fresh address space");
	let frame = mem::frame::allocate().expect("one frame");
	let above = crate::memlayout::USER_VA_END;
	assert!(space.try_map(above, frame, user_flags()).is_err(), "a USER mapping AT the top of the user half must be refused");
	assert!(space.try_map(above + 0x1000, frame, user_flags()).is_err(), "a USER mapping above the user half must be refused");
	assert!(space.try_map(u64::MAX & !0xfff, frame, user_flags()).is_err(), "a USER mapping at the top of the address space must be refused");
	// The last page that IS inside the half maps, so the bound is the boundary and not a
	// blanket refusal that would pass this test by refusing everything.
	let last = above - crate::mem::frame::PAGE_SIZE;
	space.try_map(last, frame, user_flags()).expect("the last page of the user half is inside it");
	assert_eq!(space.unmap(last), Some(frame));
	// SAFETY: allocated here, unmapped above, held by nothing else.
	unsafe { mem::frame::deallocate(frame) };
}

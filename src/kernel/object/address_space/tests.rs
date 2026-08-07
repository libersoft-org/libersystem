use super::AddressSpace;
use crate::{arch, mem};

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

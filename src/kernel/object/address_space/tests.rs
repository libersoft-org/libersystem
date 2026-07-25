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
		frame::deallocate(frame);
	}
	assert!(result.is_err(), "an out-of-frames map must fail cleanly, not panic");
	// The failed map left nothing behind: the same VA maps fine now the pool is back.
	space.try_map(0x1_0000, leaf, flags).expect("the map succeeds once frames are available");
	space.unmap(0x1_0000);
	frame::deallocate(leaf);
}

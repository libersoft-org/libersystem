use super::{PAGE_SIZE, allocate, allocate_contiguous, deallocate, free_count};

const FRAGMENTED_PAGES: u64 = (super::SEED_RUNS as u64 + 1) * 2;

// Every `deallocate` below frees a frame this test allocated itself and holds alone, which is
// the contract; the two calls that deliberately break it are in
// `the_allocator_refuses_a_frame_it_never_handed_out`, which says so where it does it.

crate::tagged_test!(frame_alloc_distinct, [Frame, Memory, Smoke]);
fn frame_alloc_distinct() {
	let first = allocate().expect("first frame");
	let second = allocate().expect("second frame");
	assert_ne!(first, second);
	unsafe {
		deallocate(first);
		deallocate(second);
	}
}

crate::tagged_test!(the_frame_pool_grows_past_the_boot_table_and_refuses_a_double_free, [Frame, Memory]);
fn the_frame_pool_grows_past_the_boot_table_and_refuses_a_double_free() {
	let before = free_count();
	let base = allocate_contiguous(FRAGMENTED_PAGES as usize).expect("a fragmented frame span");
	unsafe {
		for index in (0..FRAGMENTED_PAGES).step_by(2) {
			deallocate(base + index * PAGE_SIZE);
		}
		for index in (1..FRAGMENTED_PAGES).step_by(2) {
			deallocate(base + index * PAGE_SIZE);
		}
	}
	assert_eq!(free_count(), before, "every fragmented page returned to the pool");
	let again = allocate_contiguous(FRAGMENTED_PAGES as usize).expect("the span re-coalesced whole");
	unsafe { deallocate(again) };
	let after_free = free_count();
	// A double free of a frame that is still free. The overlap test refuses it; the ownership
	// record refuses it too, and either way nothing is added to the pool.
	unsafe { deallocate(again) };
	assert_eq!(free_count(), after_free, "a double free adds nothing to the pool");
	unsafe {
		for index in 1..FRAGMENTED_PAGES {
			deallocate(again + index * PAGE_SIZE);
		}
	}
	assert_eq!(free_count(), before, "the pool round-trips exactly");
}

crate::tagged_test!(contiguous_frame_runs_recoalesce, [Frame, Memory]);
fn contiguous_frame_runs_recoalesce() {
	let base = allocate_contiguous(64).expect("a 256 kB span");
	unsafe {
		for index in 0..64u64 {
			deallocate(base + index * PAGE_SIZE);
		}
	}
	let again = allocate_contiguous(128).expect("a 512 kB span after coalescing");
	unsafe {
		for index in 0..128u64 {
			deallocate(again + index * PAGE_SIZE);
		}
	}
}

crate::tagged_test!(the_allocator_refuses_a_frame_it_never_handed_out, [Frame, Memory]);
fn the_allocator_refuses_a_frame_it_never_handed_out() {
	// The debug ownership record's job, and the one thing the overlap test in `insert`
	// cannot do. Overlap catches a free that lands on memory the pool already calls free.
	// It says nothing about a free that lands somewhere the pool has never heard of - an
	// MMIO address, a bootloader reservation, a number that was never a frame - and
	// accepting one of those hands out non-RAM as if it were memory.
	//
	// Only meaningful where the record exists. In a release build nothing checks this and
	// the calls below would corrupt the pool, so the test does not make them.
	#[cfg(debug_assertions)]
	{
		let before = free_count();
		// Far above any plausible pool, and page-aligned so it is refused for not being
		// ours rather than for being misaligned.
		// SAFETY: violating `deallocate`'s contract is the point, and it is safe to do
		// here precisely because the record refuses the call before the pool is touched -
		// which is the assertion below.
		unsafe { deallocate(0x0000_7000_0000_0000) };
		assert_eq!(free_count(), before, "an address outside the pool must not become free memory");

		// Not page-aligned, and inside the pool: a frame offset by a few bytes is a
		// plausible slip and would insert a run that overlaps a real frame.
		let frame = allocate().expect("one frame");
		// SAFETY: as above - the misalignment is refused before anything is inserted.
		unsafe { deallocate(frame + 8) };
		assert_eq!(free_count(), before - 1, "a misaligned address must not become free memory");
		unsafe { deallocate(frame) };
		assert_eq!(free_count(), before, "and the frame itself still frees normally");
	}
}

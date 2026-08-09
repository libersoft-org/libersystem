use super::{PAGE_SIZE, allocate, allocate_contiguous, deallocate};

const FRAGMENTED_PAGES: u64 = (super::SEED_RUNS as u64 + 1) * 2;

// Every `deallocate` below frees a frame this test allocated itself and holds alone, which is
// the contract; the two calls that deliberately break it are in
// `the_allocator_refuses_a_frame_it_never_handed_out`, which says so where it does it.

crate::tagged_test!(frame_alloc_distinct, [Frame, Memory, Smoke], covers = ["kernel"]);
fn frame_alloc_distinct() {
	let first = allocate().expect("first frame");
	let second = allocate().expect("second frame");
	assert_ne!(first, second);
	unsafe {
		deallocate(first);
		deallocate(second);
	}
}

crate::tagged_test!(the_frame_pool_grows_past_the_boot_table_and_refuses_a_double_free, [Frame, Memory], covers = ["kernel"]);
fn the_frame_pool_grows_past_the_boot_table_and_refuses_a_double_free() {
	// NOTHING here reads the global free count, and that is the point.
	//
	// It used to: the span was freed and `free_count()` compared against a reading taken before,
	// and the double free proved itself by that count not moving. The count belongs to the whole
	// machine - seven other cores are online while a test runs - so any frame freed anywhere in the
	// window shifted it, and this test failed twice on aarch64 with the allocator working perfectly,
	// once by one frame and once by four. An intermittently red suite is how a real failure gets
	// waved through, so the test asserts the properties instead of a number it does not own.
	let refused_before = super::refused_frees();
	let base = allocate_contiguous(FRAGMENTED_PAGES as usize).expect("a fragmented frame span");
	// Freed in two interleaved passes, so the run table has to coalesce the odd pages into the gaps
	// the even ones left rather than take one tidy span back.
	unsafe {
		for index in (0..FRAGMENTED_PAGES).step_by(2) {
			deallocate(base + index * PAGE_SIZE);
		}
		for index in (1..FRAGMENTED_PAGES).step_by(2) {
			deallocate(base + index * PAGE_SIZE);
		}
	}
	// Every page came back AND the run table put them together again: a contiguous span of the same
	// width is only available if both are true, which is what the count was standing in for.
	let again = allocate_contiguous(FRAGMENTED_PAGES as usize).expect("the span re-coalesced whole");
	unsafe { deallocate(again) };

	// A double free of a frame that is still free. The overlap check refuses it, the ownership
	// record refuses it, and the refusal is COUNTED - so the test can assert the thing itself
	// rather than a side effect of it on a number the rest of the machine is also writing to.
	let refused = super::refused_frees();
	unsafe { deallocate(again) };
	assert_eq!(super::refused_frees(), refused + 1, "a double free is refused, not absorbed");
	assert!(super::refused_frees() > refused_before, "and the refusal happened during this test");

	// and the pool is still sound afterwards: the span allocates whole one more time, which it
	// could not if the refused free had corrupted the run table.
	let third = allocate_contiguous(FRAGMENTED_PAGES as usize).expect("the pool survives a refused free");
	unsafe {
		for index in 0..FRAGMENTED_PAGES {
			deallocate(third + index * PAGE_SIZE);
		}
	}
}

crate::tagged_test!(contiguous_frame_runs_recoalesce, [Frame, Memory], covers = ["kernel"]);
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

crate::tagged_test!(the_allocator_refuses_a_frame_it_never_handed_out, [Frame, Memory], covers = ["kernel"]);
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
		// The refusal COUNTER, not the global free count, for the reason the double-free test
		// records: the count belongs to the whole machine and seven other cores are online.
		let refused = super::refused_frees();
		// Far above any plausible pool, and page-aligned so it is refused for not being
		// ours rather than for being misaligned.
		// SAFETY: violating `deallocate`'s contract is the point, and it is safe to do
		// here precisely because the record refuses the call before the pool is touched -
		// which is the assertion below.
		unsafe { deallocate(0x0000_7000_0000_0000) };
		assert_eq!(super::refused_frees(), refused + 1, "an address outside the pool must not become free memory");

		// Not page-aligned, and inside the pool: a frame offset by a few bytes is a
		// plausible slip and would insert a run that overlaps a real frame.
		let frame = allocate().expect("one frame");
		// SAFETY: as above - the misalignment is refused before anything is inserted.
		unsafe { deallocate(frame + 8) };
		assert_eq!(super::refused_frees(), refused + 2, "a misaligned address must not become free memory");
		// and the frame itself still frees normally - no refusal, and it can be had again.
		unsafe { deallocate(frame) };
		assert_eq!(super::refused_frees(), refused + 2, "an honest free is not refused");
		let again = allocate().expect("the frame is back in the pool");
		unsafe { deallocate(again) };
	}
}

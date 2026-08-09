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

crate::tagged_test!(a_fragmenting_workload_loses_no_pages_and_says_so, [Frame, Memory], covers = ["kernel"]);
fn a_fragmenting_workload_loses_no_pages_and_says_so() {
	// The run table is bounded on purpose - it was bounded to fix a deadlock, and `insert_at`
	// refuses rather than allocating from the heap it feeds. So a free that does not fit is
	// DROPPED: the frames are gone, nothing references them, and the only trace was a warning
	// line. Under fragmentation the machine gets slowly smaller and nothing adds it up, so the
	// symptom arrives weeks later as an allocation failure with no cause attached.
	//
	// `lost_pages` is that count, and it is compiled in rather than `#[cfg(test)]` - a number
	// nobody can read in production is not a measurement. What is asserted here is the DELTA
	// across this test's own work, never the absolute value: the counter belongs to the whole
	// machine and seven other cores are running while this does. M0147 retired four tests that
	// asserted numbers belonging to the machine rather than to themselves.
	//
	// This is the guard rather than the demonstration. Driving the table to its ceiling would mean
	// deliberately losing real pages out of a live kernel, which is a poor trade for a test; what
	// this catches is a change that starts dropping frees on an ORDINARY workload, which is how
	// the quarantine defect arrived - nine pages per failed load, noticed only because something
	// happened to print a count.
	let lost_before = super::lost_pages();
	let refused_before = super::refused_frees();

	// Shred the address space: allocate a wide span, free it in two interleaved passes so every
	// other page has to become its own run, then do it again on top of the holes.
	for _ in 0..3 {
		let base = allocate_contiguous(FRAGMENTED_PAGES as usize).expect("a fragmented frame span");
		unsafe {
			for index in (0..FRAGMENTED_PAGES).step_by(2) {
				deallocate(base + index * PAGE_SIZE);
			}
			for index in (1..FRAGMENTED_PAGES).step_by(2) {
				deallocate(base + index * PAGE_SIZE);
			}
		}
	}

	assert_eq!(super::lost_pages(), lost_before, "an ordinary fragmenting workload must not lose a single page");
	assert_eq!(super::refused_frees(), refused_before, "and it frees nothing this allocator did not hand out");

	// and the pool put it all back together, which is the other half of losing nothing.
	let whole = allocate_contiguous(FRAGMENTED_PAGES as usize).expect("the span re-coalesces after the churn");
	unsafe {
		for index in 0..FRAGMENTED_PAGES {
			deallocate(whole + index * PAGE_SIZE);
		}
	}
	assert_eq!(super::lost_pages(), lost_before, "including the pass that gave it all back one page at a time");
}

crate::tagged_test!(a_dma_buffer_still_gets_a_contiguous_span_after_the_pool_is_shredded, [Frame, Memory, Dma], covers = ["kernel"]);
fn a_dma_buffer_still_gets_a_contiguous_span_after_the_pool_is_shredded() {
	// Contiguous allocation exists FOR DMA - virtqueue rings, block data stages, jumbo frames - and
	// the run table serves it by first-fitting a whole run. So the question that matters is not
	// whether `allocate_contiguous` works on a fresh pool, which every boot already proves, but
	// whether it still works once the pool has been cut to pieces and put back together.
	//
	// Through a REAL caller. `DmaBuffer::create_in` is what asks for these spans in production, and
	// it does more than call the allocator: it charges a Domain, maps the pages and refuses cleanly
	// when it cannot. A test that called `allocate_contiguous` directly would pass over a caller
	// that had stopped being able to use the answer.
	use crate::object::dma_buffer::DmaBuffer;

	let lost_before = super::lost_pages();

	// Shred: take a wide span, hand back every other page, then hand back the rest. The run table
	// has to coalesce the second pass into the holes the first left.
	let base = allocate_contiguous(FRAGMENTED_PAGES as usize).expect("a span to shred");
	unsafe {
		for index in (0..FRAGMENTED_PAGES).step_by(2) {
			deallocate(base + index * PAGE_SIZE);
		}
	}
	// A DMA buffer while the pool is at its most broken: every other page of that span is free and
	// the odd ones are still out. Small enough to fit a surviving run, which is the point - a
	// caller that needs four contiguous pages must not be refused because the pool is untidy.
	let squeezed = DmaBuffer::create_in(&crate::sched::root_domain(), 4 * PAGE_SIZE as usize);
	assert!(squeezed.is_ok(), "a small DMA buffer must be servable from a fragmented pool");
	drop(squeezed);

	unsafe {
		for index in (1..FRAGMENTED_PAGES).step_by(2) {
			deallocate(base + index * PAGE_SIZE);
		}
	}

	// And once it is whole again, a buffer spanning the width that was just returned. This is the
	// coalescing assertion: the pages came back as `FRAGMENTED_PAGES` separate frees and only a
	// table that re-formed them into one run can answer this.
	let whole = DmaBuffer::create_in(&crate::sched::root_domain(), (FRAGMENTED_PAGES * PAGE_SIZE) as usize);
	assert!(whole.is_ok(), "the shredded span re-coalesced, so a DMA buffer of its full width is servable");
	drop(whole);

	assert_eq!(super::lost_pages(), lost_before, "and none of that churn lost a page");
}

crate::tagged_test!(the_run_table_is_reserved_for_the_worst_the_pool_can_reach, [Frame, Memory], covers = ["kernel"]);
fn the_run_table_is_reserved_for_the_worst_the_pool_can_reach() {
	// The invariant that makes a free unable to fail, asserted rather than argued.
	//
	// The run table used to hold 8192 runs - "sized well past what a healthy pool fragments into",
	// which is a claim about healthy pools and not about what is possible. Past it, `insert_at`
	// refused and the freed span was LOST. The bound is now computed from the pool, and the whole
	// property rests on that computation being the real worst case.
	//
	// It is: two free runs must have an allocated page between them, so `pages / 2` rounded up is
	// the most disjoint runs a pool of `pages` frames can be split into. There is no arrangement
	// with more. This pins that arithmetic, because the day it drifts nothing else notices - the
	// table simply starts refusing again, on a fragmented machine, weeks later.
	let (total, _free) = super::totals();
	assert!(total > 0, "the pool has frames");

	// THE BUDDY IS THE ONE SERVING. Everything below is about the fallback, and if the buddy failed
	// to build, the fallback is what the machine is running - so this is the first thing to say.
	assert!(super::on_buddy(), "the buddy allocator is serving allocations, not the run table");

	// Its metadata is two bits per page - one per block per order, summed over orders - and it is
	// reserved once. A tenth of a percent of the pool, and the number a future change would have to
	// justify moving.
	let metadata = super::buddy_metadata_bytes();
	assert!(metadata > 0, "the buddy has metadata");
	assert!(metadata <= total, "the buddy's metadata is {metadata} bytes for {total} pages - it should be about a quarter of a byte per page, not a byte");

	// And the fallback, if the buddy ever cannot be built: the run table must still be sized for
	// the worst the pool can reach, because a table sized for a healthy pool is where the losing
	// started. `worst_case_runs` is the arithmetic and nothing else pins it.
	assert!(super::worst_case_runs(total) >= total.div_ceil(2), "the fallback reservation must cover every-other-page-free, which is {} runs", total.div_ceil(2));
	assert!(super::on_heap(), "the fallback table is the heap-backed one, not the bounded seed table");
}

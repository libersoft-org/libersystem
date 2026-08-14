use super::{PAGE_SIZE, allocate, allocate_contiguous, deallocate};

const FRAGMENTED_PAGES: u64 = (super::SEED_RUNS as u64 + 1) * 2;

// Every `deallocate` below frees a frame this test allocated itself and holds alone, which is
// the contract; the two calls that deliberately break it are in
// `the_allocator_refuses_a_frame_it_never_handed_out`, which says so where it does it.

crate::tagged_test!(frame_alloc_distinct, [Frame, Memory, Smoke], id = "kernel.mem.frame.frame_alloc_distinct", covers = ["kernel"]);
fn frame_alloc_distinct() {
	let first = allocate().expect("first frame");
	let second = allocate().expect("second frame");
	assert_ne!(first, second);
	unsafe {
		deallocate(first);
		deallocate(second);
	}
}

crate::tagged_test!(the_frame_pool_grows_past_the_boot_table_and_refuses_a_double_free, [Frame, Memory], id = "kernel.mem.frame.the_frame_pool_grows_past_the_boot_table_and_refuses_a_double_free", covers = ["kernel"]);
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

crate::tagged_test!(contiguous_frame_runs_recoalesce, [Frame, Memory], id = "kernel.mem.frame.contiguous_frame_runs_recoalesce", covers = ["kernel"]);
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

crate::tagged_test!(the_allocator_refuses_a_frame_it_never_handed_out, [Frame, Memory], id = "kernel.mem.frame.the_allocator_refuses_a_frame_it_never_handed_out", covers = ["kernel"]);
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

crate::tagged_test!(a_fragmenting_workload_loses_no_pages_and_says_so, [Frame, Memory], id = "kernel.mem.frame.a_fragmenting_workload_loses_no_pages_and_says_so", covers = ["kernel"]);
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
	// machine and seven other cores are running while this does. P02M0117 retired four tests that
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

crate::tagged_test!(a_dma_buffer_still_gets_a_contiguous_span_after_the_pool_is_shredded, [Frame, Memory, Dma], id = "kernel.mem.frame.a_dma_buffer_still_gets_a_contiguous_span_after_the_pool_is_shredded", covers = ["kernel"]);
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

crate::tagged_test!(the_run_table_is_reserved_for_the_worst_the_pool_can_reach, [Frame, Memory], id = "kernel.mem.frame.the_run_table_is_reserved_for_the_worst_the_pool_can_reach", covers = ["kernel"]);
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

	// The fallback is NOT allocated on the path that works, and that is worth an assertion because
	// it was not always true. The bootstrap used to reserve the worst-case table FIRST and build the
	// buddy after it, so a healthy machine paid for both: `worst_case_runs(total) * 16` bytes -
	// about 8 MiB on 4 GiB - held for the life of the boot behind a boot line reporting 383 KiB.
	// Emptying it later would not have helped; a cleared `Vec` keeps its capacity.
	//
	// Reversing the order also fixed the case that mattered more: the machine too short of memory
	// to reserve 8 MiB is not too short to reserve 383 KiB, and it was the one machine that never
	// got a buddy at all.
	assert!(!super::on_heap(), "a run table was reserved even though the buddy was built - {} bytes of fallback nobody is going to use", super::run_capacity() * 16);
}

crate::tagged_test!(no_frame_is_ever_handed_to_two_owners_at_once, [Frame, Memory], id = "kernel.mem.frame.no_frame_is_ever_handed_to_two_owners_at_once", covers = ["kernel"]);
fn no_frame_is_ever_handed_to_two_owners_at_once() {
	// The one thing a bitmap allocator can do that the run table could not.
	//
	// A frame lived in exactly one run, and leaving the run removed it, so the old allocator could
	// not hand the same page to two owners even if its arithmetic was wrong. A bitmap can: one
	// mis-set bit and two callers get the same address, both write it, and whoever reads second
	// finds the other's bytes. The failure surfaces arbitrarily far away - a corrupt ELF header, a
	// filesystem that fails its own checksum, a hang - and none of those point back at the
	// allocator. So `take_one` and `take_contiguous` ask the ownership record before handing a
	// frame out, and this reads the answer for the whole boot rather than for a workload of its
	// own: every allocation every test has made so far is what it covers.
	//
	// Debug builds only, which is where the test suite runs; the record does not exist in release.
	assert_eq!(super::double_allocations(), 0, "a frame was handed out while already on loan - see the DOUBLE ALLOCATION line in the boot log for the address");

	// And a workload of its own, because the boot may simply never have hit the case: churn the
	// pool hard enough that the buddy is splitting and merging, then check again.
	let mut held: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
	for round in 0..64 {
		for _ in 0..32 {
			if let Some(frame) = super::allocate() {
				held.push(frame);
			}
		}
		// Free every other one, so the pool fragments rather than unwinding cleanly.
		let mut keep = alloc::vec::Vec::new();
		for (index, frame) in held.drain(..).enumerate() {
			if index % 2 == round % 2 {
				unsafe { super::deallocate(frame) };
			} else {
				keep.push(frame);
			}
		}
		held = keep;
	}
	// Every address handed out in that churn must be distinct - the record's answer, checked a
	// second way, because a record that was never updated would also report zero.
	let mut sorted = held.clone();
	sorted.sort_unstable();
	let before = sorted.len();
	sorted.dedup();
	assert_eq!(sorted.len(), before, "the same frame was handed out twice and is being held twice");
	for frame in held {
		unsafe { super::deallocate(frame) };
	}
	assert_eq!(super::double_allocations(), 0, "the churn handed a frame to a second owner");

	// And the detector itself, armed deliberately, because the two assertions above pass on a
	// kernel where `check_not_owned` was deleted. A counter that only ever reads zero proves the
	// workload was clean OR that nothing is counting, and those are not the same claim.
	let before = super::double_allocations();
	let retired_before = super::retired_pages();
	super::duplicate_next_allocation();
	let first = super::allocate().expect("a frame for the injected duplicate");
	// THE REFUSAL, not the survival of the duplicate.
	//
	// The detector used to report the double allocation and then hand the frame over anyway, which
	// guarantees the run after the sighting is unreadable: the ownership record is wrong for that
	// page in a second way, the audit after the next test fires and names an innocent test, and the
	// log is a cascade with the real event at the top of it.
	//
	// So the allocation FAILS - which every caller already handles, because an allocator can be
	// empty - and the page is retired rather than returned to the buddy, because a page the two
	// views disagree about is exactly a page nothing should hand out again.
	let second = super::allocate();
	assert!(second.is_none() || second != Some(first), "the frame the detector refused was handed out anyway");
	assert_eq!(super::double_allocations(), before + 1, "the same frame went out twice and the detector said nothing");
	assert_eq!(super::retired_pages(), retired_before + 1, "the refused frame was not retired, so the buddy may hand it out again");

	// One free, not two: the frame is out on loan once however many times it was handed over, and
	// freeing it twice is a different defect that `check_owned_free` refuses.
	unsafe { super::deallocate(first) };
}

crate::tagged_test!(a_late_acknowledgement_does_not_count_for_the_next_request, [Frame, Memory, Smp], id = "kernel.mem.frame.a_late_acknowledgement_does_not_count_for_the_next_request", covers = ["kernel"]);
fn a_late_acknowledgement_does_not_count_for_the_next_request() {
	// The generation scheme's whole reason for existing, exercised rather than argued.
	//
	// It used to be a flag per core and one counter of acknowledgements, which cannot say WHICH
	// request an acknowledgement belongs to. A requester that times out releases the lock while a
	// slow core is still between its flush and its increment; the next requester zeroes the counter,
	// the stale increment lands in the NEW count, and with three or more cores the rest can reach
	// the target while one core never flushed for this request. The frame then goes back to the
	// allocator with a live translation to it - the physical use-after-free the mechanism exists to
	// prevent, reached by giving up on it.
	//
	// The late acknowledgement is injected rather than raced for: `acknowledge_for_test` publishes
	// exactly what a core coming back from a stale request would publish - an ack for a generation
	// that is already over.
	let me = crate::arch::percpu::this_cpu().cpu_id() as usize;
	let other = if me == 0 { 1 } else { 0 };
	if crate::smp::cpu_count() < 2 {
		return;
	}
	// A NONZERO generation, and one this machine has actually issued: on a quiet kernel the counter
	// can still be 0, and every comparison against 0 is trivially true - which is how the first
	// version of this test passed with the rule deleted.
	crate::mem::tlb::shootdown();
	let stale = crate::mem::tlb::request_generation();
	assert!(stale > 0, "the machine has issued a shootdown, so generations are running");
	// A core answering the request BEFORE the one about to be made.
	crate::mem::tlb::acknowledge_for_test(other, stale);
	// The next request must not be able to count that, whatever the number of cores that answer.
	assert!(!crate::mem::tlb::acknowledged_for_test(other, stale + 1), "an acknowledgement for an older generation must not satisfy a newer request");
	// And an acknowledgement for a LATER generation does satisfy an earlier one, because a flush is
	// whole-buffer: a core that has served a newer request has necessarily flushed for this one.
	crate::mem::tlb::acknowledge_for_test(other, stale + 2);
	assert!(crate::mem::tlb::acknowledged_for_test(other, stale + 1), "a later flush covers an earlier request");
}

crate::tagged_test!(a_shared_page_goes_through_the_quarantine_rather_than_the_allocator, [Frame, Memory], id = "kernel.mem.frame.a_shared_page_goes_through_the_quarantine_rather_than_the_allocator", covers = ["kernel"]);
fn a_shared_page_goes_through_the_quarantine_rather_than_the_allocator() {
	// A shared ELF page was the ONE frame in this system that went straight back to the allocator
	// when it was dropped - `SharedPage::drop` called `deallocate` while every private frame
	// retired. It is reachable through `load_module_into`, which maps into a RUNNING process whose
	// other threads are on other cores, and x86's `unmap_page_in` does a local `invlpg` only: a
	// relocation failure in a module loaded into a live process could free a frame a running thread
	// still reached.
	//
	// What this asserts is the DIFFERENCE that fix made: dropping the last reference must leave the
	// frame in the quarantine (or already drained through a completed shootdown), never back in the
	// allocator without one.
	use crate::elf::shared_page_for_test;
	let before_free = crate::mem::frame::free_count();
	let page = shared_page_for_test();
	let frame = page.frame();
	assert!(frame != 0, "a shared page owns a frame");
	// Drain first, so what the drop puts there is the only thing measured.
	assert!(crate::mem::frame::drain_quarantine_fully(64), "the shootdown completes on a quiet machine");
	let quarantined_before = crate::mem::frame::quarantined();
	drop(page);
	let quarantined_after = crate::mem::frame::quarantined();
	// Either it is sitting in the quarantine, or a drain already ran and gave it back - both are
	// "it went through the shootdown". What must never happen is the count going back up with
	// nothing having been queued.
	// QUEUED, full stop. The first version of this accepted "or the count came back up", which is
	// exactly what `deallocate` does - so it passed with the defect reintroduced and said nothing.
	// One frame cannot reach the drain threshold, so the queue must be one longer than it was.
	assert_eq!(quarantined_after, quarantined_before + 1, "a dropped shared page must go through the quarantine, not straight back to the allocator");
	assert!(crate::mem::frame::drain_quarantine_fully(64), "and the drain completes");
	assert!(crate::mem::frame::free_count() >= before_free, "after the drain the frame is back");
}

crate::tagged_test!(a_spawn_that_passes_a_bootstrap_returns_the_slot_and_the_quota, [Frame, Memory, Process, Handle], id = "kernel.mem.frame.a_spawn_that_passes_a_bootstrap_returns_the_slot_and_the_quota", covers = ["kernel"]);
fn a_spawn_that_passes_a_bootstrap_returns_the_slot_and_the_quota() {
	// `sys_thread_create` took a bootstrap capability with `take_for_transfer`, whose contract is
	// "exactly one of `commit_taken` or `restore_taken` must follow", and the SUCCESS path did
	// neither. `commit_taken` returns the slot to the free list under the generation rules and
	// uncharges one handle, so every successful spawn that passed a bootstrap leaked one slot and
	// one unit of the parent's quota - on the ordinary path of the ordinary syscall. A supervisor
	// that spawns in a loop walks its own quota down until it cannot spawn.
	//
	// A loop, because one leak is a number nobody notices and a hundred is a wall. The domain's
	// accounting is the whole assertion: what goes up must come back down.
	use crate::object::channel::Channel;
	use crate::object::rights::Rights;
	use crate::sched;

	let domain = sched::root_domain();
	let before = domain.account().handles().used();
	for _ in 0..32 {
		let (parent, child) = Channel::create();
		let process = crate::object::process::Process::new(crate::object::address_space::AddressSpace::create().expect("an address space"), domain.clone()).expect("a test process");
		let handle = process.install(child, Rights::ALL, 0);
		assert!(handle != 0, "the bootstrap is installed in the child");
		process.terminate();
		drop(process);
		drop(parent);
		sched::run_until_idle();
	}
	sched::run_until_idle();
	let after = domain.account().handles().used();
	assert!(after <= before, "thirty-two spawns left {} handle(s) charged that nothing owns", after as i64 - before as i64);
}

crate::tagged_test!(a_refused_free_leaves_the_ownership_record_it_found, [Frame, Memory], id = "kernel.mem.frame.a_refused_free_leaves_the_ownership_record_it_found", covers = ["kernel"]);
fn a_refused_free_leaves_the_ownership_record_it_found() {
	// `insert` used to clear the ownership record BEFORE it had established that the free was
	// legal, and both refusal paths - the buddy's "already free" test and the run table's overlap
	// test - returned without putting the bits back. So a refused double free left the record
	// saying "not on loan" about pages that were still out on loan to their real owner.
	//
	// Three things follow, and every one of them is worse than the free that was refused:
	//
	//   - the real owner's eventual free is REFUSED by `check_owned_free`, because the record no
	//     longer says those pages were ever handed out, and the pages leak;
	//   - `frame::audit()` compares the record against the bitmap after every test and fires,
	//     naming whichever test happened to be running rather than the free that did it;
	//   - `check_not_owned` - the double-ALLOCATION detector this milestone exists for - reads
	//     those pages as nobody's, so the one instrument that would explain a double allocation is
	//     blinded for exactly the pages a bad free just touched.
	//
	// THE STATE IS INJECTED, and that is the finding rather than a caveat about the test. The
	// refusal only runs when the record and the bitmap already disagree, which cannot happen while
	// the allocator is consistent - so there is no honest way to reach it except to put the
	// disagreement there. What is asserted is a property of the refusal alone: it leaves the state
	// it found.
	#[cfg(debug_assertions)]
	{
		let frame = allocate().expect("one frame");
		// Freed properly: the record is cleared and the buddy holds the page.
		unsafe { deallocate(frame) };
		// Now say it is on loan again while the buddy still calls it free - the disagreement.
		super::set_owned_bit_for_test(frame, true);
		assert_eq!(super::owned_bit_for_test(frame), Some(true), "the injected state is what the test is about");

		let refused = super::refused_frees();
		// SAFETY: violating `deallocate`'s contract is the point, and it is safe here precisely
		// because the page is already free in the buddy - the refusal below is what stops the
		// second free from reaching the bitmap, which is the assertion.
		unsafe { deallocate(frame) };
		assert_eq!(super::refused_frees(), refused + 1, "a free of a page the pool already calls free is refused");
		assert_eq!(super::owned_bit_for_test(frame), Some(true), "and the refusal leaves the record exactly as it found it");

		// Put the two views back in agreement, so the audit that runs after this test is auditing
		// the allocator rather than the injection.
		super::set_owned_bit_for_test(frame, false);
	}
}

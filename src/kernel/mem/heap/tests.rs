crate::tagged_test!(heap_box_vec, [Memory, Smoke], id = "kernel.mem.heap.heap_box_vec", covers = ["kernel"]);
fn heap_box_vec() {
	let boxed = alloc::boxed::Box::new(42u64);
	assert_eq!(*boxed, 42);
	let mut values = alloc::vec::Vec::new();
	for value in 0u64..1000 {
		values.push(value);
	}
	let sum: u64 = values.iter().sum();
	assert_eq!(sum, 1000 * 999 / 2);
}

crate::tagged_test!(a_growth_that_runs_out_of_frames_gives_back_what_it_took, [Memory], id = "kernel.mem.heap.a_growth_that_runs_out_of_frames_gives_back_what_it_took", covers = ["kernel"]);
fn a_growth_that_runs_out_of_frames_gives_back_what_it_took() {
	use super::{HEAP_REGION, NEXT_REGION, stats, unwind};
	use crate::arch::paging;
	use crate::mem::frame;
	use crate::mem::frame::PAGE_SIZE;
	use core::sync::atomic::Ordering;

	// Running out of memory must not COST memory.
	//
	// `grow` claims a virtual range with one bump and then maps it page by page. When the pool
	// emptied partway it returned failure on the spot: the range was claimed and handed to
	// nobody, and the frames already mapped into it were unreachable. Both were lost for the life
	// of the boot, and every later growth started past the hole. The existing OOM test
	// (`map_degrades_to_error_when_out_of_frames`) drains the pool too, but asserts only that the
	// operation fails - which is why a leak on the failure path stayed invisible.
	//
	// This drives the rollback over the REAL claim: the same `NEXT_REGION` bump `grow` makes, at
	// the address `grow` would have used, unwound the way `grow` unwinds it. What it does not do
	// is reach that state the way production does, by exhausting the frame pool.
	//
	// That was the first attempt, and it has to be written down because it looked right and was
	// not. Draining the pool does prove the bug - with the fix reverted the test reported losing
	// all sixteen frames it had lent, "0 of 16" - but the pool is shared with three other running
	// cores, and while it is empty their allocations fail too. The suite then failed in
	// `dma_buffer_maps_and_reports_phys` with an address space diverging from the kernel mapping
	// at PML4[464]: a mapping that could not get its intermediate table while the pool was empty,
	// appearing later than the address space that should have inherited it. A test that
	// destabilises unrelated subsystems to reach its own precondition is not measuring what it
	// claims. The exhaustion path itself is worth its own item; see M0107.
	const PAGES: usize = 8;
	let bytes: u64 = HEAP_REGION;

	// Two identical rollbacks, and the SECOND is what proves the point.
	//
	// A single one cannot be measured against the pool it started with: mapping into a virtual
	// range that has never been mapped makes `map_page` allocate the intermediate page tables for
	// it, and `unmap_page` deliberately leaves those in place. That is not a leak - the rollback
	// restores the region base, so the next growth maps the same addresses and finds its tables
	// already there - but it does mean one attempt legitimately costs a frame or two it keeps.
	//
	// What must cost nothing is doing it AGAIN.
	let (total_before, _) = stats();
	let frames_before = frame::free_count();
	partial_growth(PAGES, bytes);
	let frames_between = frame::free_count();
	partial_growth(PAGES, bytes);
	let frames_after = frame::free_count();
	let (total_after, _) = stats();

	assert_eq!(frames_after, frames_between, "a repeated rollback costs nothing: the first one gave its frames back");
	// Well under PAGES, or a rollback that gave back NOTHING would satisfy it.
	assert!(frames_between + 4 >= frames_before, "a rollback kept only its page tables, not the frames it mapped: {frames_between} of {frames_before}");
	assert_eq!(total_after, total_before, "a rollback released the virtual range it claimed");

	// Claim, map part of it, and unwind - `grow`'s failure path with the frame exhaustion that
	// would have caused it replaced by simply stopping early.
	fn partial_growth(pages: usize, bytes: u64) {
		let base: u64 = NEXT_REGION.fetch_add(bytes, Ordering::Relaxed);
		let mut virt: u64 = base;
		for _ in 0..pages {
			let Some(phys) = frame::allocate() else { break };
			paging::map_page(virt, phys, paging::WRITABLE | paging::NO_EXECUTE);
			virt += PAGE_SIZE;
		}
		unwind(base, virt, base + bytes);
	}
}

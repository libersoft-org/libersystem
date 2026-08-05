crate::tagged_test!(heap_box_vec, [Memory, Smoke]);
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

crate::tagged_test!(a_growth_that_runs_out_of_frames_gives_back_what_it_took, [Memory]);
fn a_growth_that_runs_out_of_frames_gives_back_what_it_took() {
	use crate::mem::frame;
	use core::alloc::Layout;

	// Running out of memory must not COST memory.
	//
	// `grow` claims a virtual range with one bump and then maps it page by page. When the pool
	// emptied partway it returned failure on the spot: the range was claimed and handed to
	// nobody, and the frames already mapped into it were unreachable. Both were lost for the
	// life of the boot, and every later growth started past the hole. The existing OOM test
	// (`map_degrades_to_error_when_out_of_frames`) drains the pool too, but asserts only that
	// the operation fails - which is why a leak on the failure path stayed invisible.
	//
	// The shape here: leave the pool with FEWER frames than a growth region needs, so the map
	// gets partway and then cannot finish. A drained pool would fail on the first frame and
	// leak nothing but the virtual range.
	const SPARE: usize = 8; // a growth region is 512 pages, so this cannot complete one

	// Reserve the holding vector before draining. Growing it inside the drained window would
	// itself call the path under test.
	let mut held: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
	held.reserve(frame::free_count() + 16);
	while let Some(frame) = frame::allocate() {
		held.push(frame);
	}
	for _ in 0..SPARE {
		match held.pop() {
			Some(frame) => frame::deallocate(frame),
			None => break,
		}
	}

	// Bigger than any free block the heap can be holding, so the allocation has to grow.
	let layout = Layout::from_size_align(HEAP_REGION as usize * 2, 16).expect("a valid layout");
	let (total_before, _) = stats();
	let frames_before = frame::free_count();
	let pointer = try_alloc(layout);
	let frames_after = frame::free_count();
	let (total_after, _) = stats();

	// Refill before asserting, so a failed assertion does not leave the pool empty for every
	// test that runs after this one.
	for frame in held {
		frame::deallocate(frame);
	}

	assert!(pointer.is_null(), "an allocation larger than the pool can back must fail, not succeed");
	assert_eq!(frames_after, frames_before, "a failed growth returned every frame it had mapped");
	assert_eq!(total_after, total_before, "a failed growth released the virtual range it claimed");

	// The heap still works: the frames really went back to the pool rather than merely being
	// counted as returned.
	let modest = Layout::from_size_align(4096, 16).expect("a valid layout");
	let pointer = try_alloc(modest);
	assert!(!pointer.is_null(), "the heap serves an ordinary allocation after a failed growth");
	dealloc(pointer, modest);
}

use super::{PAGE_SIZE, allocate, allocate_contiguous, deallocate, free_count};

const FRAGMENTED_PAGES: u64 = (super::SEED_RUNS as u64 + 1) * 2;

crate::tagged_test!(frame_alloc_distinct, [Frame, Memory, Smoke]);
fn frame_alloc_distinct() {
	let first = allocate().expect("first frame");
	let second = allocate().expect("second frame");
	assert_ne!(first, second);
	deallocate(first);
	deallocate(second);
}

crate::tagged_test!(the_frame_pool_grows_past_the_boot_table_and_refuses_a_double_free, [Frame, Memory]);
fn the_frame_pool_grows_past_the_boot_table_and_refuses_a_double_free() {
	let before = free_count();
	let base = allocate_contiguous(FRAGMENTED_PAGES as usize).expect("a fragmented frame span");
	for index in (0..FRAGMENTED_PAGES).step_by(2) {
		deallocate(base + index * PAGE_SIZE);
	}
	for index in (1..FRAGMENTED_PAGES).step_by(2) {
		deallocate(base + index * PAGE_SIZE);
	}
	assert_eq!(free_count(), before, "every fragmented page returned to the pool");
	let again = allocate_contiguous(FRAGMENTED_PAGES as usize).expect("the span re-coalesced whole");
	deallocate(again);
	let after_free = free_count();
	deallocate(again);
	assert_eq!(free_count(), after_free, "a double free adds nothing to the pool");
	for index in 1..FRAGMENTED_PAGES {
		deallocate(again + index * PAGE_SIZE);
	}
	assert_eq!(free_count(), before, "the pool round-trips exactly");
}

crate::tagged_test!(contiguous_frame_runs_recoalesce, [Frame, Memory]);
fn contiguous_frame_runs_recoalesce() {
	let base = allocate_contiguous(64).expect("a 256 kB span");
	for index in 0..64u64 {
		deallocate(base + index * PAGE_SIZE);
	}
	let again = allocate_contiguous(128).expect("a 512 kB span after coalescing");
	for index in 0..128u64 {
		deallocate(again + index * PAGE_SIZE);
	}
}

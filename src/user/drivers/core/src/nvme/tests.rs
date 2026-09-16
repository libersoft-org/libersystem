// EACH OF THESE WATCHES ONE WAY AN NVMe DRIVER IS WRONG, and every one of them is a mistake with a
// plausible-looking driver on the other side of it: a queue one entry too long, a doorbell rung at
// the wrong stride, a completion believed because its status code was zero, a transfer one page
// short because the buffer was not page aligned, and a controller that transfers nothing forever
// because "no limit" was read as "a limit of zero".

use super::*;

// A CAP value with the fields this tree's controllers actually report: MQES 0x003F (64 entries),
// DSTRD 0 (four-byte stride), TO 0x1E (fifteen seconds), NVM command set, MPSMIN 0 (4 kB).
fn cap(mqes: u64, dstrd: u64, to: u64, nvm: bool, mpsmin: u64) -> u64 {
	mqes | (dstrd << 32) | (to << 24) | ((nvm as u64) << 37) | (mpsmin << 48)
}

const PAGE: u64 = 4096;

#[test]
fn the_queue_size_is_one_more_than_the_register_says() {
	// MQES is zero-based. A driver that used the raw field as a size would make every queue one
	// entry too long, and the entry past the end is where the controller writes.
	let caps = Capabilities::decode(cap(0x003F, 0, 0x1E, true, 0), PAGE).expect("usable");
	assert_eq!(caps.max_entries, 64);
	assert_eq!(caps.timeout_500ms, 0x1E);
	assert_eq!(caps.min_page, 4096);
}

#[test]
fn a_controller_with_no_nvm_command_set_is_refused_rather_than_driven() {
	// Every command this driver sends belongs to that set, so there is nothing to send.
	assert_eq!(Capabilities::decode(cap(0x003F, 0, 0x1E, false, 0), PAGE), Err(Unusable::NoNvmCommandSet));
}

#[test]
fn a_queue_of_no_entries_and_a_page_larger_than_the_hosts_are_both_refused() {
	assert_eq!(Capabilities::decode(cap(0, 0, 0x1E, true, 0), PAGE), Err(Unusable::NoQueueEntries));
	// MPSMIN 1 is 8 kB, which a 4 kB host cannot describe in PRP pages.
	assert_eq!(Capabilities::decode(cap(0x003F, 0, 0x1E, true, 1), PAGE), Err(Unusable::PageTooSmall));
	// And the same controller IS usable on a host whose page is large enough.
	assert!(Capabilities::decode(cap(0x003F, 0, 0x1E, true, 1), 8192).is_ok());
}

#[test]
fn the_doorbell_stride_is_read_and_not_assumed() {
	// Every controller this tree has met reports a stride of four, so a hard-coded `qid * 8` works
	// everywhere it is tested and rings the wrong queue on anything else.
	let four = Capabilities::decode(cap(0x003F, 0, 0x1E, true, 0), PAGE).expect("usable");
	assert_eq!(four.doorbell(0, false), 0x1000, "admin submission");
	assert_eq!(four.doorbell(0, true), 0x1004, "admin completion");
	assert_eq!(four.doorbell(1, false), 0x1008, "I/O queue 1 submission");
	assert_eq!(four.doorbell(1, true), 0x100C);

	let wide = Capabilities::decode(cap(0x003F, 3, 0x1E, true, 0), PAGE).expect("usable");
	assert_eq!(wide.doorbell_stride, 32);
	assert_eq!(wide.doorbell(1, false), 0x1000 + 2 * 32, "a wider stride moves every queue");
}

#[test]
fn a_queue_never_exceeds_the_controllers_maximum_and_is_never_empty() {
	let caps = Capabilities::decode(cap(0x003F, 0, 0x1E, true, 0), PAGE).expect("usable");
	assert_eq!(caps.queue_entries(16), 16);
	assert_eq!(caps.queue_entries(4096), 64, "clamped to what the controller allows");
	assert_eq!(caps.queue_entries(0), 1, "and never zero");
}

// A completion entry with the fields at the offsets the specification fixes.
fn completion(command_id: u16, phase: bool, status_type: u8, status_code: u8, sq_head: u16) -> [u8; CQ_ENTRY_LEN] {
	let mut entry = [0u8; CQ_ENTRY_LEN];
	entry[8..12].copy_from_slice(&(sq_head as u32).to_le_bytes());
	let dw3 = (command_id as u32) | ((phase as u32) << 16) | ((status_code as u32) << 17) | ((status_type as u32) << 25);
	entry[12..16].copy_from_slice(&dw3.to_le_bytes());
	entry
}

#[test]
fn a_nonzero_status_type_is_a_failure_even_when_the_code_is_zero() {
	// Reading only the code would call this success. The two fields are one status.
	let entry = completion(7, true, 1, 0, 3);
	assert!(!Completion::decode(&entry).succeeded());
	assert!(Completion::decode(&completion(7, true, 0, 0, 3)).succeeded());
	assert!(!Completion::decode(&completion(7, true, 0, 0x0B, 3)).succeeded());
}

#[test]
fn the_phase_bit_is_what_tells_a_new_completion_from_last_time_around() {
	// The ring memory is never cleared, so the entry sitting in a slot is the previous pass's until
	// the controller flips the phase. Nothing else distinguishes them.
	let stale = completion(4, false, 0, 0, 1);
	assert_eq!(reap(&stale, true, 4), Reaped::Empty, "the old entry is not a completion");
	let fresh = completion(4, true, 0, 0, 1);
	assert_eq!(reap(&fresh, true, 4), Reaped::Mine { succeeded: true, sq_head: 1 });
}

#[test]
fn a_completion_for_a_command_nobody_is_waiting_for_is_its_own_answer() {
	// Not an error on the request: it means the controller and the driver disagree about what is
	// outstanding, which is a reason to stop using the queue rather than to fail one read.
	let other = completion(99, true, 0, 0, 2);
	assert_eq!(reap(&other, true, 4), Reaped::Unexpected { command_id: 99 });
}

#[test]
fn a_failed_completion_for_the_right_command_is_still_that_commands_answer() {
	let failed = completion(4, true, 0, 0x0B, 6);
	assert_eq!(reap(&failed, true, 4), Reaped::Mine { succeeded: false, sq_head: 6 });
}

#[test]
fn an_aligned_transfer_within_one_page_is_one_address() {
	assert_eq!(prp(0x1000, 4096, PAGE, 1 << 20), Ok(Prp::Single { prp1: 0x1000 }));
	assert_eq!(prp(0x1000, 512, PAGE, 1 << 20), Ok(Prp::Single { prp1: 0x1000 }));
}

#[test]
fn an_unaligned_transfer_needs_a_page_more_than_its_length_suggests() {
	// THE MISTAKE THIS CATCHES: `len / page` says one page for 4096 bytes, and a buffer starting
	// half way into a page spans two. The missing page is the tail of the caller's data.
	assert_eq!(prp(0x1800, 4096, PAGE, 1 << 20), Ok(Prp::Pair { prp1: 0x1800, prp2: 0x2000 }));
	// The boundary case in both directions.
	assert_eq!(prp(0x1800, 2048, PAGE, 1 << 20), Ok(Prp::Single { prp1: 0x1800 }), "exactly to the page end");
	assert_eq!(prp(0x1800, 2049, PAGE, 1 << 20), Ok(Prp::Pair { prp1: 0x1800, prp2: 0x2000 }), "one byte past it");
}

#[test]
fn three_pages_or_more_need_a_list_and_the_list_names_the_pages_after_the_first() {
	assert_eq!(prp(0x1000, 3 * PAGE, PAGE, 1 << 20), Ok(Prp::List { prp1: 0x1000, entries: 2 }));
	assert_eq!(list_entry(0x1000, PAGE, 0), 0x2000);
	assert_eq!(list_entry(0x1000, PAGE, 1), 0x3000);
	// Unaligned: the list starts at the next page boundary, not at base + page.
	assert_eq!(prp(0x1800, 3 * PAGE, PAGE, 1 << 20), Ok(Prp::List { prp1: 0x1800, entries: 3 }));
	assert_eq!(list_entry(0x1800, PAGE, 0), 0x2000);
	assert_eq!(list_entry(0x1800, PAGE, 1), 0x3000);
}

#[test]
fn a_transfer_that_is_empty_too_large_or_past_one_list_page_is_refused() {
	assert_eq!(prp(0x1000, 0, PAGE, 1 << 20), Err(Untransferable::Empty));
	assert_eq!(prp(0x1000, 4097, PAGE, 4096), Err(Untransferable::TooLarge));
	// One 4 kB list page names 512 addresses, so 513 pages after the first is one too many.
	let capacity = (PAGE / 8) as u64;
	assert!(prp(0x1000, (capacity + 1) * PAGE, PAGE, 1 << 30).is_ok(), "exactly a full list");
	assert_eq!(prp(0x1000, (capacity + 2) * PAGE, PAGE, 1 << 30), Err(Untransferable::TooManyPages));
}

#[test]
fn a_controller_declaring_no_transfer_limit_is_not_a_controller_with_a_limit_of_zero() {
	// MDTS zero means unbounded. A driver reading it as a size transfers nothing, forever, against
	// QEMU's own model.
	assert_eq!(max_transfer(0, 4096, 1 << 20), 1 << 20);
	// Otherwise it is `min_page << mdts`, and the driver's own bound still wins when it is smaller.
	assert_eq!(max_transfer(5, 4096, 1 << 20), 4096 << 5);
	assert_eq!(max_transfer(20, 4096, 1 << 20), 1 << 20);
	// And a shift that would overflow falls back to the driver's bound rather than wrapping.
	assert_eq!(max_transfer(255, 4096, 1 << 20), 1 << 20);
}

#[test]
fn a_namespace_with_metadata_or_protection_is_skipped_and_not_half_served() {
	// Reading a metadata namespace without carrying metadata returns SHORT DATA WITH A SUCCESS
	// STATUS, which is the worst of the three outcomes.
	let with_metadata = namespace(1024, 0, (9 << 16) | 8, 0);
	assert_eq!(with_metadata, Err(Unservable::Metadata));
	assert_eq!(namespace(1024, 0, 9 << 16, 1), Err(Unservable::Protection));
	assert_eq!(namespace(0, 0, 9 << 16, 0), Err(Unservable::Empty));
}

#[test]
fn a_block_size_the_contract_cannot_carry_is_refused() {
	assert_eq!(namespace(1024, 0, 8 << 16, 0), Err(Unservable::BlockSize), "256 bytes is below a sector");
	assert_eq!(namespace(1024, 0, 0, 0), Err(Unservable::BlockSize), "a shift of zero is not a size");
	assert_eq!(namespace(1024, 0, 40 << 16, 0), Err(Unservable::BlockSize), "nor is one past a u32");
}

#[test]
fn an_ordinary_namespace_reports_its_blocks_and_its_block_size() {
	assert_eq!(namespace(2048, 0, 9 << 16, 0), Ok(Namespace { blocks: 2048, block_bytes: 512 }));
	assert_eq!(namespace(2048, 0, 12 << 16, 0), Ok(Namespace { blocks: 2048, block_bytes: 4096 }));
}

#[test]
fn a_format_index_in_the_upper_half_is_refused_rather_than_read_from_the_wrong_entry() {
	assert_eq!(lba_format_index(0), Some(0));
	assert_eq!(lba_format_index(3), Some(3));
	assert_eq!(lba_format_index(0x1F), None, "bit 4 selects formats past the sixteen read here");
}

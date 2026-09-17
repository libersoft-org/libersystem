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

#[test]
fn a_phase_bit_that_arrived_before_its_entry_is_not_a_completion() {
	// THE WORST DEFECT THIS DRIVER HAD, and it lived in the driver where no host test could see it.
	// A sixteen-byte write from a device is not atomic to its reader: the bit that says "this entry
	// is yours" becomes visible while the id, the status and the queue head are still zero. Measured
	// on a real controller, the entry read as `sq 0 head 0` with five commands outstanding - a
	// completion that had answered nothing.
	let mut torn = [0u8; CQ_ENTRY_LEN];
	torn[14] = 1; // bit 16 of dword 3: the phase, and nothing else
	assert_eq!(reap(&torn, true, 5), Reaped::Arriving);
}

#[test]
fn arriving_and_unexpected_are_told_apart_because_they_want_opposite_answers() {
	// One says keep waiting; the other says this driver and the controller disagree about what is
	// outstanding, which is a reason to stop using the queue. Collapsing them made the bring-up fail
	// roughly one boot in three, and "drain it and carry on" - the fix that treats both as stale -
	// made it worse, because there was no right answer coming.
	let torn = completion(NEVER_ISSUED, true, 0, 0, 0);
	let other = completion(99, true, 0, 0, 2);
	assert_eq!(reap(&torn, true, 5), Reaped::Arriving);
	assert_eq!(reap(&other, true, 5), Reaped::Unexpected { command_id: 99 });
	assert_ne!(reap(&torn, true, 5), reap(&other, true, 5));
}

#[test]
fn a_half_arrived_entry_of_the_wrong_phase_is_still_simply_empty() {
	// The phase is asked FIRST, so an entry from the previous pass around the ring is empty whatever
	// else it holds - including an id of zero, which must not be reported as an arrival in a slot
	// the controller has not touched this time round.
	let mut stale = [0u8; CQ_ENTRY_LEN];
	assert_eq!(reap(&stale, true, 5), Reaped::Empty, "zeroed memory on the first pass");
	stale[14] = 1;
	assert_eq!(reap(&stale, false, 5), Reaped::Empty, "and the same entry on the pass that is not expecting it");
}

#[test]
fn the_id_this_driver_never_issues_is_the_one_that_makes_a_torn_entry_recognisable() {
	// If zero were ever issued, an arriving entry and that command's own completion would be the
	// same observation and neither could be acted on.
	assert_eq!(NEVER_ISSUED, 0);
	let real = completion(1, true, 0, 0, 1);
	assert_eq!(reap(&real, true, 1), Reaped::Mine { succeeded: true, sq_head: 1 }, "the first id a driver may use is one");
}

// ------------------------------------------------------------------ a fake controller's ring
//
// THE INDIVIDUAL TESTS ABOVE ASK ABOUT ONE ENTRY. This asks about a RING: a controller filling slots
// in order, wrapping, and flipping the phase as it goes, with a driver walking behind it. The
// properties that only appear over a sequence - that the phase discipline survives a wrap, that a
// stale entry from the previous pass is never mistaken for a fresh one, and that a reader which
// stops sees the ring exactly as it left it - cannot be seen one entry at a time.
//
// It is a MODEL OF THE CONTROLLER'S HALF and not of the driver's: it writes what a controller writes
// and nothing else, so what the assertions exercise is the decision layer against something that
// behaves the way the hardware does.
struct FakeRing {
	slots: alloc::vec::Vec<[u8; CQ_ENTRY_LEN]>,
	// The controller's own cursor, and the phase it is currently writing.
	write: usize,
	phase: bool,
}

impl FakeRing {
	fn new(entries: usize) -> FakeRing {
		// Zeroed, exactly as a driver hands it over: every slot reads phase 0 on the first pass.
		FakeRing { slots: alloc::vec![[0u8; CQ_ENTRY_LEN]; entries], write: 0, phase: true }
	}

	// The controller posts one completion and advances, flipping its phase on the wrap.
	fn post(&mut self, command_id: u16, status_code: u8, sq_head: u16) {
		self.slots[self.write] = completion(command_id, self.phase, 0, status_code, sq_head);
		self.write += 1;
		if self.write == self.slots.len() {
			self.write = 0;
			self.phase = !self.phase;
		}
	}
}

#[test]
fn a_reader_walking_behind_a_filling_ring_sees_every_completion_once_and_in_order() {
	// Three times round a four-entry ring, so the phase flips three times and every slot is reused.
	const ENTRIES: usize = 4;
	let mut ring = FakeRing::new(ENTRIES);
	let mut head = 0usize;
	let mut phase = true;
	for step in 0..(ENTRIES * 3) {
		let id = (step as u16) + 1;
		ring.post(id, 0, id);
		match reap(&ring.slots[head], phase, id) {
			Reaped::Mine { succeeded, sq_head } => {
				assert!(succeeded, "step {step} should succeed");
				assert_eq!(sq_head, id, "and carry its own head");
			}
			other => panic!("step {step} read {other:?} instead of its completion"),
		}
		head += 1;
		if head == ENTRIES {
			head = 0;
			phase = !phase;
		}
	}
}

#[test]
fn a_reader_that_stops_reads_the_slot_ahead_as_empty_and_not_as_last_time_round() {
	// THE PROPERTY A WRAP EXISTS TO BREAK. After a full pass the slots all hold real completions from
	// the previous round; the only thing that distinguishes "already read" from "new" is the phase,
	// and a driver that lost track of it would replay the whole ring.
	const ENTRIES: usize = 4;
	let mut ring = FakeRing::new(ENTRIES);
	for id in 1..=ENTRIES as u16 {
		ring.post(id, 0, id);
	}
	// The reader has consumed all four and is back at slot zero, now expecting the opposite phase.
	assert_eq!(reap(&ring.slots[0], false, 5), Reaped::Empty, "the first pass's entry is not the second pass's");
	// And once the controller overwrites it, the same slot is a completion again.
	ring.post(5, 0, 5);
	assert_eq!(reap(&ring.slots[0], false, 5), Reaped::Mine { succeeded: true, sq_head: 5 });
}

#[test]
fn a_controller_that_posts_nothing_leaves_every_slot_empty_for_ever() {
	// The timeout case, at the decision layer: there is no state in which an untouched ring reports
	// anything but `Empty`, so a driver's bound is the only thing that ends the wait - which is why
	// it has to have one.
	let ring = FakeRing::new(8);
	for slot in &ring.slots {
		assert_eq!(reap(slot, true, 1), Reaped::Empty);
	}
}

#[test]
fn a_failed_completion_in_the_middle_of_a_ring_does_not_disturb_the_ones_after_it() {
	// Resource cleanup at this layer is the reader staying in step: a failure is consumed exactly
	// like a success, because the entry has been written either way.
	const ENTRIES: usize = 4;
	let mut ring = FakeRing::new(ENTRIES);
	ring.post(1, 0, 1);
	ring.post(2, 0x0B, 2); // a failure
	ring.post(3, 0, 3);
	assert_eq!(reap(&ring.slots[0], true, 1), Reaped::Mine { succeeded: true, sq_head: 1 });
	assert_eq!(reap(&ring.slots[1], true, 2), Reaped::Mine { succeeded: false, sq_head: 2 });
	assert_eq!(reap(&ring.slots[2], true, 3), Reaped::Mine { succeeded: true, sq_head: 3 });
}

#[test]
fn a_completion_arriving_for_a_command_the_reader_has_given_up_on_is_told_apart_from_a_torn_one() {
	// The disconnect case: a command timed out, the driver moved on, and its answer turns up in the
	// slot the next command was waiting at. That is a real completion for an abandoned id - the
	// queue and the driver disagree - and it must not read as an entry still arriving, because one
	// says stop and the other says wait.
	let mut ring = FakeRing::new(4);
	ring.post(7, 0, 7);
	assert_eq!(reap(&ring.slots[0], true, 8), Reaped::Unexpected { command_id: 7 });
	let mut torn = [0u8; CQ_ENTRY_LEN];
	torn[14] = 1;
	assert_eq!(reap(&torn, true, 8), Reaped::Arriving);
}

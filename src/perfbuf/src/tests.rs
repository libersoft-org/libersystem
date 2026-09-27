use super::*;

fn buffer(capacity: usize) -> Buffer {
	let storage: &'static mut [Record] = Box::leak(vec![Record::default(); capacity].into_boxed_slice());
	let buffer = Buffer::new();
	// SAFETY: the storage is leaked, so it outlives the buffer, and nothing else holds it.
	unsafe { buffer.attach(storage.as_mut_ptr(), storage.len()) };
	buffer
}

fn site(tag: &[u8; 8], cycles: u64, value: u64) -> Record {
	Record { site: *tag, cycles, value, thread: 7, core: 1, kind: KIND_SITE, detail: 0 }
}

fn lines(buffer: &Buffer) -> (Vec<String>, Drained) {
	let mut out: Vec<String> = Vec::new();
	let drained = drain_lines(buffer, &mut |line: &[u8]| out.push(String::from_utf8(line.to_vec()).expect("a drain line is ASCII")));
	(out, drained)
}

#[test]
fn a_record_is_thirty_two_bytes_and_the_buffer_is_eight_mebibytes_of_them() {
	assert_eq!(core::mem::size_of::<Record>(), 32);
	assert_eq!(CAPACITY * RECORD_BYTES, 8 * 1024 * 1024);
	assert_eq!(CAPACITY, 262_144);
}

#[test]
fn a_buffer_with_no_storage_cannot_be_armed_and_accepts_nothing() {
	let buffer = Buffer::new();
	assert!(!buffer.attached());
	assert!(!buffer.arm(0));
	assert!(!buffer.armed());
	assert_eq!(buffer.push(site(b"acq-beg\0", 1, 0)), Push::Unarmed);
}

#[test]
fn an_unarmed_append_is_dropped_and_not_counted() {
	let buffer = buffer(4);
	assert_eq!(buffer.push(site(b"acq-beg\0", 1, 0)), Push::Unarmed);
	assert!(buffer.arm(0));
	let (_, drained) = lines(&buffer);
	assert_eq!(drained, Drained { records: 0, refused: 0, incomplete: 0, stale: 0 });
}

#[test]
fn a_full_buffer_refuses_and_counts_and_never_wraps_over_the_first_records() {
	let buffer = buffer(4);
	assert!(buffer.arm(0));
	for index in 0..6u64 {
		let expected = if index < 4 { Push::Accepted } else { Push::Refused };
		assert_eq!(buffer.push(site(b"drw-beg\0", 10 + index, index)), expected, "append {index}");
	}
	let mut kept: Vec<u64> = Vec::new();
	let drained = buffer.drain(|record| kept.push(record.value));
	// THE FIRST FOUR, IN ORDER: a ring would have kept the last four, which is the frames nobody
	// asked about, and the count is how a reader knows the run is not whole.
	assert_eq!(kept, vec![0, 1, 2, 3]);
	assert_eq!(drained.records, 4);
	assert_eq!(drained.refused, 2);
}

#[test]
fn arming_again_empties_the_buffer_and_resets_the_refusal_count() {
	let buffer = buffer(2);
	assert!(buffer.arm(0));
	for index in 0..3u64 {
		buffer.push(site(b"prs-beg\0", 5, index));
	}
	let first = buffer.generation();
	assert!(buffer.arm(100));
	assert!(buffer.generation() > first);
	buffer.push(site(b"prs-end\0", 200, 9));
	let mut kept: Vec<u64> = Vec::new();
	let drained = buffer.drain(|record| kept.push(record.value));
	assert_eq!(kept, vec![9]);
	assert_eq!(drained, Drained { records: 1, refused: 0, incomplete: 0, stale: 0 });
}

#[test]
fn a_disarmed_buffer_accepts_nothing_more() {
	let buffer = buffer(4);
	assert!(buffer.arm(0));
	buffer.push(site(b"ds-req\0\0", 1, 1));
	buffer.disarm();
	assert_eq!(buffer.push(site(b"ds-req\0\0", 2, 2)), Push::Unarmed);
	let (_, drained) = lines(&buffer);
	assert_eq!(drained.records, 1);
}

#[test]
fn a_record_older_than_the_arm_is_left_out_and_counted_as_stale() {
	let buffer = buffer(4);
	assert!(buffer.arm(1_000));
	buffer.push(site(b"ds-wait\0", 999, 3));
	buffer.push(site(b"ds-wait\0", 1_001, 4));
	let mut kept: Vec<u64> = Vec::new();
	let drained = buffer.drain(|record| kept.push(record.value));
	assert_eq!(kept, vec![4]);
	assert_eq!(drained.stale, 1);
}

#[test]
fn a_slot_claimed_and_never_completed_is_counted_as_incomplete_not_read() {
	let buffer = buffer(4);
	assert!(buffer.arm(0));
	buffer.push(site(b"vq-ntfy\0", 1, 1));
	// A writer that claimed the next slot and never published it.
	buffer.next.fetch_add(1, Ordering::AcqRel);
	buffer.push(site(b"vq-done\0", 3, 2));
	let mut kept: Vec<u64> = Vec::new();
	let drained = buffer.drain(|record| kept.push(record.value));
	assert_eq!(kept, vec![1, 2]);
	assert_eq!(drained.incomplete, 1);
}

#[test]
fn a_group_is_contiguous_and_is_refused_whole_rather_than_split() {
	let buffer = buffer(6);
	assert!(buffer.arm(0));
	buffer.push(site(b"acq-beg\0", 1, 0));
	buffer.push(site(b"acq-end\0", 2, 0));
	assert_eq!(buffer.push_group(&name_records(42, 9, b"display_service", 3, 0)), Push::Accepted);
	// Two slots are left, and a four-chunk name does not fit in two.
	assert_eq!(buffer.push_group(&name_records(43, 10, b"virtio_gpu", 4, 0)), Push::Refused);
	let (out, drained) = lines(&buffer);
	assert_eq!(drained.refused, 4);
	assert!(out.iter().any(|line| line == "\x1ePERF-THREAD 42 9 display_service\n"), "{out:?}");
	assert!(!out.iter().any(|line| line.contains("virtio_gpu")), "a refused group names nothing: {out:?}");
}

#[test]
fn the_drain_writes_records_in_claim_order_then_the_thread_table_then_the_counts() {
	let buffer = buffer(32);
	assert!(buffer.arm(0));
	buffer.push(site(b"prs-beg\0", 100, 0));
	buffer.push(Record { site: [0; 8], cycles: 101, value: 7, thread: 12, core: 2, kind: KIND_SWITCH, detail: LEFT_BLOCKED });
	buffer.push_group(&name_records(7, 3, b"test2d-sw", 101, 2));
	buffer.push(Record { site: [0; 8], cycles: 102, value: 12, thread: 7, core: 0, kind: KIND_WAKE, detail: WOKEN_BY_MESSAGE });
	buffer.push(Record { site: [0; 8], cycles: 103, value: 0, thread: 7, core: 0, kind: KIND_WAKE, detail: WOKEN_BY_DEADLINE });
	buffer.push_group(&name_records(12, 4, b"a-process-name-longer-than-thirty-two-bytes", 104, 1));
	let (out, drained) = lines(&buffer);
	assert_eq!(
		out,
		vec![
			"\x1ePERF prs-beg 100 0 7 1 site\n".to_string(),
			"\x1ePERF switch 101 7 12 2 blocked\n".to_string(),
			"\x1ePERF wake 102 12 7 0 message\n".to_string(),
			"\x1ePERF wake 103 0 7 0 deadline\n".to_string(),
			"\x1ePERF-THREAD 7 3 test2d-sw\n".to_string(),
			"\x1ePERF-THREAD 12 4 a-process-name-longer-than-thirt\n".to_string(),
			"\x1ePERF-END 12 0 0 0\n".to_string(),
		]
	);
	assert_eq!(drained.records, 12);
}

#[test]
fn a_site_tag_can_never_split_a_line_or_add_a_field() {
	assert_eq!(&site_tag(u64::from_le_bytes(*b"ok tag\n\0")), b"ok?tag?\0");
	let buffer = buffer(2);
	assert!(buffer.arm(0));
	buffer.push(Record { site: *b"a b\nc\x01d\xff", cycles: 1, value: 2, thread: 3, core: 4, kind: KIND_SITE, detail: 0 });
	buffer.push(Record { site: [0; 8], cycles: 1, value: 2, thread: 3, core: 4, kind: KIND_SITE, detail: 0 });
	let (out, _) = lines(&buffer);
	assert_eq!(out[0], "\x1ePERF a?b?c?d? 1 2 3 4 site\n");
	assert_eq!(out[1], "\x1ePERF ? 1 2 3 4 site\n");
	assert_eq!(name_records(1, 2, b"sp ace\n", 0, 0)[0].site, *b"sp?ace?\0");
}

#[test]
fn every_switch_reason_and_wake_cause_has_its_own_word() {
	let reasons: Vec<&[u8]> = [LEFT_BLOCKED, LEFT_PREEMPTED, LEFT_YIELDED, LEFT_EXITED].iter().map(|reason| left_name(*reason)).collect();
	assert_eq!(reasons, vec![&b"blocked"[..], b"preempted", b"yielded", b"exited"]);
	let causes: Vec<&[u8]> = [WOKEN_BY_MESSAGE, WOKEN_BY_DEADLINE, WOKEN_BY_OTHER].iter().map(|cause| woken_name(*cause)).collect();
	assert_eq!(causes, vec![&b"message"[..], b"deadline", b"other"]);
}

#[test]
fn appends_from_several_threads_land_in_distinct_slots_and_are_all_accounted() {
	let buffer: &'static Buffer = Box::leak(Box::new(buffer(1000)));
	assert!(buffer.arm(0));
	let writers: Vec<_> = (0..4u32)
		.map(|writer| {
			std::thread::spawn(move || {
				for index in 0..300u64 {
					let _ = buffer.push(Record { site: *b"stress\0\0", cycles: 1 + index, value: index, thread: writer, core: writer as u16, kind: KIND_SITE, detail: 0 });
				}
			})
		})
		.collect();
	for writer in writers {
		writer.join().expect("a writer thread panicked");
	}
	let mut per_writer = [0u64; 4];
	let drained = buffer.drain(|record| per_writer[record.thread as usize] += 1);
	assert_eq!(drained.records + drained.refused, 1200);
	assert_eq!(drained.records, 1000);
	assert_eq!(per_writer.iter().sum::<u64>(), 1000);
}

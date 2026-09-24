use super::*;

// Commit `count` records as a writer would report them: at the end of the file it found.
fn filled(journal: &mut Journal, request: u64, count: u32, length: usize) {
	for _ in 0..count {
		let placement = journal.place(length).unwrap();
		let sequence = journal.sequence();
		let offset = if placement.new_segment { HEADER as u64 } else { placement.offset };
		journal.committed(placement, sequence, request, length, offset, offset + length as u64);
	}
}

#[test]
fn a_segment_takes_1024_records_or_8_mb_whichever_is_first() {
	let mut journal = Journal::new();
	filled(&mut journal, 0, RECORDS, 64);
	assert_eq!(journal.segments().len(), 1);
	assert_eq!(journal.place(64).unwrap(), Placement { segment: 2, offset: 0, new_segment: true, retire: None }, "the 1025th opens a new segment");
	// Bytes: records of the largest size fill 8 MB before 1024 of them do.
	let mut large = Journal::new();
	let per = (SEGMENT_BYTES as usize - HEADER) / RECORD_BYTES;
	filled(&mut large, 0, per as u32, RECORD_BYTES);
	assert!(large.place(RECORD_BYTES).unwrap().new_segment, "no record crosses 8 MB");
	assert_eq!(large.place(RECORD_BYTES + 1), Err(Refusal::Oversized));
	assert_eq!(frame(&[0u8; RECORD_BYTES - FRAME + 1]), Err(Refusal::Oversized), "the frame counts toward the bound");
}

#[test]
fn only_complete_unpinned_segments_retire_and_space_is_reserved_at_admission() {
	let mut journal = Journal::new();
	filled(&mut journal, 7, 1, 64);
	filled(&mut journal, 0, RECORDS * SEGMENTS as u32 - 1, 64);
	assert_eq!(journal.segments().len(), SEGMENTS);
	// The oldest segment holds a record of request 7, still in flight: it may not be retired.
	assert_eq!(journal.place(64), Err(Refusal::Pinned));
	journal.unpin(7);
	let placement = journal.place(64).unwrap();
	assert_eq!(placement.retire, Some(1));
	let sequence = journal.sequence();
	journal.committed(placement, sequence, 0, 64, HEADER as u64, HEADER as u64 + 64);
	assert_eq!(journal.segments().len(), SEGMENTS);
	assert!(journal.oldest() > 1, "the retired segment's records are no longer readable");
	// RESERVATION: a journal that cannot promise a request's records refuses the request.
	let mut tight = Journal::new();
	filled(&mut tight, 9, 1, 64);
	filled(&mut tight, 0, RECORDS * SEGMENTS as u32 - 1, 64);
	tight.unpin(9);
	filled(&mut tight, 9, 1, 64);
	assert_eq!(tight.reserve(10), Ok(()), "the retirable segments count");
	let mut pinned = Journal::new();
	for request in 1..=(SEGMENTS as u64) {
		filled(&mut pinned, request, RECORDS, 64);
	}
	assert_eq!(pinned.reserve(10), Err(Refusal::Pinned), "every segment pinned and the last full");
	// A RESERVATION IS THE REQUEST'S OWN: what it wrote comes off it, and ending it frees the rest - never
	// another request's.
	let mut shared = Journal::new();
	for request in 101..=(100 + SEGMENTS as u64 - 1) {
		filled(&mut shared, request, RECORDS, 64);
	}
	filled(&mut shared, 0, RECORDS - 10, 64);
	assert_eq!(shared.reserve(1), Ok(()));
	assert_eq!(shared.reserve(2), Ok(()));
	assert_eq!(shared.reserve(3), Err(Refusal::Pinned), "two requests' records fill what is left");
	filled(&mut shared, 1, 2, 64);
	assert_eq!(shared.reserve(3), Err(Refusal::Pinned), "what it wrote came off its reservation, not off anyone else's");
	shared.unpin(1);
	assert_eq!(shared.reserve(3), Ok(()), "the finished request's unwritten records are free again");
	assert_eq!(shared.reserve(4), Err(Refusal::Pinned), "and the other request's reservation still stands");
}

#[test]
fn a_record_lands_where_the_writer_says_it_did() {
	let mut journal = Journal::new();
	filled(&mut journal, 0, 1, 40);
	// A COMMIT WHOSE ANSWER WAS LOST LANDED ALL THE SAME: the next record went after it, and the writer's
	// reported length is what places it - not the bookkeeping that never saw the first land.
	let placement = journal.place(40).unwrap();
	assert_eq!(placement.offset, HEADER as u64 + 40);
	let sequence = journal.sequence();
	journal.committed(placement, sequence, 0, 40, HEADER as u64 + 80, HEADER as u64 + 120);
	assert_eq!(journal.find(sequence, 1)[0].offset, HEADER as u64 + 80);
	assert_eq!(journal.place(40).unwrap().offset, HEADER as u64 + 120);
}

#[test]
fn the_ring_keeps_order_retries_the_head_and_counts_what_it_drops() {
	let mut journal = Journal::new();
	for sequence in 1..=3u64 {
		journal.push(Entry { sequence, request: 1, bytes: alloc::vec![sequence as u8], first_attempt: true });
	}
	assert_eq!(journal.written(false), None, "storage refused the head");
	assert!(journal.failing);
	assert_eq!(journal.head().map(|entry| (entry.sequence, entry.first_attempt)), Some((1, false)), "still first, and its first attempt is answered");
	assert_eq!(journal.written(true).map(|entry| entry.sequence), Some(1));
	assert_eq!(journal.head().map(|entry| entry.sequence), Some(2), "then in order");
	for sequence in 4..(4 + RING as u64) {
		journal.push(Entry { sequence, request: 1, bytes: alloc::vec![], first_attempt: true });
	}
	assert_eq!(journal.waiting(), RING);
	assert_eq!(journal.evicted, 2, "two dropped to stay within 256");
	assert_eq!(journal.head().map(|entry| entry.sequence), Some(2), "never the head being written");
}

#[test]
fn a_segment_reads_back_to_its_last_whole_frame() {
	let mut bytes = header(3, 40).to_vec();
	bytes.extend(frame(b"first").unwrap());
	bytes.extend(frame(b"second").unwrap());
	let (number, first, frames) = scan(&bytes).unwrap();
	assert_eq!((number, first), (3, 40));
	assert_eq!(frames.iter().map(|(_, record)| *record).collect::<Vec<_>>(), [&b"first"[..], &b"second"[..]]);
	assert_eq!(frames[0].0, HEADER as u64);
	let mut torn = bytes.clone();
	torn.extend(frame(b"third").unwrap());
	torn.truncate(torn.len() - 2);
	assert_eq!(scan(&torn).unwrap().2.len(), 2, "a torn tail is not believed");
	assert!(scan(b"not a journal at all, and long enough").is_none());
	// Recovery numbers on from what it found.
	let mut journal = Journal::new();
	journal.recover(alloc::vec![Segment { number: 3, first: 40, records: 2, bytes: bytes.len() as u64 }], alloc::vec![Located { sequence: 40, segment: 3, offset: 32, length: 9 }, Located { sequence: 41, segment: 3, offset: 41, length: 10 }]);
	assert_eq!((journal.sequence(), journal.oldest()), (42, 40));
	assert_eq!(journal.find(41, 16).len(), 1);
	assert_eq!(journal.place(10).unwrap().segment, 3);
}

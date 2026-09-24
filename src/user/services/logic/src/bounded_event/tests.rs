use super::*;
use event_proto::generated::liber::event::v1::{EventHeader, GenericEvent};

// A NON-MIDI USER OF THE SAME VOCABULARY AND THE SAME QUEUE: key presses carried as `liber:event@1`'s generic
// event, costed at their encoded size.
fn key(sequence: u64, received_ns: u64, code: u8) -> (GenericEvent, usize) {
	let event = GenericEvent { header: EventHeader { sequence, received_ns }, payload: alloc::vec![code] };
	let bytes = event.encode_vec().map(|encoded| encoded.len()).unwrap_or(usize::MAX);
	(event, bytes)
}

#[test]
fn a_non_midi_source_reuses_the_vocabulary_and_the_queue() {
	let mut queue: Queue<GenericEvent> = Queue::new();
	for (at, code) in [b'a', b'b', b'c'].iter().enumerate() {
		let (event, bytes) = key(at as u64, 1000, *code);
		assert_eq!(queue.push(event, bytes), Ok(at as u64));
	}
	let pulled = queue.pull(8);
	let codes: Vec<u8> = pulled.events.iter().map(|(_, event)| event.payload[0]).collect();
	assert_eq!(codes, b"abc", "observation order, at one equal timestamp");
	assert!(pulled.events.iter().all(|(_, event)| event.header.received_ns == 1000), "the receipt time is what was sampled");
	assert_eq!(pulled.end, None);
}

#[test]
fn both_bounds_hold_exactly_and_an_overflow_ends_the_stream() {
	let mut queue: Queue<u8> = Queue::new();
	for n in 0..MAX_EVENTS {
		assert!(queue.push(n as u8, 1).is_ok());
	}
	// The 257th ends it: everything queued is gone, and the end is readable.
	let end = queue.push(0, 1).unwrap_err();
	assert_eq!(end, End { reason: Reason::Overflow, next_sequence: MAX_EVENTS as u64 });
	assert_eq!((queue.len(), queue.bytes()), (0, 0));
	assert_eq!(queue.pull(8), Pulled { events: Vec::new(), end: Some(end) });
	assert_eq!(queue.push(1, 1), Err(end), "an ended stream admits nothing");
	// The byte bound: 32 kB exactly, and one byte past it.
	let mut bytes: Queue<u8> = Queue::new();
	assert!(bytes.push(0, MAX_BYTES - 10).is_ok());
	assert!(bytes.push(1, 10).is_ok());
	assert_eq!(bytes.push(2, 1).unwrap_err().reason, Reason::Overflow);
}

#[test]
fn the_end_waits_behind_what_was_queued_before_it() {
	let mut queue: Queue<u8> = Queue::new();
	queue.push(1, 1).unwrap();
	queue.push(2, 1).unwrap();
	// A removal DISCARDS what was queued: a stream that lost its source is not presented as continuing.
	let end = queue.terminate(Reason::Removed);
	assert_eq!(end.next_sequence, 2);
	assert_eq!(queue.pull(8), Pulled { events: Vec::new(), end: Some(end) });
	// The first end stands.
	assert_eq!(queue.terminate(Reason::Overflow).reason, Reason::Removed);
	// A partial pull leaves the end for the read that drains the rest - and an end reached with events
	// queued is only reachable by terminating, which discards them, so the end never follows a gap.
	let mut partial: Queue<u8> = Queue::new();
	for n in 0..3 {
		partial.push(n, 1).unwrap();
	}
	let first = partial.pull(2);
	assert_eq!((first.events.len(), first.end), (2, None));
	assert_eq!(partial.pull(2).events, alloc::vec![(2, 2)]);
}

#[test]
fn the_sequence_never_wraps() {
	let mut queue: Queue<u8> = Queue::new();
	queue.next_sequence = u64::MAX - 1;
	assert_eq!(queue.push(0, 1), Ok(u64::MAX - 1));
	assert_eq!(queue.push(1, 1).unwrap_err(), End { reason: Reason::Exhausted, next_sequence: u64::MAX });
}

use super::*;

const MB: u64 = 1024 * 1024;

fn formats() -> Vec<Format> {
	alloc::vec![
		Format { index: 1, kind: Kind::Yuy2, sizes: alloc::vec![Size { index: 1, width: 640, height: 480, max_bytes: 640 * 480 * 2, intervals: Intervals::Discrete(alloc::vec![(333_333, 10_000_000), (666_666, 10_000_000)]) }] },
		Format { index: 2, kind: Kind::Mjpeg, sizes: alloc::vec![Size { index: 1, width: 1280, height: 720, max_bytes: 2 * MB as u32, intervals: Intervals::Stepwise { minimum: (1, 30), maximum: (1, 10), step: (1, 30) } }] },
	]
}

fn stream() -> Stream {
	let selected = expect(&formats(), Request { format: 1, size: 1, interval: (333_333, 10_000_000) }).unwrap();
	Stream::new(7, selected)
}

#[test]
fn an_advertisement_is_held_to_the_normalizers_bounds() {
	assert_eq!(validate(&formats()), Ok(()));
	let mut nine = formats();
	for index in 3..=9 {
		nine.push(Format { index, kind: Kind::Mjpeg, sizes: formats()[1].sizes.clone() });
	}
	assert_eq!(validate(&nine), Err(Refusal::TooLarge), "nine formats");
	let mut duplicate = formats();
	duplicate[1].index = 1;
	assert_eq!(validate(&duplicate), Err(Refusal::Invalid), "a duplicate format index");
	let mut zero = formats();
	zero[0].sizes[0].width = 0;
	assert_eq!(validate(&zero), Err(Refusal::Invalid), "a zero width");
	let mut short = formats();
	short[0].sizes[0].max_bytes -= 1;
	assert_eq!(validate(&short), Err(Refusal::Invalid), "a YUY2 buffer that cannot hold its frame");
	let mut huge = formats();
	huge[1].sizes[0].max_bytes = (MAX_BUFFER_BYTES + 1) as u32;
	assert_eq!(validate(&huge), Err(Refusal::TooLarge), "a frame past one buffer");
	let mut upside_down = formats();
	upside_down[1].sizes[0].intervals = Intervals::Stepwise { minimum: (1, 10), maximum: (1, 30), step: (1, 30) };
	assert_eq!(validate(&upside_down), Err(Refusal::Invalid), "a range upside down");
	let mut unreachable = formats();
	unreachable[1].sizes[0].intervals = Intervals::Stepwise { minimum: (1, 30), maximum: (1, 10), step: (1, 7) };
	assert_eq!(validate(&unreachable), Err(Refusal::Invalid), "a step that never reaches the maximum");
	let mut many = formats();
	many[0].sizes[0].intervals = Intervals::Discrete((1..=33).map(|n| (n, 30)).collect());
	assert_eq!(validate(&many), Err(Refusal::TooLarge), "thirty-three intervals");
}

#[test]
fn pages_are_bounded_and_named_by_offset() {
	let mut wide = formats();
	wide[1].sizes = (1..=20).map(|index| Size { index, width: 64, height: 64, max_bytes: 4096, intervals: Intervals::Discrete(alloc::vec![(1, 30)]) }).collect();
	let (_, first) = page(&wide, 2, 0).unwrap();
	assert_eq!(first.len(), PAGE_SIZES);
	let (_, last) = page(&wide, 2, 16).unwrap();
	assert_eq!(last.len(), 4);
	assert_eq!(page(&wide, 2, 20).err(), Some(Refusal::NotFound), "past the end");
	assert_eq!(page(&wide, 3, 0).err(), Some(Refusal::NotFound), "no such format");
}

#[test]
fn a_selection_is_exactly_what_was_asked_or_refused() {
	let expected = expect(&formats(), Request { format: 1, size: 1, interval: (666_666, 10_000_000) }).unwrap();
	assert_eq!((expected.width, expected.height, expected.stride, expected.plane_offset), (640, 480, 1280, 0));
	// Equal rationals are the same interval, whatever their terms.
	assert!(expect(&formats(), Request { format: 1, size: 1, interval: (1_333_332, 20_000_000) }).is_ok());
	assert_eq!(expect(&formats(), Request { format: 1, size: 1, interval: (1, 20) }).err(), Some(Refusal::Unsupported), "an interval not offered");
	assert_eq!(expect(&formats(), Request { format: 9, size: 1, interval: (1, 30) }).err(), Some(Refusal::Unsupported), "a format not offered");
	// Inside a range, on a step; off a step; past it.
	assert!(expect(&formats(), Request { format: 2, size: 1, interval: (2, 30) }).is_ok());
	assert!(expect(&formats(), Request { format: 2, size: 1, interval: (1, 15) }).is_ok(), "2/30 again, in other terms");
	assert_eq!(expect(&formats(), Request { format: 2, size: 1, interval: (1, 20) }).err(), Some(Refusal::Unsupported), "1/20 is between 1/30 and 2/30");
	assert_eq!(expect(&formats(), Request { format: 2, size: 1, interval: (1, 5) }).err(), Some(Refusal::Unsupported));
	// The provider's selection: any silent change is refused.
	assert_eq!(verify(&expected, &expected), Ok(()));
	assert_eq!(verify(&expected, &Selected { width: 320, ..expected }), Err(Refusal::Invalid));
	assert_eq!(verify(&expected, &Selected { interval: (333_333, 10_000_000), ..expected }), Err(Refusal::Invalid));
	assert_eq!(verify(&expected, &Selected { stride: 1300, ..expected }), Err(Refusal::Invalid));
	assert_eq!(verify(&expected, &Selected { max_bytes: expected.max_bytes + 1, ..expected }), Err(Refusal::Invalid), "a larger frame than the buffers were sized for");
}

#[test]
fn registration_holds_every_bound_exactly() {
	let mut stream = stream();
	let frame = 640 * 480 * 2;
	assert_eq!(stream.register(1, frame - 1, 0), Err(Refusal::TooLarge), "a buffer too small for the frame");
	assert_eq!(stream.register(1, MAX_BUFFER_BYTES + 1, 0), Err(Refusal::TooLarge), "a buffer past 8 MB");
	for koid in 1..=4 {
		assert_eq!(stream.register(koid, MAX_BUFFER_BYTES, 0), Ok((koid - 1) as u8));
	}
	assert_eq!(stream.registered(), MAX_STREAM_BYTES, "four of 8 MB is exactly 32 MB");
	assert_eq!(stream.register(5, frame, 0), Err(Refusal::TooLarge), "a fifth buffer");
	let mut again = super::tests::stream();
	again.register(1, frame, 0).unwrap();
	assert_eq!(again.register(1, frame, 0), Err(Refusal::Invalid), "the same backing twice");
	assert_eq!(again.register(2, frame, MAX_TOTAL_BYTES - frame), Err(Refusal::TooLarge), "past 128 MB across every stream");
	assert!(again.register(2, frame, MAX_TOTAL_BYTES - 2 * frame).is_ok(), "exactly 128 MB");
	// A registration the producer refused is undone, and its slot and backing are free again.
	again.forget(1);
	assert_eq!(again.buffers().len(), 1);
	assert_eq!(again.register(3, frame, 0), Ok(1));
}

#[test]
fn a_buffer_moves_only_by_start_completion_and_its_exact_lease() {
	let mut stream = stream();
	let frame = 640 * 480 * 2;
	stream.register(1, frame, 0).unwrap();
	stream.register(2, frame, 0).unwrap();
	let queued = stream.start().unwrap();
	assert_eq!(queued.len(), 2);
	let (buffer, lease) = queued[0];
	// A completion naming another lease, or a buffer the producer does not hold, changes nothing.
	assert_eq!(stream.completed(buffer, lease + 100, 0, 100), Err(Refusal::Stale));
	assert_eq!(stream.completed(9, lease, 0, 100), Err(Refusal::Stale));
	assert_eq!(stream.completed(buffer, lease, 0, frame as u32 + 1), Err(Refusal::Invalid), "a length past the frame");
	let delivered = stream.completed(buffer, lease, 0, 1000).unwrap();
	assert_eq!((delivered.sequence, delivered.valid_bytes), (0, 1000));
	// LEASED: a second completion of the same buffer cannot overwrite it.
	assert_eq!(stream.completed(buffer, lease, 1, 1000), Err(Refusal::Stale));
	// The client gives back exactly its lease, once.
	assert_eq!(stream.release(buffer, lease + 1), Err(Refusal::Stale), "another lease");
	let (requeued, new_lease) = stream.release(buffer, lease).unwrap().unwrap();
	assert_eq!(requeued, buffer);
	assert_ne!(new_lease, lease, "a new lease for a new hand-over");
	assert_eq!(stream.release(buffer, lease), Err(Refusal::Stale), "a double return");
	// The old lease's late completion is stale; the new one's is taken.
	assert_eq!(stream.completed(buffer, lease, 1, 1000), Err(Refusal::Stale));
	assert!(stream.completed(buffer, new_lease, 1, 1000).is_ok());
}

#[test]
fn every_buffer_leased_means_drops_are_counted_and_nothing_is_overwritten() {
	let mut stream = stream();
	let frame = 640 * 480 * 2;
	for koid in 1..=4 {
		stream.register(koid, frame, 0).unwrap();
	}
	let queued = stream.start().unwrap();
	for (at, (buffer, lease)) in queued.iter().enumerate() {
		stream.completed(*buffer, *lease, at as u64, 100).unwrap();
	}
	assert!(stream.buffers().iter().all(|buffer| matches!(buffer.holder, Holder::Leased(_))));
	// The producer, with nothing queued, drops and says so: the sequence continues past the loss.
	stream.dropped(4, 3, DropReason::NoBuffer).unwrap();
	assert_eq!((stream.dropped_no_buffer, stream.next_sequence()), (3, 7));
	assert_eq!(stream.dropped(5, 1, DropReason::Device), Err(Refusal::Stale), "a drop inside what was already counted");
	// One lease back is one buffer back to the producer: bounded recovery.
	let (buffer, lease) = queued[2];
	let (_, requeued) = stream.release(buffer, lease).unwrap().unwrap();
	let delivered = stream.completed(buffer, requeued, 7, 100).unwrap();
	assert_eq!((delivered.sequence, delivered.discontinuities), (7, 0), "the counted drops left no unknown gap");
}

#[test]
fn losses_nobody_can_count_are_discontinuities_never_estimates() {
	let mut stream = stream();
	stream.register(1, 640 * 480 * 2, 0).unwrap();
	let (buffer, lease) = stream.start().unwrap()[0];
	stream.gap(10).unwrap();
	assert_eq!((stream.unknown_discontinuities, stream.next_sequence()), (1, 10));
	// A jump nobody explained is one more discontinuity, not a count of frames.
	let delivered = stream.completed(buffer, lease, 25, 100).unwrap();
	assert_eq!((delivered.discontinuities, stream.dropped_no_buffer + stream.dropped_device), (2, 0));
	assert_eq!(stream.gap(3), Err(Refusal::Stale), "a gap backwards");
}

#[test]
fn stop_waits_for_every_buffer_or_quarantines() {
	let mut stream = stream();
	stream.register(1, 640 * 480 * 2, 0).unwrap();
	stream.register(2, 640 * 480 * 2, 0).unwrap();
	let queued = stream.start().unwrap();
	let (leased, lease) = queued[0];
	stream.completed(leased, lease, 0, 100).unwrap();
	stream.stop(100).unwrap();
	assert_eq!(stream.phase(), Phase::Stopping { deadline: 100 + STOP_TICKS });
	// While stopping, a returned lease stays with the client: nothing goes back to the producer.
	assert_eq!(stream.release(leased, lease), Ok(None));
	assert_eq!(stream.register(3, 640 * 480 * 2, 0), Err(Refusal::Busy), "no renegotiation while stopping");
	// Confirmed: everything the producer held is the client's again, and the stream is retired.
	stream.stopped();
	assert_eq!(stream.phase(), Phase::Retired);
	assert!(stream.buffers().iter().all(|buffer| buffer.holder == Holder::Client));
	assert_eq!(stream.completed(queued[1].0, queued[1].1, 1, 100), Err(Refusal::Stale), "a completion after the stop");
	// Unconfirmed: quarantined at the deadline, not a tick before.
	let mut silent = super::tests::stream();
	silent.register(1, 640 * 480 * 2, 0).unwrap();
	silent.start().unwrap();
	silent.stop(0).unwrap();
	assert!(!silent.tick(STOP_TICKS - 1));
	assert!(silent.tick(STOP_TICKS));
	assert_eq!(silent.phase(), Phase::Quarantined);
	assert_eq!(silent.stop(STOP_TICKS + 1), Err(Refusal::Stale));
	// A stream never started has nothing to wait for.
	let mut idle = super::tests::stream();
	idle.stop(0).unwrap();
	assert_eq!(idle.phase(), Phase::Retired);
}

#[test]
fn device_time_is_read_only_on_one_timeline() {
	let at = |ticks, reset_generation| DeviceTime { ticks, width_bits: 32, frequency_hz: Some(1_000_000), clock_domain: 1, reset_generation };
	// Modulo the width: a wrap is a short interval, not a huge one.
	assert_eq!(DeviceClock::elapsed(&at(u32::MAX as u64 - 9, 0), &at(10, 0)), Some(20));
	// Across a reset, another domain or another width there is no difference to take.
	assert_eq!(DeviceClock::elapsed(&at(10, 0), &at(20, 1)), None);
	assert_eq!(DeviceClock::elapsed(&at(10, 0), &DeviceTime { clock_domain: 2, ..at(20, 0) }), None);
	assert_eq!(DeviceClock::elapsed(&at(10, 0), &DeviceTime { width_bits: 16, ..at(20, 0) }), None);
	// Malformed readings are not readings.
	assert!(!DeviceClock::valid(&DeviceTime { width_bits: 0, ..at(0, 0) }));
	assert!(!DeviceClock::valid(&DeviceTime { width_bits: 8, ..at(256, 0) }));
	assert!(!DeviceClock::valid(&DeviceTime { frequency_hz: Some(0), ..at(0, 0) }));
	assert!(DeviceClock::valid(&DeviceTime { frequency_hz: None, ..at(0, 0) }), "an unknown frequency is allowed");
	// The timeline generation: the same across frames of one timeline, advanced by a reset and by an
	// unknown discontinuity, and never carried across either.
	let mut clock = DeviceClock::default();
	let first = clock.observe(at(1, 0));
	assert_eq!(clock.observe(at(2, 0)), first);
	let reset = clock.observe(at(3, 1));
	assert_ne!(reset, first);
	clock.discontinuity();
	let after_gap = clock.observe(at(4, 1));
	assert_ne!(after_gap, reset);
	assert_eq!(clock.observe(at(5, 1)), after_gap);
}

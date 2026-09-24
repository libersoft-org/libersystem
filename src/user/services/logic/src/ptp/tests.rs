use super::*;

fn container(kind: u16, code: u16, transaction: u32, payload: &[u8]) -> Vec<u8> {
	let mut bytes = ((HEADER + payload.len()) as u32).to_le_bytes().to_vec();
	bytes.extend_from_slice(&kind.to_le_bytes());
	bytes.extend_from_slice(&code.to_le_bytes());
	bytes.extend_from_slice(&transaction.to_le_bytes());
	bytes.extend_from_slice(payload);
	bytes
}

fn response(code: u16, transaction: u32, params: &[u32]) -> Vec<u8> {
	let payload: Vec<u8> = params.iter().flat_map(|param| param.to_le_bytes()).collect();
	container(RESPONSE, code, transaction, &payload)
}

// Feed a stream in pieces of the given sizes and collect what it said.
fn parse(inbound: &mut Inbound, stream: &[u8], pieces: &[usize]) -> Result<(Option<u32>, Vec<u8>, Option<Response>), Fault> {
	let (mut declared, mut payload, mut answer) = (None, Vec::new(), None);
	let mut at = 0;
	let mut sizes = pieces.iter().copied().cycle();
	while at < stream.len() {
		let take = sizes.next().unwrap_or(stream.len()).max(1).min(stream.len() - at);
		inbound.feed(&stream[at..at + take], &mut |piece| match piece {
			Piece::Data(length) => declared = Some(length),
			Piece::Payload(bytes) => payload.extend_from_slice(bytes),
			Piece::Response(response) => answer = Some(response),
		})?;
		at += take;
	}
	Ok((declared, payload, answer))
}

#[test]
fn a_command_is_one_container_of_at_most_thirty_two_bytes() {
	let bytes = command(GET_OBJECT_HANDLES, 7, &[0x0001_0001, 0, 0]).unwrap();
	assert_eq!(bytes.len(), 24);
	assert_eq!(&bytes[..12], &[24, 0, 0, 0, 1, 0, 0x07, 0x10, 7, 0, 0, 0]);
	assert_eq!(command(OPEN_SESSION, 1, &[1, 2, 3, 4, 5]).unwrap().len(), MAX_SHORT);
	assert_eq!(command(OPEN_SESSION, 1, &[0; 6]), None, "six parameters are not a command");
}

#[test]
fn fragmented_and_coalesced_containers_read_the_same() {
	let body: Vec<u8> = (0..5000u32).map(|n| n as u8).collect();
	let mut stream = container(DATA, GET_OBJECT, 9, &body);
	stream.extend(response(OK, 9, &[]));
	for pieces in [&[1usize][..], &[5, 7, 11], &[4096], &[stream.len()]] {
		let mut inbound = Inbound::new(GET_OBJECT, 9, true, u32::MAX - 12);
		let (declared, payload, answer) = parse(&mut inbound, &stream, pieces).unwrap();
		assert_eq!(declared, Some(5000), "pieces {pieces:?}");
		assert_eq!(payload, body, "pieces {pieces:?}");
		assert_eq!(answer.map(|answer| answer.code), Some(OK));
		assert!(inbound.done());
	}
	// A response with parameters, split across pulls.
	let mut inbound = Inbound::new(OPEN_SESSION, 1, false, 0);
	let (declared, _, answer) = parse(&mut inbound, &response(OK, 1, &[3, 4]), &[3]).unwrap();
	assert_eq!(declared, None);
	let answer = answer.unwrap();
	assert_eq!((answer.count, answer.params[0], answer.params[1]), (2, 3, 4));
}

#[test]
fn the_stream_is_checked_before_it_is_believed() {
	let data = |length: usize| container(DATA, GET_OBJECT_INFO, 3, &alloc::vec![0; length]);
	// Refused from the header, before a byte of payload.
	let mut inbound = Inbound::new(GET_OBJECT_INFO, 3, true, 4096);
	let mut seen = 0;
	assert_eq!(inbound.feed(&data(4097)[..HEADER], &mut |_| seen += 1), Err(Fault::TooLarge(4097)));
	assert_eq!(seen, 0, "nothing of an oversized container was passed on");
	// The four-gigabyte sentinel is not a length.
	let mut sentinel = data(0);
	sentinel[..4].copy_from_slice(&LENGTH_UNKNOWN.to_le_bytes());
	assert_eq!(Inbound::new(GET_OBJECT_INFO, 3, true, u32::MAX).feed(&sentinel, &mut |_| {}), Err(Fault::Unrepresentable));
	// Another transaction, another operation, a type bulk-IN never carries, a short length.
	assert_eq!(Inbound::new(GET_OBJECT_INFO, 4, true, 4096).feed(&data(10), &mut |_| {}), Err(Fault::Mismatch));
	assert_eq!(Inbound::new(GET_OBJECT, 3, true, 4096).feed(&data(10), &mut |_| {}), Err(Fault::Mismatch));
	assert_eq!(Inbound::new(GET_OBJECT_INFO, 3, true, 4096).feed(&container(EVENT, OBJECT_ADDED, 3, &[]), &mut |_| {}), Err(Fault::Malformed));
	let mut short = data(0);
	short[..4].copy_from_slice(&8u32.to_le_bytes());
	assert_eq!(Inbound::new(GET_OBJECT_INFO, 3, true, 4096).feed(&short, &mut |_| {}), Err(Fault::Malformed));
	// A response that is not whole parameters, or longer than five.
	let mut ragged = response(OK, 3, &[1]);
	ragged[..4].copy_from_slice(&14u32.to_le_bytes());
	assert_eq!(Inbound::new(GET_OBJECT_INFO, 3, true, 4096).feed(&ragged, &mut |_| {}), Err(Fault::Malformed));
	assert_eq!(Inbound::new(GET_OBJECT_INFO, 3, true, 4096).feed(&response(OK, 3, &[0; 6]), &mut |_| {}), Err(Fault::Malformed));
	// A data phase an operation does not have, a second one, and anything after the final response.
	assert_eq!(Inbound::new(OPEN_SESSION, 3, false, 0).feed(&container(DATA, OPEN_SESSION, 3, &[]), &mut |_| {}), Err(Fault::Sequence));
	let mut twice = data(4);
	twice.extend(data(4));
	assert_eq!(Inbound::new(GET_OBJECT_INFO, 3, true, 4096).feed(&twice, &mut |_| {}), Err(Fault::Sequence));
	let mut after = response(OK, 3, &[]);
	after.push(0);
	assert_eq!(Inbound::new(GET_OBJECT_INFO, 3, true, 4096).feed(&after, &mut |_| {}), Err(Fault::Sequence));
}

#[test]
fn a_truncated_stream_is_simply_not_done() {
	// Half a container is not an answer: the stream waits, and a caller's deadline decides.
	let mut stream = container(DATA, GET_OBJECT, 5, &[7; 100]);
	stream.extend(response(OK, 5, &[]));
	let mut inbound = Inbound::new(GET_OBJECT, 5, true, u32::MAX - 12);
	let (_, payload, answer) = parse(&mut inbound, &stream[..60], &[60]).unwrap();
	assert_eq!((payload.len(), answer), (48, None));
	assert!(!inbound.done() && inbound.seen_data());
	// A response alone is an answer too - an error the device gave instead of the data.
	let mut refused = Inbound::new(GET_OBJECT_INFO, 6, true, 4096);
	let (declared, _, answer) = parse(&mut refused, &response(INVALID_OBJECT_HANDLE, 6, &[]), &[4]).unwrap();
	assert_eq!((declared, answer.map(|answer| answer.code)), (None, Some(INVALID_OBJECT_HANDLE)));
	assert!(!refused.seen_data());
}

fn ptp_string(text: &str) -> Vec<u8> {
	if text.is_empty() {
		return alloc::vec![0];
	}
	let units: Vec<u16> = text.encode_utf16().chain(core::iter::once(0)).collect();
	let mut bytes = alloc::vec![units.len() as u8];
	bytes.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
	bytes
}

fn u16s(values: &[u16]) -> Vec<u8> {
	let mut bytes = (values.len() as u32).to_le_bytes().to_vec();
	bytes.extend(values.iter().flat_map(|value| value.to_le_bytes()));
	bytes
}

pub(crate) fn device_dataset(operations: &[u16]) -> Vec<u8> {
	let mut bytes = Vec::new();
	bytes.extend_from_slice(&100u16.to_le_bytes());
	bytes.extend_from_slice(&0u32.to_le_bytes());
	bytes.extend_from_slice(&0u16.to_le_bytes());
	bytes.extend(ptp_string(""));
	bytes.extend_from_slice(&0u16.to_le_bytes());
	bytes.extend(u16s(operations));
	for _ in 0..4 {
		bytes.extend(u16s(&[]));
	}
	for text in ["Liber", "Responder", "1.0", "0001"] {
		bytes.extend(ptp_string(text));
	}
	bytes
}

pub(crate) fn object_dataset(storage: u32, format: u16, size: u32, parent: u32, name: &str, captured: &str) -> Vec<u8> {
	let mut bytes = Vec::new();
	bytes.extend_from_slice(&storage.to_le_bytes());
	bytes.extend_from_slice(&format.to_le_bytes());
	bytes.extend_from_slice(&0u16.to_le_bytes());
	bytes.extend_from_slice(&size.to_le_bytes());
	bytes.extend_from_slice(&0u16.to_le_bytes());
	for _ in 0..6 {
		bytes.extend_from_slice(&0u32.to_le_bytes());
	}
	bytes.extend_from_slice(&parent.to_le_bytes());
	bytes.extend_from_slice(&0u16.to_le_bytes());
	bytes.extend_from_slice(&0u32.to_le_bytes());
	bytes.extend_from_slice(&0u32.to_le_bytes());
	bytes.extend(ptp_string(name));
	bytes.extend(ptp_string(captured));
	bytes.extend(ptp_string(""));
	bytes.extend(ptp_string(""));
	bytes
}

#[test]
fn a_device_is_usable_only_with_the_whole_read_only_subset() {
	let info = device_info(&device_dataset(&REQUIRED)).unwrap();
	assert!(info.usable());
	assert_eq!((info.manufacturer.as_str(), info.model.as_str(), info.serial.as_str()), ("Liber", "Responder", "0001"));
	assert!(!device_info(&device_dataset(&REQUIRED[..7])).unwrap().usable(), "without GetObject it is listed and not usable");
	// A count past what remains is refused before anything is kept.
	let mut hostile = device_dataset(&REQUIRED);
	hostile[11..15].copy_from_slice(&u32::MAX.to_le_bytes());
	assert_eq!(device_info(&hostile), None);
	assert_eq!(device_info(&device_dataset(&REQUIRED)[..20]), None, "a dataset that ends early");
}

#[test]
fn storage_ids_are_counted_and_bounded() {
	let mut bytes = 2u32.to_le_bytes().to_vec();
	bytes.extend(0x0001_0001u32.to_le_bytes());
	bytes.extend(0x0002_0001u32.to_le_bytes());
	assert_eq!(ids(&bytes, 32), Some(alloc::vec![0x0001_0001, 0x0002_0001]));
	assert_eq!(ids(&bytes[..8], 32), None, "a count the bytes do not carry");
	let mut many = 33u32.to_le_bytes().to_vec();
	many.extend(alloc::vec![0u8; 33 * 4]);
	assert_eq!(ids(&many, 32), None, "past the bound");
}

#[test]
fn object_info_is_read_as_far_as_it_can_be() {
	let dataset = object_dataset(0x0001_0001, 0x3801, 12_288, 0, "IMG_0007.JPG", "20260921T101500.5+0130");
	let info = object_info(&dataset).unwrap();
	assert!(info.complete);
	assert_eq!((info.storage, info.format, info.size, info.parent), (0x0001_0001, 0x3801, Some(12_288), None));
	assert_eq!(info.filename, "IMG_0007.JPG");
	assert_eq!(info.captured, Some(Time { year: 2026, month: 9, day: 21, hour: 10, minute: 15, second: 0, offset_minutes: Some(90) }));
	// The size sentinel is no size, and a parent is kept.
	let big = object_info(&object_dataset(1, 0x300b, SIZE_UNKNOWN, 44, "CLIP.MPG", "")).unwrap();
	assert_eq!((big.size, big.parent, big.captured, big.complete), (None, Some(44), None, true));
	// An unreadable time leaves the time absent and the record partial - nothing else is lost.
	let odd = object_info(&object_dataset(1, 0x3801, 10, 0, "A.JPG", "yesterday")).unwrap();
	assert_eq!((odd.captured, odd.complete, odd.filename.as_str()), (None, false, "A.JPG"));
	// A filename that is not UCS-2 text.
	let mut unpaired = object_dataset(1, 0x3801, 10, 0, "AB", "");
	unpaired[OBJECT_INFO_FIXED + 1..OBJECT_INFO_FIXED + 3].copy_from_slice(&0xd800u16.to_le_bytes());
	let unpaired = object_info(&unpaired).unwrap();
	assert_eq!((unpaired.filename.as_str(), unpaired.complete), ("", false));
	// A dataset that ends after the fixed part: the typed numbers, and a partial record.
	let cut = object_info(&dataset[..OBJECT_INFO_FIXED]).unwrap();
	assert_eq!((cut.format, cut.complete), (0x3801, false));
	assert_eq!(object_info(&dataset[..OBJECT_INFO_FIXED - 1]), None, "without the fixed part nothing is known");
	assert!(format_known(0x3801) && format_known(0x3000) && !format_known(0x3806) && !format_known(0xb101));
}

#[test]
fn times_are_the_standard_form_or_nothing() {
	assert_eq!(time("20260101T000000"), Some(Time { year: 2026, month: 1, day: 1, hour: 0, minute: 0, second: 0, offset_minutes: None }));
	assert_eq!(time("20260101T000000Z").unwrap().offset_minutes, Some(0));
	assert_eq!(time("20260101T000000-0800").unwrap().offset_minutes, Some(-480));
	for bad in ["2026-01-01", "20261301T000000", "20260101T240000", "20260101T000000.", "20260101T000000+08", "20260101X000000"] {
		assert_eq!(time(bad), None, "{bad}");
	}
}

#[test]
fn events_are_whole_containers_of_at_most_three_parameters() {
	let added = container(EVENT, OBJECT_ADDED, 0, &9u32.to_le_bytes());
	assert_eq!(event(&added), Some(Event { code: OBJECT_ADDED, transaction: 0, params: [9, 0, 0], count: 1 }));
	assert_eq!(event(&added[..14]), None);
	assert_eq!(event(&container(RESPONSE, OK, 0, &[])), None);
	assert_eq!(event(&container(EVENT, OBJECT_ADDED, 0, &[0; 16])), None, "four parameters");
}

#[test]
fn a_revision_changes_with_any_byte() {
	let dataset = object_dataset(1, 0x3801, 10, 0, "A.JPG", "");
	let mut changed = dataset.clone();
	*changed.last_mut().unwrap() ^= 1;
	assert_ne!(revision(&dataset), revision(&changed));
	assert_eq!(revision(&dataset), revision(&dataset.clone()));
}

use super::*;

fn decode(decoder: &mut Decoder, packets: &[[u8; 4]], now: u64) -> Vec<Output> {
	let mut out = Vec::new();
	decoder.decode(&packets.concat(), now, &mut out);
	out
}

fn chunk(output: &Output) -> Chunk {
	match output {
		Output::Chunk(chunk) => *chunk,
		other => panic!("a chunk, not {other:?}"),
	}
}

#[test]
fn short_messages_decode_exactly_and_padding_is_checked() {
	let mut decoder = Decoder::new(1);
	let out = decode(&mut decoder, &[[0x09, 0x90, 0x3c, 0x64], [0x0c, 0xc0, 0x05, 0x00], [0x0f, 0xf8, 0x00, 0x00]], 0);
	assert_eq!(out.len(), 3);
	assert_eq!((chunk(&out[0]).kind, chunk(&out[0]).bytes()), (Kind::Short, &[0x90, 0x3c, 0x64][..]));
	assert_eq!(chunk(&out[1]).bytes(), &[0xc0, 0x05]);
	assert_eq!((chunk(&out[2]).kind, chunk(&out[2]).bytes()), (Kind::Short, &[0xf8][..]), "a realtime byte is a complete message");
	assert_eq!(decode(&mut decoder, &[[0x0c, 0xc0, 0x05, 0x01]], 0), alloc::vec![Output::Fault { cable: Some(0), code: FaultCode::Padding }]);
	assert_eq!(decode(&mut decoder, &[[0x09, 0x80, 0x3c, 0x64]], 0), alloc::vec![Output::Fault { cable: Some(0), code: FaultCode::Status }], "a status that does not match its code index");
	assert_eq!(decode(&mut decoder, &[[0x09, 0x90, 0xbc, 0x64]], 0), alloc::vec![Output::Fault { cable: Some(0), code: FaultCode::Status }], "a data byte with the top bit set");
	assert_eq!(decode(&mut decoder, &[[0x00, 0x00, 0x00, 0x00]], 0), alloc::vec![Output::Fault { cable: Some(0), code: FaultCode::Reserved }]);
}

#[test]
fn a_sysex_is_fragments_under_one_number_and_realtime_passes_through() {
	let mut decoder = Decoder::new(1);
	let out = decode(&mut decoder, &[[0x04, 0xf0, 0x7e, 0x7f], [0x0f, 0xf8, 0x00, 0x00], [0x04, 0x06, 0x01, 0x02], [0x06, 0x03, 0xf7, 0x00]], 0);
	let kinds: Vec<(Kind, bool, bool)> = out.iter().map(|output| chunk(output)).map(|chunk| (chunk.kind, chunk.start, chunk.end)).collect();
	assert_eq!(kinds, alloc::vec![(Kind::SysexStart, true, false), (Kind::Short, false, false), (Kind::SysexContinue, false, false), (Kind::SysexEnd, false, true)]);
	assert_eq!(chunk(&out[0]).message, Some(0));
	assert_eq!(chunk(&out[2]).message, Some(0), "the realtime byte neither ended nor counted towards it");
	// A one-fragment SysEx carries both flags; the next message is the next number.
	let one = decode(&mut decoder, &[[0x06, 0xf0, 0xf7, 0x00], [0x07, 0xf0, 0x01, 0xf7]], 0);
	assert_eq!(one.iter().map(|output| (chunk(output).message, chunk(output).start, chunk(output).end)).collect::<Vec<_>>(), alloc::vec![(Some(1), true, true), (Some(2), true, true)]);
}

#[test]
fn cables_are_independent_and_their_order_is_kept() {
	let mut decoder = Decoder::new(2);
	let out = decode(&mut decoder, &[[0x04, 0xf0, 0x01, 0x02], [0x19, 0x91, 0x40, 0x40], [0x05, 0xf7, 0x00, 0x00]], 0);
	// Cable 1's note did not interrupt cable 0's SysEx, and the batch's order is the output's order.
	assert_eq!(out.iter().map(|output| (chunk(output).cable, chunk(output).kind)).collect::<Vec<_>>(), alloc::vec![(0, Kind::SysexStart), (1, Kind::Short), (0, Kind::SysexEnd)]);
}

#[test]
fn the_single_byte_form_is_tracked_like_packets() {
	let mut decoder = Decoder::new(1);
	let out = decode(&mut decoder, &[[0x0f, 0xf0, 0, 0], [0x0f, 0x01, 0, 0], [0x0f, 0xfe, 0, 0], [0x0f, 0x02, 0, 0], [0x0f, 0xf7, 0, 0]], 0);
	let kinds: Vec<Kind> = out.iter().map(|output| chunk(output).kind).collect();
	assert_eq!(kinds, alloc::vec![Kind::SysexStart, Kind::SysexContinue, Kind::Short, Kind::SysexContinue, Kind::SysexEnd]);
	// Outside a SysEx a data byte and a status byte are raw bytes - NOT assumed realtime.
	let raw = decode(&mut decoder, &[[0x0f, 0x90, 0, 0], [0x0f, 0x3c, 0, 0]], 0);
	assert!(raw.iter().all(|output| chunk(output).kind == Kind::Raw));
	// A stray end ends nothing.
	assert_eq!(decode(&mut decoder, &[[0x0f, 0xf7, 0, 0]], 0), alloc::vec![Output::Fault { cable: Some(0), code: FaultCode::Status }]);
}

#[test]
fn an_interrupted_or_restarted_sysex_is_aborted_and_its_remainder_discarded() {
	let mut decoder = Decoder::new(1);
	decode(&mut decoder, &[[0x04, 0xf0, 0x01, 0x02]], 0);
	let out = decode(&mut decoder, &[[0x09, 0x90, 0x3c, 0x64], [0x04, 0x03, 0x04, 0x05], [0x05, 0xf7, 0, 0]], 1);
	assert_eq!(out[0], Output::Abort { cable: 0, message: 0, reason: AbortReason::Interrupted });
	assert_eq!(chunk(&out[1]).kind, Kind::Short);
	assert_eq!(out.len(), 2, "the remainder and its end were discarded, and the end is NOT a successful one");
	// With nothing open and nothing being discarded, a continuation is a fault.
	assert_eq!(decode(&mut decoder, &[[0x04, 0x01, 0x02, 0x03]], 2), alloc::vec![Output::Fault { cable: Some(0), code: FaultCode::Status }]);
	// A new start while one is open restarts: the old one is aborted, the new one numbered anew.
	decode(&mut decoder, &[[0x04, 0xf0, 0x01, 0x02]], 3);
	let restart = decode(&mut decoder, &[[0x04, 0xf0, 0x03, 0x04]], 4);
	assert_eq!(restart[0], Output::Abort { cable: 0, message: 1, reason: AbortReason::Restarted });
	assert_eq!(chunk(&restart[1]).message, Some(2));
	// And recovery: a message ends normally after all of that.
	assert_eq!(chunk(&decode(&mut decoder, &[[0x05, 0xf7, 0, 0]], 5)[0]).kind, Kind::SysexEnd);
}

#[test]
fn the_cap_is_counted_across_many_small_fragments_exactly() {
	// Exactly 64 kB with the delimiters: a three-byte start, 21 844 three-byte continuations, a one-byte end.
	let mut decoder = Decoder::new(1);
	let mut out = decode(&mut decoder, &[[0x04, 0xf0, 0, 0]], 0);
	for _ in 0..21_844 {
		decoder.decode(&[0x04, 0, 0, 0], 0, &mut out);
	}
	decoder.decode(&[0x05, 0xf7, 0, 0], 0, &mut out);
	assert_eq!(out.last().map(chunk).map(|chunk| chunk.kind), Some(Kind::SysexEnd), "exactly the cap ends normally");
	assert!(!out.iter().any(|output| matches!(output, Output::Abort { .. })));
	// One continuation more crosses it: aborted by number, and its end discarded.
	let mut over = Decoder::new(1);
	let mut out = decode(&mut over, &[[0x04, 0xf0, 0, 0]], 0);
	for _ in 0..21_845 {
		over.decode(&[0x04, 0, 0, 0], 0, &mut out);
	}
	over.decode(&[0x05, 0xf7, 0, 0], 0, &mut out);
	assert_eq!(out.last(), Some(&Output::Abort { cable: 0, message: 0, reason: AbortReason::Cap }));
}

#[test]
fn two_quiet_seconds_abort_an_open_sysex() {
	let mut decoder = Decoder::new(1);
	decode(&mut decoder, &[[0x04, 0xf0, 0x01, 0x02]], 100);
	assert_eq!(decoder.next_deadline(), Some(100 + SYSEX_IDLE_TICKS));
	let mut out = Vec::new();
	decoder.tick(100 + SYSEX_IDLE_TICKS - 1, &mut out);
	assert!(out.is_empty());
	decoder.tick(100 + SYSEX_IDLE_TICKS, &mut out);
	assert_eq!(out, alloc::vec![Output::Abort { cable: 0, message: 0, reason: AbortReason::Inactivity }]);
	assert_eq!(decoder.next_deadline(), None);
}

#[test]
fn what_cannot_be_attributed_resets_every_cable() {
	let mut decoder = Decoder::new(2);
	decode(&mut decoder, &[[0x04, 0xf0, 0x01, 0x02], [0x14, 0xf0, 0x01, 0x02]], 0);
	// A batch that is not whole packets: nothing of it is emitted, and both SysEx are aborted.
	let mut out = Vec::new();
	decoder.decode(&[0x09, 0x90, 0x3c, 0x64, 0x00], 0, &mut out);
	assert_eq!(
		out,
		alloc::vec![
			Output::Abort { cable: 0, message: 0, reason: AbortReason::Reset },
			Output::Abort { cable: 1, message: 0, reason: AbortReason::Reset },
			Output::Fault { cable: None, code: FaultCode::Alignment }
		]
	);
	// A cable the endpoint does not have is not attributable either.
	decode(&mut decoder, &[[0x04, 0xf0, 0x01, 0x02]], 0);
	let out = decode(&mut decoder, &[[0x39, 0x90, 0x3c, 0x64]], 0);
	assert_eq!(out, alloc::vec![Output::Abort { cable: 0, message: 1, reason: AbortReason::Reset }, Output::Fault { cable: None, code: FaultCode::Cable }]);
	// A reserved code on a KNOWN cable resets that cable alone.
	decode(&mut decoder, &[[0x04, 0xf0, 0x01, 0x02], [0x14, 0xf0, 0x01, 0x02]], 0);
	let out = decode(&mut decoder, &[[0x10, 0, 0, 0], [0x05, 0xf7, 0, 0]], 0);
	assert_eq!(out[0], Output::Abort { cable: 1, message: 1, reason: AbortReason::Malformed });
	assert_eq!(out[1], Output::Fault { cable: Some(1), code: FaultCode::Reserved });
	assert_eq!(chunk(&out[2]).kind, Kind::SysexEnd, "cable 0's message was untouched and ends");
}

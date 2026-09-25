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

// A MIDI Streaming configuration the way usb_f_midi builds one: an audio control interface, then a MIDI
// Streaming interface whose header says 1.0, a bulk OUT with one embedded jack and a bulk IN with two.
fn streaming(revision: u16, in_cables: u8) -> Vec<u8> {
	use crate::descriptor;
	use crate::usb_function::{DT_CS_ENDPOINT, DT_CS_INTERFACE};
	let mut out: Vec<u8> = alloc::vec![9, descriptor::DT_CONFIG, 0, 0, 2, 1, 0, 0x80, 50];
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 0, CLASS_AUDIO, 0x01, 0, 0]);
	out.extend_from_slice(&[9, DT_CS_INTERFACE, 0x01, 0x00, 0x01, 9, 0, 1, 1]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 0, 2, CLASS_AUDIO, SUBCLASS_MIDI_STREAMING, 0, 0]);
	let revision = revision.to_le_bytes();
	out.extend_from_slice(&[7, DT_CS_INTERFACE, MS_HEADER, revision[0], revision[1], 0, 0]);
	out.extend_from_slice(&[9, descriptor::DT_ENDPOINT, 0x01, 0x02, 0x40, 0x00, 0, 0, 0]);
	out.extend_from_slice(&[5, DT_CS_ENDPOINT, MS_GENERAL, 1, 1]);
	out.extend_from_slice(&[9, descriptor::DT_ENDPOINT, 0x82, 0x02, 0x40, 0x00, 0, 0, 0]);
	let mut general: Vec<u8> = alloc::vec![4 + in_cables, DT_CS_ENDPOINT, MS_GENERAL, in_cables];
	general.extend((0..in_cables).map(|jack| 3 + jack));
	out.extend_from_slice(&general);
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out
}

#[test]
fn the_midi_1_0_receive_endpoint_binds_with_its_cable_count() {
	let bound = bind(&streaming(MS_REVISION_1_0, 2)).expect("a MIDI 1.0 device binds");
	assert_eq!((bound.interface, bound.alternate, bound.input.address, bound.cables), (1, 0, 0x82, 2));
	// AND THE SAME SETTING'S OUT ENDPOINT, with the one cable ITS record declares - not the receive side's two.
	assert_eq!(bound.output.map(|(endpoint, cables)| (endpoint.address, cables)), Some((0x01, 1)));
}

#[test]
fn a_midi_2_0_setting_is_not_read_as_event_packets() {
	assert_eq!(bind(&streaming(0x0200, 2)), Err(NotBindable::NoMidiInterface));
}

#[test]
fn a_cable_count_a_packet_cannot_name_is_refused() {
	assert_eq!(bind(&streaming(MS_REVISION_1_0, 0)), Err(NotBindable::BadCableCount));
	assert_eq!(bind(&streaming(MS_REVISION_1_0, 17)), Err(NotBindable::BadCableCount));
}

// ---------------------------------------------------------------------------------------------
// The way out.
// ---------------------------------------------------------------------------------------------

fn short(cable: u8, bytes: &[u8]) -> Chunk {
	let mut chunk = Chunk { cable, kind: Kind::Short, bytes: [0; 3], len: bytes.len() as u8, message: None, start: false, end: false };
	chunk.bytes[..bytes.len()].copy_from_slice(bytes);
	chunk
}

fn fragment(cable: u8, message: u32, bytes: &[u8], start: bool, end: bool) -> Chunk {
	let kind = if end {
		Kind::SysexEnd
	} else if start {
		Kind::SysexStart
	} else {
		Kind::SysexContinue
	};
	let mut chunk = Chunk { cable, kind, bytes: [0; 3], len: bytes.len() as u8, message: Some(message), start, end };
	chunk.bytes[..bytes.len()].copy_from_slice(bytes);
	chunk
}

#[test]
fn every_short_message_gets_the_code_index_its_status_demands() {
	let mut encoder = Encoder::new(2);
	let cases: [(&[u8], [u8; 4]); 12] = [
		(&[0x80, 0x3c, 0x00], [0x08, 0x80, 0x3c, 0x00]),
		(&[0x90, 0x3c, 0x64], [0x09, 0x90, 0x3c, 0x64]),
		(&[0xa0, 0x3c, 0x10], [0x0a, 0xa0, 0x3c, 0x10]),
		(&[0xb1, 0x07, 0x64], [0x0b, 0xb1, 0x07, 0x64]),
		(&[0xc0, 0x05], [0x0c, 0xc0, 0x05, 0x00]),
		(&[0xd0, 0x40], [0x0d, 0xd0, 0x40, 0x00]),
		(&[0xe0, 0x00, 0x40], [0x0e, 0xe0, 0x00, 0x40]),
		(&[0xf1, 0x12], [0x02, 0xf1, 0x12, 0x00]),
		(&[0xf2, 0x01, 0x02], [0x03, 0xf2, 0x01, 0x02]),
		(&[0xf3, 0x04], [0x02, 0xf3, 0x04, 0x00]),
		(&[0xf6], [0x05, 0xf6, 0x00, 0x00]),
		(&[0xf8], [0x0f, 0xf8, 0x00, 0x00]),
	];
	for (bytes, packet) in cases {
		assert_eq!(encoder.encode(&short(0, bytes)), Ok(packet), "{bytes:02x?}");
	}
	assert_eq!(encoder.encode(&short(1, &[0x91, 0x40, 0x7f])), Ok([0x19, 0x91, 0x40, 0x7f]), "the cable is the header's high nibble");
}

#[test]
fn a_chunk_that_is_not_a_message_has_no_packet() {
	let mut encoder = Encoder::new(2);
	for bytes in [&[0x90, 0x3c][..], &[0xc0, 0x05, 0x01][..], &[0x90, 0xbc, 0x64][..], &[0x3c, 0x64][..], &[0xf4][..], &[0xf5][..], &[0xf0, 0x7e, 0x7f][..], &[0xf7][..], &[][..]] {
		assert_eq!(encoder.encode(&short(0, bytes)), Err(Refusal::Status), "{bytes:02x?}");
	}
	assert_eq!(encoder.encode(&short(2, &[0x90, 0x3c, 0x64])), Err(Refusal::Cable), "a cable past the endpoint's two");
	let raw = Chunk { kind: Kind::Raw, ..short(0, &[0x42]) };
	assert_eq!(encoder.encode(&raw), Err(Refusal::Status), "a raw byte has no packet on the way out");
}

#[test]
fn a_sysex_goes_out_as_the_sender_cut_it_and_nothing_else() {
	let mut encoder = Encoder::new(1);
	assert_eq!(encoder.encode(&fragment(0, 5, &[0xf0, 0x7e, 0x7f], true, false)), Ok([0x04, 0xf0, 0x7e, 0x7f]));
	assert!(encoder.open(), "the message is open until its end");
	assert_eq!(encoder.encode(&short(0, &[0xf8])), Ok([0x0f, 0xf8, 0x00, 0x00]), "a realtime byte may stand inside it");
	assert_eq!(encoder.encode(&short(0, &[0x90, 0x3c, 0x64])), Err(Refusal::Sequence), "anything else would end it on the device");
	assert_eq!(encoder.encode(&fragment(0, 6, &[0x01, 0x02, 0x03], false, false)), Err(Refusal::Sequence), "another message's number");
	assert_eq!(encoder.encode(&fragment(0, 5, &[0x01, 0x02], false, false)), Err(Refusal::Sequence), "a fragment short of three that does not end it");
	assert_eq!(encoder.encode(&fragment(0, 5, &[0x01, 0x82, 0x03], false, false)), Err(Refusal::Sequence), "a status byte inside");
	assert_eq!(encoder.encode(&fragment(0, 5, &[0xf0, 0x02, 0x03], true, false)), Err(Refusal::Sequence), "a start while one is open");
	assert_eq!(encoder.encode(&fragment(0, 5, &[0x01, 0x02, 0x03], false, false)), Ok([0x04, 0x01, 0x02, 0x03]));
	assert_eq!(encoder.encode(&fragment(0, 5, &[0x06, 0xf7], false, true)), Ok([0x06, 0x06, 0xf7, 0x00]));
	assert!(!encoder.open());
	assert_eq!(encoder.encode(&fragment(0, 5, &[0xf7], false, true)), Err(Refusal::Sequence), "an end with nothing open");
	// ONE FRAGMENT, BOTH ENDS, and the three end sizes.
	assert_eq!(encoder.encode(&fragment(0, 7, &[0xf0, 0xf7], true, true)), Ok([0x06, 0xf0, 0xf7, 0x00]));
	assert_eq!(encoder.encode(&fragment(0, 8, &[0xf0, 0x01, 0xf7], true, true)), Ok([0x07, 0xf0, 0x01, 0xf7]));
	assert_eq!(encoder.encode(&fragment(0, 9, &[0xf0, 0x01, 0x02], true, false)), Ok([0x04, 0xf0, 0x01, 0x02]));
	assert_eq!(encoder.encode(&fragment(0, 9, &[0xf7], false, true)), Ok([0x05, 0xf7, 0x00, 0x00]));
	// A KIND THAT DISAGREES WITH ITS FLAGS is refused rather than believed either way.
	let lying = Chunk { kind: Kind::SysexContinue, ..fragment(0, 10, &[0xf0, 0x01, 0x02], true, false) };
	assert_eq!(encoder.encode(&lying), Err(Refusal::Sequence));
}

#[test]
fn the_cap_counts_what_went_out_with_its_delimiters() {
	let mut encoder = Encoder::new(1);
	assert!(encoder.encode(&fragment(0, 1, &[0xf0, 0x00, 0x00], true, false)).is_ok());
	// 3 bytes are out; 21844 more fragments of three reach 65535, one short of the cap.
	for _ in 0..21_844 {
		assert!(encoder.encode(&fragment(0, 1, &[0x00, 0x00, 0x00], false, false)).is_ok());
	}
	assert_eq!(encoder.encode(&fragment(0, 1, &[0x00, 0x00, 0x00], false, false)), Err(Refusal::Cap), "three more would cross 64 kB");
	assert_eq!(encoder.encode(&fragment(0, 1, &[0xf7], false, true)), Ok([0x05, 0xf7, 0x00, 0x00]), "and the end that makes it exactly 64 kB is not");
}

#[test]
fn a_batch_is_refused_whole_and_the_state_does_not_move() {
	let mut encoder = Encoder::new(2);
	let mut out = Vec::new();
	let batch = [short(0, &[0x90, 0x3c, 0x64]), fragment(1, 3, &[0xf0, 0x01, 0x02], true, false), short(0, &[0x80, 0x3c, 0x00]), short(1, &[0xc0, 0x05])];
	assert_eq!(encoder.encode_all(&batch, &mut out), Err((3, Refusal::Sequence)), "the fourth chunk would interrupt cable 1's message");
	assert!(out.is_empty() && !encoder.open(), "nothing of the batch went out, and cable 1 has no message open");
	assert_eq!(encoder.encode_all(&batch[..3], &mut out), Ok(()));
	assert_eq!(out, [[0x09, 0x90, 0x3c, 0x64], [0x14, 0xf0, 0x01, 0x02], [0x08, 0x80, 0x3c, 0x00]].concat());
	assert!(encoder.open());
}

#[test]
fn what_goes_out_decodes_back_to_what_was_sent() {
	// A PHRASE OVER TWO CABLES with every kind the encoder takes, through the encoder and then the decoder the
	// receiving side runs: the same chunks come back, message numbers apart - those are each side's own.
	let sent = [
		short(0, &[0x90, 0x3c, 0x64]),
		fragment(0, 40, &[0xf0, 0x7e, 0x7f], true, false),
		short(0, &[0xf8]),
		fragment(0, 40, &[0x06, 0x01, 0x02], false, false),
		short(1, &[0xb1, 0x07, 0x64]),
		fragment(0, 40, &[0x03, 0xf7], false, true),
		short(1, &[0xf2, 0x10, 0x20]),
		fragment(1, 2, &[0xf0, 0x55, 0xf7], true, true),
		short(0, &[0xc0, 0x05]),
		short(1, &[0xf6]),
	];
	let mut encoder = Encoder::new(2);
	let mut packets = Vec::new();
	encoder.encode_all(&sent, &mut packets).expect("every chunk is a message");
	let mut decoder = Decoder::new(2);
	let mut out = Vec::new();
	decoder.decode(&packets, 0, &mut out);
	let back: Vec<Chunk> = out.iter().map(chunk).collect();
	assert_eq!(back.len(), sent.len());
	for (got, want) in back.iter().zip(sent.iter()) {
		assert_eq!((got.cable, got.kind, got.bytes(), got.start, got.end), (want.cable, want.kind, want.bytes(), want.start, want.end));
		assert_eq!(got.message.is_some(), want.message.is_some());
	}
}

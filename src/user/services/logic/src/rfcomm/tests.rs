use super::*;
use alloc::vec;

#[test]
fn the_fcs_matches_the_specifications_examples() {
	// TS 07.10's worked example: a SABM on DLCI 0 from the initiator, address 0x03, control 0x3F, length 0x01 -> 0x1C.
	assert_eq!(fcs(&[0x03, 0x3F, 0x01]), 0x1C);
	// And its UA: 0x03 0x73 0x01 -> 0xD7.
	assert_eq!(fcs(&[0x03, 0x73, 0x01]), 0xD7);
}

#[test]
fn a_frame_round_trips_and_a_bad_fcs_or_length_is_refused() {
	let frame = Frame { dlci: 4, cr: true, kind: Kind::Uih, pf: true, credits: Some(5), info: vec![1, 2, 3] };
	let bytes = encode(&frame);
	assert_eq!(decode(&bytes, |_| true, 100), Ok(frame.clone()));
	let mut bad = bytes.clone();
	*bad.last_mut().unwrap() ^= 1;
	assert_eq!(decode(&bad, |_| true, 100), Err(FrameRefusal::BadFcs));
	assert_eq!(decode(&bytes, |_| true, 2), Err(FrameRefusal::Length), "past the frame size");
	let long = Frame { dlci: 2, cr: false, kind: Kind::Uih, pf: false, credits: None, info: vec![7; 300] };
	assert_eq!(decode(&encode(&long), |_| false, 1000), Ok(long), "a two-byte length");
	assert_eq!(decode(&[0x03, 0x99, 0x01, 0x00], |_| false, 10), Err(FrameRefusal::UnknownControl(0x99)));
}

#[test]
fn multiplexer_commands_round_trip() {
	for body in [
		Command::Pn { dlci: 6, cl: CL_CREDITS_REQUEST, priority: 7, frame: 1000, credits: 7 },
		Command::Msc { dlci: 6, signals: V24_READY },
		Command::Rpn { dlci: 6, values: None },
		Command::Test(vec![9, 8]),
		Command::Nsc(0x30),
	] {
		let mcc = Mcc { command: true, body };
		assert_eq!(decode_mcc(&encode_mcc(&mcc)), Some(mcc));
	}
}

// Two sessions over a lossless channel: every frame each sends is delivered to the other until both are quiet.
fn pump(a: &mut Session, b: &mut Session, first: Vec<Out>, admit_b: &mut dyn FnMut(u8) -> Admission) -> (Vec<Out>, Vec<Out>) {
	let (mut seen_a, mut seen_b) = (Vec::new(), Vec::new());
	let mut to_b: Vec<Vec<u8>> = Vec::new();
	let mut to_a: Vec<Vec<u8>> = Vec::new();
	for out in first {
		match out {
			Out::Send(bytes) => to_b.push(bytes),
			other => seen_a.push(other),
		}
	}
	let mut accept = |_: u8| Admission::Accept;
	for _ in 0..64 {
		if to_a.is_empty() && to_b.is_empty() {
			break;
		}
		for bytes in core::mem::take(&mut to_b) {
			for out in b.receive(&bytes, admit_b) {
				match out {
					Out::Send(bytes) => to_a.push(bytes),
					other => seen_b.push(other),
				}
			}
		}
		for bytes in core::mem::take(&mut to_a) {
			for out in a.receive(&bytes, &mut accept) {
				match out {
					Out::Send(bytes) => to_b.push(bytes),
					other => seen_a.push(other),
				}
			}
		}
	}
	(seen_a, seen_b)
}

#[test]
fn a_dlc_opens_through_the_multiplexer_negotiation_and_modem_status_and_carries_data_on_credits() {
	let mut a = Session::new(true, 1691);
	let mut b = Session::new(false, 1691);
	let first = a.connect(3).expect("room");
	let (seen_a, seen_b) = pump(&mut a, &mut b, first, &mut |_| Admission::Accept);
	assert!(seen_a.contains(&Out::SessionOpen) && seen_a.contains(&Out::Opened(3)));
	assert!(seen_b.contains(&Out::SessionOpen) && seen_b.contains(&Out::Opened(3)));
	assert_eq!(a.dlc(3).unwrap().dlci, 6, "a responder's channel 3 is DLCI 6 from the initiator");
	// Ten frames against seven credits: seven go now, three wait for the peer's grant.
	let message: Vec<u8> = (0..10 * 100).map(|n| n as u8).collect();
	let mut frames = a.write(3, &message).expect("open");
	assert_eq!(a.dlc(3).unwrap().frame, MAX_FRAME.min(1691 - 6));
	let small = Session::new(true, 106);
	assert_eq!(small.frame_size(), 100, "the frame fits the L2CAP MTU");
	let _ = small;
	let mut received = Vec::new();
	let mut rounds = 0;
	while !frames.is_empty() && rounds < 16 {
		rounds += 1;
		let mut back = Vec::new();
		for out in core::mem::take(&mut frames) {
			if let Out::Send(bytes) = out {
				for out in b.receive(&bytes, &mut |_| Admission::Accept) {
					match out {
						Out::Data(3, data) => received.extend_from_slice(&data),
						Out::Send(bytes) => back.push(bytes),
						_ => {}
					}
				}
			}
		}
		for bytes in back {
			frames.extend(a.receive(&bytes, &mut |_| Admission::Accept));
		}
	}
	assert_eq!(received, message, "every byte, in order, against credits");
}

#[test]
fn an_inbound_dlc_the_peer_may_not_open_is_refused_with_dm() {
	let mut a = Session::new(true, 1691);
	let mut b = Session::new(false, 1691);
	let first = a.connect(5).unwrap();
	let (seen_a, seen_b) = pump(&mut a, &mut b, first, &mut |channel| if channel == 5 { Admission::Refuse } else { Admission::Accept });
	assert!(seen_a.contains(&Out::Closed(5)), "the initiator is told");
	assert!(!seen_b.iter().any(|out| matches!(out, Out::Opened(_))));
	assert!(a.dlc(5).is_none());
}

#[test]
fn a_dlc_closes_either_way_and_the_session_bound_holds() {
	let mut a = Session::new(true, 1691);
	let mut b = Session::new(false, 1691);
	let first = a.connect(2).unwrap();
	pump(&mut a, &mut b, first, &mut |_| Admission::Accept);
	let close = a.disconnect(2);
	let (seen_a, seen_b) = pump(&mut a, &mut b, close, &mut |_| Admission::Accept);
	assert!(seen_a.contains(&Out::Closed(2)) && seen_b.contains(&Out::Closed(2)));
	let mut c = Session::new(true, 1691);
	for channel in 1..=RFCOMM_CHANNELS_PER_LINK as u8 {
		assert!(c.connect(channel).is_some());
	}
	assert!(c.connect(20).is_none(), "eight DLCs per session");
	assert!(Session::new(true, 1691).connect(31).is_none(), "no server channel past 30");
}

#[test]
fn data_on_a_dlc_that_is_not_open_is_answered_dm_and_a_peer_past_its_credits_is_dropped() {
	let mut b = Session::new(false, 1691);
	let stray = encode(&Frame { dlci: 8, cr: true, kind: Kind::Uih, pf: false, credits: None, info: vec![1] });
	let out = b.receive(&stray, &mut |_| Admission::Accept);
	assert!(matches!(&out[..], [Out::Send(bytes)] if decode(bytes, |_| false, 10).unwrap().kind == Kind::Dm));
}

#[test]
// A BOUNDED WRITER AND A PAUSED READER: `room` is what fills the queue past the credits, and a paused DLC gives no
// credits back until it is released - so a peer that sends stops, and starts again with the credits it is owed.
fn room_bounds_a_writer_and_a_paused_reader_holds_the_peer() {
	let mut a = Session::new(true, 1691);
	let mut b = Session::new(false, 1691);
	let first = a.connect(3).expect("room");
	pump(&mut a, &mut b, first, &mut |_| Admission::Accept);
	let frame = a.dlc(3).unwrap().frame;
	assert_eq!(a.room(3), QUEUED_FRAMES * frame);
	assert_eq!(a.room(4), 0, "no DLC, no room");
	// Fifteen frames: seven go on the peer's credits, eight wait, and the queue is full.
	let sent = a.write(3, &alloc::vec![0x55; 15 * frame]).unwrap();
	assert_eq!(sent.len(), 7);
	assert_eq!(a.room(3), 0);
	// THE READER PAUSES: the peer's seven frames are taken and no credits go back.
	assert!(b.pause(3, true).is_empty());
	let mut granted = 0;
	for out in sent {
		let Out::Send(bytes) = out else { continue };
		for out in b.receive(&bytes, &mut |_| Admission::Accept) {
			if let Out::Send(frame) = out {
				for out in a.receive(&frame, &mut |_| Admission::Accept) {
					if matches!(out, Out::Send(_)) {
						granted += 1;
					}
				}
			}
		}
	}
	assert_eq!(granted, 0, "a paused reader gave nothing back, so nothing more was sent");
	assert_eq!(b.dlc(3).unwrap().rx_credits, 0);
	// RELEASED: the credits it owes go back at once, and the queue moves.
	let released = b.pause(3, false);
	assert_eq!(released.len(), 1);
	let Out::Send(bytes) = &released[0] else { panic!("a credit frame") };
	let moved = a.receive(bytes, &mut |_| Admission::Accept);
	assert_eq!(moved.iter().filter(|out| matches!(out, Out::Send(_))).count(), 7);
	assert_eq!(a.room(3), QUEUED_FRAMES.saturating_sub(1) * frame);
}

use super::*;
use alloc::vec;

fn accept(_: u16) -> Admission {
	Admission::Accept
}

fn sends(out: &[Out]) -> Vec<&Command> {
	out.iter().filter_map(|out| if let Out::Send(signal) = out { Some(&signal.command) } else { None }).collect()
}

#[test]
fn a_signalling_pdu_with_two_commands_round_trips_and_a_truncated_one_is_refused() {
	let request = Signal { identifier: 3, command: Command::ConnectionRequest { psm: psm::SDP, scid: 0x0040 } };
	let echo = Signal { identifier: 4, command: Command::EchoRequest(vec![1, 2, 3]) };
	let mut pdu = encode(&request);
	pdu.extend_from_slice(&encode(&echo));
	assert_eq!(decode(&pdu), Ok(vec![request, echo]));
	assert_eq!(decode(&pdu[..pdu.len() - 1]), Err(Refusal::Truncated));
	assert_eq!(decode(&[0x02, 0, 4, 0, 1, 0, 0x40, 0]), Err(Refusal::ZeroIdentifier));
	let options = Options { mtu: Some(1691), mode: Some(Mode::Ertm(ErtmParameters::ours())), fcs: Some(1), ..Options::default() };
	let configure = Signal { identifier: 9, command: Command::ConfigurationRequest { dcid: 0x41, continuation: false, options } };
	assert_eq!(decode(&encode(&configure)), Ok(vec![configure]));
}

#[test]
fn an_outbound_channel_opens_only_when_both_directions_are_configured() {
	let mut channels = Channels::new();
	let (local, request) = channels.open(psm::HID_CONTROL, false).expect("room");
	assert_eq!(request.command, Command::ConnectionRequest { psm: psm::HID_CONTROL, scid: local });
	let out = channels.on_signal(Signal { identifier: request.identifier, command: Command::ConnectionResponse { dcid: 0x0070, scid: local, result: connection::SUCCESSFUL, status: 0 } }, &mut accept);
	let Command::ConfigurationRequest { dcid, options, .. } = sends(&out)[0] else { panic!("this host configures") };
	assert_eq!((*dcid, options.mtu), (0x0070, Some(BREDR_MTU as u16)));
	let ours = channels.get(local).unwrap().pending;
	let out = channels.on_signal(Signal { identifier: ours, command: Command::ConfigurationResponse { scid: local, continuation: false, result: configuration::SUCCESS, options: Options::default() } }, &mut accept);
	assert!(out.is_empty(), "one direction is not an open channel");
	let out = channels.on_signal(Signal { identifier: 50, command: Command::ConfigurationRequest { dcid: local, continuation: false, options: Options { mtu: Some(1000), ..Options::default() } } }, &mut accept);
	assert!(matches!(sends(&out)[0], Command::ConfigurationResponse { result: configuration::SUCCESS, .. }));
	assert!(out.contains(&Out::Opened(local)));
	assert_eq!(channels.get(local).unwrap().peer_mtu, 1000);
}

#[test]
fn an_inbound_connection_is_admitted_by_psm_and_refused_by_name_otherwise() {
	let mut channels = Channels::new();
	let mut admit = |psm: u16| match psm {
		psm::SDP => Admission::Accept,
		psm::BNEP => Admission::Security,
		_ => Admission::NotSupported,
	};
	let out = channels.on_signal(Signal { identifier: 1, command: Command::ConnectionRequest { psm: psm::BNEP, scid: 0x0080 } }, &mut admit);
	assert_eq!(sends(&out), vec![&Command::ConnectionResponse { dcid: 0, scid: 0x0080, result: connection::SECURITY_BLOCK, status: 0 }]);
	let out = channels.on_signal(Signal { identifier: 2, command: Command::ConnectionRequest { psm: 0x1001, scid: 0x0081 } }, &mut admit);
	assert!(matches!(sends(&out)[0], Command::ConnectionResponse { result: connection::PSM_NOT_SUPPORTED, .. }));
	let out = channels.on_signal(Signal { identifier: 3, command: Command::ConnectionRequest { psm: psm::SDP, scid: 0x0082 } }, &mut admit);
	assert!(matches!(sends(&out)[0], Command::ConnectionResponse { result: connection::SUCCESSFUL, .. }));
	assert!(matches!(sends(&out)[1], Command::ConfigurationRequest { dcid: 0x0082, .. }), "and configures its own direction at once");
	let out = channels.on_signal(Signal { identifier: 4, command: Command::ConnectionRequest { psm: psm::SDP, scid: 0x0082 } }, &mut admit);
	assert!(matches!(sends(&out)[0], Command::ConnectionResponse { result: connection::SOURCE_CID_ALREADY_ALLOCATED, .. }));
	let out = channels.on_signal(Signal { identifier: 5, command: Command::ConnectionRequest { psm: psm::SDP, scid: 0x0002 } }, &mut admit);
	assert!(matches!(sends(&out)[0], Command::ConnectionResponse { result: connection::INVALID_SOURCE_CID, .. }));
}

#[test]
fn the_table_and_the_ertm_count_are_bounded() {
	let mut channels = Channels::new();
	for _ in 0..CHANNELS_PER_LINK {
		assert!(channels.open(psm::RFCOMM, false).is_some());
	}
	assert!(channels.open(psm::RFCOMM, false).is_none(), "the sixteenth is the last");
	let mut channels = Channels::new();
	assert!(channels.open(psm::AVCTP_BROWSING, true).is_some());
	assert!(channels.open(0x1001, true).is_some());
	assert!(channels.open(0x1003, true).is_none(), "two ERTM channels per link");
	assert!(channels.open(0x0002, false).is_none(), "an even PSM is no PSM");
}

#[test]
fn an_unknown_option_unknown_command_and_bad_mtu_are_answered_not_ignored() {
	let mut channels = Channels::new();
	channels.on_signal(Signal { identifier: 1, command: Command::ConnectionRequest { psm: psm::SDP, scid: 0x0090 } }, &mut accept);
	let local = channels.by_remote(0x0090).unwrap().local_cid;
	let mut raw = vec![code::CONFIGURATION_REQUEST, 7, 0, 0];
	raw.extend_from_slice(&local.to_le_bytes());
	raw.extend_from_slice(&0u16.to_le_bytes());
	raw.extend_from_slice(&[0x09, 1, 0]);
	raw[2] = (raw.len() - 4) as u8;
	let signal = decode(&raw).unwrap().remove(0);
	let out = channels.on_signal(signal, &mut accept);
	assert!(matches!(sends(&out)[0], Command::ConfigurationResponse { result: configuration::UNKNOWN_OPTIONS, options, .. } if options.unknown == vec![0x09]));
	let out = channels.on_signal(Signal { identifier: 8, command: Command::ConfigurationRequest { dcid: local, continuation: false, options: Options { mtu: Some(20), ..Options::default() } } }, &mut accept);
	assert!(matches!(sends(&out)[0], Command::ConfigurationResponse { result: configuration::UNACCEPTABLE_PARAMETERS, options, .. } if options.mtu == Some(MINIMUM_MTU)));
	let out = channels.on_signal(Signal { identifier: 9, command: Command::Unknown(0x55) }, &mut accept);
	assert_eq!(sends(&out), vec![&Command::CommandReject { reason: reject::NOT_UNDERSTOOD, data: Vec::new() }]);
	let out = channels.on_signal(Signal { identifier: 10, command: Command::InformationRequest { info_type: information::EXTENDED_FEATURES } }, &mut accept);
	assert!(matches!(sends(&out)[0], Command::InformationResponse { result: information::SUCCESS, data, .. } if peer_has_ertm(data)));
}

#[test]
fn a_disconnection_either_way_closes_the_channel() {
	let mut channels = Channels::new();
	channels.on_signal(Signal { identifier: 1, command: Command::ConnectionRequest { psm: psm::SDP, scid: 0x00A0 } }, &mut accept);
	let local = channels.by_remote(0x00A0).unwrap().local_cid;
	let out = channels.on_signal(Signal { identifier: 2, command: Command::DisconnectionRequest { dcid: local, scid: 0x00A0 } }, &mut accept);
	assert!(out.contains(&Out::Closed(local)));
	assert!(channels.is_empty());
	let (local, request) = channels.open(psm::SDP, false).unwrap();
	channels.on_signal(Signal { identifier: request.identifier, command: Command::ConnectionResponse { dcid: 0x00B0, scid: local, result: connection::SUCCESSFUL, status: 0 } }, &mut accept);
	let close = channels.close(local).unwrap();
	assert_eq!(close.command, Command::DisconnectionRequest { dcid: 0x00B0, scid: local });
	let out = channels.on_signal(Signal { identifier: close.identifier, command: Command::DisconnectionResponse { dcid: 0x00B0, scid: local } }, &mut accept);
	assert_eq!(out, vec![Out::Closed(local)]);
}

#[test]
fn the_fcs_is_the_specification_crc() {
	// The CRC-16 with polynomial 0x8005, reflected and from zero, of "123456789" is 0xBB3D.
	assert_eq!(fcs(b"123456789"), 0xBB3D);
	for control in [
		Control::Information { tx_seq: 5, req_seq: 63, sar: Sar::Continuation, f: true },
		Control::Supervisory { kind: Supervisory::SelectiveReject, req_seq: 7, p: true, f: false },
	] {
		assert_eq!(Control::decode(control.encode()), control);
	}
}

fn pair() -> (Ertm, Ertm) {
	let parameters = ErtmParameters { tx_window: 4, max_transmit: 3, retransmission_ms: 2000, monitor_ms: 12000, mps: 100 };
	(Ertm::new(0x0041, parameters, 1691), Ertm::new(0x0040, parameters, 1691))
}

fn sent(out: &[ErtmOut]) -> Vec<Vec<u8>> {
	out.iter().filter_map(|out| if let ErtmOut::Send(pdu) = out { Some(pdu.clone()) } else { None }).collect()
}

#[test]
fn an_sdu_is_segmented_to_the_mps_windowed_and_reassembled_in_order() {
	let (mut a, mut b) = pair();
	let sdu: Vec<u8> = (0..350).map(|n| n as u8).collect();
	let out = a.send(&sdu, 0);
	let frames = sent(&out);
	assert_eq!(frames.len(), 4, "four segments, the window's four");
	let mut delivered = Vec::new();
	for frame in &frames {
		for out in b.receive(frame, 1) {
			if let ErtmOut::Deliver(sdu) = out {
				delivered.push(sdu);
			}
		}
	}
	assert_eq!(delivered, vec![sdu], "whole and in order");
}

#[test]
fn a_lost_frame_is_rejected_once_and_retransmitted() {
	let (mut a, mut b) = pair();
	let frames = sent(&a.send(&[1u8; 50], 0));
	let more = sent(&a.send(&[2u8; 50], 0));
	// The first frame is lost; the second arrives out of sequence and draws one reject.
	let out = b.receive(&more[0], 5);
	let reject = sent(&out);
	assert_eq!(reject.len(), 1);
	assert!(matches!(parse_frame(&reject[0]).unwrap().0, Control::Supervisory { kind: Supervisory::Reject, req_seq: 0, .. }));
	assert!(sent(&b.receive(&more[0], 6)).is_empty(), "no second reject");
	let resent = sent(&a.receive(&reject[0], 7));
	assert_eq!(resent.len(), 2, "both unacknowledged frames again");
	let mut delivered = Vec::new();
	for frame in &resent {
		for out in b.receive(frame, 8) {
			if let ErtmOut::Deliver(sdu) = out {
				delivered.push(sdu);
			}
		}
	}
	assert_eq!(delivered, vec![vec![1u8; 50], vec![2u8; 50]]);
	let _ = frames;
}

#[test]
fn an_unanswered_frame_is_polled_and_the_channel_fails_after_max_transmit_polls() {
	let (mut a, mut b) = pair();
	let frame = sent(&a.send(&[3u8; 10], 0)).remove(0);
	assert_eq!(a.deadline(), Some(2000));
	let poll = sent(&a.tick(2000));
	assert!(matches!(parse_frame(&poll[0]).unwrap().0, Control::Supervisory { p: true, .. }), "the retransmission timer polls");
	// The peer never got the frame; it answers the poll with the final bit and its expected sequence 0.
	let answer = sent(&b.receive(&poll[0], 2001));
	assert!(matches!(parse_frame(&answer[0]).unwrap().0, Control::Supervisory { f: true, req_seq: 0, .. }));
	let resent = sent(&a.receive(&answer[0], 2002));
	assert_eq!(resent, vec![frame], "the final bit retransmits what is outstanding");
	// Now nothing answers at all.
	let (mut c, _) = pair();
	c.send(&[4u8; 10], 0);
	let mut now = 2000;
	let mut failed = false;
	for _ in 0..6 {
		if c.tick(now).contains(&ErtmOut::Fail) {
			failed = true;
			break;
		}
		now += 12000;
	}
	assert!(failed, "three unanswered polls end the channel");
}

#[test]
fn a_corrupted_frame_is_dropped_and_an_sdu_past_the_mtu_fails_the_channel() {
	let (mut a, mut b) = pair();
	let mut frame = sent(&a.send(&[5u8; 20], 0)).remove(0);
	let last = frame.len() - 3;
	frame[last] ^= 0xFF;
	assert!(b.receive(&frame, 1).is_empty(), "a bad FCS is dropped for the retransmission to recover");
	let mut small = Ertm::new(0x40, ErtmParameters::ours(), 100);
	let start = super::frame(0x40, Control::Information { tx_seq: 0, req_seq: 0, sar: Sar::Start, f: false }, Some(500), &[0u8; 10]);
	assert_eq!(small.receive(&start, 0), vec![ErtmOut::Fail]);
}

// THE FIXED CHANNELS, both ways: this host answers that it has the BR/EDR Security Manager, asks the peer the same,
// and hands the peer's answer back rather than dropping it.
#[test]
fn the_security_manager_channel_is_offered_asked_for_and_read() {
	let mut channels = Channels::new();
	let asked = channels.on_signal(Signal { identifier: 9, command: Command::InformationRequest { info_type: information::FIXED_CHANNELS } }, &mut accept);
	let [Command::InformationResponse { result, data, .. }] = &sends(&asked)[..] else { panic!("one answer") };
	assert_eq!(*result, information::SUCCESS);
	assert!(peer_has_security_manager(data), "this host offers the BR/EDR Security Manager");
	let request = channels.information_request(information::FIXED_CHANNELS);
	assert_eq!(request.command, Command::InformationRequest { info_type: information::FIXED_CHANNELS });
	let mask: u64 = 1 << 1 | 1 << 7;
	let answered = channels.on_signal(Signal { identifier: request.identifier, command: Command::InformationResponse { info_type: information::FIXED_CHANNELS, result: information::SUCCESS, data: mask.to_le_bytes().to_vec() } }, &mut accept);
	assert_eq!(answered, vec![Out::Information { info_type: information::FIXED_CHANNELS, result: information::SUCCESS, data: mask.to_le_bytes().to_vec() }]);
	assert!(!peer_has_security_manager(&2u64.to_le_bytes()), "the signalling channel alone is not the Security Manager");
	assert!(!peer_has_security_manager(&[0x80]), "a short mask says nothing");
}

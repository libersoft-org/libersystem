use super::*;
use crate::sbc::{Allocation, Config, Mode};

#[test]
// A HEADER IS ITS LABEL, ITS KIND AND ITS SIGNAL, and a fragmented one is not read.
fn messages_round_trip_and_a_fragment_is_refused() {
	let command = Message::command(5, signal::GET_CAPABILITIES, &[1 << 2]);
	assert_eq!(command.encode(), [0x50, 0x02, 0x04]);
	assert_eq!(Message::decode(&command.encode()), Some(command));
	let accept = Message::accept(5, signal::DISCOVER, &[0x04, 0x08]);
	assert_eq!(Message::decode(&accept.encode()).map(|message| message.kind), Some(Kind::Accept));
	assert_eq!(Message::decode(&[0x54, 0x02]), None, "a start packet is not read");
	assert_eq!(Message::decode(&[0x51]).map(|message| message.kind), Some(Kind::GeneralReject));
	assert_eq!(Message::decode(&[0x50]), None);
}

#[test]
// A DISCOVERY'S ANSWER: the end-point's id, whether it is in use, its media type and whether it is a sink.
fn endpoints_are_read_as_discovery_writes_them() {
	let sink = Endpoint { seid: 1, in_use: false, media: 0, role: Role::Sink };
	let source = Endpoint { seid: 2, in_use: true, media: 0, role: Role::Source };
	let mut params = alloc::vec::Vec::new();
	params.extend_from_slice(&sink.encode());
	params.extend_from_slice(&source.encode());
	assert_eq!(sink.encode(), [0x04, 0x08]);
	assert_eq!(endpoints(&params), [sink, source]);
}

#[test]
// THE CHOICE: 48 kHz, joint stereo, sixteen blocks, eight subbands, loudness, and the highest bitpool both allow - and
// none where the two share nothing.
fn the_configuration_chosen_is_the_best_both_support() {
	let headset = Sbc { frequencies: 0x3, modes: 0xf, blocks: 0xf, subbands: 0x3, allocations: 0x3, min_bitpool: 2, max_bitpool: 250 };
	let chosen = headset.choose(&Sbc::OFFERED).unwrap();
	assert_eq!(chosen, Config { frequency: 48_000, blocks: 16, mode: Mode::JointStereo, allocation: Allocation::Loudness, subbands: 8, bitpool: 53 });
	let plain = Sbc { frequencies: 0x2, modes: 0x8, blocks: 0x4, subbands: 0x2, allocations: 0x2, min_bitpool: 10, max_bitpool: 20 };
	let chosen = plain.choose(&Sbc::OFFERED).unwrap();
	assert_eq!(chosen, Config { frequency: 44_100, blocks: 8, mode: Mode::Mono, allocation: Allocation::Snr, subbands: 4, bitpool: 20 });
	let alien = Sbc { frequencies: 0, ..headset };
	assert_eq!(alien.choose(&Sbc::OFFERED), None);
	let narrow = Sbc { min_bitpool: 60, max_bitpool: 70, ..headset };
	assert_eq!(narrow.choose(&Sbc::OFFERED), None, "no bitpool both allow");
	// A CONFIGURATION'S SINGLE FLAGS round-trip to the same configuration.
	let config = headset.choose(&Sbc::OFFERED).unwrap();
	assert_eq!(Sbc::of(&config).config(), Some(config));
	assert_eq!(Sbc::OFFERED.config(), None, "a set of flags is a capability, not a configuration");
}

#[test]
// A CAPABILITY LIST: the transport, SBC's information and delay reporting written and read back, and a list that runs
// past its own length refused.
fn capability_lists_round_trip() {
	let list = capability_list(&Sbc::OFFERED, true);
	let read = capabilities(&list).unwrap();
	assert!(read.transport && read.delay_reporting && !read.other_codec);
	assert_eq!(read.sbc, Some(Sbc::OFFERED));
	assert_eq!(Sbc::OFFERED.encode(), [0xff, 0xff, 2, 53]);
	let mut truncated = list.clone();
	truncated.pop();
	assert!(capabilities(&truncated[..truncated.len() - 3]).is_none());
	// ANOTHER CODEC is noticed and not taken for SBC.
	let aac = [category::MEDIA_CODEC, 4, 0x00, 0x02, 0x80, 0x01];
	assert_eq!(capabilities(&aac).map(|caps| (caps.sbc, caps.other_codec)), Some((None, true)));
}

#[test]
// A MEDIA PACKET: RTP version 2, the dynamic payload type, sequence and timestamp, and the frame count before the
// frames; a fragmented one is not read.
fn media_packets_round_trip() {
	let packet = media_packet(7, 1_024, 0x1122_3344, 3, &[0x9c, 1, 2]);
	assert_eq!(&packet[..2], &[0x80, 96]);
	assert_eq!(read_media(&packet), Some((7, 1_024, 3, &[0x9c, 1, 2][..])));
	let mut fragment = packet.clone();
	fragment[RTP_HEADER] |= 0x80;
	assert_eq!(read_media(&fragment), None);
	assert_eq!(frames_per_packet(672, 119), 5);
	assert_eq!(frames_per_packet(895, 57), 15, "the payload header counts at most fifteen");
	assert_eq!(frames_per_packet(20, 119), 1, "a frame that does not fit still goes alone");
}

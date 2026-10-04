use super::*;
use alloc::vec::Vec;

fn ump(words: &[u32]) -> Ump {
	Ump::new(words).expect("as many words as the type says")
}

#[test]
fn every_message_type_says_its_own_length() {
	let lengths: Vec<usize> = (0u8..16).map(words_of).collect();
	assert_eq!(lengths, [1, 1, 1, 2, 2, 4, 1, 1, 2, 2, 2, 3, 3, 4, 4, 4], "the specification's sizes, reserved types included");
	assert!(Ump::new(&[0x2090_3c40]).is_some(), "a MIDI 1.0 channel voice message is one word");
	assert!(Ump::new(&[0x4090_3c00]).is_none(), "a MIDI 2.0 one is two, and one word of it is not a message");
	assert!(Ump::new(&[]).is_none());
}

#[test]
fn a_group_is_read_where_the_type_has_one_and_never_invented() {
	assert_eq!(ump(&[0x2590_3c40]).group(), Some(5));
	assert_eq!(ump(&[0x0010_0000]).group(), None, "utility messages have none");
	assert_eq!(ump(&[0xF000_0000, 0, 0, 0]).group(), None, "nor do stream messages");
	assert_eq!(ump(&[0x2590_3c40]).on_group(9).words(), &[0x2990_3c40]);
	assert_eq!(ump(&[0x0010_0000]).on_group(9).words(), &[0x0010_0000], "a message without a group is not given one");
	assert_eq!(ump(&[0x2590_3c40]).bytes(), alloc::vec![0x40, 0x3c, 0x90, 0x25], "each word little-endian on the wire");
}

#[test]
fn a_word_stream_is_cut_into_messages_and_a_message_it_ends_inside_is_a_fault() {
	let words = [0x2090_3c40, 0x4090_3c00, 0xffff_0000, 0x10f8_0000];
	let (messages, fault) = split(&words);
	assert_eq!((messages.len(), fault), (3, None));
	assert_eq!(messages[1].words(), &[0x4090_3c00, 0xffff_0000]);
	let (kept, fault) = split(&[0x2090_3c40, 0x4090_3c00]);
	assert_eq!((kept.len(), fault), (1, Some(Fault::Truncated)), "what came before the cut is kept");
	assert_eq!(words_of_bytes(&[1, 2, 3]), Err(Fault::Alignment));
	assert_eq!(words_of_bytes(&[0x40, 0x3c, 0x90, 0x20]), Ok(alloc::vec![0x2090_3c40]));
}

// A SysEx7 message on group `group`: its part, how many bytes, and those bytes.
fn sysex7(group: u8, status: u32, bytes: &[u8]) -> Ump {
	let mut data = [0u8; 6];
	data[..bytes.len()].copy_from_slice(bytes);
	ump(&[
		3 << 28 | u32::from(group) << 24 | status << 20 | (bytes.len() as u32) << 16 | u32::from(data[0]) << 8 | u32::from(data[1]),
		u32::from_be_bytes([data[2], data[3], data[4], data[5]]),
	])
}

#[test]
fn sysex_is_counted_per_group_and_a_part_without_its_message_is_a_stray() {
	let mut tracker = Tracker::new();
	let start = sysex7(2, 1, &[1, 2, 3, 4, 5, 6]);
	assert_eq!(tracker.take(start), alloc::vec![Tracked::Message(start, Some(0))]);
	let other = sysex7(3, 0, &[9]);
	assert_eq!(tracker.take(other), alloc::vec![Tracked::Message(other, Some(0))], "another group has its own numbers");
	let end = sysex7(2, 3, &[7]);
	assert_eq!(tracker.take(end), alloc::vec![Tracked::Message(end, Some(0))]);
	let stray = sysex7(2, 2, &[8]);
	assert_eq!(tracker.take(stray), alloc::vec![Tracked::Stray(stray)], "a continuation with nothing open");
	// A START BEFORE THE END restarts: the open message is aborted by number.
	let first = sysex7(2, 1, &[1]);
	tracker.take(first);
	let again = sysex7(2, 1, &[2]);
	assert_eq!(tracker.take(again), alloc::vec![Tracked::Aborted { group: 2, number: 1, reason: Abort::Restarted }, Tracked::Message(again, Some(2))]);
	// MORE BYTES THAN A SYSEX7 CARRIES ends the message it continued.
	let mut lying = sysex7(2, 2, &[1, 2, 3, 4, 5, 6]);
	lying.words[0] |= 0x7 << 16;
	assert_eq!(tracker.take(lying), alloc::vec![Tracked::Aborted { group: 2, number: 2, reason: Abort::Malformed }]);
}

#[test]
fn sysex_past_the_cap_is_aborted_and_what_follows_is_a_stray() {
	let mut tracker = Tracker::new();
	tracker.take(sysex7(0, 1, &[0; 6]));
	let mut sent = 6u32;
	loop {
		let out = tracker.take(sysex7(0, 2, &[0; 6]));
		sent += 6;
		if sent > SYSEX_CAP {
			assert_eq!(out, alloc::vec![Tracked::Aborted { group: 0, number: 0, reason: Abort::Cap }]);
			break;
		}
		assert!(matches!(out[..], [Tracked::Message(..)]));
	}
	let end = sysex7(0, 3, &[1]);
	assert_eq!(tracker.take(end), alloc::vec![Tracked::Stray(end)], "the aborted message's end belongs to nothing");
}

#[test]
fn midi1_packets_and_midi1_in_ump_translate_both_ways() {
	let mut back = ToPackets::new();
	// Channel voice, system common and realtime, each on its cable as its group.
	for packet in [[0x39u8, 0x91, 0x3c, 0x40], [0x2C, 0xC2, 0x05, 0x00], [0x5F, 0xF8, 0x00, 0x00], [0x02, 0xF3, 0x07, 0x00], [0x05, 0xF6, 0x00, 0x00]] {
		let message = ump_of_packet(packet).expect("a message");
		assert_eq!(message.group(), Some(packet[0] >> 4), "the cable is the group");
		assert_eq!(back.take(&message), alloc::vec![packet], "and back to the same packet");
	}
	assert_eq!(ump_of_packet([0x39, 0x91, 0x3c, 0x40]).unwrap().words(), &[0x2391_3c40]);
	// SYSEX, F0 01 02 03 04 05 F7 as USB-MIDI packets: three bytes a packet, and the end with what is left.
	let packets = [[0x14u8, 0xF0, 0x01, 0x02], [0x14, 0x03, 0x04, 0x05], [0x15, 0xF7, 0x00, 0x00]];
	let messages: Vec<Ump> = packets.iter().map(|&packet| ump_of_packet(packet).expect("a SysEx part")).collect();
	assert_eq!(messages[0].words()[0] >> 20 & 0xF, 1, "the F0 makes a start");
	assert_eq!(messages[0].words()[0] >> 16 & 0xF, 2, "of the two bytes after it");
	assert_eq!(messages[2].words()[0] >> 20 & 0xF, 3, "the F7 an end, of nothing");
	let again: Vec<Packet> = messages.iter().flat_map(|message| back.take(message)).collect();
	assert_eq!(again, packets, "cut back at threes");
	// A WHOLE SYSEX IN ONE UMP comes out at threes, its last packet ending it.
	let whole = sysex7(1, 0, &[1, 2, 3, 4, 5]);
	assert_eq!(back.take(&whole), alloc::vec![[0x14, 0xF0, 1, 2], [0x14, 3, 4, 5], [0x15, 0xF7, 0, 0]]);
	assert_eq!(ump_of_packet([0x0F, 0x00, 0x00, 0x00]), None, "a code index that carries no message");
	assert!(back.take(&ump(&[0xF000_0000, 0, 0, 0])).is_empty(), "a stream message has no MIDI 1.0 form");
}

#[test]
fn midi2_channel_voice_comes_down_by_the_default_translation() {
	let message = |status: u32, index: u32, low: u32, data: u32| ump(&[4 << 28 | status << 20 | 3 << 16 | index << 8 | low, data]);
	assert_eq!(midi1_of_midi2(&message(0x9, 60, 0, 0xFFFF_0000)), alloc::vec![[0x93, 60, 127]]);
	assert_eq!(midi1_of_midi2(&message(0x9, 60, 0, 0x0100_0000)), alloc::vec![[0x93, 60, 1]], "a note on stays a note on");
	assert_eq!(midi1_of_midi2(&message(0x8, 60, 0, 0x8000_0000)), alloc::vec![[0x83, 60, 64]]);
	assert_eq!(midi1_of_midi2(&message(0xB, 7, 0, 0x8000_0000)), alloc::vec![[0xB3, 7, 64]]);
	assert_eq!(midi1_of_midi2(&message(0xE, 0, 0, 0x8000_0000)), alloc::vec![[0xE3, 0, 64]], "pitch bend's centre is the centre");
	assert_eq!(midi1_of_midi2(&message(0xC, 0, 1, 0x0500_0203)), alloc::vec![[0xB3, 0, 2], [0xB3, 32, 3], [0xC3, 5, 0]], "the bank first when it is valid");
	assert_eq!(midi1_of_midi2(&message(0xC, 0, 0, 0x0500_0203)), alloc::vec![[0xC3, 5, 0]]);
	assert_eq!(midi1_of_midi2(&message(0x2, 1, 2, 0x8000_0000)), alloc::vec![[0xB3, 101, 1], [0xB3, 100, 2], [0xB3, 6, 64], [0xB3, 38, 0]], "a registered controller as its sequence");
	assert!(midi1_of_midi2(&message(0x0, 60, 1, 0)).is_empty(), "a per-note controller has no MIDI 1.0 form");
	let mut back = ToPackets::new();
	assert_eq!(back.take(&message(0x9, 60, 0, 0xFFFF_0000).on_group(2)), alloc::vec![[0x29, 0x93, 60, 127]], "and to packets on its group's cable");
}

// A Group Terminal Block answer of these (id, type, first group, groups) blocks.
fn answer(blocks: &[(u8, u8, u8, u8)]) -> Vec<u8> {
	let mut out = alloc::vec![5, CS_GR_TRM_BLOCK, GR_TRM_BLOCK_HEADER, 0, 0];
	for &(id, kind, first, count) in blocks {
		out.extend_from_slice(&[13, CS_GR_TRM_BLOCK, GR_TRM_BLOCK, id, kind, first, count, 0, 0x11, 0, 0, 0, 0]);
	}
	let total = out.len() as u16;
	out[3..5].copy_from_slice(&total.to_le_bytes());
	out
}

#[test]
fn group_terminal_blocks_are_read_and_a_hostile_answer_refused() {
	let read = blocks(&answer(&[(1, 0, 0, 2), (2, 2, 2, 2)])).expect("two blocks");
	assert_eq!(read[0], Block { id: 1, direction: Direction::Both, first_group: 0, groups: 2, name: 0, protocol: 0x11 });
	assert_eq!((read[1].direction, read[1].first_group, read[1].groups), (Direction::Sends, 2, 2));
	let mut short = answer(&[(1, 0, 0, 2)]);
	short.pop();
	assert_eq!(blocks(&short), Err(BlockRefusal::Header), "a total that is not what arrived");
	assert_eq!(blocks(&answer(&[(1, 0, 15, 2)])), Err(BlockRefusal::Malformed), "groups past fifteen");
	assert_eq!(blocks(&answer(&[(1, 3, 0, 1)])), Err(BlockRefusal::Malformed), "a type that is not one of the three");
	assert_eq!(blocks(&answer(&[(1, 0, 0, 0)])), Err(BlockRefusal::Malformed), "a block of no groups");
	assert_eq!(blocks(&answer(&[(1, 0, 0, 1); 9])), Err(BlockRefusal::TooMany));
	let mut lying = answer(&[(1, 0, 0, 1)]);
	lying[5] = 40;
	assert_eq!(blocks(&lying), Err(BlockRefusal::Malformed), "a block longer than the answer");
}

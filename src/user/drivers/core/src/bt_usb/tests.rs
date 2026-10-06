use super::*;
use crate::descriptor;
use alloc::vec::Vec;

fn controller() -> Vec<u8> {
	let mut out: Vec<u8> = alloc::vec![9, descriptor::DT_CONFIG, 0, 0, 1, 1, 0, 0xe0, 50];
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 3, CLASS_WIRELESS, SUBCLASS_RF, PROTOCOL_BLUETOOTH, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x81, 0x03, 0x10, 0x00, 1]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x82, 0x02, 0x40, 0x00, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x02, 0x02, 0x40, 0x00, 0]);
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out
}

#[test]
fn a_controller_binds_its_event_pipe_and_its_bulk_pair() {
	let bound = bind(&controller()).expect("a controller binds");
	assert_eq!((bound.events.address, bound.acl_in.address, bound.acl_out.address), (0x81, 0x82, 0x02));
}

#[test]
fn an_event_that_ends_on_a_packet_boundary_is_cut_by_its_length() {
	let mut events = Reassembly::new(Kind::Event);
	// A 16-byte event - one full interrupt packet, no zero-length packet after it - then a 6-byte one.
	let first: Vec<u8> = [0x0e, 14].into_iter().chain(0..14).collect();
	let second: Vec<u8> = alloc::vec![0x0e, 4, 1, 0x03, 0x0c, 0x00];
	assert_eq!(events.push(&first), alloc::vec![first.clone()]);
	assert_eq!(events.push(&second[..3]), Vec::<Vec<u8>>::new(), "half an event is held");
	assert_eq!(events.push(&second[3..]), alloc::vec![second]);
}

#[test]
fn several_acl_packets_in_one_transfer_are_each_cut_out() {
	let mut acl = Reassembly::new(Kind::Acl);
	let packet = |length: u16| -> Vec<u8> { [0x40, 0x20].into_iter().chain(length.to_le_bytes()).chain((0..length).map(|n| n as u8)).collect() };
	let transfer: Vec<u8> = packet(60).into_iter().chain(packet(3)).collect();
	assert_eq!(acl.push(&transfer), alloc::vec![packet(60), packet(3)]);
}

#[test]
fn a_length_past_the_ceiling_drops_the_stream_rather_than_waiting() {
	let mut acl = Reassembly::new(Kind::Acl);
	assert!(acl.push(&[0x40, 0x20, 0xff, 0xff, 1, 2, 3]).is_empty());
	// Nothing of the bad header is held to swallow what follows.
	assert_eq!(acl.push(&[0x40, 0x20, 1, 0, 9]), alloc::vec![alloc::vec![0x40, 0x20, 1, 0, 9]]);
}

#[test]
fn packets_are_whole_only_when_their_lengths_say_so() {
	assert!(command_is_whole(&[0x03, 0x0c, 0x00]));
	assert!(!command_is_whole(&[0x03, 0x0c, 0x01]));
	assert!(acl_is_whole(&[0x40, 0x20, 2, 0, 7, 8]));
	assert!(!acl_is_whole(&[0x40, 0x20, 3, 0, 7, 8]));
}

// A controller with its voice interface: alternate zero with empty isochronous endpoints, as the specification has it,
// then alternates 1 to 5 of 9, 17, 25, 33 and 49 bytes.
fn with_voice() -> Vec<u8> {
	let mut out = controller();
	for (alternate, packet) in [0u8, 9, 17, 25, 33, 49].into_iter().enumerate() {
		out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, alternate as u8, 2, CLASS_WIRELESS, SUBCLASS_RF, PROTOCOL_BLUETOOTH, 0]);
		out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x83, 0x01, packet, 0x00, 1]);
		out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x03, 0x01, packet, 0x00, 1]);
	}
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out[4] = 2;
	out
}

#[test]
fn the_voice_alternates_are_read_with_their_isochronous_pairs() {
	let bound = bind(&with_voice()).expect("a controller with voice binds");
	assert_eq!(bound.voice_interface, Some(1));
	let two = bound.voice_at(2).expect("alternate 2");
	assert_eq!((two.iso_in.address, two.iso_out.address, two.iso_in.max_packet(), two.iso_out.max_packet()), (0x83, 0x03, 17, 17));
	assert_eq!(bound.voice_at(0), None, "alternate zero carries nothing");
	assert_eq!(bound.voice_at(6), None, "and this controller has no wideband alternate");
	assert!(bound.has_voice());
	assert!(!bind(&controller()).expect("a controller without voice").has_voice());
}

#[test]
fn a_voice_setting_takes_the_tables_alternate() {
	assert_eq!([(1, 8), (2, 8), (3, 8)].map(|(channels, bits)| alternate_for(channels, bits, false)), [Some(1), Some(2), Some(3)]);
	assert_eq!([(1, 16), (2, 16), (3, 16)].map(|(channels, bits)| alternate_for(channels, bits, false)), [Some(2), Some(4), Some(5)]);
	assert_eq!(alternate_for(1, 16, true), Some(6), "wideband is one channel on alternate 6");
	assert_eq!((alternate_for(0, 16, false), alternate_for(4, 8, false), alternate_for(1, 12, false), alternate_for(2, 16, true)), (None, None, None, None));
}

// A SCO packet on `handle` with `length` bytes of a pattern.
fn sco(handle: u16, length: u8, seed: u8) -> Vec<u8> {
	handle.to_le_bytes().into_iter().chain([length]).chain((0..length).map(|n| n.wrapping_mul(7) ^ seed)).collect()
}

#[test]
fn sco_packets_are_cut_into_pieces_and_put_back_at_every_piece_size() {
	for capacity in [9usize, 17, 25, 33, 49, 63] {
		let mut pieces = ScoPieces::new(capacity);
		for length in [0u8, 1, 6, 14, 30, 46, 48, 60, 120, 255] {
			let packet = sco(0x0021, length, capacity as u8);
			assert!(sco_is_whole(&packet));
			let mut out = Vec::new();
			for piece in packet.chunks(capacity) {
				out.extend(pieces.piece(piece));
				assert!(pieces.piece(&[]).is_empty(), "an empty interval between pieces is no piece");
			}
			assert_eq!(out, alloc::vec![packet], "{length} bytes in pieces of {capacity}");
		}
	}
	assert!(!sco_is_whole(&[0x21, 0x00, 4, 1, 2]));
}

#[test]
fn a_packet_that_lost_a_piece_is_refused_and_the_next_one_delivered() {
	let mut pieces = ScoPieces::new(17);
	let before = sco(0x0021, 48, 1);
	assert_eq!(before.chunks(17).flat_map(|piece| pieces.piece(piece)).collect::<Vec<_>>(), alloc::vec![before.clone()]);
	// ITS MIDDLE PIECE LOST: the header said three pieces, so the last of them is skipped.
	let torn = sco(0x0021, 48, 2);
	let after = sco(0x0021, 30, 3);
	let mut chunks = torn.chunks(17);
	assert!(pieces.piece(chunks.next().unwrap()).is_empty());
	chunks.next();
	pieces.lost();
	assert!(pieces.piece(chunks.next().unwrap()).is_empty(), "the torn packet's last piece is not a packet");
	assert_eq!(after.chunks(17).flat_map(|piece| pieces.piece(piece)).collect::<Vec<_>>(), alloc::vec![after.clone()]);
	// ITS HEADER LOST: the pieces after it are skipped until one opens a packet on the connection's handle.
	let headless = sco(0x0021, 48, 4);
	pieces.lost();
	for piece in headless.chunks(17).skip(1) {
		assert!(pieces.piece(piece).is_empty(), "a piece of the packet whose header was lost");
	}
	assert_eq!(after.chunks(17).flat_map(|piece| pieces.piece(piece)).collect::<Vec<_>>(), alloc::vec![after]);
}

#[test]
// ISO ON THE BULK PAIR: an ISO packet past the ACL ceiling - which only the host's handle table can tell is ISO - is cut
// whole out of the IN pipe's transfers, and an outbound one is whole by its fourteen-bit load length.
fn iso_packets_share_the_bulk_pair() {
	let load = MAX_ISO - ACL_HEADER;
	let mut packet = alloc::vec![0x60, 0x20];
	packet.extend_from_slice(&(load as u16).to_le_bytes());
	packet.resize(MAX_ISO, 0x5a);
	assert!(packet.len() > MAX_ACL);
	let mut reassembly = Reassembly::new(Kind::Acl);
	assert!(reassembly.push(&packet[..512]).is_empty());
	assert_eq!(reassembly.push(&packet[512..]), alloc::vec![packet.clone()]);
	assert!(iso_is_whole(&packet));
	assert!(!acl_is_whole(&packet), "past the ACL ceiling");
	let mut flagged = packet[..12].to_vec();
	flagged[2..4].copy_from_slice(&(8u16 | 0xc000).to_le_bytes());
	assert!(iso_is_whole(&flagged), "the load length's top bits are not its length");
	assert!(!iso_is_whole(&packet[..12]));
}

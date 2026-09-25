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

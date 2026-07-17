use super::*;
use alloc::vec;

fn page(flags: u8, serial: u32, sequence: u32, granule: u64, lacing: &[u8], body: &[u8]) -> Vec<u8> {
	assert_eq!(lacing.iter().map(|value| *value as usize).sum::<usize>(), body.len());
	let mut bytes = b"OggS".to_vec();
	bytes.push(0);
	bytes.push(flags);
	bytes.extend_from_slice(&granule.to_le_bytes());
	bytes.extend_from_slice(&serial.to_le_bytes());
	bytes.extend_from_slice(&sequence.to_le_bytes());
	bytes.extend_from_slice(&0u32.to_le_bytes());
	bytes.push(lacing.len() as u8);
	bytes.extend_from_slice(lacing);
	bytes.extend_from_slice(body);
	let crc = ogg_crc(&bytes);
	bytes[22..26].copy_from_slice(&crc.to_le_bytes());
	bytes
}

#[test]
fn frames_packets_across_pages_and_assigns_final_granule() {
	let mut bytes = page(0x02, 7, 0, 0, &[3, 255], &[b'h', b'd', b'r'].into_iter().chain(core::iter::repeat_n(b'a', 255)).collect::<Vec<_>>());
	bytes.extend_from_slice(&page(0x05, 7, 1, 123, &[2, 1], b"bc!"));
	let mut reader = PacketReader::new(&bytes);
	let first = reader.next_packet().unwrap().unwrap();
	assert_eq!(first.data, b"hdr");
	assert!(first.bos);
	let second = reader.next_packet().unwrap().unwrap();
	assert_eq!(second.data.len(), 257);
	assert_eq!(&second.data[255..], b"bc");
	assert_eq!(second.page_granule_position, Some(123));
	assert_eq!(second.granule_position, None);
	let third = reader.next_packet().unwrap().unwrap();
	assert_eq!(third.data, b"!");
	assert_eq!(third.granule_position, Some(123));
	assert!(third.eos);
	assert_eq!(reader.next_packet(), Ok(None));
}

#[test]
fn rejects_crc_sequence_continuation_and_truncation_errors() {
	let valid = page(0x02, 9, 0, 0, &[1], b"x");
	assert_eq!(PacketReader::new(&[]).next_packet(), Ok(None));
	for len in [1, 4, 26, valid.len() - 1] {
		assert!(PacketReader::new(&valid[..len]).next_packet().is_err());
	}
	let mut corrupt = valid.clone();
	*corrupt.last_mut().unwrap() ^= 1;
	assert_eq!(PacketReader::new(&corrupt).next_packet(), Err(Error::Checksum));
	let continued = page(1, 9, 0, 0, &[1], b"x");
	assert_eq!(PacketReader::new(&continued).next_packet(), Err(Error::Invalid));
	let mut skipped = valid;
	skipped.extend_from_slice(&page(0, 9, 2, 0, &[1], b"y"));
	let mut reader = PacketReader::new(&skipped);
	assert!(reader.next_packet().unwrap().is_some());
	assert_eq!(reader.next_packet(), Err(Error::Sequence));
}

#[test]
fn enforces_packet_size_cap_before_allocation() {
	let bytes = [1u8];
	let mut reader = PacketReader::new(&bytes);
	reader.pending = vec![0; MAX_PACKET_SIZE];
	reader.segments_start = 0;
	reader.segment_count = 1;
	reader.body_cursor = 0;
	reader.page_end = 1;
	assert_eq!(reader.next_packet(), Err(Error::TooLarge));
}

#[test]
fn frames_staged_vorbis_stream() {
	let mut reader = PacketReader::new(include_bytes!("../../../volume/test.ogg"));
	for signature in [b"\x01vorbis".as_slice(), b"\x03vorbis".as_slice(), b"\x05vorbis".as_slice()] {
		let packet = reader.next_packet().unwrap().unwrap();
		assert!(packet.data.starts_with(signature));
	}
	let mut audio_packets = 0usize;
	let mut final_granule = None;
	let mut saw_eos = false;
	while let Some(packet) = reader.next_packet().unwrap() {
		audio_packets += 1;
		final_granule = packet.granule_position.or(final_granule);
		saw_eos |= packet.eos;
	}
	assert!(audio_packets != 0);
	assert_eq!(final_granule, Some(328_104));
	assert!(saw_eos);
}

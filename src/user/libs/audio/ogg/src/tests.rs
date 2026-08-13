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

// The page writer, tested through the reader in this file: what makes a writer correct is that the
// packets come back, and comparing bytes against a table somebody typed would only prove that two
// pieces of this file agree about a layout neither of them has to be right about.
mod writer {
	use super::super::{Error, MAX_PACKET_SIZE, PacketReader, PageWriter};
	use alloc::vec::Vec;

	// Round-trip `packets` and return what the reader made of them.
	fn round_trip(packets: &[(&[u8], Option<u64>)]) -> Vec<(Vec<u8>, Option<u64>, bool, bool)> {
		let mut writer = PageWriter::new(0x1234_5678);
		for (data, granule) in packets {
			writer.write_packet(data, *granule).expect("the packet is written");
		}
		let stream = writer.finish().expect("the stream is finished");
		let mut reader = PacketReader::new(&stream);
		let mut out = Vec::new();
		while let Some(packet) = reader.next_packet().expect("the stream reads back") {
			out.push((packet.data, packet.granule_position, packet.bos, packet.eos));
		}
		out
	}

	#[test]
	fn packets_come_back_as_they_went_in() {
		let read = round_trip(&[(b"first", Some(0)), (b"second", Some(1024)), (b"third", Some(2048))]);
		assert_eq!(read.len(), 3);
		assert_eq!(read[0].0, b"first");
		assert_eq!(read[1].0, b"second");
		assert_eq!(read[2].0, b"third");
		// The first packet is the start of the stream and the last one is the end of it, which is
		// how a decoder knows it has the whole thing rather than a piece of it.
		assert!(read[0].2, "the first packet carries the beginning-of-stream flag");
		assert!(read[2].3, "the last packet carries the end-of-stream flag");
		assert_eq!(read[2].1, Some(2048), "the granule position travels with the packet that ends the page");
	}

	#[test]
	fn a_packet_whose_length_is_a_multiple_of_255_ends_with_a_zero_segment() {
		// THE CASE THE FORMAT EXISTS TO CATCH. A packet of exactly 255 bytes is one full segment,
		// and without a zero-length segment after it the reader cannot tell where it stopped - it
		// would run on into whatever follows. Every length here is a multiple of 255.
		for length in [255usize, 510, 765] {
			let packet: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
			let read = round_trip(&[(&packet, Some(1)), (b"after", Some(2))]);
			assert_eq!(read.len(), 2, "both packets came back for length {length}");
			assert_eq!(read[0].0, packet, "the {length}-byte packet is whole");
			assert_eq!(read[1].0, b"after", "and the packet after it did not get absorbed");
		}
	}

	#[test]
	fn a_packet_larger_than_one_page_spans_pages_and_comes_back_whole() {
		// 255 segments of 255 bytes is the most one page carries; this is comfortably past it, so
		// the packet is continued across pages and the continuation flag has to be right on each.
		let packet: Vec<u8> = (0..200_000usize).map(|index| (index % 253) as u8).collect();
		let read = round_trip(&[(&packet, Some(4096)), (b"tail", Some(8192))]);
		assert_eq!(read.len(), 2);
		assert_eq!(read[0].0.len(), packet.len());
		assert_eq!(read[0].0, packet);
		assert_eq!(read[1].0, b"tail");
	}

	#[test]
	fn an_empty_packet_is_a_packet() {
		// Vorbis writes them: a silent frame can encode to nothing at all, and a writer that
		// dropped an empty packet would shift every granule position after it.
		let read = round_trip(&[(b"", Some(0)), (b"x", Some(64)), (b"", Some(128))]);
		assert_eq!(read.len(), 3);
		assert!(read[0].0.is_empty());
		assert_eq!(read[1].0, b"x");
		assert!(read[2].0.is_empty());
	}

	#[test]
	fn the_pages_carry_the_serial_and_a_rising_sequence() {
		let big: Vec<u8> = (0..100_000usize).map(|index| index as u8).collect();
		let mut writer = PageWriter::new(0x0bad_c0de);
		writer.write_packet(&big, Some(1)).expect("written");
		let stream = writer.finish().expect("finished");
		let mut at: usize = 0;
		let mut sequence: u32 = 0;
		let mut pages: u32 = 0;
		while at < stream.len() {
			assert_eq!(&stream[at..at + 4], b"OggS", "every page starts with the capture pattern");
			assert_eq!(u32::from_le_bytes(stream[at + 14..at + 18].try_into().unwrap()), 0x0bad_c0de, "and carries the stream's serial");
			assert_eq!(u32::from_le_bytes(stream[at + 18..at + 22].try_into().unwrap()), sequence, "and the next sequence number");
			let segments: usize = stream[at + 26] as usize;
			let body: usize = stream[at + 27..at + 27 + segments].iter().map(|length| *length as usize).sum();
			at += 27 + segments + body;
			sequence += 1;
			pages += 1;
		}
		assert!(pages > 1, "a hundred thousand bytes is more than one page");
	}

	#[test]
	fn a_packet_past_the_ceiling_is_refused_rather_than_written() {
		let huge: Vec<u8> = alloc::vec![0u8; MAX_PACKET_SIZE + 1];
		let mut writer = PageWriter::new(1);
		assert_eq!(writer.write_packet(&huge, None), Err(Error::TooLarge));
		// And the writer is still usable: the refusal happened before anything was written.
		writer.write_packet(b"small", Some(1)).expect("the writer still works");
		let stream = writer.finish().expect("finished");
		let mut reader = PacketReader::new(&stream);
		assert_eq!(reader.next_packet().expect("reads").expect("one packet").data, b"small");
	}
}

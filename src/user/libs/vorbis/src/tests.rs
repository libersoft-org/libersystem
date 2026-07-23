use super::*;

#[test]
fn decodes_staged_stream_to_independent_golden() {
	let vorbis = Vorbis::parse(include_bytes!("../../../../volume/test.ogg")).unwrap();
	assert_eq!(vorbis.metadata(), Metadata { rate: 44_100, channels: 1, frames: 328_104, duration_ms: 7_440 });
	let mut decoder = vorbis.decoder();
	let mut pcm = Vec::new();
	let mut chunk = Vec::new();
	loop {
		let frames = decoder.read_i16_le(37, &mut chunk).unwrap();
		if frames == 0 {
			break;
		}
		pcm.extend_from_slice(&chunk);
	}
	assert_eq!(pcm.len(), 656_208);
	let golden = include_bytes!("../tests/test.pcm");
	for (actual, expected) in pcm.chunks_exact(2).zip(golden.chunks_exact(2)) {
		let actual = i16::from_le_bytes([actual[0], actual[1]]) as i32;
		let expected = i16::from_le_bytes([expected[0], expected[1]]) as i32;
		assert!((actual - expected).abs() <= 1);
	}
	assert_eq!(decoder.remaining_frames(), 0);
}

#[test]
fn rejects_truncated_corrupt_and_non_vorbis_streams() {
	let source = include_bytes!("../../../../volume/test.ogg");
	for length in [0, 4, 26, source.len() - 1] {
		assert!(Vorbis::parse(&source[..length]).is_err());
	}
	let mut corrupt = source.to_vec();
	*corrupt.last_mut().unwrap() ^= 1;
	assert_eq!(Vorbis::parse(&corrupt).err(), Some(Error::Checksum));
	let mut wrong_header = source.to_vec();
	let signature = wrong_header.windows(7).position(|bytes| bytes == b"\x01vorbis").unwrap();
	wrong_header[signature] = 3;
	let crc = ogg::ogg_crc(&wrong_header[..wrong_header[26] as usize + 27 + wrong_header[27..27 + wrong_header[26] as usize].iter().map(|length| *length as usize).sum::<usize>()]);
	wrong_header[22..26].copy_from_slice(&crc.to_le_bytes());
	assert_eq!(Vorbis::parse(&wrong_header).err(), Some(Error::Invalid));
}

#[test]
fn rejects_zero_frame_reads() {
	let vorbis = Vorbis::parse(include_bytes!("../../../../volume/test.ogg")).unwrap();
	let mut decoder = vorbis.decoder();
	assert_eq!(decoder.read_i16_le(0, &mut Vec::new()), Err(Error::Invalid));
}

#[test]
fn rejects_compact_oversized_header_allocations() {
	let mut setup = b"\x05vorbis\x00BCV\x01\x00".to_vec();
	setup.extend_from_slice(&[1, 0, 4]);
	assert!(matches!(header::read_header_setup(&setup, 1, (6, 6)), Err(header::HeaderReadError::BufferNotAddressable)));

	let mut comments = b"\x03vorbis\x00\x00\x00\x00".to_vec();
	comments.extend_from_slice(&4_097u32.to_le_bytes());
	assert!(matches!(header::read_header_comment(&comments), Err(header::HeaderReadError::BufferNotAddressable)));
}

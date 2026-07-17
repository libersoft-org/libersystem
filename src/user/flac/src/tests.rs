use super::*;

#[test]
fn validates_checksums_and_utf8_numbers() {
	assert_eq!(crc8(b"123456789"), 0xf4);
	assert_eq!(crc16(b"123456789"), 0xfee8);
	assert_eq!(read_utf8_number(&[0x7f]), Ok((127, 1)));
	assert_eq!(read_utf8_number(&[0xc2, 0x80]), Ok((128, 2)));
	assert_eq!(read_utf8_number(&[0xc0, 0x80]), Err(Error::Invalid));
}

#[test]
fn rejects_bad_stream_info_bounds() {
	assert!(matches!(Flac::parse(b"fLaC"), Err(Error::Truncated)));
	let mut bytes = alloc::vec![b'f', b'L', b'a', b'C', 0x80, 0, 0, 34];
	bytes.resize(42, 0);
	assert!(matches!(Flac::parse(&bytes), Err(Error::Invalid)));
}

#[test]
fn decodes_staged_flac_bit_exactly_in_bounded_chunks() {
	let flac = Flac::parse(include_bytes!("../../../volume/test.flac")).unwrap();
	assert_eq!(flac.metadata().rate, 44_100);
	assert_eq!(flac.metadata().channels, 1);
	assert_eq!(flac.metadata().bits_per_sample, 16);
	assert_eq!(flac.metadata().frames, 328_104);
	let mut decoder = flac.decoder();
	let mut chunk = Vec::new();
	let mut decoded = Vec::new();
	while decoder.remaining_frames() != 0 {
		let frames = decoder.read_i16_le(127, &mut chunk).unwrap();
		assert!((1..=127).contains(&frames));
		decoded.extend_from_slice(&chunk);
	}
	assert_eq!(decoded, include_bytes!("../tests/data/test-s16le.pcm"));
}

#[test]
fn rejects_truncated_and_corrupt_frames() {
	let source = include_bytes!("../../../volume/test.flac");
	for len in [0, 1, 4, 8, 41, source.len() - 1] {
		let result = Flac::parse(&source[..len]);
		if let Ok(flac) = result {
			let mut decoder = flac.decoder();
			let mut chunk = Vec::new();
			loop {
				match decoder.read_i16_le(1_024, &mut chunk) {
					Err(_) => break,
					Ok(0) => panic!("truncated FLAC reached a clean end"),
					Ok(_) => {}
				}
			}
		}
	}
	let mut corrupt = source.to_vec();
	*corrupt.last_mut().unwrap() ^= 1;
	let flac = Flac::parse(&corrupt).unwrap();
	let mut decoder = flac.decoder();
	let mut chunk = Vec::new();
	loop {
		match decoder.read_i16_le(1_024, &mut chunk) {
			Err(error) => {
				assert_eq!(error, Error::Checksum);
				break;
			}
			Ok(0) => panic!("corrupt FLAC reached a clean end"),
			Ok(_) => {}
		}
	}
}

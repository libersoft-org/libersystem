use super::*;
use alloc::vec;

#[test]
fn skips_bounded_id3v2_metadata() {
	let mut bytes = b"ID3\x04\0\0\0\0\0\x03abc".to_vec();
	bytes.extend_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
	assert_eq!(id3v2_end(&bytes), Ok(13));
	bytes[9] = 0x80;
	assert_eq!(id3v2_end(&bytes), Err(Error::Unsupported));
}

#[test]
fn validates_mpeg_layer_and_profile() {
	let header = FrameHeader::parse(&[0xff, 0xfb, 0x90, 0x00]).unwrap();
	assert_eq!((header.rate, header.channels), (44_100, 2));
	assert_eq!(FrameHeader::parse(&[0xff, 0xf3, 0x90, 0xc0]).unwrap().rate, 22_050);
	assert!(matches!(FrameHeader::parse(&[0xff, 0xe3, 0x90, 0xc0]), Err(Error::Unsupported)));
	assert!(matches!(FrameHeader::parse(&[0xff, 0xfd, 0x90, 0x00]), Err(Error::Unsupported)));
	assert_eq!(id3v2_end(&vec![b'I', b'D', b'3']), Err(Error::Truncated));
}

#[test]
fn parses_info_gapless_range() {
	let header = FrameHeader::parse(&[0xff, 0xfb, 0x90, 0xc0]).unwrap();
	let mut frame = vec![0u8; 200];
	frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0xc0]);
	let xing = 4 + header.side_info_bytes();
	frame[xing..xing + 4].copy_from_slice(b"Info");
	frame[xing + 4..xing + 8].copy_from_slice(&1u32.to_be_bytes());
	frame[xing + 8..xing + 12].copy_from_slice(&286u32.to_be_bytes());
	let encoder = xing + 12;
	frame[encoder..encoder + 9].copy_from_slice(b"LAME3.100");
	frame[encoder + 21..encoder + 24].copy_from_slice(&[0x24, 0x03, 0x18]);
	let info = gapless_info(&frame, 0, header).unwrap();
	assert_eq!(info.frames, 328_104);
	assert_eq!(info.skip_frames, 2_257);
}

#[test]
fn decodes_staged_mpeg1_stream_in_bounded_chunks() {
	let mp3 = Mp3::parse(include_bytes!("../../../../volume/test.mp3")).unwrap();
	assert_eq!(mp3.metadata(), Metadata { rate: 44_100, channels: 1, frames: 328_104, duration_ms: 7_440 });
	for chunk_frames in [127, 1_024] {
		let mut decoder = mp3.decoder();
		let mut chunk = Vec::new();
		let mut decoded = Vec::new();
		loop {
			let frames = decoder.read_i16_le(chunk_frames, &mut chunk).unwrap();
			if frames == 0 {
				break;
			}
			assert!(frames <= chunk_frames);
			decoded.extend_from_slice(&chunk);
		}
		let golden = include_bytes!("../tests/test.pcm");
		assert_eq!(decoded.len(), golden.len());
		let errors = decoded.chunks_exact(2).zip(golden.chunks_exact(2)).map(|(actual, expected)| {
			let actual = i16::from_le_bytes([actual[0], actual[1]]) as i32;
			let expected = i16::from_le_bytes([expected[0], expected[1]]) as i32;
			actual.abs_diff(expected)
		});
		let total_error: u64 = errors.clone().map(u64::from).sum();
		let peak_error = errors.max().unwrap_or(0);
		assert!(total_error / mp3.metadata().frames <= 2, "MP3 output diverges from the independent PCM golden");
		assert!(peak_error <= 16, "MP3 output contains an impulsive error of {peak_error} samples");
		assert_eq!(decoder.remaining_frames(), 0);
	}
}

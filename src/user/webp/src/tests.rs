use super::*;
use alloc::vec;

fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn top_level_kinds(data: &[u8]) -> Vec<[u8; 4]> {
	let mut kinds = Vec::new();
	let mut cursor = 12usize;
	while cursor < data.len() {
		let length = u32::from_le_bytes(data[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
		kinds.push(data[cursor..cursor + 4].try_into().unwrap());
		cursor += 8 + length + (length & 1);
	}
	kinds
}

fn insert_chunk(data: &mut Vec<u8>, offset: usize, kind: &[u8; 4], payload: &[u8]) {
	let mut chunk = Vec::new();
	append_chunk(&mut chunk, kind, payload).unwrap();
	data.splice(offset..offset, chunk);
	let size = u32::try_from(data.len() - 8).unwrap();
	data[4..8].copy_from_slice(&size.to_le_bytes());
}

fn top_level_offsets(data: &[u8], target: &[u8; 4]) -> Vec<usize> {
	let mut offsets = Vec::new();
	let mut cursor = 12usize;
	while cursor < data.len() {
		let length = u32::from_le_bytes(data[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
		if &data[cursor..cursor + 4] == target {
			offsets.push(cursor);
		}
		cursor += 8 + length + (length & 1);
	}
	offsets
}

#[test]
fn lossless_endpoints_round_trip_rgba() {
	let image = pix::RgbaImage::new(3, 2, vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4]).unwrap();
	let plain = encode_lossless_profile(&image, false).unwrap();
	let predicted = encode_lossless_profile(&image, true).unwrap();
	for compression in [0, 1, 24, 25, 49, 50, 74, 75, 99, 100] {
		let encoded = encode_lossless(&image, compression).unwrap();
		assert_eq!(encoded, encode_lossless(&image, compression).unwrap());
		assert_eq!(decode(&encoded).unwrap(), image);
	}
	let compact = encode_lossless(&image, 100).unwrap();
	assert!(compact.len() <= plain.len() && compact.len() <= predicted.len());

	let mut pixels = Vec::new();
	for x in 0..16usize {
		let value = if x & 1 == 0 { 0 } else { 255 };
		pixels.extend_from_slice(&[value, value, value, 255]);
	}
	for _ in 0..48 {
		pixels.extend_from_slice(&[128, 128, 128, 255]);
	}
	let search_fixture = pix::RgbaImage::new(16, 4, pixels).unwrap();
	assert!(!predictor_is_promising(&search_fixture, 25));
	assert!(predictor_is_promising(&search_fixture, 50));
	assert_ne!(encode_lossless(&search_fixture, 25).unwrap(), encode_lossless(&search_fixture, 50).unwrap());
}

#[test]
fn rejects_out_of_range_effort_and_bad_input() {
	let image = pix::RgbaImage::new(1, 1, vec![0, 0, 0, 255]).unwrap();
	assert_eq!(encode_lossless(&image, 101), Err(Error::Unsupported));
	assert_eq!(decode(b"RIFF"), Err(Error::Invalid));
}

#[test]
fn lossy_quality_improves_rgb_and_preserves_alpha() {
	let mut pixels = Vec::new();
	for y in 0..17u8 {
		for x in 0..19u8 {
			pixels.extend_from_slice(&[x.wrapping_mul(11), y.wrapping_mul(13), x.wrapping_mul(5).wrapping_add(y.wrapping_mul(7)), x.wrapping_mul(17).wrapping_add(y.wrapping_mul(3))]);
		}
	}
	let source = pix::RgbaImage::new(19, 17, pixels).unwrap();
	let low_bytes = encode_lossy(&source, 0, 100).unwrap();
	let high_bytes = encode_lossy(&source, 100, 100).unwrap();
	assert_eq!(high_bytes, encode_lossy(&source, 100, 100).unwrap());
	assert_ne!(encode_lossy(&source, 80, 0).unwrap(), encode_lossy(&source, 80, 100).unwrap());
	assert_eq!(&high_bytes[12..16], b"VP8X");
	assert!(high_bytes.windows(4).any(|window| window == b"ALPH"));
	for effort in [0, 24, 25, 49, 50, 74, 75, 100] {
		let decoded = decode(&encode_lossy(&source, 80, effort).unwrap()).unwrap();
		assert_eq!((decoded.width, decoded.height), (19, 17));
	}
	let low = decode(&low_bytes).unwrap();
	let high = decode(&high_bytes).unwrap();
	let error = |actual: &pix::RgbaImage| -> u64 { actual.pixels.chunks_exact(4).zip(source.pixels.chunks_exact(4)).map(|(actual, expected)| (0..3).map(|channel| u64::from(actual[channel].abs_diff(expected[channel]))).sum::<u64>()).sum() };
	assert!(error(&high) < error(&low));
	for actual in [low, high] {
		assert_eq!((actual.width, actual.height), (source.width, source.height));
		assert_eq!(actual.pixels.iter().skip(3).step_by(4).copied().collect::<Vec<_>>(), source.pixels.iter().skip(3).step_by(4).copied().collect::<Vec<_>>());
	}
	assert_eq!(encode_lossy(&source, 101, 100), Err(Error::Unsupported));
	assert_eq!(encode_lossy(&source, 100, 101), Err(Error::Unsupported));

	let opaque = pix::RgbaImage::new(1, 1, vec![31, 127, 223, 255]).unwrap();
	let opaque = encode_lossy(&opaque, 100, 100).unwrap();
	assert_eq!(&opaque[12..16], b"VP8 ");
	for end in [0, 4, 12, 20, opaque.len() / 2] {
		assert_eq!(decode(&opaque[..end]), Err(Error::Invalid));
	}
	let mut corrupt = opaque;
	corrupt[23] ^= 0xff;
	assert_eq!(decode(&corrupt), Err(Error::Invalid));
}

#[test]
fn decodes_animation_with_exact_anmf_metadata_and_composited_preview() {
	let animation = decode_animation(include_bytes!("../tests/animated.webp")).unwrap();
	assert_eq!((animation.width, animation.height), (2, 2));
	assert_eq!(animation.frames.len(), 2);
	assert_eq!(animation.loop_count, 0);
	assert!(animation.frames.iter().all(|frame| frame.duration_ms == 500 && frame.x == 0 && frame.y == 0 && frame.image.width == 2 && frame.image.height == 2 && frame.disposal == pix::Disposal::Keep));
	assert_eq!(animation.frames[0].blend, pix::Blend::Source);
	assert_eq!(animation.frames[1].blend, pix::Blend::Over);
	assert_ne!(animation.frames[0].image.pixels, animation.frames[1].image.pixels);
	let mut compositor = pix::Compositor::new(2, 2).unwrap();
	assert_eq!(decode(include_bytes!("../tests/animated.webp")).unwrap(), compositor.render(&animation.frames[0]).unwrap());
}

#[test]
fn lossless_animation_round_trips_visual_frames_timing_and_loop() {
	let first = pix::RgbaImage::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
	let second = pix::RgbaImage::new(1, 1, vec![0, 255, 0, 128]).unwrap();
	let source = pix::Animation::new(
		2,
		1,
		7,
		vec![
			pix::Frame { image: first, x: 0, y: 0, duration_ms: 20, blend: pix::Blend::Source, disposal: pix::Disposal::Keep },
			pix::Frame { image: second, x: 1, y: 0, duration_ms: 30, blend: pix::Blend::Over, disposal: pix::Disposal::Previous },
		],
	)
	.unwrap();
	let mut compositor = pix::Compositor::new(2, 1).unwrap();
	let expected: Vec<pix::RgbaImage> = source.frames.iter().map(|frame| compositor.render(frame).unwrap()).collect();
	for effort in [50, 100] {
		let encoded = encode_animation(&source, effort).unwrap();
		let decoded = decode_animation(&encoded).unwrap();
		assert_eq!((decoded.width, decoded.height, decoded.loop_count), (2, 1, 7));
		assert_eq!(decoded.frames.iter().map(|frame| frame.duration_ms).collect::<Vec<_>>(), vec![20, 30]);
		assert_eq!(decoded.frames.into_iter().map(|frame| frame.image).collect::<Vec<_>>(), expected);
	}
}

#[test]
fn animation_preserves_background_zero_duration_and_disposal() {
	let background = [9, 19, 29, 200];
	let source = pix::Animation::new_with_background(
		2,
		1,
		background,
		3,
		vec![
			pix::Frame { image: pix::RgbaImage::new(1, 1, vec![255, 0, 0, 255]).unwrap(), x: 0, y: 0, duration_ms: 0, blend: pix::Blend::Source, disposal: pix::Disposal::Background },
			pix::Frame { image: pix::RgbaImage::new(1, 1, vec![0, 255, 0, 255]).unwrap(), x: 1, y: 0, duration_ms: 30, blend: pix::Blend::Source, disposal: pix::Disposal::Keep },
		],
	)
	.unwrap();
	let mut compositor = pix::Compositor::new_with_background(2, 1, background).unwrap();
	let expected: Vec<pix::RgbaImage> = source.frames.iter().map(|frame| compositor.render(frame).unwrap()).collect();
	let encoded = encode_animation(&source, 100).unwrap();
	let anim = encoded.windows(4).position(|window| window == b"ANIM").unwrap();
	assert_eq!(&encoded[anim + 8..anim + 12], &[background[2], background[1], background[0], background[3]]);
	let decoded = decode_animation(&encoded).unwrap();
	assert_eq!(decoded.background, background);
	assert_eq!(decoded.frames.iter().map(|frame| frame.duration_ms).collect::<Vec<_>>(), vec![0, 30]);
	assert_eq!(decoded.frames.into_iter().map(|frame| frame.image).collect::<Vec<_>>(), expected);
	assert_eq!(decode(&encoded).unwrap(), expected[0]);
	let mut before_header = encoded.clone();
	insert_chunk(&mut before_header, 12, b"JUNK", &[]);
	assert_eq!(decode_animation(&before_header), Err(Error::Invalid));
	let mut between_frames = encoded.clone();
	let second_frame = top_level_offsets(&between_frames, b"ANMF")[1];
	insert_chunk(&mut between_frames, second_frame, b"EXIF", &[]);
	assert_eq!(decode_animation(&between_frames), Err(Error::Invalid));
}

#[test]
fn animation_refuses_unrepresentable_loop_and_duration() {
	let image = pix::RgbaImage::new(1, 1, vec![0, 0, 0, 255]).unwrap();
	let loop_overflow = pix::Animation::new(1, 1, 65_536, vec![pix::Frame { image: image.clone(), x: 0, y: 0, duration_ms: 1, blend: pix::Blend::Source, disposal: pix::Disposal::Keep }]).unwrap();
	assert_eq!(encode_animation(&loop_overflow, 100), Err(Error::Unsupported));
	let duration_overflow = pix::Animation::new(1, 1, 1, vec![pix::Frame { image, x: 0, y: 0, duration_ms: 0x0100_0000, blend: pix::Blend::Source, disposal: pix::Disposal::Keep }]).unwrap();
	assert_eq!(encode_animation(&duration_overflow, 100), Err(Error::TooLarge));
}

#[test]
fn animation_parser_rejects_truncation_and_out_of_canvas_frames() {
	let source = include_bytes!("../tests/animated.webp");
	assert_eq!(decode_animation(&source[..source.len() - 1]), Err(Error::Invalid));
	let mut outside = source.to_vec();
	outside[0x32..0x35].copy_from_slice(&[1, 0, 0]);
	assert_eq!(decode_animation(&outside), Err(Error::Invalid));
	let mut bad_size = source.to_vec();
	bad_size[0x30..0x34].copy_from_slice(&u32::MAX.to_le_bytes());
	assert_eq!(decode_animation(&bad_size), Err(Error::Invalid));
	let mut reserved_vp8x = source.to_vec();
	let vp8x = reserved_vp8x.windows(4).position(|window| window == b"VP8X").unwrap();
	reserved_vp8x[vp8x + 8] |= 1;
	assert_eq!(decode_animation(&reserved_vp8x), Err(Error::Invalid));
	let mut reserved_anmf = source.to_vec();
	let anmf = reserved_anmf.windows(4).position(|window| window == b"ANMF").unwrap();
	reserved_anmf[anmf + 8 + 15] |= 0x80;
	assert_eq!(decode_animation(&reserved_anmf), Err(Error::Invalid));
}

#[test]
fn decodes_external_libwebp_static_and_animation_profiles() {
	for (data, kinds, dimensions, hash) in [
		(include_bytes!("../tests/data/external-vp8.webp").as_slice(), vec![*b"VP8 "], (23, 15), 0xf2da_7877_eabb_5d1e),
		(include_bytes!("../tests/data/external-alph-vp8.webp").as_slice(), vec![*b"VP8X", *b"ALPH", *b"VP8 "], (19, 13), 0x3bb4_6987_825a_b3fc),
		(include_bytes!("../tests/data/external-vp8l.webp").as_slice(), vec![*b"VP8L"], (19, 13), 0x35fa_330e_0391_3460),
	] {
		assert_eq!(top_level_kinds(data), kinds);
		let image = decode(data).unwrap();
		assert_eq!((image.width, image.height, fnv1a(&image.pixels)), (dimensions.0, dimensions.1, hash));
	}

	let data = include_bytes!("../tests/data/external-animation.webp");
	assert_eq!(top_level_kinds(data), vec![*b"VP8X", *b"ANIM", *b"ANMF", *b"ANMF"]);
	let animation = decode_animation(data).unwrap();
	assert_eq!((animation.width, animation.height, animation.background, animation.loop_count, animation.frames.len()), (23, 15, [9, 19, 29, 200], 3, 2));
	assert_eq!(animation.frames.iter().map(|frame| (frame.x, frame.y, frame.image.width, frame.image.height, frame.duration_ms, frame.blend, frame.disposal)).collect::<Vec<_>>(), vec![(0, 0, 23, 15, 0, pix::Blend::Source, pix::Disposal::Background), (2, 2, 19, 13, 37, pix::Blend::Over, pix::Disposal::Keep)]);
	assert_eq!(animation.frames.iter().map(|frame| fnv1a(&frame.image.pixels)).collect::<Vec<_>>(), vec![0x8cb7_e5da_66d8_51a1, 0x35fa_330e_0391_3460]);
	let mut compositor = pix::Compositor::new_with_background(animation.width, animation.height, animation.background).unwrap();
	assert_eq!(animation.frames.iter().map(|frame| fnv1a(&compositor.render(frame).unwrap().pixels)).collect::<Vec<_>>(), vec![0x8cb7_e5da_66d8_51a1, 0xa9ed_68c4_c84d_1792]);
}

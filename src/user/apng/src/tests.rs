use super::*;
use alloc::vec;

fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn displayed_hashes(animation: &Animation) -> Vec<u64> {
	let mut compositor = pix::Compositor::new_with_background(animation.width, animation.height, animation.background).unwrap();
	animation.frames.iter().map(|frame| fnv1a(&compositor.render(frame).unwrap().pixels)).collect()
}

fn png_chunks(data: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
	let mut chunks = Vec::new();
	let mut cursor = 8usize;
	while cursor < data.len() {
		let length = read_u32(data, cursor).unwrap() as usize;
		let kind = data[cursor + 4..cursor + 8].try_into().unwrap();
		let start = cursor + 8;
		chunks.push((kind, data[start..start + length].to_vec()));
		cursor = start + length + 4;
	}
	chunks
}

fn control(sequence: u32, width: u32, height: u32) -> Vec<u8> {
	let mut body = Vec::new();
	body.extend_from_slice(&sequence.to_be_bytes());
	body.extend_from_slice(&width.to_be_bytes());
	body.extend_from_slice(&height.to_be_bytes());
	body.extend_from_slice(&[0; 8]);
	body.extend_from_slice(&1u16.to_be_bytes());
	body.extend_from_slice(&1_000u16.to_be_bytes());
	body.extend_from_slice(&[0, 0]);
	body
}

#[test]
fn frame_rect_timing_blend_disposal_and_loop_round_trip() {
	let first = pix::RgbaImage::new(2, 2, vec![1; 16]).unwrap();
	let second = pix::RgbaImage::new(1, 2, vec![2; 8]).unwrap();
	let animation = Animation::new(
		2,
		2,
		3,
		vec![
			Frame { image: first, x: 0, y: 0, duration_ms: 0, blend: Blend::Source, disposal: Disposal::Keep },
			Frame { image: second, x: 1, y: 0, duration_ms: 35, blend: Blend::Over, disposal: Disposal::Previous },
		],
	)
	.unwrap();
	assert_eq!(decode(&encode(&animation, 100).unwrap()).unwrap(), animation);
	let mut unsupported_background = animation;
	unsupported_background.background = [1, 2, 3, 255];
	assert_eq!(encode(&unsupported_background, 100), Err(Error::Unsupported));
}

#[test]
fn compression_endpoints_preserve_frames_and_exercise_distinct_streams() {
	let image = |seed: u32| {
		let mut pixels = Vec::new();
		for y in 0..19u32 {
			for x in 0..31u32 {
				pixels.extend_from_slice(&[
					((x * 17 + y * 3 + seed) & 255) as u8,
					((x * 5 + y * 23 + seed * 2) & 255) as u8,
					((x * 11 + y * 7 + seed * 3) & 255) as u8,
					((x * 9 + y * 13 + seed * 5) & 255) as u8,
				]);
			}
		}
		pix::RgbaImage::new(31, 19, pixels).unwrap()
	};
	let animation = Animation::new(
		31,
		19,
		3,
		vec![
			Frame { image: image(1), x: 0, y: 0, duration_ms: 40, blend: Blend::Source, disposal: Disposal::Keep },
			Frame { image: image(7), x: 0, y: 0, duration_ms: 75, blend: Blend::Source, disposal: Disposal::Previous },
		],
	)
	.unwrap();
	let fast = encode(&animation, 0).unwrap();
	let compact = encode(&animation, 100).unwrap();
	assert_ne!(fast, compact, "APNG compression endpoints must exercise distinct deflate streams");
	assert_eq!(decode(&fast).unwrap(), animation);
	assert_eq!(decode(&compact).unwrap(), animation);
}

#[test]
fn decodes_indexed_frame_split_across_multiple_idat_chunks() {
	let image = pix::RgbaImage::new(2, 1, vec![255, 0, 0, 0, 0, 255, 0, 255]).unwrap();
	let source = png::encode_indexed(&image, 0, 100).unwrap();
	let expected = png::decode_rgba(&source).unwrap();
	let chunks = png_chunks(&source);
	let mut encoded = SIGNATURE.to_vec();
	for kind in [b"IHDR", b"PLTE", b"tRNS"] {
		if let Some((_, body)) = chunks.iter().find(|(candidate, _)| candidate == kind) {
			chunk(&mut encoded, kind, body).unwrap();
		}
	}
	let mut animation_header = Vec::new();
	animation_header.extend_from_slice(&1u32.to_be_bytes());
	animation_header.extend_from_slice(&0u32.to_be_bytes());
	chunk(&mut encoded, b"acTL", &animation_header).unwrap();
	chunk(&mut encoded, b"fcTL", &control(0, 2, 1)).unwrap();
	let (_, payload) = chunks.iter().find(|(kind, _)| kind == b"IDAT").unwrap();
	let split = payload.len() / 2;
	chunk(&mut encoded, b"IDAT", &payload[..split]).unwrap();
	let second_idat = encoded.len();
	chunk(&mut encoded, b"IDAT", &payload[split..]).unwrap();
	chunk(&mut encoded, b"IEND", &[]).unwrap();
	let mut nonconsecutive = encoded.clone();
	let mut ancillary = Vec::new();
	chunk(&mut ancillary, b"tEXt", b"key\0value").unwrap();
	nonconsecutive.splice(second_idat..second_idat, ancillary);
	assert_eq!(decode(&nonconsecutive), Err(Error::Invalid));

	let animation = decode(&encoded).unwrap();
	assert_eq!(animation.frames.len(), 1);
	assert_eq!(animation.frames[0].image, expected);
}

#[test]
fn decodes_animation_whose_static_image_is_not_a_frame() {
	let static_image = pix::RgbaImage::new(1, 1, vec![1, 2, 3, 255]).unwrap();
	let frame = pix::RgbaImage::new(1, 1, vec![4, 5, 6, 128]).unwrap();
	let source = png::encode_rgba(&static_image, png::EncodeOptions::default()).unwrap();
	let chunks = png_chunks(&source);
	let mut encoded = SIGNATURE.to_vec();
	let (_, header) = chunks.iter().find(|(kind, _)| kind == b"IHDR").unwrap();
	chunk(&mut encoded, b"IHDR", header).unwrap();
	let mut animation_header = Vec::new();
	animation_header.extend_from_slice(&1u32.to_be_bytes());
	animation_header.extend_from_slice(&0u32.to_be_bytes());
	chunk(&mut encoded, b"acTL", &animation_header).unwrap();
	let (_, default_payload) = chunks.iter().find(|(kind, _)| kind == b"IDAT").unwrap();
	chunk(&mut encoded, b"IDAT", default_payload).unwrap();
	chunk(&mut encoded, b"fcTL", &control(0, 1, 1)).unwrap();
	let mut frame_data = 1u32.to_be_bytes().to_vec();
	frame_data.extend_from_slice(&png::encode_rgba_payload(&frame, 50).unwrap());
	chunk(&mut encoded, b"fdAT", &frame_data).unwrap();
	chunk(&mut encoded, b"IEND", &[]).unwrap();

	let animation = decode(&encoded).unwrap();
	assert_eq!(animation.frames.len(), 1);
	assert_eq!(animation.frames[0].image, frame);
}

#[test]
fn rejects_static_png_and_corrupt_sequence() {
	let static_png = png::encode_rgba(&pix::RgbaImage::new(1, 1, vec![0; 4]).unwrap(), png::EncodeOptions::default()).unwrap();
	assert!(decode(&static_png).is_err());
	let animation = Animation::still(pix::RgbaImage::new(1, 1, vec![0; 4]).unwrap());
	let mut encoded = encode(&animation, 50).unwrap();
	let fctl = encoded.windows(4).position(|window| window == b"fcTL").unwrap();
	encoded[fctl + 4] ^= 1;
	assert_eq!(decode(&encoded), Err(Error::Invalid));
}

#[test]
fn decodes_external_apng_and_separate_default_image() {
	let animation = decode(include_bytes!("../tests/data/external-animation.png")).unwrap();
	assert_eq!((animation.width, animation.height, animation.loop_count, animation.frames.len()), (31, 19, 2, 3));
	assert!(animation.frames.iter().all(|frame| frame.duration_ms == 60));
	assert_eq!(displayed_hashes(&animation), vec![0x5fad_a2ef_c37e_917f, 0xfa2f_ff14_7b88_5f15, 0x566d_1dac_d369_b6ab]);

	let separate = decode(include_bytes!("../tests/data/external-separate-default.png")).unwrap();
	assert_eq!((separate.width, separate.height, separate.loop_count, separate.frames.len()), (31, 19, 2, 2));
	assert!(separate.frames.iter().all(|frame| frame.duration_ms == 60));
	assert_eq!(displayed_hashes(&separate), vec![0xfa2f_ff14_7b88_5f15, 0x566d_1dac_d369_b6ab]);
}

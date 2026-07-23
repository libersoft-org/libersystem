use super::*;
use alloc::vec;

fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn displayed_hashes(animation: &Animation) -> Vec<u64> {
	let mut compositor = pix::Compositor::new_with_background(animation.width, animation.height, animation.background).unwrap();
	animation.frames.iter().map(|frame| fnv1a(&compositor.render(frame).unwrap().pixels)).collect()
}

fn image_structure(data: &[u8]) -> Vec<(u8, Vec<u8>)> {
	let packed = data[10];
	let mut cursor = 13 + if packed & 0x80 != 0 { (1usize << ((packed & 7) + 1)) * 3 } else { 0 };
	let mut images = Vec::new();
	while data[cursor] != 0x3b {
		if data[cursor] == 0x21 {
			cursor += 2;
			if data[cursor - 1] == 0xf9 {
				cursor += 6;
			} else {
				let fixed = data[cursor] as usize;
				cursor += fixed + 1;
				loop {
					let length = data[cursor] as usize;
					cursor += 1;
					if length == 0 {
						break;
					}
					cursor += length;
				}
			}
			continue;
		}
		assert_eq!(data[cursor], 0x2c);
		let descriptor = data[cursor + 9];
		cursor += 10;
		if descriptor & 0x80 != 0 {
			cursor += (1usize << ((descriptor & 7) + 1)) * 3;
		}
		cursor += 1;
		let mut blocks = Vec::new();
		loop {
			let length = data[cursor];
			cursor += 1;
			if length == 0 {
				break;
			}
			blocks.push(length);
			cursor += length as usize;
		}
		images.push((descriptor, blocks));
	}
	images
}

const IMAGEMAGICK_BACKGROUND_GIF: &[u8] = &[
	0x47,
	0x49,
	0x46,
	0x38,
	0x39,
	0x61,
	0x02,
	0x00,
	0x01,
	0x00,
	0xf1,
	0x01,
	0x00,
	0xff,
	0x00,
	0x00,
	0x00,
	0x00,
	0xff,
	0x00,
	0x80,
	0x00,
	0x00,
	0x00,
	0x00,
	0x21,
	0xff,
	0x0b,
	0x4e,
	0x45,
	0x54,
	0x53,
	0x43,
	0x41,
	0x50,
	0x45,
	0x32,
	0x2e,
	0x30,
	0x03,
	0x01,
	0x00,
	0x00,
	0x00,
	0x21,
	0xf9,
	0x04,
	0x04,
	0x03,
	0x00,
	0x00,
	0x00,
	0x2c,
	0x00,
	0x00,
	0x00,
	0x00,
	0x02,
	0x00,
	0x01,
	0x00,
	0x00,
	0x02,
	0x02,
	0x0c,
	0x0a,
	0x00,
	0x21,
	0xf9,
	0x04,
	0x04,
	0x03,
	0x00,
	0x00,
	0x00,
	0x2c,
	0x01,
	0x00,
	0x00,
	0x00,
	0x01,
	0x00,
	0x01,
	0x00,
	0x00,
	0x02,
	0x02,
	0x54,
	0x01,
	0x00,
	0x3b,
];

#[test]
fn timing_loop_disposal_and_binary_alpha_round_trip() {
	let first = pix::RgbaImage::new(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
	let second = pix::RgbaImage::new(1, 1, vec![0, 255, 0, 255]).unwrap();
	let animation = Animation::new(
		2,
		1,
		5,
		vec![
			Frame { image: first, x: 0, y: 0, duration_ms: 0, blend: Blend::Over, disposal: Disposal::Background },
			Frame { image: second, x: 1, y: 0, duration_ms: 30, blend: Blend::Over, disposal: Disposal::Previous },
		],
	)
	.unwrap();
	assert_eq!(decode(&encode(&animation).unwrap()).unwrap(), animation);
}

#[test]
fn logical_screen_background_matches_imagemagick_transparency_convention() {
	let opaque = decode(IMAGEMAGICK_BACKGROUND_GIF).unwrap();
	assert_eq!(opaque.background, [0, 0, 255, 255]);
	let mut compositor = pix::Compositor::new_with_background(opaque.width, opaque.height, opaque.background).unwrap();
	let displayed: Vec<_> = opaque.frames.iter().map(|frame| compositor.render(frame).unwrap()).collect();
	assert_eq!(displayed[0].pixels, vec![0, 0, 255, 255, 255, 0, 0, 255]);
	assert_eq!(displayed[1].pixels, vec![0, 0, 255, 255, 0, 128, 0, 255]);

	let mut transparent = IMAGEMAGICK_BACKGROUND_GIF.to_vec();
	transparent[47] |= 1;
	transparent[50] = transparent[11];
	let transparent = decode(&transparent).unwrap();
	assert_eq!(transparent.background, [0, 0, 255, 0]);

	let mut later_only = IMAGEMAGICK_BACKGROUND_GIF.to_vec();
	later_only[70] |= 1;
	later_only[73] = later_only[11];
	assert_eq!(decode(&later_only).unwrap().background, [0, 0, 255, 255]);

	let mut bad_background = IMAGEMAGICK_BACKGROUND_GIF.to_vec();
	bad_background[11] = 4;
	assert_eq!(decode(&bad_background), Err(Error::Invalid));
	let mut reserved_control = IMAGEMAGICK_BACKGROUND_GIF.to_vec();
	reserved_control[47] |= 0x20;
	assert_eq!(decode(&reserved_control), Err(Error::Invalid));
}

#[test]
fn opaque_and_transparent_background_round_trip_exactly() {
	for background in [[7, 17, 27, 255], [7, 17, 27, 0]] {
		let animation = Animation::new_with_background(
			2,
			1,
			background,
			2,
			vec![
				Frame { image: pix::RgbaImage::new(1, 1, vec![255, 0, 0, 255]).unwrap(), x: 0, y: 0, duration_ms: 0, blend: Blend::Over, disposal: Disposal::Background },
				Frame { image: pix::RgbaImage::new(1, 1, vec![0, 255, 0, 255]).unwrap(), x: 1, y: 0, duration_ms: 30, blend: Blend::Over, disposal: Disposal::Keep },
			],
		)
		.unwrap();
		let encoded = encode(&animation).unwrap();
		let background_index = encoded[11] as usize;
		assert_eq!(&encoded[13 + background_index * 3..16 + background_index * 3], &background[..3]);
		assert_eq!(decode(&encoded).unwrap(), animation);
	}
	let partial = Animation::new_with_background(1, 1, [1, 2, 3, 128], 1, vec![Frame { image: pix::RgbaImage::new(1, 1, vec![4, 5, 6, 255]).unwrap(), x: 0, y: 0, duration_ms: 1, blend: Blend::Over, disposal: Disposal::Keep }]).unwrap();
	assert_eq!(encode(&partial), Err(Error::Unsupported));
}

#[test]
fn quantizes_partial_alpha_and_more_than_256_exact_colors() {
	let partial = Animation::still(pix::RgbaImage::new(1, 1, vec![1, 2, 3, 4]).unwrap());
	let partial = decode(&encode(&partial).unwrap()).unwrap();
	assert_eq!(partial.frames[0].image.pixels, vec![0, 0, 0, 0]);
	let mut pixels = Vec::new();
	for value in 0..257u16 {
		pixels.extend_from_slice(&[(value & 255) as u8, (value >> 8) as u8, 0, 255]);
	}
	let many = Animation::still(pix::RgbaImage::new(257, 1, pixels).unwrap());
	let decoded = decode(&encode(&many).unwrap()).unwrap();
	assert_eq!(decoded.frames[0].image.width, 257);
	assert!(decoded.frames[0].image.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
}

#[test]
fn quality_changes_palette_budget() {
	let mut pixels = Vec::new();
	for value in 0..1024u32 {
		pixels.extend_from_slice(&[(value & 255) as u8, ((value * 37) & 255) as u8, ((value * 91) & 255) as u8, 255]);
	}
	let animation = Animation::still(pix::RgbaImage::new(32, 32, pixels).unwrap());
	let low = encode_with_options(&animation, EncodeOptions { quality: 0, dither: true, alpha_threshold: 128 }).unwrap();
	let high = encode_with_options(&animation, EncodeOptions { quality: 100, dither: true, alpha_threshold: 128 }).unwrap();
	assert!(low.len() < high.len());
	assert_eq!(decode(&low).unwrap().frames[0].image.width, 32);
	assert_eq!(decode(&high).unwrap().frames[0].image.width, 32);
}

#[test]
fn decodes_external_interlace_local_palette_disposal_and_subblocks() {
	for data in [include_bytes!("../tests/data/external-animation.gif").as_slice(), include_bytes!("../tests/data/derived-local-subblocks.gif").as_slice()] {
		let animation = decode(data).unwrap();
		assert_eq!((animation.width, animation.height, animation.loop_count, animation.frames.len()), (29, 17, 2, 3));
		assert_eq!(animation.frames.iter().map(|frame| (frame.x, frame.y, frame.image.width, frame.image.height)).collect::<Vec<_>>(), vec![(0, 0, 29, 17), (5, 4, 11, 9), (12, 2, 9, 7)]);
		assert_eq!(animation.frames.iter().map(|frame| frame.duration_ms).collect::<Vec<_>>(), vec![0, 30, 50]);
		assert_eq!(animation.frames.iter().map(|frame| frame.disposal).collect::<Vec<_>>(), vec![Disposal::Keep, Disposal::Background, Disposal::Previous]);
		assert_eq!(displayed_hashes(&animation), vec![0x16b2_57ac_54ae_dc1c, 0xc053_dfb1_daa4_c81e, 0x1e5f_d5b6_c7a7_055a]);
	}

	let external = include_bytes!("../tests/data/external-animation.gif");
	let derived = include_bytes!("../tests/data/derived-local-subblocks.gif");
	let external_structure = image_structure(external);
	let derived_structure = image_structure(derived);
	assert_eq!(external_structure.len(), 3);
	assert!(external_structure.iter().all(|(descriptor, _)| descriptor & 0x40 != 0 && descriptor & 0x80 == 0));
	assert!(derived_structure.iter().all(|(descriptor, _)| descriptor & 0x40 != 0));
	assert!(derived_structure[1].0 & 0x80 != 0);
	assert!(derived_structure.iter().flat_map(|(_, blocks)| blocks).any(|length| *length == 1));
}

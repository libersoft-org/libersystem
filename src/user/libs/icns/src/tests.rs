use super::*;
use alloc::vec;

fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn solid(size: u32, color: [u8; 4]) -> pix::RgbaImage {
	let mut pixels = Vec::new();
	pixels.resize(size as usize * size as usize * 4, 0);
	for pixel in pixels.chunks_exact_mut(4) {
		pixel.copy_from_slice(&color);
	}
	pix::RgbaImage::new(size, size, pixels).unwrap()
}

#[test]
fn modern_png_entries_round_trip_and_largest_wins() {
	let small = solid(128, [1, 2, 3, 4]);
	let large = solid(256, [5, 6, 7, 8]);
	let encoded = encode(&[large.clone(), small.clone()], 100).unwrap();
	assert_eq!(decode_all(&encoded).unwrap(), vec![small, large.clone()]);
	assert_eq!(decode(&encoded).unwrap(), large);
}

#[test]
fn compression_endpoints_preserve_modern_entry_and_exercise_distinct_streams() {
	let mut pixels = Vec::new();
	for y in 0..128u32 {
		for x in 0..128u32 {
			pixels.extend_from_slice(&[((x * 17 + y * 3) & 255) as u8, ((x * 5 + y * 23) & 255) as u8, ((x * 11 + y * 7) & 255) as u8, ((x * 9 + y * 13) & 255) as u8]);
		}
	}
	let image = pix::RgbaImage::new(128, 128, pixels).unwrap();
	let fast = encode(core::slice::from_ref(&image), 0).unwrap();
	let compact = encode(core::slice::from_ref(&image), 100).unwrap();
	assert_ne!(fast, compact, "ICNS compression endpoints must exercise distinct embedded PNG streams");
	assert_eq!(decode(&fast).unwrap(), image);
	assert_eq!(decode(&compact).unwrap(), image);
}

#[test]
fn decodes_external_icnsutils_classic_and_modern_corpus() {
	let decoded = decode_all(include_bytes!("../tests/data/external-gradient.icns")).unwrap();
	assert_eq!(decoded.iter().map(|image| image.width).collect::<Vec<_>>(), vec![16, 32, 128]);
	assert_eq!(decoded.iter().map(|image| fnv1a(&image.pixels)).collect::<Vec<_>>(), vec![0x40df_aed0_3a6f_7825, 0x1f5c_3caa_89bf_3ee5, 0xcec7_119d_af9c_2425]);
	assert_eq!(fnv1a(&decode(include_bytes!("../tests/data/external-gradient.icns")).unwrap().pixels), 0xcec7_119d_af9c_2425);
	let classic_48 = decode(include_bytes!("../tests/data/external-48.icns")).unwrap();
	assert_eq!((classic_48.width, classic_48.height, fnv1a(&classic_48.pixels)), (48, 48, 0x2fc2_7b31_ff9d_8545));
	let classic_128 = decode(include_bytes!("../tests/data/external-128-legacy.icns")).unwrap();
	assert_eq!((classic_128.width, classic_128.height, fnv1a(&classic_128.pixels)), (128, 128, 0xcec7_119d_af9c_2425));
}

#[test]
fn classic_rle_and_alpha_entries_round_trip() {
	let small = solid(16, [5, 6, 7, 8]);
	let mut varied = solid(32, [0, 0, 0, 255]);
	for (index, pixel) in varied.pixels.chunks_exact_mut(4).enumerate() {
		pixel.copy_from_slice(&[index as u8, (index / 3) as u8, (index * 17) as u8, (index / 5) as u8]);
	}
	let encoded = encode(&[varied.clone(), small.clone()], 50).unwrap();
	let decoded = decode_all(&encoded).unwrap();
	assert_eq!(decoded, vec![small, varied.clone()]);
	assert_eq!(decode(&encoded).unwrap(), varied);
}

#[test]
fn rejects_nonstandard_output_and_malformed_legacy_rle() {
	assert_eq!(encode(&[solid(64, [0; 4])], 50), Err(Error::Unsupported));
	let legacy = b"icns\x00\x00\x00\x10is32\x00\x00\x00\x08";
	assert_eq!(decode(legacy), Err(Error::Truncated));
}

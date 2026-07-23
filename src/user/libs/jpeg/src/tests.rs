use super::*;
use alloc::vec;

fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn mean_error(left: &pix::RgbaImage, right: &pix::RgbaImage) -> f64 {
	left.pixels.chunks_exact(4).zip(right.pixels.chunks_exact(4)).flat_map(|(left, right)| (0..3).map(move |channel| (left[channel] as i16 - right[channel] as i16).unsigned_abs() as u64)).sum::<u64>() as f64 / (left.pixel_count() * 3) as f64
}

#[test]
fn quality_endpoints_decode_with_expected_fidelity() {
	let mut pixels = Vec::new();
	for y in 0..16u8 {
		for x in 0..16u8 {
			pixels.extend_from_slice(&[x * 16, y * 16, x.wrapping_add(y) * 8, 255]);
		}
	}
	let image = pix::RgbaImage::new(16, 16, pixels).unwrap();
	let low = decode(&encode(&image, 10).unwrap()).unwrap();
	let high = decode(&encode(&image, 100).unwrap()).unwrap();
	assert!(mean_error(&high, &image) < mean_error(&low, &image));
	assert!(mean_error(&high, &image) < 4.0);
}

#[test]
fn rejects_alpha_invalid_quality_and_progressive_marker() {
	assert_eq!(encode(&pix::RgbaImage::new(1, 1, vec![1, 2, 3, 4]).unwrap(), 90), Err(Error::Unsupported));
	assert_eq!(encode(&pix::RgbaImage::new(1, 1, vec![1, 2, 3, 255]).unwrap(), 101), Err(Error::Invalid));
	assert_eq!(is_progressive(&[0xff, 0xd8, 0xff, 0xc2]), Ok(true));
}

#[test]
fn decodes_external_baseline_profiles_and_rejects_progressive() {
	let gray = include_bytes!("../tests/data/external-gray-baseline.jpg");
	let decoded = decode(gray).unwrap();
	assert_eq!((decoded.width, decoded.height, fnv1a(&decoded.pixels)), (23, 11, 0x114e_cee6_0ce1_ccc3));

	let ycbcr = include_bytes!("../tests/data/external-ycbcr-baseline.jpg");
	let decoded = decode(ycbcr).unwrap();
	let expected = include_bytes!("../tests/data/external-ycbcr-baseline.rgba");
	let maximum = decoded.pixels.iter().zip(expected).map(|(actual, expected)| actual.abs_diff(*expected)).max().unwrap();
	let total = decoded.pixels.iter().zip(expected).map(|(actual, expected)| u64::from(actual.abs_diff(*expected))).sum::<u64>();
	assert_eq!((decoded.width, decoded.height, fnv1a(&decoded.pixels)), (19, 13, 0xaa27_fe0a_c440_e9e4));
	assert!(maximum <= 2);
	assert!(total as f64 / decoded.pixels.len() as f64 <= 0.25);

	let progressive = include_bytes!("../tests/data/external-progressive.jpg");
	assert_eq!(is_progressive(progressive), Ok(true));
	assert_eq!(decode(progressive), Err(Error::Unsupported));
}

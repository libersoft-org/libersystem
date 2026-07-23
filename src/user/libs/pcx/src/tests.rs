use super::*;
use alloc::vec;

fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

#[test]
fn rgb_round_trips_odd_width_and_escaped_bytes() {
	let image = pix::RgbaImage::new(3, 2, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 192, 193, 194, 255, 1, 2, 3, 255, 4, 5, 6, 255]).unwrap();
	assert_eq!(decode(&encode(&image).unwrap()).unwrap(), image);
}

#[test]
fn decodes_external_imagemagick_indexed_and_rgb_profiles() {
	let indexed = include_bytes!("../tests/data/indexed.pcx");
	assert_eq!((indexed[65], u16::from_le_bytes([indexed[66], indexed[67]])), (1, 17));
	assert_eq!(indexed[indexed.len() - PALETTE_LEN], 0x0c);
	let indexed = decode(indexed).unwrap();
	assert_eq!((indexed.width, indexed.height, indexed.pixels.len(), fnv1a(&indexed.pixels)), (17, 9, 612, 0x847c_d3e9_781c_6ac8));

	let rgb = include_bytes!("../tests/data/rgb.pcx");
	assert_eq!((rgb[65], u16::from_le_bytes([rgb[66], rgb[67]])), (3, 19));
	let rgb = decode(rgb).unwrap();
	assert_eq!((rgb.width, rgb.height, rgb.pixels.len(), fnv1a(&rgb.pixels)), (19, 7, 532, 0x4f4f_f2d9_0ee4_ac63));
}

#[test]
fn indexed_round_trips_exact_colors_and_honors_quality_budget() {
	let exact = pix::RgbaImage::new(3, 1, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255]).unwrap();
	let encoded = encode_indexed(&exact, 100).unwrap();
	assert_eq!(encoded[65], 1);
	assert_eq!(encoded[encoded.len() - PALETTE_LEN], 0x0c);
	assert_eq!(decode(&encoded).unwrap(), exact);

	let mut pixels = Vec::new();
	for value in 0..512u32 {
		pixels.extend_from_slice(&[value as u8, (value >> 1) as u8, value.wrapping_mul(47) as u8, 255]);
	}
	let true_color = pix::RgbaImage::new(512, 1, pixels).unwrap();
	let low = encode_indexed(&true_color, 0).unwrap();
	let high = encode_indexed(&true_color, 100).unwrap();
	assert_ne!(&low[low.len() - 768..], &high[high.len() - 768..]);
	assert_eq!(decode(&low).unwrap().width, true_color.width);
}

#[test]
fn rejects_truncation_and_unsupported_depth() {
	assert_eq!(decode(&[]), Err(Error::Truncated));
	let mut header = [0u8; HEADER_LEN];
	header[0] = 0x0a;
	header[1] = 5;
	header[2] = 1;
	header[3] = 4;
	assert_eq!(decode(&header), Err(Error::Unsupported));
	header[1] = 4;
	header[3] = 8;
	assert_eq!(decode(&header), Err(Error::Unsupported));
}

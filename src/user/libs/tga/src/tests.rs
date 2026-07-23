use super::*;
use alloc::vec;

fn fnv1a(bytes: &[u8]) -> u64 {
	let mut hash = 0xcbf2_9ce4_8422_2325u64;
	for &byte in bytes {
		hash ^= byte as u64;
		hash = hash.wrapping_mul(0x100_0000_01b3);
	}
	hash
}

#[test]
fn raw_and_rle_round_trip_rgba() {
	let image = pix::RgbaImage::new(3, 2, vec![255, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 255, 1, 2, 3, 4, 1, 2, 3, 4]).unwrap();
	for rle in [false, true] {
		assert_eq!(decode(&encode(&image, EncodeOptions { rle }).unwrap()).unwrap(), image);
	}
}

#[test]
fn rejects_truncation_and_unsupported_colormap() {
	assert_eq!(decode(&[]), Err(Error::Truncated));
	let mut header = [0u8; 18];
	header[1] = 1;
	header[2] = 2;
	assert_eq!(decode(&header), Err(Error::Unsupported));
	let image = pix::RgbaImage::new(1, 1, vec![1, 2, 3, 255]).unwrap();
	let mut reserved = encode(&image, EncodeOptions { rle: false }).unwrap();
	reserved[17] |= 0x40;
	assert_eq!(decode(&reserved), Err(Error::Invalid));
}

#[test]
fn decodes_external_imagemagick_profiles_and_origins() {
	let fixtures: [(&[u8], u8, u8, u8, u64); 8] = [
		(include_bytes!("../tests/data/raw24-top-left.tga"), 2, 24, 0x20, 0xd82e_2877_e771_e430),
		(include_bytes!("../tests/data/raw24-top-right.tga"), 2, 24, 0x30, 0xd82e_2877_e771_e430),
		(include_bytes!("../tests/data/rle24-bottom-left.tga"), 10, 24, 0x00, 0xd82e_2877_e771_e430),
		(include_bytes!("../tests/data/rle24-bottom-right.tga"), 10, 24, 0x10, 0xd82e_2877_e771_e430),
		(include_bytes!("../tests/data/raw32-top-left.tga"), 2, 32, 0x28, 0x6d9c_01b7_743f_6b90),
		(include_bytes!("../tests/data/raw32-bottom-right-id.tga"), 2, 32, 0x18, 0x6d9c_01b7_743f_6b90),
		(include_bytes!("../tests/data/rle32-top-right.tga"), 10, 32, 0x38, 0x6d9c_01b7_743f_6b90),
		(include_bytes!("../tests/data/rle32-bottom-left.tga"), 10, 32, 0x08, 0x6d9c_01b7_743f_6b90),
	];
	for (data, image_type, depth, descriptor, expected_hash) in fixtures {
		assert_eq!(data[2], image_type);
		assert_eq!(data[16], depth);
		assert_eq!(data[17], descriptor);
		let image = decode(data).unwrap();
		assert_eq!((image.width, image.height), (11, 7));
		assert_eq!(fnv1a(&image.pixels), expected_hash);
	}
	let with_id = include_bytes!("../tests/data/raw32-bottom-right-id.tga");
	assert_eq!(with_id[0], 22);
	assert_eq!(&with_id[18..40], b"LiberSystem TGA corpus");
}

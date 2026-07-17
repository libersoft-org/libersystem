use super::*;
use alloc::vec;

fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn dib_icon(xor: &[u8], mask: &[u8]) -> Vec<u8> {
	let payload_len = 40 + xor.len() + mask.len();
	let mut icon = vec![0; 22 + payload_len];
	icon[..6].copy_from_slice(&[0, 0, 1, 0, 1, 0]);
	icon[6..10].copy_from_slice(&[2, 1, 0, 0]);
	icon[10..12].copy_from_slice(&1u16.to_le_bytes());
	icon[12..14].copy_from_slice(&32u16.to_le_bytes());
	icon[14..18].copy_from_slice(&(payload_len as u32).to_le_bytes());
	icon[18..22].copy_from_slice(&22u32.to_le_bytes());
	let dib = &mut icon[22..62];
	dib[..4].copy_from_slice(&40u32.to_le_bytes());
	dib[4..8].copy_from_slice(&2i32.to_le_bytes());
	dib[8..12].copy_from_slice(&2i32.to_le_bytes());
	dib[12..14].copy_from_slice(&1u16.to_le_bytes());
	dib[14..16].copy_from_slice(&32u16.to_le_bytes());
	dib[20..24].copy_from_slice(&(xor.len() as u32).to_le_bytes());
	icon[62..62 + xor.len()].copy_from_slice(xor);
	icon[62 + xor.len()..].copy_from_slice(mask);
	icon
}

#[test]
fn png_entries_round_trip_and_best_size_wins() {
	let small = pix::RgbaImage::new(1, 1, vec![1, 2, 3, 4]).unwrap();
	let large = pix::RgbaImage::new(2, 2, vec![5; 16]).unwrap();
	let encoded = encode(&[small.clone(), large.clone()], 100).unwrap();
	assert_eq!(decode_all(&encoded).unwrap(), vec![small, large.clone()]);
	assert_eq!(decode(&encoded).unwrap(), large);
}

#[test]
fn compression_endpoints_preserve_png_entry_and_exercise_distinct_streams() {
	let mut pixels = Vec::new();
	for y in 0..32u32 {
		for x in 0..32u32 {
			pixels.extend_from_slice(&[((x * 17 + y * 3) & 255) as u8, ((x * 5 + y * 23) & 255) as u8, ((x * 11 + y * 7) & 255) as u8, ((x * 9 + y * 13) & 255) as u8]);
		}
	}
	let image = pix::RgbaImage::new(32, 32, pixels).unwrap();
	let fast = encode(core::slice::from_ref(&image), 0).unwrap();
	let compact = encode(core::slice::from_ref(&image), 100).unwrap();
	assert_ne!(fast, compact, "ICO compression endpoints must exercise distinct embedded PNG streams");
	assert_eq!(decode(&fast).unwrap(), image);
	assert_eq!(decode(&compact).unwrap(), image);
}

#[test]
fn thirty_two_bit_xor_alpha_ignores_and_mask_and_needs_no_fallback() {
	let nonzero = dib_icon(&[0, 0, 255, 128, 0, 255, 0, 255], &[0xc0, 0, 0, 0]);
	assert_eq!(decode(&nonzero).unwrap().pixels, vec![255, 0, 0, 128, 0, 255, 0, 255]);
	let all_zero = dib_icon(&[0, 0, 255, 0, 0, 255, 0, 0], &[0x80, 0, 0, 0]);
	assert_eq!(decode(&all_zero).unwrap().pixels, vec![255, 0, 0, 0, 0, 255, 0, 0]);
	let no_mask = dib_icon(&[0, 0, 255, 128, 0, 255, 0, 255], &[]);
	assert_eq!(decode(&no_mask).unwrap().pixels, vec![255, 0, 0, 128, 0, 255, 0, 255]);
}

#[test]
fn rejects_bad_table_and_oversized_entry() {
	assert_eq!(decode(&[]), Err(Error::Truncated));
	let large = pix::RgbaImage::new(257, 1, vec![0; 257 * 4]).unwrap();
	assert_eq!(encode(&[large], 50), Err(Error::Unsupported));
	let image = pix::RgbaImage::new(1, 1, vec![1, 2, 3, 255]).unwrap();
	let mut overlap = encode(&[image.clone(), image], 50).unwrap();
	let first_offset = overlap[18..22].to_vec();
	overlap[34..38].copy_from_slice(&first_offset);
	assert_eq!(decode(&overlap), Err(Error::Invalid));
	let mut empty = overlap;
	empty[14..18].fill(0);
	assert_eq!(decode(&empty), Err(Error::Invalid));
}

#[test]
fn decodes_external_imagemagick_png_and_dib_profiles() {
	let png = include_bytes!("../tests/data/external-png.ico");
	let offset = u32::from_le_bytes(png[18..22].try_into().unwrap()) as usize;
	assert_eq!(&png[offset..offset + 8], b"\x89PNG\r\n\x1a\n");
	let image = decode(png).unwrap();
	assert_eq!((image.width, image.height, fnv1a(&image.pixels)), (256, 256, 0x58a2_1c35_7737_84bc));

	for (data, expected_hash) in [
		(include_bytes!("../tests/data/external-dib-alpha.ico").as_slice(), 0x2cae_8d72_a65b_cac1),
		(include_bytes!("../tests/data/external-dib-maskless.ico").as_slice(), 0x2cae_8d72_a65b_cac1),
		(include_bytes!("../tests/data/external-dib-zero-alpha.ico").as_slice(), 0x8fa6_b411_bfca_0325),
	] {
		let offset = u32::from_le_bytes(data[18..22].try_into().unwrap()) as usize;
		assert_eq!(&data[offset..offset + 4], &40u32.to_le_bytes());
		let image = decode(data).unwrap();
		assert_eq!((image.width, image.height, fnv1a(&image.pixels)), (32, 32, expected_hash));
	}

	let zero = include_bytes!("../tests/data/external-dib-zero-alpha.ico");
	let offset = u32::from_le_bytes(zero[18..22].try_into().unwrap()) as usize;
	assert!(zero[offset + 43..offset + 40 + 32 * 32 * 4].iter().step_by(4).all(|alpha| *alpha == 0));
	let maskless = include_bytes!("../tests/data/external-dib-maskless.ico");
	assert_eq!(u32::from_le_bytes(maskless[14..18].try_into().unwrap()) as usize, 40 + 32 * 32 * 4);
}

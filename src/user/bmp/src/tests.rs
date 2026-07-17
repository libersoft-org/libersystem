use super::*;
use alloc::vec;

fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn info_bmp(width: i32, height: i32, bits: u16, compression: u32, palette: &[[u8; 4]], pixels: &[u8]) -> Vec<u8> {
	let pixel_offset = FILE_HEADER_LEN + INFO_HEADER_LEN + palette.len() * 4;
	let file_len = pixel_offset + pixels.len();
	let mut bmp = vec![0; file_len];
	bmp[..2].copy_from_slice(b"BM");
	bmp[2..6].copy_from_slice(&(file_len as u32).to_le_bytes());
	bmp[10..14].copy_from_slice(&(pixel_offset as u32).to_le_bytes());
	bmp[14..18].copy_from_slice(&(INFO_HEADER_LEN as u32).to_le_bytes());
	bmp[18..22].copy_from_slice(&width.to_le_bytes());
	bmp[22..26].copy_from_slice(&height.to_le_bytes());
	bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
	bmp[28..30].copy_from_slice(&bits.to_le_bytes());
	bmp[30..34].copy_from_slice(&compression.to_le_bytes());
	bmp[34..38].copy_from_slice(&(pixels.len() as u32).to_le_bytes());
	bmp[46..50].copy_from_slice(&(palette.len() as u32).to_le_bytes());
	for (index, entry) in palette.iter().enumerate() {
		let start = FILE_HEADER_LEN + INFO_HEADER_LEN + index * 4;
		bmp[start..start + 4].copy_from_slice(entry);
	}
	bmp[pixel_offset..].copy_from_slice(pixels);
	bmp
}

fn bitfield_bmp(dib_size: usize, compression: u32, masks: [u32; 4], pixels: &[u8]) -> Vec<u8> {
	let external = if dib_size >= 56 { 0 } else { 16 };
	let pixel_offset = FILE_HEADER_LEN + dib_size + external;
	let file_len = pixel_offset + pixels.len();
	let mut bmp = vec![0; file_len];
	bmp[..2].copy_from_slice(b"BM");
	bmp[2..6].copy_from_slice(&(file_len as u32).to_le_bytes());
	bmp[10..14].copy_from_slice(&(pixel_offset as u32).to_le_bytes());
	bmp[14..18].copy_from_slice(&(dib_size as u32).to_le_bytes());
	bmp[18..22].copy_from_slice(&2i32.to_le_bytes());
	bmp[22..26].copy_from_slice(&1i32.to_le_bytes());
	bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
	bmp[28..30].copy_from_slice(&32u16.to_le_bytes());
	bmp[30..34].copy_from_slice(&compression.to_le_bytes());
	bmp[34..38].copy_from_slice(&(pixels.len() as u32).to_le_bytes());
	let mask_offset = FILE_HEADER_LEN + 40;
	for (index, mask) in masks.iter().enumerate() {
		bmp[mask_offset + index * 4..mask_offset + index * 4 + 4].copy_from_slice(&mask.to_le_bytes());
	}
	bmp[pixel_offset..].copy_from_slice(pixels);
	bmp
}

fn colors(image: &Image) -> Vec<u32> {
	image.pixels.chunks_exact(4).map(|pixel| u32::from_le_bytes(pixel.try_into().unwrap())).collect()
}

#[test]
fn decodes_bottom_up_and_top_down_24_bit_rows() {
	let bottom_up = info_bmp(2, 2, 24, BI_RGB, &[], &[0xff, 0, 0, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0xff, 0, 0xff, 0, 0, 0]);
	let top_down = info_bmp(2, -2, 24, BI_RGB, &[], &[0, 0, 0xff, 0, 0xff, 0, 0, 0, 0xff, 0, 0, 0xff, 0xff, 0xff, 0, 0]);
	let expected = vec![0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0x00ff_ffff];
	assert_eq!(colors(&decode(&bottom_up).unwrap()), expected);
	assert_eq!(colors(&decode(&top_down).unwrap()), expected);
	assert_eq!(decode_rgba(&bottom_up).unwrap().pixels, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]);
}

#[test]
fn decodes_explicit_alpha_masks_but_ignores_bi_rgb_high_byte() {
	let masks = [0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0xff00_0000];
	let pixels = [0, 0, 255, 128, 0, 255, 0, 0];
	for bmp in [bitfield_bmp(108, BI_BITFIELDS, masks, &pixels), bitfield_bmp(40, BI_ALPHA_BITFIELDS, masks, &pixels)] {
		assert_eq!(decode_rgba(&bmp).unwrap().pixels, vec![255, 0, 0, 128, 0, 255, 0, 0]);
		assert_eq!(colors(&decode(&bmp).unwrap()), vec![0x00ff_0000, 0x0000_ff00]);
	}
	let rgb = info_bmp(2, 1, 32, BI_RGB, &[], &pixels);
	assert_eq!(decode_rgba(&rgb).unwrap().pixels, vec![255, 0, 0, 255, 0, 255, 0, 255]);
}

#[test]
fn decodes_indexed_rows_and_rejects_bad_palette_indices() {
	let palette = [[0, 0, 0, 0], [0xff, 0xff, 0xff, 0]];
	let bmp = info_bmp(4, 1, 1, BI_RGB, &palette, &[0b1010_0000, 0, 0, 0]);
	assert_eq!(colors(&decode(&bmp).unwrap()), vec![0x00ff_ffff, 0, 0x00ff_ffff, 0]);

	let invalid = info_bmp(1, 1, 8, BI_RGB, &palette, &[2, 0, 0, 0]);
	assert_eq!(decode(&invalid), Err(Error::Invalid));
}

#[test]
fn decodes_rle8_encoded_and_absolute_runs() {
	let palette = [[0, 0, 0, 0], [0, 0, 0xff, 0]];
	let encoded = [4, 1, 0, 0, 0, 4, 0, 1, 0, 1, 0, 0, 0, 1];
	let image = decode(&info_bmp(4, 2, 8, BI_RLE8, &palette, &encoded)).unwrap();
	assert_eq!(colors(&image), vec![0, 0x00ff_0000, 0, 0x00ff_0000, 0x00ff_0000, 0x00ff_0000, 0x00ff_0000, 0x00ff_0000]);
}

#[test]
fn decodes_rle4_nibble_runs() {
	let palette = [[0, 0, 0, 0], [0, 0, 0xff, 0], [0, 0xff, 0, 0]];
	let image = decode(&info_bmp(4, 1, 4, BI_RLE4, &palette, &[4, 0x12, 0, 1])).unwrap();
	assert_eq!(colors(&image), vec![0x00ff_0000, 0x0000_ff00, 0x00ff_0000, 0x0000_ff00]);
}

#[test]
fn rejects_truncation_oversized_geometry_and_out_of_bounds_rle() {
	assert_eq!(decode(b"BM"), Err(Error::Truncated));
	let mut too_wide = info_bmp(1, 1, 24, BI_RGB, &[], &[0, 0, 0, 0]);
	too_wide[18..22].copy_from_slice(&20_000i32.to_le_bytes());
	assert_eq!(decode(&too_wide), Err(Error::TooLarge));
	let palette = [[0, 0, 0, 0], [0xff, 0xff, 0xff, 0]];
	let invalid_run = info_bmp(2, 1, 8, BI_RLE8, &palette, &[3, 1, 0, 1]);
	assert_eq!(decode(&invalid_run), Err(Error::Invalid));
}

#[test]
fn staged_sample_image_is_a_valid_two_by_two_bmp() {
	let image = decode(include_bytes!("../../../volume/sample.bmp")).unwrap();
	assert_eq!((image.width, image.height, image.pitch), (2, 2, 8));
	assert_eq!(image.pixels.len(), 16);
}

#[test]
fn encodes_opaque_rgba_and_refuses_alpha_loss() {
	let image = pix::RgbaImage::new(2, 2, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 1, 2, 3, 255]).unwrap();
	assert_eq!(decode_rgba(&encode_rgba(&image).unwrap()).unwrap(), image);
	let transparent = pix::RgbaImage::new(1, 1, vec![1, 2, 3, 4]).unwrap();
	assert_eq!(encode_rgba(&transparent), Err(Error::Unsupported));
}

#[test]
fn encodes_quantized_eight_bit_rows_with_quality_budget() {
	let exact = pix::RgbaImage::new(3, 1, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255]).unwrap();
	let encoded = encode_indexed(&exact, 100).unwrap();
	assert_eq!(read_u16(&encoded, 28).unwrap(), 8);
	assert_eq!(read_u32(&encoded, 46).unwrap(), 3);
	assert_eq!(decode_rgba(&encoded).unwrap(), exact);

	let mut pixels = Vec::new();
	for value in 0..512u32 {
		pixels.extend_from_slice(&[value as u8, (value >> 1) as u8, value.wrapping_mul(47) as u8, 255]);
	}
	let true_color = pix::RgbaImage::new(512, 1, pixels).unwrap();
	let low = encode_indexed(&true_color, 0).unwrap();
	let high = encode_indexed(&true_color, 100).unwrap();
	assert!(read_u32(&low, 46).unwrap() <= 16);
	assert!(read_u32(&high, 46).unwrap() > read_u32(&low, 46).unwrap());
	assert_eq!(decode_rgba(&low).unwrap().width, true_color.width);
}

#[test]
fn decodes_external_truecolor_indexed_and_alpha_mask_profiles() {
	for (data, dib, depth, compression, width, height, hash) in [
		(include_bytes!("../tests/data/external-rgb24.bmp").as_slice(), 40, 24, BI_RGB, 19, 7, 0xe625_4907_e10a_8c80),
		(include_bytes!("../tests/data/derived-rgb32.bmp").as_slice(), 40, 32, BI_RGB, 19, 7, 0xad99_3a39_14fe_5247),
		(include_bytes!("../tests/data/external-indexed8.bmp").as_slice(), 40, 8, BI_RGB, 37, 7, 0x2941_c44b_e6b7_19ed),
		(include_bytes!("../tests/data/derived-v3-alpha.bmp").as_slice(), 56, 32, BI_BITFIELDS, 19, 7, 0xf61b_87cd_e45b_3532),
		(include_bytes!("../tests/data/derived-v4-alpha.bmp").as_slice(), 108, 32, BI_BITFIELDS, 19, 7, 0xf61b_87cd_e45b_3532),
		(include_bytes!("../tests/data/external-v5-alpha.bmp").as_slice(), 124, 32, BI_BITFIELDS, 19, 7, 0xf61b_87cd_e45b_3532),
	] {
		assert_eq!(read_u32(data, 14).unwrap(), dib);
		assert_eq!(read_u16(data, 28).unwrap(), depth);
		assert_eq!(read_u32(data, 30).unwrap(), compression);
		let image = decode_rgba(data).unwrap();
		assert_eq!((image.width, image.height, fnv1a(&image.pixels)), (width, height, hash));
	}

	let rgb32 = include_bytes!("../tests/data/derived-rgb32.bmp");
	let offset = read_u32(rgb32, 10).unwrap() as usize;
	assert!(rgb32[offset + 3..].iter().step_by(4).any(|high| *high != 255));
	assert!(decode_rgba(rgb32).unwrap().pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
}

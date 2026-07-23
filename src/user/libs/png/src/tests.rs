use super::*;

fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn chunk_kinds(data: &[u8]) -> Vec<[u8; 4]> {
	let mut kinds = Vec::new();
	let mut cursor = SIGNATURE.len();
	while cursor < data.len() {
		let length = read_u32(data, cursor).unwrap() as usize;
		kinds.push(data[cursor + 4..cursor + 8].try_into().unwrap());
		cursor += length + 12;
	}
	kinds
}

fn adler32(data: &[u8]) -> u32 {
	let mut a = 1u32;
	let mut b = 0u32;
	for &byte in data {
		a = (a + byte as u32) % 65_521;
		b = (b + a) % 65_521;
	}
	b << 16 | a
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
	assert!(data.len() <= u16::MAX as usize);
	let mut out = vec![0x78, 0x01, 0x01];
	let len = data.len() as u16;
	out.extend_from_slice(&len.to_le_bytes());
	out.extend_from_slice(&(!len).to_le_bytes());
	out.extend_from_slice(data);
	out.extend_from_slice(&adler32(data).to_be_bytes());
	out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
	out.extend_from_slice(&(body.len() as u32).to_be_bytes());
	out.extend_from_slice(kind);
	out.extend_from_slice(body);
	out.extend_from_slice(&crc32(kind.iter().chain(body.iter()).copied()).to_be_bytes());
}

fn png(width: u32, height: u32, depth: u8, color_type: u8, interlace: u8, palette: &[[u8; 3]], transparency: &[u8], filtered: &[u8]) -> Vec<u8> {
	let mut out = SIGNATURE.to_vec();
	let mut header = Vec::new();
	header.extend_from_slice(&width.to_be_bytes());
	header.extend_from_slice(&height.to_be_bytes());
	header.extend_from_slice(&[depth, color_type, 0, 0, interlace]);
	chunk(&mut out, b"IHDR", &header);
	if !palette.is_empty() {
		let entries: Vec<u8> = palette.iter().flatten().copied().collect();
		chunk(&mut out, b"PLTE", &entries);
	}
	if !transparency.is_empty() {
		chunk(&mut out, b"tRNS", transparency);
	}
	chunk(&mut out, b"IDAT", &zlib_stored(filtered));
	chunk(&mut out, b"IEND", &[]);
	out
}

fn colors(image: &Image) -> Vec<u32> {
	image.pixels.chunks_exact(4).map(|pixel| u32::from_le_bytes(pixel.try_into().unwrap())).collect()
}

fn adam7_rgb(width: u32, height: u32, pixels: &[[u8; 3]]) -> Vec<u8> {
	let mut out = Vec::new();
	for pass in 0..7 {
		let (pass_width, pass_height) = pass_size(width, height, 1, pass);
		if pass_width == 0 || pass_height == 0 {
			continue;
		}
		for y in 0..pass_height {
			out.push(0);
			for x in 0..pass_width {
				let source_x = PASS_X[pass] + x * PASS_DX[pass];
				let source_y = PASS_Y[pass] + y * PASS_DY[pass];
				out.extend_from_slice(&pixels[(source_y * width + source_x) as usize]);
			}
		}
	}
	out
}

#[test]
fn decodes_rgb_and_adam7_images() {
	let raw = [0, 255, 0, 0, 0, 255, 0, 0, 0, 0, 255, 255, 255, 255];
	let image = decode(&png(2, 2, 8, 2, 0, &[], &[], &raw)).unwrap();
	assert_eq!(colors(&image), vec![0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0x00ff_ffff]);

	let source = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [0, 255, 255], [255, 0, 255], [32, 64, 96], [96, 64, 32], [255, 255, 255]];
	let adam7 = adam7_rgb(3, 3, &source);
	let encoded = png(3, 3, 8, 2, 1, &[], &[], &adam7);
	let parsed = parse(&encoded).unwrap();
	let filtered_size = filtered_len(&parsed).unwrap();
	assert_eq!(filtered_size, adam7.len());
	let filtered = inflate::zlib(&parsed.compressed, filtered_size).unwrap();
	let mut pass_output = vec![0; 3 * 3 * 4];
	decode_passes(&parsed, &filtered, &mut pass_output).unwrap();
	let interlaced = decode(&encoded).unwrap();
	let expected: Vec<u32> = source.iter().map(|rgb| (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32).collect();
	assert_eq!(colors(&interlaced), expected);
}

#[test]
fn decodes_indexed_transparency_and_all_row_filters() {
	let indexed = png(2, 1, 1, 3, 0, &[[255, 0, 0], [0, 255, 0]], &[255, 0], &[0, 0b0100_0000]);
	assert_eq!(colors(&decode(&indexed).unwrap()), vec![0x00ff_0000, 0]);
	assert_eq!(decode_rgba(&indexed).unwrap().pixels, vec![255, 0, 0, 255, 0, 255, 0, 0]);

	let previous = [10, 20, 30, 40, 50, 60];
	for filter in 0..=4 {
		let mut encoded = [1, 2, 3, 4, 5, 6];
		let mut decoded = encoded;
		unfilter(filter, &mut decoded, &previous, 3).unwrap();
		if filter == 0 {
			assert_eq!(decoded, encoded);
		} else {
			assert_ne!(decoded, encoded);
		}
		encoded.fill(0);
	}
}

#[test]
fn rejects_crc_truncation_and_oversized_dimensions() {
	let mut valid = png(1, 1, 8, 6, 0, &[], &[], &[0, 1, 2, 3, 255]);
	let last = valid.len() - 1;
	valid[last] ^= 1;
	assert_eq!(decode(&valid), Err(Error::Invalid));
	assert_eq!(decode(&SIGNATURE[..4]), Err(Error::Truncated));
	let oversized = png(20_000, 1, 8, 2, 0, &[], &[], &[]);
	assert_eq!(decode(&oversized), Err(Error::TooLarge));
}

#[test]
fn staged_sample_image_is_a_valid_two_by_two_png() {
	let image = decode(include_bytes!("../../../../volume/sample.png")).unwrap();
	assert_eq!((image.width, image.height, image.pitch), (2, 2, 8));
	assert_eq!(image.pixels.len(), 16);
	assert!(image.pixels.iter().any(|byte| *byte != 0));
}

#[test]
fn encodes_straight_rgba_at_compression_endpoints() {
	let image = pix::RgbaImage::new(2, 2, vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 1, 2, 3, 4]).unwrap();
	for compression in [0, 100] {
		let encoded = encode_rgba(&image, EncodeOptions { compression }).unwrap();
		assert_eq!(decode_rgba(&encoded).unwrap(), image);
	}
	assert_eq!(encode_rgba(&image, EncodeOptions { compression: 101 }), Err(Error::Invalid));
}

#[test]
fn encodes_indexed_palette_depth_and_binary_transparency() {
	let image = pix::RgbaImage::new(5, 1, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 0, 0, 255, 9, 8, 7, 0]).unwrap();
	let encoded = encode_indexed(&image, 100, 100).unwrap();
	let parsed = parse(&encoded).unwrap();
	assert_eq!((parsed.color_type, parsed.bit_depth), (3, 2));
	let mut expected = image;
	expected.pixels[16..20].copy_from_slice(&[0, 0, 0, 0]);
	assert_eq!(decode_rgba(&encoded).unwrap(), expected);
}

#[test]
fn indexed_quality_quantizes_true_color_but_rejects_partial_alpha() {
	let mut pixels = Vec::new();
	for value in 0..1024u32 {
		pixels.extend_from_slice(&[(value & 255) as u8, ((value >> 2) & 255) as u8, ((value * 91) & 255) as u8, 255]);
	}
	let image = pix::RgbaImage::new(32, 32, pixels).unwrap();
	let low = encode_indexed(&image, 50, 0).unwrap();
	let high = encode_indexed(&image, 50, 100).unwrap();
	assert_eq!(parse(&low).unwrap().palette.len(), 16);
	assert!(parse(&high).unwrap().palette.len() > 16);
	let partial = pix::RgbaImage::new(1, 1, vec![1, 2, 3, 128]).unwrap();
	assert_eq!(encode_indexed(&partial, 50, 100), Err(Error::Unsupported));
}

#[test]
fn decodes_external_profiles_and_consecutive_multi_idat() {
	for (data, profile, dimensions, hash) in [
		(include_bytes!("../tests/data/external-gray4.png").as_slice(), [4, 0, 0], (17, 9), 0xaa3a_7646_5cdc_dbf8),
		(include_bytes!("../tests/data/external-indexed-trns.png").as_slice(), [8, 3, 0], (19, 7), 0x9016_c6cb_8c8b_27d1),
		(include_bytes!("../tests/data/external-rgba16.png").as_slice(), [16, 6, 0], (13, 11), 0x1658_7931_a19e_490a),
		(include_bytes!("../tests/data/external-adam7-rgb.png").as_slice(), [8, 2, 1], (23, 15), 0x8cb7_e5da_66d8_51a1),
		(include_bytes!("../tests/data/derived-multi-idat.png").as_slice(), [8, 2, 1], (23, 15), 0x8cb7_e5da_66d8_51a1),
	] {
		assert_eq!([data[24], data[25], data[28]], profile);
		let image = decode_rgba(data).unwrap();
		assert_eq!((image.width, image.height, fnv1a(&image.pixels)), (dimensions.0, dimensions.1, hash));
	}
	let indexed = chunk_kinds(include_bytes!("../tests/data/external-indexed-trns.png"));
	assert!(indexed.contains(b"PLTE") && indexed.contains(b"tRNS"));
	let multi = chunk_kinds(include_bytes!("../tests/data/derived-multi-idat.png"));
	assert_eq!(multi.iter().filter(|kind| kind.as_slice() == b"IDAT").count(), 3);
	assert_eq!(multi.windows(3).filter(|window| window.iter().all(|kind| kind.as_slice() == b"IDAT")).count(), 1);
}

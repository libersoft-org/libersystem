#[derive(Clone, Copy)]
enum Codec {
	Apng,
	Bmp,
	Gif,
	Ico,
	Icns,
	Jpeg,
	Pcx,
	Png,
	Ppm,
	Qoi,
	Tga,
	WebP,
}

impl Codec {
	fn decode(self, data: &[u8]) {
		match self {
			Self::Apng => {
				let _ = apng::decode(data);
			}
			Self::Bmp => {
				let _ = bmp::decode_rgba(data);
			}
			Self::Gif => {
				let _ = gif::decode(data);
			}
			Self::Ico => {
				let _ = ico::decode(data);
			}
			Self::Icns => {
				let _ = icns::decode(data);
			}
			Self::Jpeg => {
				let _ = jpeg::decode(data);
			}
			Self::Pcx => {
				let _ = pcx::decode(data);
			}
			Self::Png => {
				let _ = png::decode_rgba(data);
			}
			Self::Ppm => {
				let _ = ppm::decode(data);
			}
			Self::Qoi => {
				let _ = qoi::decode(data);
			}
			Self::Tga => {
				let _ = tga::decode(data);
			}
			Self::WebP => {
				let _ = webp::decode(data);
			}
		}
	}

	fn valid(self, data: &[u8]) -> bool {
		match self {
			Self::Apng => apng::decode(data).is_ok(),
			Self::Bmp => bmp::decode_rgba(data).is_ok(),
			Self::Gif => gif::decode(data).is_ok(),
			Self::Ico => ico::decode(data).is_ok(),
			Self::Icns => icns::decode(data).is_ok(),
			Self::Jpeg => jpeg::decode(data).is_ok(),
			Self::Pcx => pcx::decode(data).is_ok(),
			Self::Png => png::decode_rgba(data).is_ok(),
			Self::Ppm => ppm::decode(data).is_ok(),
			Self::Qoi => qoi::decode(data).is_ok(),
			Self::Tga => tga::decode(data).is_ok(),
			Self::WebP => webp::decode(data).is_ok(),
		}
	}
}

struct Fixture {
	name: &'static str,
	codec: Codec,
	format: imgconv::Format,
	data: &'static [u8],
	fields: &'static [(usize, usize)],
}

const PNG_FIELDS: &[(usize, usize)] = &[(8, 8), (16, 13), (29, 4)];
const APNG_FIELDS: &[(usize, usize)] = &[(8, 8), (16, 13), (33, 16)];
const BMP_FIELDS: &[(usize, usize)] = &[(2, 4), (10, 4), (14, 40), (54, 16)];
const GIF_FIELDS: &[(usize, usize)] = &[(6, 7), (13, 24), (37, 16)];
const ICO_FIELDS: &[(usize, usize)] = &[(4, 2), (6, 16), (22, 40)];
const ICNS_FIELDS: &[(usize, usize)] = &[(4, 4), (8, 8), (16, 24)];
const JPEG_FIELDS: &[(usize, usize)] = &[(0, 16), (16, 32), (48, 32)];
const PCX_FIELDS: &[(usize, usize)] = &[(0, 4), (4, 8), (65, 4), (128, 32)];
const PPM_FIELDS: &[(usize, usize)] = &[(0, 16), (16, 32), (48, 32)];
const QOI_FIELDS: &[(usize, usize)] = &[(0, 14), (14, 32), (64, 16)];
const TGA_FIELDS: &[(usize, usize)] = &[(0, 18), (18, 32), (50, 16)];
const WEBP_FIELDS: &[(usize, usize)] = &[(0, 12), (12, 18), (30, 24), (54, 32)];

const FIXTURES: &[Fixture] = &[
	Fixture { name: "png-gray4", codec: Codec::Png, format: imgconv::Format::Png, data: include_bytes!("../../../user/libs/png/tests/data/external-gray4.png"), fields: PNG_FIELDS },
	Fixture { name: "png-indexed-trns", codec: Codec::Png, format: imgconv::Format::Png, data: include_bytes!("../../../user/libs/png/tests/data/external-indexed-trns.png"), fields: PNG_FIELDS },
	Fixture { name: "png-multi-idat", codec: Codec::Png, format: imgconv::Format::Png, data: include_bytes!("../../../user/libs/png/tests/data/derived-multi-idat.png"), fields: PNG_FIELDS },
	Fixture { name: "apng", codec: Codec::Apng, format: imgconv::Format::Apng, data: include_bytes!("../../../user/libs/apng/tests/data/external-animation.png"), fields: APNG_FIELDS },
	Fixture { name: "apng-separate-default", codec: Codec::Apng, format: imgconv::Format::Apng, data: include_bytes!("../../../user/libs/apng/tests/data/external-separate-default.png"), fields: APNG_FIELDS },
	Fixture { name: "bmp-indexed", codec: Codec::Bmp, format: imgconv::Format::Bmp, data: include_bytes!("../../../user/libs/bmp/tests/data/external-indexed8.bmp"), fields: BMP_FIELDS },
	Fixture { name: "bmp-v5-alpha", codec: Codec::Bmp, format: imgconv::Format::Bmp, data: include_bytes!("../../../user/libs/bmp/tests/data/external-v5-alpha.bmp"), fields: BMP_FIELDS },
	Fixture { name: "gif", codec: Codec::Gif, format: imgconv::Format::Gif, data: include_bytes!("../../../user/libs/gif/tests/data/derived-local-subblocks.gif"), fields: GIF_FIELDS },
	Fixture { name: "ico", codec: Codec::Ico, format: imgconv::Format::Ico, data: include_bytes!("../../../user/libs/ico/tests/data/external-png.ico"), fields: ICO_FIELDS },
	Fixture { name: "icns", codec: Codec::Icns, format: imgconv::Format::Icns, data: include_bytes!("../../../user/libs/icns/tests/data/external-gradient.icns"), fields: ICNS_FIELDS },
	Fixture { name: "jpeg", codec: Codec::Jpeg, format: imgconv::Format::Jpeg, data: include_bytes!("../../../user/libs/jpeg/tests/data/external-ycbcr-baseline.jpg"), fields: JPEG_FIELDS },
	Fixture { name: "pcx", codec: Codec::Pcx, format: imgconv::Format::Pcx, data: include_bytes!("../../../user/libs/pcx/tests/data/indexed.pcx"), fields: PCX_FIELDS },
	Fixture { name: "ppm", codec: Codec::Ppm, format: imgconv::Format::Ppm, data: include_bytes!("../../../user/libs/ppm/tests/data/external-p3-max31.ppm"), fields: PPM_FIELDS },
	Fixture { name: "qoi", codec: Codec::Qoi, format: imgconv::Format::Qoi, data: include_bytes!("../../../user/libs/qoi/tests/data/external-rgb.qoi"), fields: QOI_FIELDS },
	Fixture { name: "tga", codec: Codec::Tga, format: imgconv::Format::Tga, data: include_bytes!("../../../user/libs/tga/tests/data/rle32-top-right.tga"), fields: TGA_FIELDS },
	Fixture { name: "webp-vp8", codec: Codec::WebP, format: imgconv::Format::WebP, data: include_bytes!("../../../user/libs/webp/tests/data/external-vp8.webp"), fields: WEBP_FIELDS },
	Fixture { name: "webp-vp8l", codec: Codec::WebP, format: imgconv::Format::WebP, data: include_bytes!("../../../user/libs/webp/tests/data/external-vp8l.webp"), fields: WEBP_FIELDS },
	Fixture { name: "webp-animation", codec: Codec::WebP, format: imgconv::Format::WebP, data: include_bytes!("../../../user/libs/webp/tests/data/external-animation.webp"), fields: WEBP_FIELDS },
];

struct SplitMix64(u64);

impl SplitMix64 {
	fn next(&mut self) -> u64 {
		self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
		let mut value = self.0;
		value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
		value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
		value ^ (value >> 31)
	}
}

fn exercise(fixture: &Fixture, data: &[u8]) {
	fixture.codec.decode(data);
	let _ = imgconv::decode_frame(data, 0);
}

fn targeted_mutations(fixture: &Fixture) -> usize {
	let mut count = 0usize;
	for offset in 0..fixture.data.len() {
		for value in [0, 0xff, fixture.data[offset] ^ 0x80] {
			let mut mutated = fixture.data.to_vec();
			mutated[offset] = value;
			exercise(fixture, &mutated);
			count += 1;
		}
	}
	for &(start, length) in fixture.fields {
		let end = start.saturating_add(length).min(fixture.data.len());
		for value in [0, 0xff] {
			let mut mutated = fixture.data.to_vec();
			mutated[start.min(fixture.data.len())..end].fill(value);
			exercise(fixture, &mutated);
			count += 1;
		}
	}
	count
}

fn seeded_mutations(fixture: &Fixture, seed: u64) -> usize {
	let mut random = SplitMix64(seed);
	for _ in 0..128 {
		let mut mutated = fixture.data.to_vec();
		let changes = (random.next() as usize % 8) + 1;
		for _ in 0..changes {
			let offset = random.next() as usize % mutated.len();
			let bit = 1u8 << (random.next() & 7);
			mutated[offset] ^= bit;
		}
		exercise(fixture, &mutated);
	}
	128
}

fn main() {
	let mut prefixes = 0usize;
	let mut targeted = 0usize;
	let mut seeded = 0usize;
	for (index, fixture) in FIXTURES.iter().enumerate() {
		assert!(fixture.codec.valid(fixture.data), "{} does not pass its leaf decoder", fixture.name);
		let (format, _) = imgconv::decode_frame(fixture.data, 0).unwrap_or_else(|error| panic!("{} does not pass central decode: {error:?}", fixture.name));
		assert_eq!(format, fixture.format, "{} central format mismatch", fixture.name);
		for end in 0..fixture.data.len() {
			exercise(fixture, &fixture.data[..end]);
			prefixes += 1;
		}
		targeted += targeted_mutations(fixture);
		seeded += seeded_mutations(fixture, 0x4c53_494d_4755_5441 ^ index as u64);
	}
	println!("Image hostile-input gate: {} fixtures, {prefixes} prefixes, {targeted} targeted mutations, {seeded} seeded mutations passed", FIXTURES.len());
}

use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_secs(5);

fn main() {
	let source = fixture(512, 512);
	let source_png = png::encode_rgba(&source, png::EncodeOptions { compression: 50 }).unwrap();
	println!("| output | options | bytes | RGB MSE | encode | decode |");
	println!("| --- | --- | ---: | ---: | ---: | ---: |");
	let mut webp_lossy_mse = [0u64; 2];
	let mut webp_effort_bytes = [0usize; 2];
	for (name, arguments) in [
		("BMP", "in.png out.bmp"),
		("BMP indexed q0", "--quality 0 in.png out.bmp"),
		("BMP indexed q100", "--quality 100 in.png out.bmp"),
		("PNG fast", "--compression 0 in.png out.png"),
		("PNG compact", "--compression 100 in.png out.png"),
		("PNG indexed q0", "--quality 0 --compression 100 in.png out.png"),
		("PNG indexed q100", "--quality 100 --compression 100 in.png out.png"),
		("PCX", "in.png out.pcx"),
		("PCX indexed q0", "--quality 0 in.png out.pcx"),
		("PCX indexed q100", "--quality 100 in.png out.pcx"),
		("PPM", "in.png out.ppm"),
		("QOI", "in.png out.qoi"),
		("TGA", "in.png out.tga"),
		("ICO", "--resize 256x256 --compression 100 in.png out.ico"),
		("ICNS classic", "--resize 32x32 --compression 100 in.png out.icns"),
		("ICNS", "--resize 512x512 --compression 100 in.png out.icns"),
		("JPEG q10", "--quality 10 in.png out.jpg"),
		("JPEG q100", "--quality 100 in.png out.jpg"),
		("WebP fast", "--lossless --compression 0 in.png out.webp"),
		("WebP compact", "--lossless --compression 100 in.png out.webp"),
		("WebP lossy q0", "--lossy --quality 0 in.png out.webp"),
		("WebP lossy q100", "--lossy --quality 100 in.png out.webp"),
		("WebP lossy effort 0", "--lossy --quality 90 --compression 0 in.png out.webp"),
		("WebP lossy effort 100", "--lossy --quality 90 --compression 100 in.png out.webp"),
		("APNG", "--compression 100 in.png out.apng"),
		("GIF q0", "--quality 0 in.png out.gif"),
		("GIF q100", "--quality 100 in.png out.gif"),
	] {
		let config = imgconv::parse_args(arguments.as_bytes()).unwrap();
		let start = Instant::now();
		let (encoded, _) = imgconv::convert(&source_png, &config).unwrap();
		let encode_time = start.elapsed();
		let start = Instant::now();
		let (_, decoded) = imgconv::decode_frame(&encoded, 0).unwrap();
		let decode_time = start.elapsed();
		assert!(encoded.len() != 0 && decoded.width != 0 && decoded.height != 0);
		let mse = ((decoded.width, decoded.height) == (source.width, source.height)).then(|| rgb_mse(&source, &decoded));
		if name == "WebP lossy q0" {
			webp_lossy_mse[0] = mse.unwrap();
		} else if name == "WebP lossy q100" {
			webp_lossy_mse[1] = mse.unwrap();
		} else if name == "WebP lossy effort 0" {
			webp_effort_bytes[0] = encoded.len();
		} else if name == "WebP lossy effort 100" {
			webp_effort_bytes[1] = encoded.len();
		}
		assert!(encode_time < BUDGET, "{name} encode exceeded {BUDGET:?}");
		assert!(decode_time < BUDGET, "{name} decode exceeded {BUDGET:?}");
		let mse = mse.map(|value| value.to_string()).unwrap_or_else(|| "-".to_string());
		println!("| {name} | `{arguments}` | {} | {mse} | {:.3} ms | {:.3} ms |", encoded.len(), encode_time.as_secs_f64() * 1_000.0, decode_time.as_secs_f64() * 1_000.0);
	}
	assert!(webp_lossy_mse[1] < webp_lossy_mse[0], "WebP quality 100 must improve RGB MSE over quality 0");
	assert!(webp_lossy_mse[1] <= 300, "WebP quality 100 exceeded the RGB MSE fidelity floor");
	assert_ne!(webp_effort_bytes[0], webp_effort_bytes[1], "WebP lossy effort endpoints must exercise different searches");
	let animation = animation_fixture();
	let start = Instant::now();
	let encoded = webp::encode_animation(&animation, 100).unwrap();
	let encode_time = start.elapsed();
	let start = Instant::now();
	let decoded = webp::decode_animation(&encoded).unwrap();
	let decode_time = start.elapsed();
	assert_eq!((decoded.frames.len(), decoded.loop_count), (2, 3));
	assert!(encode_time < BUDGET, "WebP animation encode exceeded {BUDGET:?}");
	assert!(decode_time < BUDGET, "WebP animation decode exceeded {BUDGET:?}");
	println!("| WebP animation | `lossless effort 100, 2 frames` | {} | 0 | {:.3} ms | {:.3} ms |", encoded.len(), encode_time.as_secs_f64() * 1_000.0, decode_time.as_secs_f64() * 1_000.0);
}

fn rgb_mse(expected: &pix::RgbaImage, actual: &pix::RgbaImage) -> u64 {
	let squared_error: u64 = expected
		.pixels
		.chunks_exact(4)
		.zip(actual.pixels.chunks_exact(4))
		.map(|(expected, actual)| {
			(0..3)
				.map(|channel| {
					let difference = i64::from(expected[channel]) - i64::from(actual[channel]);
					u64::try_from(difference * difference).unwrap()
				})
				.sum::<u64>()
		})
		.sum();
	squared_error / (u64::from(expected.width) * u64::from(expected.height) * 3)
}

fn fixture(width: u32, height: u32) -> pix::RgbaImage {
	let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
	for y in 0..height {
		for x in 0..width {
			pixels.extend_from_slice(&[x as u8, y as u8, (x.wrapping_mul(13) + y.wrapping_mul(7)) as u8, 255]);
		}
	}
	pix::RgbaImage::new(width, height, pixels).unwrap()
}

fn animation_fixture() -> pix::Animation {
	let first = fixture(256, 256);
	let second = fixture(128, 128);
	pix::Animation::new(
		256,
		256,
		3,
		vec![
			pix::Frame { image: first, x: 0, y: 0, duration_ms: 40, blend: pix::Blend::Source, disposal: pix::Disposal::Keep },
			pix::Frame { image: second, x: 64, y: 64, duration_ms: 60, blend: pix::Blend::Over, disposal: pix::Disposal::Background },
		],
	)
	.unwrap()
}

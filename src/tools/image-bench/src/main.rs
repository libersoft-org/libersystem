use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_secs(5);

fn main() {
	let source = fixture(512, 512);
	let source_png = png::encode_rgba(&source, png::EncodeOptions { compression: 50 }).unwrap();
	println!("| output | options | bytes | encode | decode |");
	println!("| --- | --- | ---: | ---: | ---: |");
	for (name, arguments) in [
		("BMP", "in.png out.bmp"),
		("PNG fast", "--compression 0 in.png out.png"),
		("PNG compact", "--compression 100 in.png out.png"),
		("PCX", "in.png out.pcx"),
		("PPM", "in.png out.ppm"),
		("QOI", "in.png out.qoi"),
		("TGA", "in.png out.tga"),
		("ICO", "--resize 256x256 --compression 100 in.png out.ico"),
		("ICNS", "--resize 512x512 --compression 100 in.png out.icns"),
		("JPEG q10", "--quality 10 in.png out.jpg"),
		("JPEG q100", "--quality 100 in.png out.jpg"),
		("WebP fast", "--lossless --compression 0 in.png out.webp"),
		("WebP compact", "--lossless --compression 100 in.png out.webp"),
		("APNG", "--compression 100 in.png out.apng"),
		("GIF", "in.png out.gif"),
	] {
		let config = imgconv::parse_args(arguments.as_bytes()).unwrap();
		let start = Instant::now();
		let (encoded, _) = imgconv::convert(&source_png, &config).unwrap();
		let encode_time = start.elapsed();
		let start = Instant::now();
		let (_, decoded) = imgconv::decode_frame(&encoded, 0).unwrap();
		let decode_time = start.elapsed();
		assert!(encoded.len() != 0 && decoded.width != 0 && decoded.height != 0);
		assert!(encode_time < BUDGET, "{name} encode exceeded {BUDGET:?}");
		assert!(decode_time < BUDGET, "{name} decode exceeded {BUDGET:?}");
		println!("| {name} | `{arguments}` | {} | {:.3} ms | {:.3} ms |", encoded.len(), encode_time.as_secs_f64() * 1_000.0, decode_time.as_secs_f64() * 1_000.0);
	}
}

fn fixture(width: u32, height: u32) -> pix::RgbaImage {
	let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
	for y in 0..height {
		for x in 0..width {
			pixels.extend_from_slice(&[((x / 64) * 32) as u8, ((y / 64) * 32) as u8, (((x + y) / 128) * 32) as u8, 255]);
		}
	}
	pix::RgbaImage::new(width, height, pixels).unwrap()
}

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

fn image() -> pix::RgbaImage {
	let (width, height) = (63u32, 41u32);
	let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
	for y in 0..height {
		for x in 0..width {
			let pixel = if (x / 9 + y / 7) % 2 == 0 { [(x * 255 / (width - 1)) as u8, (y * 255 / (height - 1)) as u8, ((x * 11 + y * 17) & 255) as u8, 255] } else { [((x * 29 + y * 3) & 255) as u8, ((x * 5 + y * 23) & 255) as u8, ((x + y) * 255 / (width + height - 2)) as u8, 255] };
			pixels.extend_from_slice(&pixel);
		}
	}
	pix::RgbaImage::new(width, height, pixels).unwrap()
}

fn run(command: &mut Command) -> ExitStatus {
	let description = format!("{command:?}");
	let status = command.status().unwrap_or_else(|error| panic!("cannot run {description}: {error}"));
	assert!(status.success(), "command failed: {description}");
	status
}

fn magick_rgba(source: &Path, output: &Path) -> Vec<u8> {
	run(Command::new("magick").arg(source).args(["-depth", "8"]).arg(format!("rgba:{}", output.display())));
	fs::read(output).unwrap()
}

fn pillow_rgba(source: &Path, output: &Path) -> Vec<u8> {
	run(Command::new("python3").args(["-c", "from PIL import Image; import sys; open(sys.argv[2], 'wb').write(Image.open(sys.argv[1]).convert('RGBA').tobytes())"]).arg(source).arg(output));
	fs::read(output).unwrap()
}

fn is_baseline_three_component(data: &[u8]) -> bool {
	let mut cursor = 2usize;
	while cursor < data.len() {
		while data.get(cursor) == Some(&0xff) {
			cursor += 1;
		}
		let Some(&marker) = data.get(cursor) else { return false };
		cursor += 1;
		if marker == 0xc0 {
			return data.get(cursor + 7) == Some(&3);
		}
		if marker == 0xda || marker == 0xd9 {
			return false;
		}
		if matches!(marker, 0x01 | 0xd0..=0xd7) {
			continue;
		}
		let Some(length) = data.get(cursor..cursor + 2) else { return false };
		let length = u16::from_be_bytes(length.try_into().unwrap()) as usize;
		if length < 2 {
			return false;
		}
		cursor += length;
	}
	false
}

fn mse(actual: &[u8], expected: &[u8]) -> f64 {
	actual
		.chunks_exact(4)
		.zip(expected.chunks_exact(4))
		.flat_map(|(actual, expected)| {
			(0..3).map(move |channel| {
				let difference = i32::from(actual[channel]) - i32::from(expected[channel]);
				(difference * difference) as u64
			})
		})
		.sum::<u64>() as f64
		/ (actual.len() / 4 * 3) as f64
}

fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn validate(root: &Path, source: &pix::RgbaImage, quality: u8) -> (f64, usize, u64) {
	let encoded = jpeg::encode(source, quality).unwrap();
	assert_eq!(&encoded[..2], &[0xff, 0xd8]);
	assert!(is_baseline_three_component(&encoded));
	let path = root.join(format!("q{quality}.jpg"));
	fs::write(&path, encoded).unwrap();
	let magick = magick_rgba(&path, &root.join(format!("q{quality}-magick.rgba")));
	let pillow = pillow_rgba(&path, &root.join(format!("q{quality}-pillow.rgba")));
	assert_eq!(magick, pillow, "external decoders differ at quality {quality}");
	(mse(&magick, &source.pixels), fs::metadata(path).unwrap().len() as usize, fnv1a(&fs::read(root.join(format!("q{quality}.jpg"))).unwrap()))
}

fn main() {
	let root: PathBuf = env::temp_dir().join(format!("libersystem-jpeg-conformance-{}", std::process::id()));
	fs::create_dir_all(&root).unwrap();
	let source = image();
	let (low, low_size, low_hash) = validate(&root, &source, 10);
	let (high, high_size, high_hash) = validate(&root, &source, 100);
	assert_eq!((low_size, low_hash), (939, 0x56b6_ea10_65d7_fb11));
	assert_eq!((high_size, high_hash), (9_833, 0x70a6_ab10_e167_dbb8));
	assert!(high < low, "quality 100 MSE {high} is not below quality 10 MSE {low}");
	assert!(high <= 25.0, "quality 100 MSE {high} exceeds the fidelity floor");
	fs::remove_dir_all(&root).unwrap();
	println!("JPEG interoperability: baseline quality endpoints passed through ImageMagick and Pillow");
}

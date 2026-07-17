use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

fn image() -> pix::RgbaImage {
	let (width, height) = (37u32, 11u32);
	let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
	for y in 0..height {
		for x in 0..width {
			pixels.extend_from_slice(&[(x * 255 / (width - 1)) as u8, (y * 255 / (height - 1)) as u8, ((x * 17 + y * 29) & 255) as u8, 255]);
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

fn netpbm_rgba(source: &Path, ppm: &Path, output: &Path) -> Vec<u8> {
	let file = File::create(ppm).unwrap();
	run(Command::new("bmptopnm").arg(source).stdout(Stdio::from(file)));
	magick_rgba(ppm, output)
}

fn validate(root: &Path, name: &str, encoded: &[u8], expected: &pix::RgbaImage, depth: u16) {
	assert_eq!(&encoded[..2], b"BM");
	assert_eq!(u32::from_le_bytes(encoded[14..18].try_into().unwrap()), 40);
	assert_eq!(u16::from_le_bytes(encoded[28..30].try_into().unwrap()), depth);
	assert_eq!(u32::from_le_bytes(encoded[30..34].try_into().unwrap()), 0);
	let path = root.join(format!("{name}.bmp"));
	fs::write(&path, encoded).unwrap();
	let magick = magick_rgba(&path, &root.join(format!("{name}-magick.rgba")));
	let netpbm = netpbm_rgba(&path, &root.join(format!("{name}.ppm")), &root.join(format!("{name}-netpbm.rgba")));
	assert_eq!(magick, expected.pixels, "ImageMagick differs for {name}");
	assert_eq!(netpbm, expected.pixels, "Netpbm differs for {name}");
}

fn main() {
	let root: PathBuf = env::temp_dir().join(format!("libersystem-bmp-conformance-{}", std::process::id()));
	fs::create_dir_all(&root).unwrap();
	let source = image();
	let truecolor = bmp::encode_rgba(&source).unwrap();
	validate(&root, "rgb24", &truecolor, &source, 24);
	let indexed = bmp::encode_indexed(&source, 100).unwrap();
	let expected = bmp::decode_rgba(&indexed).unwrap();
	validate(&root, "indexed8", &indexed, &expected, 8);
	fs::remove_dir_all(&root).unwrap();
	println!("BMP interoperability: 24-bit and indexed 8-bit BI_RGB output passed");
}

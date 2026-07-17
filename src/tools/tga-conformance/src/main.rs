use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

fn image(alpha: bool) -> pix::RgbaImage {
	let mut pixels = Vec::with_capacity(11 * 7 * 4);
	for y in 0..7 {
		for x in 0..11 {
			let pixel = match (x < 5, y < 3) {
				(true, true) => [255, 33, 0, 255],
				(false, true) => [0, 212, 63, 255],
				(true, false) => [23, 76, 255, 255],
				(false, false) => [228, 201, 0, if alpha { 128 } else { 255 }],
			};
			pixels.extend_from_slice(&pixel);
		}
	}
	pix::RgbaImage::new(11, 7, pixels).unwrap()
}

fn run(command: &mut Command) -> ExitStatus {
	let description = format!("{command:?}");
	let status = command.status().unwrap_or_else(|error| panic!("cannot run {description}: {error}"));
	assert!(status.success(), "command failed: {description}");
	status
}

fn validate(root: &Path, name: &str, encoded: &[u8], expected: &pix::RgbaImage, image_type: u8, depth: u8) {
	assert_eq!(encoded[0], 0);
	assert_eq!(encoded[1], 0);
	assert_eq!(encoded[2], image_type);
	assert_eq!(&encoded[12..14], &11u16.to_le_bytes());
	assert_eq!(&encoded[14..16], &7u16.to_le_bytes());
	assert_eq!(encoded[16], depth);
	assert_eq!(encoded[17], 0x20 | if depth == 32 { 8 } else { 0 });
	let tga = root.join(format!("{name}.tga"));
	let rgba = root.join(format!("{name}.rgba"));
	fs::write(&tga, encoded).unwrap();
	run(Command::new("magick").arg(&tga).arg("-auto-orient").args(["-depth", "8"]).arg(format!("rgba:{}", rgba.display())));
	assert_eq!(fs::read(rgba).unwrap(), expected.pixels, "ImageMagick differs for {name}");
}

fn main() {
	let root: PathBuf = env::temp_dir().join(format!("libersystem-tga-conformance-{}", std::process::id()));
	fs::create_dir_all(&root).unwrap();
	for (alpha, depth) in [(false, 24), (true, 32)] {
		let source = image(alpha);
		for rle in [false, true] {
			let encoded = tga::encode(&source, tga::EncodeOptions { rle }).unwrap();
			validate(&root, &format!("{}-{depth}", if rle { "rle" } else { "raw" }), &encoded, &source, if rle { 10 } else { 2 }, depth);
		}
	}
	fs::remove_dir_all(&root).unwrap();
	println!("TGA interoperability: raw/RLE 24/32-bit profiles passed through ImageMagick");
}

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

fn image(size: u32) -> pix::RgbaImage {
	let mut pixels = Vec::with_capacity(size as usize * size as usize * 4);
	for y in 0..size {
		for x in 0..size {
			pixels.extend_from_slice(&[(x * 255 / (size - 1)) as u8, (y * 255 / (size - 1)) as u8, ((x + y) * 255 / (2 * (size - 1))) as u8, ((x + 2 * y) * 255 / (3 * (size - 1))) as u8]);
		}
	}
	pix::RgbaImage::new(size, size, pixels).unwrap()
}

fn run(command: &mut Command) -> ExitStatus {
	let description = format!("{command:?}");
	let status = command.status().unwrap_or_else(|error| panic!("cannot run {description}: {error}"));
	assert!(status.success(), "command failed: {description}");
	status
}

fn decode_png_with_magick(source: &Path, output: &Path) -> Vec<u8> {
	run(Command::new("magick").arg(source).args(["-depth", "8"]).arg(format!("rgba:{}", output.display())));
	fs::read(output).unwrap()
}

fn modern_png<'a>(icns: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
	let mut cursor = 8usize;
	while cursor < icns.len() {
		let length = u32::from_be_bytes(icns[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
		if &icns[cursor..cursor + 4] == kind {
			return &icns[cursor + 8..cursor + length];
		}
		cursor += length;
	}
	panic!("missing modern ICNS entry");
}

fn main() {
	let images = [image(16), image(32), image(128)];
	let encoded = icns::encode(&images, 100).unwrap();
	let root: PathBuf = env::temp_dir().join(format!("libersystem-icns-conformance-{}", std::process::id()));
	let extracted = root.join("extracted");
	fs::create_dir_all(&extracted).unwrap();
	let container = root.join("conformance.icns");
	fs::write(&container, &encoded).unwrap();
	run(Command::new("icns2png").args(["-x", "-o"]).arg(&extracted).arg(&container).stdout(std::process::Stdio::null()));
	for (size, expected) in [(16, &images[0]), (32, &images[1])] {
		let png = extracted.join(format!("conformance_{size}x{size}x32.png"));
		let raw = decode_png_with_magick(&png, &root.join(format!("{size}.rgba")));
		assert_eq!(raw, expected.pixels, "external classic ICNS decode differs at {size}x{size}");
	}
	let modern = root.join("ic07.png");
	fs::write(&modern, modern_png(&encoded, b"ic07")).unwrap();
	let raw = decode_png_with_magick(&modern, &root.join("128.rgba"));
	assert_eq!(raw, images[2].pixels, "external modern ICNS payload decode differs");
	fs::remove_dir_all(&root).unwrap();
	println!("ICNS interoperability: classic 16/32 and PNG-backed 128 profiles passed");
}

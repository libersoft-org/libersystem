use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

fn image(width: u32, height: u32, seed: u32) -> pix::RgbaImage {
	let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
	for y in 0..height {
		for x in 0..width {
			pixels.extend_from_slice(&[
				((x * 17 + y * 3 + seed) & 255) as u8,
				((x * 5 + y * 23 + seed * 2) & 255) as u8,
				((x * 11 + y * 7 + seed * 3) & 255) as u8,
				((x * 9 + y * 13 + seed * 5) & 255) as u8,
			]);
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

fn pngcheck(source: &Path) {
	run(Command::new("pngcheck").arg("-q").arg(source).stdout(Stdio::null()));
}

fn validate_png(root: &Path, source: &pix::RgbaImage, compression: u8) {
	let encoded = png::encode_rgba(source, png::EncodeOptions { compression }).unwrap();
	let path = root.join(format!("compression-{compression}.png"));
	fs::write(&path, encoded).unwrap();
	pngcheck(&path);
	assert_eq!(magick_rgba(&path, &root.join(format!("compression-{compression}-magick.rgba"))), source.pixels);
	assert_eq!(pillow_rgba(&path, &root.join(format!("compression-{compression}-pillow.rgba"))), source.pixels);
}

fn validate_apng(root: &Path) {
	let first = image(31, 19, 1);
	let second = image(31, 19, 7);
	let animation = pix::Animation::new(
		31,
		19,
		3,
		vec![
			pix::Frame { image: first.clone(), x: 0, y: 0, duration_ms: 40, blend: pix::Blend::Source, disposal: pix::Disposal::Keep },
			pix::Frame { image: second.clone(), x: 0, y: 0, duration_ms: 75, blend: pix::Blend::Source, disposal: pix::Disposal::Keep },
		],
	)
	.unwrap();
	let path = root.join("animation.png");
	fs::write(&path, apng::encode(&animation, 100).unwrap()).unwrap();
	pngcheck(&path);
	let extracted = root.join("extracted");
	fs::create_dir_all(&extracted).unwrap();
	let copy = extracted.join("animation.png");
	fs::copy(&path, &copy).unwrap();
	run(Command::new("apngdis").arg(&copy).stdout(Stdio::null()));
	for (index, expected) in [(1, &first), (2, &second)] {
		let frame = extracted.join(format!("apngframe{index}.png"));
		assert_eq!(magick_rgba(&frame, &root.join(format!("frame-{index}.rgba"))), expected.pixels);
	}
	let first_control = fs::read_to_string(extracted.join("apngframe1.txt")).unwrap();
	let second_control = fs::read_to_string(extracted.join("apngframe2.txt")).unwrap();
	assert!(first_control.contains("delay=40/1000"));
	assert!(second_control.contains("delay=75/1000"));
	assert_eq!(apng::decode(&fs::read(path).unwrap()).unwrap().loop_count, 3);
}

fn main() {
	let root: PathBuf = env::temp_dir().join(format!("libersystem-png-conformance-{}", std::process::id()));
	fs::create_dir_all(&root).unwrap();
	let source = image(37, 21, 3);
	validate_png(&root, &source, 0);
	validate_png(&root, &source, 100);
	validate_apng(&root);
	fs::remove_dir_all(&root).unwrap();
	println!("PNG/APNG interoperability: PNG endpoints and APNG frames passed external tools");
}

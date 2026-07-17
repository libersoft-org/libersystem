use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

fn image(width: u32, height: u32) -> pix::RgbaImage {
	let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
	for y in 0..height {
		for x in 0..width {
			pixels.extend_from_slice(&[(x * 255 / (width - 1)) as u8, (y * 255 / (height - 1)) as u8, ((x + y) * 255 / (width + height - 2)) as u8, 255]);
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
	run(Command::new("pcxtoppm").arg(source).stdout(Stdio::from(file)));
	magick_rgba(ppm, output)
}

fn validate(root: &Path, name: &str, encoded: &[u8], expected: &pix::RgbaImage, planes: u8) {
	assert_eq!(encoded[65], planes);
	if planes == 1 {
		assert_eq!(encoded[encoded.len() - 769], 0x0c);
	}
	let pcx = root.join(format!("{name}.pcx"));
	fs::write(&pcx, encoded).unwrap();
	let magick = magick_rgba(&pcx, &root.join(format!("{name}-magick.rgba")));
	let netpbm = netpbm_rgba(&pcx, &root.join(format!("{name}.ppm")), &root.join(format!("{name}-netpbm.rgba")));
	assert_eq!(magick, expected.pixels, "ImageMagick differs for {name}");
	assert_eq!(netpbm, expected.pixels, "Netpbm differs for {name}");
}

fn main() {
	let root: PathBuf = env::temp_dir().join(format!("libersystem-pcx-conformance-{}", std::process::id()));
	fs::create_dir_all(&root).unwrap();
	let source = image(19, 7);
	let rgb = pcx::encode(&source).unwrap();
	validate(&root, "rgb", &rgb, &source, 3);
	let indexed = pcx::encode_indexed(&source, 100).unwrap();
	let expected = pcx::decode(&indexed).unwrap();
	validate(&root, "indexed", &indexed, &expected, 1);
	fs::remove_dir_all(&root).unwrap();
	println!("PCX interoperability: indexed one-plane and RGB three-plane profiles passed");
}

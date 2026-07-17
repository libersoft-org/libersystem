use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

fn image(alpha: bool) -> pix::RgbaImage {
	let (width, height) = (37u32, 11u32);
	let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
	for y in 0..height {
		for x in 0..width {
			let repeated = x < 7 || (x > 24 && y % 3 == 0);
			let pixel = if repeated { [32, 64, 96, if alpha { 96 } else { 255 }] } else { [(x * 7 + y * 3) as u8, (x * 5 + y * 11) as u8, (x * 13 + y) as u8, if alpha { (x * 9 + y * 17) as u8 } else { 255 }] };
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

fn netpbm_rgba(source: &Path, pam: &Path, output: &Path) -> Vec<u8> {
	let file = File::create(pam).unwrap();
	run(Command::new("qoitopam").arg(source).stdout(Stdio::from(file)));
	magick_rgba(pam, output)
}

fn validate(root: &Path, name: &str, expected: &pix::RgbaImage, channels: u8) {
	let encoded = qoi::encode(expected).unwrap();
	assert_eq!(&encoded[..4], b"qoif");
	assert_eq!(encoded[12], channels);
	assert_eq!(&encoded[encoded.len() - 8..], &[0, 0, 0, 0, 0, 0, 0, 1]);
	let qoi = root.join(format!("{name}.qoi"));
	fs::write(&qoi, encoded).unwrap();
	let magick = magick_rgba(&qoi, &root.join(format!("{name}-magick.rgba")));
	let netpbm = netpbm_rgba(&qoi, &root.join(format!("{name}.pam")), &root.join(format!("{name}-netpbm.rgba")));
	assert_eq!(magick, expected.pixels, "ImageMagick differs for {name}");
	assert_eq!(netpbm, expected.pixels, "Netpbm differs for {name}");
}

fn main() {
	let root: PathBuf = env::temp_dir().join(format!("libersystem-qoi-conformance-{}", std::process::id()));
	fs::create_dir_all(&root).unwrap();
	validate(&root, "rgb", &image(false), 3);
	validate(&root, "rgba", &image(true), 4);
	fs::remove_dir_all(&root).unwrap();
	println!("QOI interoperability: RGB/RGBA output passed through ImageMagick and Netpbm");
}

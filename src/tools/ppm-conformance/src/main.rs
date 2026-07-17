use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

fn image() -> pix::RgbaImage {
	let (width, height) = (29u32, 13u32);
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

fn p6_rgba(data: &[u8], width: u32, height: u32) -> Vec<u8> {
	let marker = b"\n255\n";
	let start = data.windows(marker.len()).position(|window| window == marker).map(|position| position + marker.len()).expect("Netpbm output is not P6/255");
	assert_eq!(&data[..2], b"P6");
	let rgb = &data[start..];
	assert_eq!(rgb.len(), width as usize * height as usize * 3);
	let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
	for pixel in rgb.chunks_exact(3) {
		rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
	}
	rgba
}

fn main() {
	let source = image();
	let encoded = ppm::encode(&source).unwrap();
	assert!(encoded.starts_with(b"P6\n29 13\n255\n"));
	let root: PathBuf = env::temp_dir().join(format!("libersystem-ppm-conformance-{}", std::process::id()));
	fs::create_dir_all(&root).unwrap();
	let ppm = root.join("conformance.ppm");
	fs::write(&ppm, encoded).unwrap();
	let magick = magick_rgba(&ppm, &root.join("magick.rgba"));
	let netpbm_path = root.join("netpbm.ppm");
	let file = File::create(&netpbm_path).unwrap();
	run(Command::new("pamdepth").arg("255").arg(&ppm).stdout(Stdio::from(file)));
	let netpbm = p6_rgba(&fs::read(netpbm_path).unwrap(), source.width, source.height);
	assert_eq!(magick, source.pixels, "ImageMagick differs for P6/255 output");
	assert_eq!(netpbm, source.pixels, "Netpbm differs for P6/255 output");
	fs::remove_dir_all(&root).unwrap();
	println!("PPM interoperability: P6/255 output passed through ImageMagick and Netpbm");
}

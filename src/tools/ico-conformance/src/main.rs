use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

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

fn magick_rgba(source: &Path, output: &Path) -> Vec<u8> {
	run(Command::new("magick").arg(source).args(["-depth", "8"]).arg(format!("rgba:{}", output.display())));
	fs::read(output).unwrap()
}

fn icotool_rgba(root: &Path, source: &Path, name: &str) -> Vec<u8> {
	let extracted = root.join(format!("{name}-icotool"));
	fs::create_dir_all(&extracted).unwrap();
	run(Command::new("icotool").args(["-x", "-o"]).arg(format!("{}/", extracted.display())).arg(source).stdout(Stdio::null()));
	let pngs: Vec<PathBuf> = fs::read_dir(&extracted).unwrap().map(|entry| entry.unwrap().path()).filter(|path| path.extension().is_some_and(|extension| extension == "png")).collect();
	assert_eq!(pngs.len(), 1, "expected one extracted image for {name}");
	magick_rgba(&pngs[0], &root.join(format!("{name}-icotool.rgba")))
}

fn validate_encoded(root: &Path, size: u32) {
	let source = image(size);
	let encoded = ico::encode(core::slice::from_ref(&source), 100).unwrap();
	let offset = u32::from_le_bytes(encoded[18..22].try_into().unwrap()) as usize;
	assert_eq!(&encoded[offset..offset + 8], b"\x89PNG\r\n\x1a\n");
	let name = format!("encoded-{size}");
	let path = root.join(format!("{name}.ico"));
	fs::write(&path, encoded).unwrap();
	assert_eq!(magick_rgba(&path, &root.join(format!("{name}-magick.rgba"))), source.pixels);
	assert_eq!(icotool_rgba(root, &path, &name), source.pixels);
}

fn validate_external(root: &Path, name: &str, data: &[u8], icotool: bool) {
	let path = root.join(format!("{name}.ico"));
	fs::write(&path, data).unwrap();
	let expected = ico::decode(data).unwrap().pixels;
	assert_eq!(magick_rgba(&path, &root.join(format!("{name}-magick.rgba"))), expected, "ImageMagick differs for {name}");
	if icotool {
		assert_eq!(icotool_rgba(root, &path, name), expected, "icotool differs for {name}");
	}
}

fn main() {
	let root: PathBuf = env::temp_dir().join(format!("libersystem-ico-conformance-{}", std::process::id()));
	fs::create_dir_all(&root).unwrap();
	validate_encoded(&root, 32);
	validate_encoded(&root, 256);
	validate_external(&root, "external-png", include_bytes!("../../../user/libs/ico/tests/data/external-png.ico"), true);
	validate_external(&root, "external-dib-alpha", include_bytes!("../../../user/libs/ico/tests/data/external-dib-alpha.ico"), true);
	validate_external(&root, "external-dib-zero-alpha", include_bytes!("../../../user/libs/ico/tests/data/external-dib-zero-alpha.ico"), true);
	validate_external(&root, "external-dib-maskless", include_bytes!("../../../user/libs/ico/tests/data/external-dib-maskless.ico"), false);
	fs::remove_dir_all(&root).unwrap();
	println!("ICO interoperability: PNG-backed output and 32-bit DIB input profiles passed");
}

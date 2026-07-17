use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

fn image(width: u32, height: u32, alpha: bool, seed: u32) -> pix::RgbaImage {
	let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
	for y in 0..height {
		for x in 0..width {
			pixels.extend_from_slice(&[
				((x * 17 + y * 3 + seed) & 255) as u8,
				((x * 5 + y * 23 + seed * 2) & 255) as u8,
				((x * 11 + y * 7 + seed * 3) & 255) as u8,
				if alpha { ((x * 9 + y * 13 + seed * 5) & 255) as u8 } else { 255 },
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

fn pam_rgba(data: &[u8]) -> Vec<u8> {
	let marker = b"ENDHDR\n";
	let start = data.windows(marker.len()).position(|window| window == marker).map(|position| position + marker.len()).expect("invalid PAM header");
	let header = core::str::from_utf8(&data[..start]).unwrap();
	let depth = header.lines().find_map(|line| line.strip_prefix("DEPTH ")).unwrap().parse::<usize>().unwrap();
	let body = &data[start..];
	if depth == 4 {
		return body.to_vec();
	}
	assert_eq!(depth, 3);
	let mut rgba = Vec::with_capacity(body.len() / 3 * 4);
	for pixel in body.chunks_exact(3) {
		rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
	}
	rgba
}

fn dwebp_rgba(root: &Path, source: &Path, name: &str) -> Vec<u8> {
	let pam = root.join(format!("{name}.pam"));
	run(Command::new("dwebp").arg(source).args(["-pam", "-o"]).arg(&pam).stdout(Stdio::null()).stderr(Stdio::null()));
	pam_rgba(&fs::read(pam).unwrap())
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

fn validate_static(root: &Path, name: &str, encoded: &[u8], source: &pix::RgbaImage, lossless: bool) -> f64 {
	let path = root.join(format!("{name}.webp"));
	fs::write(&path, encoded).unwrap();
	let info = Command::new("webpinfo").arg(&path).output().unwrap();
	assert!(info.status.success(), "webpinfo rejected {name}");
	let magick = magick_rgba(&path, &root.join(format!("{name}-magick.rgba")));
	let libwebp = dwebp_rgba(root, &path, name);
	assert_eq!(magick, libwebp, "external decoders differ for {name}");
	if lossless {
		assert_eq!(libwebp, source.pixels, "lossless pixels differ for {name}");
	}
	assert_eq!(libwebp.iter().skip(3).step_by(4).copied().collect::<Vec<_>>(), source.pixels.iter().skip(3).step_by(4).copied().collect::<Vec<_>>(), "alpha differs for {name}");
	mse(&libwebp, &source.pixels)
}

fn validate_animation(root: &Path) {
	let first = image(23, 15, false, 1);
	let second = image(19, 13, true, 7);
	let source = pix::Animation::new_with_background(
		23,
		15,
		[9, 19, 29, 200],
		3,
		vec![
			pix::Frame { image: first, x: 0, y: 0, duration_ms: 0, blend: pix::Blend::Source, disposal: pix::Disposal::Background },
			pix::Frame { image: second, x: 2, y: 2, duration_ms: 37, blend: pix::Blend::Over, disposal: pix::Disposal::Keep },
		],
	)
	.unwrap();
	let mut compositor = pix::Compositor::new_with_background(source.width, source.height, source.background).unwrap();
	let expected: Vec<_> = source.frames.iter().map(|frame| compositor.render(frame).unwrap()).collect();
	let path = root.join("animation.webp");
	fs::write(&path, webp::encode_animation(&source, 100).unwrap()).unwrap();
	let info = Command::new("webpinfo").arg(&path).output().unwrap();
	assert!(info.status.success());
	let text = String::from_utf8_lossy(&info.stdout);
	assert!(text.contains("Animation: 1") && text.contains("Loop count      : 3"));
	assert!(text.contains("Duration: 0") && text.contains("Duration: 37"));
	let extracted = root.join("frames");
	fs::create_dir_all(&extracted).unwrap();
	run(Command::new("anim_dump").args(["-folder"]).arg(&extracted).args(["-prefix", "frame_", "-pam"]).arg(&path).stdout(Stdio::null()));
	for (index, expected) in expected.iter().enumerate() {
		let actual = pam_rgba(&fs::read(extracted.join(format!("frame_{index:04}.pam"))).unwrap());
		assert_eq!(actual, expected.pixels, "libwebp animation frame {index} differs");
	}
}

fn main() {
	let root: PathBuf = env::temp_dir().join(format!("libersystem-webp-conformance-{}", std::process::id()));
	fs::create_dir_all(&root).unwrap();
	let opaque = image(37, 21, false, 3);
	validate_static(&root, "vp8l", &webp::encode_lossless(&opaque, 100).unwrap(), &opaque, true);
	let low = validate_static(&root, "vp8-q0", &webp::encode_lossy(&opaque, 0, 100).unwrap(), &opaque, false);
	let high = validate_static(&root, "vp8-q100", &webp::encode_lossy(&opaque, 100, 100).unwrap(), &opaque, false);
	assert!(high < low, "quality 100 MSE {high} is not below quality 0 MSE {low}");
	assert!(high <= 800.0, "quality 100 VP8 MSE {high} exceeds the high-frequency fidelity floor");
	let alpha = image(19, 13, true, 5);
	validate_static(&root, "alph-vp8", &webp::encode_lossy(&alpha, 90, 100).unwrap(), &alpha, false);
	validate_animation(&root);
	fs::remove_dir_all(&root).unwrap();
	println!("WebP interoperability: VP8, VP8L, ALPH and animation passed libwebp/ImageMagick");
}

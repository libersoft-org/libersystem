use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};

fn image(width: u32, height: u32, color: [u8; 4], inset: [u8; 4]) -> pix::RgbaImage {
	let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
	for y in 0..height {
		for x in 0..width {
			pixels.extend_from_slice(if x > 1 && y > 1 && x + 2 < width && y + 2 < height { &inset } else { &color });
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

fn displayed(animation: &pix::Animation) -> Vec<pix::RgbaImage> {
	let mut compositor = pix::Compositor::new_with_background(animation.width, animation.height, animation.background).unwrap();
	animation.frames.iter().map(|frame| compositor.render(frame).unwrap()).collect()
}

fn main() {
	let animation = pix::Animation::new_with_background(
		29,
		17,
		[32, 16, 48, 255],
		2,
		vec![
			pix::Frame { image: image(29, 17, [32, 16, 48, 255], [255, 32, 16, 255]), x: 0, y: 0, duration_ms: 0, blend: pix::Blend::Over, disposal: pix::Disposal::Keep },
			pix::Frame { image: image(11, 9, [32, 208, 96, 255], [255, 255, 255, 255]), x: 5, y: 4, duration_ms: 30, blend: pix::Blend::Over, disposal: pix::Disposal::Background },
			pix::Frame { image: image(9, 7, [32, 64, 255, 255], [240, 192, 0, 255]), x: 12, y: 2, duration_ms: 50, blend: pix::Blend::Over, disposal: pix::Disposal::Previous },
		],
	)
	.unwrap();
	let encoded = gif::encode(&animation).unwrap();
	let root: PathBuf = env::temp_dir().join(format!("libersystem-gif-conformance-{}", std::process::id()));
	fs::create_dir_all(&root).unwrap();
	let path = root.join("animation.gif");
	fs::write(&path, encoded).unwrap();
	let info = Command::new("gifsicle").arg("--info").arg(&path).output().unwrap();
	assert!(info.status.success());
	let info = format!("{}{}", String::from_utf8_lossy(&info.stdout), String::from_utf8_lossy(&info.stderr));
	assert!(info.contains("3 images") && info.contains("loop count 2"));
	assert!(info.contains("disposal background delay 0.03s"));
	assert!(info.contains("disposal previous delay 0.05s"));
	let unoptimized = root.join("unoptimized.gif");
	let output = fs::File::create(&unoptimized).unwrap();
	run(Command::new("gifsicle").arg("--unoptimize").arg(&path).stdout(Stdio::from(output)));
	run(Command::new("magick").arg(&unoptimized).arg("-coalesce").args(["-depth", "8"]).arg(format!("rgba:{}/frame-%d.rgba", root.display())));
	for (index, expected) in displayed(&animation).iter().enumerate() {
		assert_eq!(fs::read(root.join(format!("frame-{index}.rgba"))).unwrap(), expected.pixels, "external GIF frame {index} differs");
	}
	fs::remove_dir_all(&root).unwrap();
	println!("GIF interoperability: timing, disposal, loop and composited frames passed gifsicle/ImageMagick");
}

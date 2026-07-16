#![no_std]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	InvalidOptions,
	UnsupportedOption,
	UnsupportedFormat,
	InvalidImage,
	TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
	Apng,
	Bmp,
	Gif,
	Ico,
	Icns,
	Jpeg,
	Png,
	Pcx,
	Ppm,
	Qoi,
	Tga,
	WebP,
}

impl Format {
	pub const fn name(self) -> &'static str {
		match self {
			Self::Apng => "APNG",
			Self::Bmp => "BMP",
			Self::Gif => "GIF",
			Self::Ico => "ICO",
			Self::Icns => "ICNS",
			Self::Jpeg => "JPEG",
			Self::Png => "PNG",
			Self::Pcx => "PCX",
			Self::Ppm => "PPM",
			Self::Qoi => "QOI",
			Self::Tga => "TGA",
			Self::WebP => "WebP",
		}
	}

	fn parse(value: &[u8]) -> Option<Self> {
		if value.eq_ignore_ascii_case(b"apng") {
			Some(Self::Apng)
		} else if value.eq_ignore_ascii_case(b"bmp") {
			Some(Self::Bmp)
		} else if value.eq_ignore_ascii_case(b"gif") {
			Some(Self::Gif)
		} else if value.eq_ignore_ascii_case(b"ico") {
			Some(Self::Ico)
		} else if value.eq_ignore_ascii_case(b"icns") {
			Some(Self::Icns)
		} else if value.eq_ignore_ascii_case(b"jpg") || value.eq_ignore_ascii_case(b"jpeg") {
			Some(Self::Jpeg)
		} else if value.eq_ignore_ascii_case(b"png") {
			Some(Self::Png)
		} else if value.eq_ignore_ascii_case(b"pcx") {
			Some(Self::Pcx)
		} else if value.eq_ignore_ascii_case(b"ppm") || value.eq_ignore_ascii_case(b"pnm") {
			Some(Self::Ppm)
		} else if value.eq_ignore_ascii_case(b"qoi") {
			Some(Self::Qoi)
		} else if value.eq_ignore_ascii_case(b"tga") {
			Some(Self::Tga)
		} else if value.eq_ignore_ascii_case(b"webp") {
			Some(Self::WebP)
		} else {
			None
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
	Nearest,
	Bilinear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
	pub quality: bool,
	pub compression: bool,
	pub lossless_mode: bool,
	pub lossy_mode: bool,
	pub animation: bool,
	pub alpha: bool,
}

pub const fn capabilities(format: Format) -> Capabilities {
	match format {
		Format::Apng => Capabilities { quality: false, compression: true, lossless_mode: false, lossy_mode: false, animation: true, alpha: true },
		Format::Bmp => Capabilities { quality: true, compression: false, lossless_mode: false, lossy_mode: false, animation: false, alpha: false },
		Format::Gif => Capabilities { quality: true, compression: false, lossless_mode: false, lossy_mode: false, animation: true, alpha: true },
		Format::Ico => Capabilities { quality: false, compression: true, lossless_mode: false, lossy_mode: false, animation: false, alpha: true },
		Format::Icns => Capabilities { quality: false, compression: true, lossless_mode: false, lossy_mode: false, animation: false, alpha: true },
		Format::Jpeg => Capabilities { quality: true, compression: false, lossless_mode: false, lossy_mode: true, animation: false, alpha: false },
		Format::Png => Capabilities { quality: true, compression: true, lossless_mode: false, lossy_mode: false, animation: false, alpha: true },
		Format::Pcx => Capabilities { quality: true, compression: false, lossless_mode: false, lossy_mode: false, animation: false, alpha: false },
		Format::Ppm => Capabilities { quality: false, compression: false, lossless_mode: false, lossy_mode: false, animation: false, alpha: false },
		Format::Qoi | Format::Tga => Capabilities { quality: false, compression: false, lossless_mode: false, lossy_mode: false, animation: false, alpha: true },
		Format::WebP => Capabilities { quality: true, compression: true, lossless_mode: true, lossy_mode: true, animation: true, alpha: true },
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
	pub input: String,
	pub output: String,
	pub format: Format,
	pub force: bool,
	pub resize: Option<(u32, u32)>,
	pub filter: Filter,
	pub frame: Option<usize>,
	pub loop_count: Option<u32>,
	pub quality: Option<u8>,
	pub compression: Option<u8>,
	pub mode: Option<Mode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
	Lossless,
	Lossy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResultInfo {
	pub input_format: Format,
	pub output_format: Format,
	pub source_width: u32,
	pub source_height: u32,
	pub output_width: u32,
	pub output_height: u32,
	pub output_bytes: usize,
	pub quality: Option<u8>,
	pub compression: Option<u8>,
	pub mode: Option<Mode>,
}

pub fn parse_args(args: &[u8]) -> Result<Config, Error> {
	let words: Vec<&[u8]> = args.split(|byte| byte.is_ascii_whitespace()).filter(|word| !word.is_empty()).collect();
	let mut format = None;
	let mut force = false;
	let mut resize = None;
	let mut filter = Filter::Bilinear;
	let mut frame = None;
	let mut loop_count = None;
	let mut quality = None;
	let mut compression = None;
	let mut mode = None;
	let mut paths = Vec::new();
	let mut index = 0usize;
	while index < words.len() {
		match words[index] {
			b"--format" => {
				index += 1;
				let value = words.get(index).and_then(|value| Format::parse(value)).ok_or(Error::InvalidOptions)?;
				if format.replace(value).is_some() {
					return Err(Error::InvalidOptions);
				}
			}
			b"--force" if !force => force = true,
			b"--resize" => {
				index += 1;
				let value = *words.get(index).ok_or(Error::InvalidOptions)?;
				if resize.replace(parse_size(value)?).is_some() {
					return Err(Error::InvalidOptions);
				}
			}
			b"--filter" => {
				index += 1;
				filter = match words.get(index).copied() {
					Some(b"nearest") => Filter::Nearest,
					Some(b"bilinear") => Filter::Bilinear,
					_ => return Err(Error::InvalidOptions),
				};
			}
			b"--compression" => {
				index += 1;
				let value = parse_percent(words.get(index).copied().ok_or(Error::InvalidOptions)?)?;
				if compression.replace(value).is_some() {
					return Err(Error::InvalidOptions);
				}
			}
			b"--frame" => {
				index += 1;
				let value = usize::try_from(parse_u32(words.get(index).copied().ok_or(Error::InvalidOptions)?)?).map_err(|_| Error::InvalidOptions)?;
				if frame.replace(value).is_some() {
					return Err(Error::InvalidOptions);
				}
			}
			b"--loop" => {
				index += 1;
				let value = parse_u32(words.get(index).copied().ok_or(Error::InvalidOptions)?)?;
				if loop_count.replace(value).is_some() {
					return Err(Error::InvalidOptions);
				}
			}
			b"--quality" => {
				index += 1;
				let value = parse_percent(words.get(index).copied().ok_or(Error::InvalidOptions)?)?;
				if quality.replace(value).is_some() {
					return Err(Error::InvalidOptions);
				}
			}
			b"--lossless" if mode.is_none() => mode = Some(Mode::Lossless),
			b"--lossy" if mode.is_none() => mode = Some(Mode::Lossy),
			word if word.starts_with(b"-") => return Err(Error::UnsupportedOption),
			word if paths.len() < 2 => paths.push(core::str::from_utf8(word).map_err(|_| Error::InvalidOptions)?.to_string()),
			_ => return Err(Error::InvalidOptions),
		}
		index += 1;
	}
	if paths.len() != 2 {
		return Err(Error::InvalidOptions);
	}
	let suffix_format = format_from_path(&paths[1]).ok_or(Error::UnsupportedFormat)?;
	let format = format.unwrap_or(suffix_format);
	if format != suffix_format {
		return Err(Error::InvalidOptions);
	}
	match format {
		Format::Apng | Format::Ico | Format::Icns | Format::Png => {
			compression.get_or_insert(50);
		}
		Format::Gif => {
			quality.get_or_insert(100);
		}
		Format::Jpeg => {
			quality.get_or_insert(90);
			mode.get_or_insert(Mode::Lossy);
		}
		Format::WebP => {
			mode.get_or_insert(Mode::Lossless);
			match mode {
				Some(Mode::Lossless) => {
					compression.get_or_insert(100);
				}
				Some(Mode::Lossy) => {
					quality.get_or_insert(90);
					compression.get_or_insert(100);
				}
				None => unreachable!(),
			}
		}
		_ => {}
	};
	let caps = capabilities(format);
	if compression.is_some() && !caps.compression {
		return Err(Error::UnsupportedOption);
	}
	if quality.is_some() && !caps.quality {
		return Err(Error::UnsupportedOption);
	}
	if matches!(mode, Some(Mode::Lossless)) && !caps.lossless_mode || matches!(mode, Some(Mode::Lossy)) && !caps.lossy_mode {
		return Err(Error::UnsupportedOption);
	}
	if format == Format::WebP && matches!(mode, Some(Mode::Lossless)) && quality.is_some() {
		return Err(Error::UnsupportedOption);
	}
	if loop_count.is_some() && !caps.animation {
		return Err(Error::UnsupportedOption);
	}
	Ok(Config { input: paths.remove(0), output: paths.remove(0), format, force, resize, filter, frame, loop_count, quality, compression, mode })
}

pub fn convert(input: &[u8], config: &Config) -> Result<(Vec<u8>, ResultInfo), Error> {
	let (input_format, decoded) = decode_input(input)?;
	let (source_width, source_height) = decoded.dimensions();
	let animated_webp = config.format == Format::WebP && config.frame.is_none() && matches!(&decoded, Decoded::Animation(_));
	if animated_webp && matches!(config.mode, Some(Mode::Lossy)) {
		return Err(Error::UnsupportedOption);
	}
	if matches!(config.format, Format::Apng | Format::Gif) || animated_webp {
		let mut animation = match decoded {
			Decoded::Still(image) => pix::Animation::still(match config.resize {
				Some((width, height)) => resize(&image, width, height, config.filter)?,
				None => image,
			}),
			Decoded::Animation(mut animation) => {
				if config.resize.is_some() {
					return Err(Error::UnsupportedOption);
				}
				if let Some(frame) = config.frame {
					pix::Animation::still(composite_frame(&animation, frame)?)
				} else {
					animation.loop_count = config.loop_count.unwrap_or(animation.loop_count);
					animation
				}
			}
		};
		animation.loop_count = config.loop_count.unwrap_or(animation.loop_count);
		let encoded = match config.format {
			Format::Apng => apng::encode(&animation, config.compression.ok_or(Error::InvalidOptions)?).map_err(map_apng_error)?,
			Format::Gif => gif::encode_with_options(&animation, gif::EncodeOptions { quality: config.quality.ok_or(Error::InvalidOptions)?, dither: true, alpha_threshold: 128 }).map_err(map_gif_error)?,
			Format::WebP => webp::encode_animation(&animation, config.compression.ok_or(Error::InvalidOptions)?).map_err(map_webp_error)?,
			_ => return Err(Error::InvalidOptions),
		};
		let info = ResultInfo { input_format, output_format: config.format, source_width, source_height, output_width: animation.width, output_height: animation.height, output_bytes: encoded.len(), quality: config.quality, compression: config.compression, mode: config.mode };
		return Ok((encoded, info));
	}
	let image = match decoded {
		Decoded::Still(image) => image,
		Decoded::Animation(animation) => composite_frame(&animation, config.frame.ok_or(Error::InvalidOptions)?)?,
	};
	let image = match config.resize {
		Some((width, height)) => resize(&image, width, height, config.filter)?,
		None => image,
	};
	let encoded = match config.format {
		Format::Apng => return Err(Error::InvalidOptions),
		Format::Bmp => match config.quality {
			Some(quality) => bmp::encode_indexed(&image, quality).map_err(map_bmp_error)?,
			None => bmp::encode_rgba(&image).map_err(map_bmp_error)?,
		},
		Format::Gif => return Err(Error::InvalidOptions),
		Format::Ico => ico::encode(core::slice::from_ref(&image), config.compression.ok_or(Error::InvalidOptions)?).map_err(map_ico_error)?,
		Format::Icns => icns::encode(core::slice::from_ref(&image), config.compression.ok_or(Error::InvalidOptions)?).map_err(map_icns_error)?,
		Format::Jpeg => jpeg::encode(&image, config.quality.ok_or(Error::InvalidOptions)?).map_err(map_jpeg_error)?,
		Format::Png => match config.quality {
			Some(quality) => png::encode_indexed(&image, config.compression.ok_or(Error::InvalidOptions)?, quality).map_err(map_png_error)?,
			None => png::encode_rgba(&image, png::EncodeOptions { compression: config.compression.ok_or(Error::InvalidOptions)? }).map_err(map_png_error)?,
		},
		Format::Pcx => match config.quality {
			Some(quality) => pcx::encode_indexed(&image, quality).map_err(map_pcx_error)?,
			None => pcx::encode(&image).map_err(map_pcx_error)?,
		},
		Format::Ppm => ppm::encode(&image).map_err(map_ppm_error)?,
		Format::Qoi => qoi::encode(&image).map_err(map_qoi_error)?,
		Format::Tga => tga::encode(&image, tga::EncodeOptions { rle: true }).map_err(map_tga_error)?,
		Format::WebP => match config.mode {
			Some(Mode::Lossless) => webp::encode_lossless(&image, config.compression.ok_or(Error::InvalidOptions)?).map_err(map_webp_error)?,
			Some(Mode::Lossy) => webp::encode_lossy(&image, config.quality.ok_or(Error::InvalidOptions)?, config.compression.ok_or(Error::InvalidOptions)?).map_err(map_webp_error)?,
			None => return Err(Error::InvalidOptions),
		},
	};
	let info = ResultInfo { input_format, output_format: config.format, source_width, source_height, output_width: image.width, output_height: image.height, output_bytes: encoded.len(), quality: config.quality, compression: config.compression, mode: config.mode };
	Ok((encoded, info))
}

pub fn decode_frame(input: &[u8], frame: usize) -> Result<(Format, pix::RgbaImage), Error> {
	let (format, decoded) = decode_input(input)?;
	let image = match decoded {
		Decoded::Still(image) if frame == 0 => image,
		Decoded::Still(_) => return Err(Error::InvalidOptions),
		Decoded::Animation(animation) => composite_frame(&animation, frame)?,
	};
	Ok((format, image))
}

fn decode_input(input: &[u8]) -> Result<(Format, Decoded), Error> {
	let decoded = if is_apng(input) {
		(Format::Apng, Decoded::Animation(apng::decode(input).map_err(map_apng_error)?))
	} else if matches!(input.get(..6), Some(b"GIF87a") | Some(b"GIF89a")) {
		(Format::Gif, Decoded::Animation(gif::decode(input).map_err(map_gif_error)?))
	} else if input.starts_with(b"BM") {
		(Format::Bmp, Decoded::Still(bmp::decode_rgba(input).map_err(map_bmp_error)?))
	} else if input.starts_with(b"\x00\x00\x01\x00") {
		(Format::Ico, Decoded::Still(ico::decode(input).map_err(map_ico_error)?))
	} else if input.starts_with(b"icns") {
		(Format::Icns, Decoded::Still(icns::decode(input).map_err(map_icns_error)?))
	} else if input.starts_with(b"\xff\xd8") {
		(Format::Jpeg, Decoded::Still(jpeg::decode(input).map_err(map_jpeg_error)?))
	} else if input.starts_with(b"\x89PNG\r\n\x1a\n") {
		(Format::Png, Decoded::Still(png::decode_rgba(input).map_err(map_png_error)?))
	} else if input.starts_with(b"P3") || input.starts_with(b"P6") {
		(Format::Ppm, Decoded::Still(ppm::decode(input).map_err(map_ppm_error)?))
	} else if input.first() == Some(&0x0a) {
		(Format::Pcx, Decoded::Still(pcx::decode(input).map_err(map_pcx_error)?))
	} else if input.starts_with(b"qoif") {
		(Format::Qoi, Decoded::Still(qoi::decode(input).map_err(map_qoi_error)?))
	} else if input.starts_with(b"RIFF") && input.get(8..12) == Some(b"WEBP") {
		match webp::decode_animation(input) {
			Ok(animation) => (Format::WebP, Decoded::Animation(animation)),
			Err(webp::Error::Unsupported) => (Format::WebP, Decoded::Still(webp::decode(input).map_err(map_webp_error)?)),
			Err(error) => return Err(map_webp_error(error)),
		}
	} else if looks_like_tga(input) {
		(Format::Tga, Decoded::Still(tga::decode(input).map_err(map_tga_error)?))
	} else {
		return Err(Error::UnsupportedFormat);
	};
	Ok(decoded)
}

enum Decoded {
	Still(pix::RgbaImage),
	Animation(pix::Animation),
}

impl Decoded {
	fn dimensions(&self) -> (u32, u32) {
		match self {
			Self::Still(image) => (image.width, image.height),
			Self::Animation(animation) => (animation.width, animation.height),
		}
	}
}

fn is_apng(input: &[u8]) -> bool {
	input.starts_with(b"\x89PNG\r\n\x1a\n") && input.windows(4).any(|window| window == b"acTL")
}

fn composite_frame(animation: &pix::Animation, target: usize) -> Result<pix::RgbaImage, Error> {
	if target >= animation.frames.len() {
		return Err(Error::InvalidOptions);
	}
	let mut compositor = pix::Compositor::new(animation.width, animation.height).map_err(map_pix_error)?;
	for (index, frame) in animation.frames.iter().enumerate() {
		let displayed = compositor.render(frame).map_err(map_pix_error)?;
		if index == target {
			return Ok(displayed);
		}
	}
	Err(Error::InvalidOptions)
}

fn format_from_path(path: &str) -> Option<Format> {
	let suffix = path.rsplit_once('.')?.1.as_bytes();
	Format::parse(suffix)
}

fn looks_like_tga(input: &[u8]) -> bool {
	input.len() >= 18 && input[1] == 0 && matches!(input[2], 2 | 10) && matches!(input[16], 24 | 32)
}

fn parse_size(value: &[u8]) -> Result<(u32, u32), Error> {
	let separator = value.iter().position(|byte| matches!(byte, b'x' | b'X')).ok_or(Error::InvalidOptions)?;
	let width = parse_u32(&value[..separator])?;
	let height = parse_u32(&value[separator + 1..])?;
	if width == 0 || height == 0 || width > pix::MAX_DIMENSION || height > pix::MAX_DIMENSION || width as u64 * height as u64 > pix::MAX_PIXELS {
		return Err(Error::TooLarge);
	}
	Ok((width, height))
}

fn parse_percent(value: &[u8]) -> Result<u8, Error> {
	let value = parse_u32(value)?;
	u8::try_from(value).ok().filter(|value| *value <= 100).ok_or(Error::InvalidOptions)
}

fn parse_u32(value: &[u8]) -> Result<u32, Error> {
	if value.is_empty() {
		return Err(Error::InvalidOptions);
	}
	value.iter().try_fold(0u32, |result, byte| {
		if !byte.is_ascii_digit() {
			return Err(Error::InvalidOptions);
		}
		result.checked_mul(10).and_then(|result| result.checked_add((byte - b'0') as u32)).ok_or(Error::InvalidOptions)
	})
}

fn resize(source: &pix::RgbaImage, width: u32, height: u32, filter: Filter) -> Result<pix::RgbaImage, Error> {
	let output_len = usize::try_from(width).ok().and_then(|width| width.checked_mul(height as usize)).and_then(|pixels| pixels.checked_mul(4)).ok_or(Error::TooLarge)?;
	let mut pixels = Vec::new();
	pixels.try_reserve_exact(output_len).map_err(|_| Error::TooLarge)?;
	pixels.resize(output_len, 0);
	match filter {
		Filter::Nearest => resize_nearest(source, width, height, &mut pixels),
		Filter::Bilinear => resize_bilinear(source, width, height, &mut pixels),
	}
	pix::RgbaImage::new(width, height, pixels).map_err(map_pix_error)
}

fn resize_nearest(source: &pix::RgbaImage, width: u32, height: u32, output: &mut [u8]) {
	for y in 0..height {
		let source_y = y as u64 * source.height as u64 / height as u64;
		for x in 0..width {
			let source_x = x as u64 * source.width as u64 / width as u64;
			let source_offset = (source_y * source.pitch as u64 + source_x * 4) as usize;
			let output_offset = (y as usize * width as usize + x as usize) * 4;
			output[output_offset..output_offset + 4].copy_from_slice(&source.pixels[source_offset..source_offset + 4]);
		}
	}
}

fn resize_bilinear(source: &pix::RgbaImage, width: u32, height: u32, output: &mut [u8]) {
	for y in 0..height {
		let source_y = if height == 1 { 0 } else { y as u64 * (source.height - 1) as u64 * 65_536 / (height - 1) as u64 };
		let y0 = (source_y >> 16) as u32;
		let y1 = (y0 + 1).min(source.height - 1);
		let fy = (source_y & 0xffff) as u32;
		for x in 0..width {
			let source_x = if width == 1 { 0 } else { x as u64 * (source.width - 1) as u64 * 65_536 / (width - 1) as u64 };
			let x0 = (source_x >> 16) as u32;
			let x1 = (x0 + 1).min(source.width - 1);
			let fx = (source_x & 0xffff) as u32;
			let output_offset = (y as usize * width as usize + x as usize) * 4;
			for channel in 0..4 {
				let sample = |sx: u32, sy: u32| source.pixels[(sy as usize * source.pitch as usize + sx as usize * 4) + channel] as u64;
				let top = sample(x0, y0) * (65_536 - fx) as u64 + sample(x1, y0) * fx as u64;
				let bottom = sample(x0, y1) * (65_536 - fx) as u64 + sample(x1, y1) * fx as u64;
				output[output_offset + channel] = (((top * (65_536 - fy) as u64 + bottom * fy as u64) + (1u64 << 31)) >> 32) as u8;
			}
		}
	}
}

fn map_bmp_error(error: bmp::Error) -> Error {
	match error {
		bmp::Error::TooLarge => Error::TooLarge,
		bmp::Error::Unsupported => Error::UnsupportedFormat,
		_ => Error::InvalidImage,
	}
}

fn map_apng_error(error: apng::Error) -> Error {
	match error {
		apng::Error::TooLarge => Error::TooLarge,
		apng::Error::Unsupported => Error::UnsupportedFormat,
		_ => Error::InvalidImage,
	}
}

fn map_gif_error(error: gif::Error) -> Error {
	match error {
		gif::Error::TooLarge => Error::TooLarge,
		gif::Error::Unsupported => Error::UnsupportedFormat,
		_ => Error::InvalidImage,
	}
}

fn map_ico_error(error: ico::Error) -> Error {
	match error {
		ico::Error::TooLarge => Error::TooLarge,
		ico::Error::Unsupported => Error::UnsupportedFormat,
		_ => Error::InvalidImage,
	}
}

fn map_icns_error(error: icns::Error) -> Error {
	match error {
		icns::Error::TooLarge => Error::TooLarge,
		icns::Error::Unsupported => Error::UnsupportedFormat,
		_ => Error::InvalidImage,
	}
}

fn map_jpeg_error(error: jpeg::Error) -> Error {
	match error {
		jpeg::Error::TooLarge => Error::TooLarge,
		jpeg::Error::Unsupported => Error::UnsupportedFormat,
		jpeg::Error::Invalid => Error::InvalidImage,
	}
}

fn map_png_error(error: png::Error) -> Error {
	match error {
		png::Error::TooLarge => Error::TooLarge,
		png::Error::Unsupported => Error::UnsupportedFormat,
		_ => Error::InvalidImage,
	}
}

fn map_ppm_error(error: ppm::Error) -> Error {
	match error {
		ppm::Error::TooLarge => Error::TooLarge,
		ppm::Error::Unsupported => Error::UnsupportedFormat,
		_ => Error::InvalidImage,
	}
}

fn map_pcx_error(error: pcx::Error) -> Error {
	match error {
		pcx::Error::TooLarge => Error::TooLarge,
		pcx::Error::Unsupported => Error::UnsupportedFormat,
		_ => Error::InvalidImage,
	}
}

fn map_qoi_error(error: qoi::Error) -> Error {
	match error {
		qoi::Error::TooLarge => Error::TooLarge,
		qoi::Error::Unsupported => Error::UnsupportedFormat,
		qoi::Error::Invalid => Error::InvalidImage,
	}
}

fn map_tga_error(error: tga::Error) -> Error {
	match error {
		tga::Error::TooLarge => Error::TooLarge,
		tga::Error::Unsupported => Error::UnsupportedFormat,
		_ => Error::InvalidImage,
	}
}

fn map_webp_error(error: webp::Error) -> Error {
	match error {
		webp::Error::TooLarge => Error::TooLarge,
		webp::Error::Unsupported => Error::UnsupportedFormat,
		webp::Error::Invalid => Error::InvalidImage,
	}
}

fn map_pix_error(error: pix::Error) -> Error {
	match error {
		pix::Error::Invalid => Error::InvalidImage,
		pix::Error::TooLarge => Error::TooLarge,
	}
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_capabilities_and_rejects_inapplicable_options() {
		let png = parse_args(b"--compression 100 --resize 4x3 --filter nearest in.bmp out.png").unwrap();
		assert_eq!(png.format, Format::Png);
		assert_eq!(png.resize, Some((4, 3)));
		assert_eq!(png.compression, Some(100));
		assert_eq!(parse_args(b"--compression 1 in.png out.bmp"), Err(Error::UnsupportedOption));
		let indexed_png = parse_args(b"--quality 90 --compression 75 in.png out.png").unwrap();
		assert_eq!((indexed_png.quality, indexed_png.compression), (Some(90), Some(75)));
		let jpeg = parse_args(b"--quality 90 --lossy in.png out.jpeg").unwrap();
		assert_eq!((jpeg.quality, jpeg.mode), (Some(90), Some(Mode::Lossy)));
		assert_eq!(parse_args(b"--lossless in.png out.jpg"), Err(Error::UnsupportedOption));
		let gif = parse_args(b"--quality 25 in.png out.gif").unwrap();
		assert_eq!(gif.quality, Some(25));
		assert_eq!(parse_args(b"in.png out.gif").unwrap().quality, Some(100));
		assert_eq!(parse_args(b"--quality 25 in.png out.bmp").unwrap().quality, Some(25));
		assert_eq!(parse_args(b"--quality 75 in.png out.pcx").unwrap().quality, Some(75));
		let webp = parse_args(b"--lossless --compression 100 in.png out.webp").unwrap();
		assert_eq!(webp.mode, Some(Mode::Lossless));
		let lossy_webp = parse_args(b"--lossy --quality 80 in.png out.webp").unwrap();
		assert_eq!((lossy_webp.mode, lossy_webp.quality, lossy_webp.compression), (Some(Mode::Lossy), Some(80), Some(100)));
		assert_eq!(parse_args(b"--lossy in.png out.webp").unwrap().quality, Some(90));
		assert_eq!(parse_args(b"--quality 80 in.png out.webp"), Err(Error::UnsupportedOption));
		assert_eq!(parse_args(b"--lossless --quality 80 in.png out.webp"), Err(Error::UnsupportedOption));
		assert_eq!(parse_args(b"--lossy --compression 0 in.png out.webp").unwrap().compression, Some(0));
		assert_eq!(parse_args(b"--format bmp in.png out.png"), Err(Error::InvalidOptions));
	}

	#[test]
	fn converts_staged_bmp_png_and_resizes() {
		let png_config = parse_args(b"--compression 100 --resize 4x4 in.bmp out.png").unwrap();
		let (encoded, info) = convert(include_bytes!("../../../volume/sample.bmp"), &png_config).unwrap();
		let decoded = png::decode_rgba(&encoded).unwrap();
		assert_eq!((decoded.width, decoded.height), (4, 4));
		assert_eq!(info.input_format, Format::Bmp);
		let bmp_config = parse_args(b"in.png out.bmp").unwrap();
		let (encoded, _) = convert(include_bytes!("../../../volume/sample.png"), &bmp_config).unwrap();
		assert_eq!(bmp::decode_rgba(&encoded).unwrap().pixels.len(), 16);
	}

	#[test]
	fn converts_opaque_bmp_to_explicit_indexed_png() {
		let source = bmp::decode_rgba(include_bytes!("../../../volume/sample.bmp")).unwrap();
		let config = parse_args(b"--quality 0 --compression 100 in.bmp out.png").unwrap();
		let (encoded, info) = convert(include_bytes!("../../../volume/sample.bmp"), &config).unwrap();
		assert!(encoded.windows(4).any(|window| window == b"PLTE"));
		assert_eq!(png::decode_rgba(&encoded).unwrap(), source);
		assert_eq!((info.quality, info.compression), (Some(0), Some(100)));
	}

	#[test]
	fn converts_true_color_to_explicit_indexed_bmp_and_pcx() {
		let mut pixels = Vec::new();
		for value in 0..512u32 {
			pixels.extend_from_slice(&[value as u8, (value >> 1) as u8, value.wrapping_mul(47) as u8, 255]);
		}
		let source = pix::RgbaImage::new(512, 1, pixels).unwrap();
		let source_png = png::encode_rgba(&source, png::EncodeOptions { compression: 0 }).unwrap();
		let (bmp_bytes, bmp_info) = convert(&source_png, &parse_args(b"--quality 0 in.png out.bmp").unwrap()).unwrap();
		assert_eq!(u16::from_le_bytes([bmp_bytes[28], bmp_bytes[29]]), 8);
		assert_eq!(bmp_info.quality, Some(0));
		assert_eq!(bmp::decode_rgba(&bmp_bytes).unwrap().width, source.width);

		let (pcx_bytes, pcx_info) = convert(&source_png, &parse_args(b"--quality 100 in.png out.pcx").unwrap()).unwrap();
		assert_eq!(pcx_bytes[65], 1);
		assert_eq!(pcx_info.quality, Some(100));
		assert_eq!(pcx::decode(&pcx_bytes).unwrap().width, source.width);
	}

	#[test]
	fn converts_png_to_ico_pcx_ppm_qoi_and_tga() {
		let source = png::decode_rgba(include_bytes!("../../../volume/sample.png")).unwrap();
		for (arguments, expected) in [
			(b"--compression 100 in.png out.ico".as_slice(), Format::Ico),
			(b"in.png out.pcx".as_slice(), Format::Pcx),
			(b"in.png out.ppm".as_slice(), Format::Ppm),
			(b"in.png out.qoi".as_slice(), Format::Qoi),
			(b"in.png out.tga".as_slice(), Format::Tga),
		] {
			let config = parse_args(arguments).unwrap();
			let converted = convert(include_bytes!("../../../volume/sample.png"), &config);
			if matches!(expected, Format::Pcx | Format::Ppm) && source.pixels.chunks_exact(4).any(|pixel| pixel[3] != 255) {
				assert_eq!(converted, Err(Error::UnsupportedFormat));
				continue;
			}
			let (encoded, info) = converted.unwrap();
			let decoded = match expected {
				Format::Ico => ico::decode(&encoded).unwrap(),
				Format::Pcx => pcx::decode(&encoded).unwrap(),
				Format::Ppm => ppm::decode(&encoded).unwrap(),
				Format::Qoi => qoi::decode(&encoded).unwrap(),
				Format::Tga => tga::decode(&encoded).unwrap(),
				_ => unreachable!(),
			};
			assert_eq!(decoded, source);
			assert_eq!(info.output_format, expected);
		}
	}

	#[test]
	fn converts_opaque_bmp_to_quality_jpeg() {
		let source = bmp::decode_rgba(include_bytes!("../../../volume/sample.bmp")).unwrap();
		let config = parse_args(b"--quality 100 in.bmp out.jpg").unwrap();
		let (encoded, info) = convert(include_bytes!("../../../volume/sample.bmp"), &config).unwrap();
		let decoded = jpeg::decode(&encoded).unwrap();
		assert_eq!((decoded.width, decoded.height), (source.width, source.height));
		assert_eq!(info.quality, Some(100));
	}

	#[test]
	fn converts_png_to_lossless_webp_endpoints() {
		let source = png::decode_rgba(include_bytes!("../../../volume/sample.png")).unwrap();
		for compression in [0, 100] {
			let arguments = if compression == 0 { b"--lossless --compression 0 in.png out.webp".as_slice() } else { b"--lossless --compression 100 in.png out.webp".as_slice() };
			let (encoded, info) = convert(include_bytes!("../../../volume/sample.png"), &parse_args(arguments).unwrap()).unwrap();
			assert_eq!(webp::decode(&encoded).unwrap(), source);
			assert_eq!(info.mode, Some(Mode::Lossless));
		}
		let config = parse_args(b"--lossless --compression 50 in.png out.webp").unwrap();
		assert_eq!(convert(include_bytes!("../../../volume/sample.png"), &config), Err(Error::UnsupportedFormat));
	}

	#[test]
	fn converts_png_to_lossy_webp_with_quality_and_alpha() {
		let mut pixels = Vec::new();
		for y in 0..17u8 {
			for x in 0..19u8 {
				pixels.extend_from_slice(&[x.wrapping_mul(11), y.wrapping_mul(13), x.wrapping_mul(5).wrapping_add(y.wrapping_mul(7)), x.wrapping_mul(17).wrapping_add(y.wrapping_mul(3))]);
			}
		}
		let source = pix::RgbaImage::new(19, 17, pixels).unwrap();
		let source_png = png::encode_rgba(&source, png::EncodeOptions { compression: 0 }).unwrap();
		let (low_bytes, low_info) = convert(&source_png, &parse_args(b"--lossy --quality 0 in.png out.webp").unwrap()).unwrap();
		let (high_bytes, high_info) = convert(&source_png, &parse_args(b"--lossy --quality 100 in.png out.webp").unwrap()).unwrap();
		assert_ne!(low_bytes, high_bytes);
		assert_eq!((low_info.mode, low_info.quality, low_info.compression), (Some(Mode::Lossy), Some(0), Some(100)));
		assert_eq!((high_info.mode, high_info.quality, high_info.compression), (Some(Mode::Lossy), Some(100), Some(100)));
		let low = webp::decode(&low_bytes).unwrap();
		let high = webp::decode(&high_bytes).unwrap();
		let error = |actual: &pix::RgbaImage| -> u64 { actual.pixels.chunks_exact(4).zip(source.pixels.chunks_exact(4)).map(|(actual, expected)| (0..3).map(|channel| u64::from(actual[channel].abs_diff(expected[channel]))).sum::<u64>()).sum() };
		assert!(error(&high) < error(&low));
		for actual in [low, high] {
			assert_eq!((actual.width, actual.height), (source.width, source.height));
			assert_eq!(actual.pixels.iter().skip(3).step_by(4).copied().collect::<Vec<_>>(), source.pixels.iter().skip(3).step_by(4).copied().collect::<Vec<_>>());
		}
	}

	#[test]
	fn animated_webp_requires_frame_for_static_and_converts_to_apng() {
		let source = include_bytes!("../../webp/tests/animated.webp");
		assert_eq!(convert(source, &parse_args(b"in.webp out.png").unwrap()), Err(Error::InvalidOptions));
		let (frame, _) = convert(source, &parse_args(b"--frame 1 in.webp out.png").unwrap()).unwrap();
		assert_eq!(png::decode_rgba(&frame).unwrap(), webp::decode_animation(source).unwrap().frames[1].image);
		let (apng, _) = convert(source, &parse_args(b"--compression 100 in.webp out.apng").unwrap()).unwrap();
		assert_eq!(apng::decode(&apng).unwrap().frames.len(), 2);
	}

	#[test]
	fn converts_resized_png_to_modern_icns() {
		let config = parse_args(b"--resize 128x128 --compression 100 in.png out.icns").unwrap();
		let (encoded, info) = convert(include_bytes!("../../../volume/sample.png"), &config).unwrap();
		let decoded = icns::decode(&encoded).unwrap();
		assert_eq!((decoded.width, decoded.height), (128, 128));
		assert_eq!(info.output_format, Format::Icns);
	}

	#[test]
	fn converts_resized_png_to_classic_rle_icns() {
		let config = parse_args(b"--resize 32x32 --compression 100 in.png out.icns").unwrap();
		let (encoded, info) = convert(include_bytes!("../../../volume/sample.png"), &config).unwrap();
		assert!(encoded.windows(4).any(|window| window == b"il32"));
		assert!(encoded.windows(4).any(|window| window == b"l8mk"));
		let decoded = icns::decode(&encoded).unwrap();
		assert_eq!((decoded.width, decoded.height), (32, 32));
		assert_eq!(info.output_format, Format::Icns);
	}

	#[test]
	fn preserves_apng_or_extracts_only_an_explicit_composited_frame() {
		let first = pix::RgbaImage::new(2, 1, alloc::vec![255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
		let second = pix::RgbaImage::new(1, 1, alloc::vec![0, 255, 0, 128]).unwrap();
		let animation = pix::Animation::new(
			2,
			1,
			2,
			alloc::vec![
				pix::Frame { image: first, x: 0, y: 0, duration_ms: 20, blend: pix::Blend::Source, disposal: pix::Disposal::Keep },
				pix::Frame { image: second, x: 1, y: 0, duration_ms: 30, blend: pix::Blend::Over, disposal: pix::Disposal::Previous }
			],
		)
		.unwrap();
		let source = apng::encode(&animation, 50).unwrap();
		let preserve = parse_args(b"--loop 5 --compression 100 in.apng out.apng").unwrap();
		let (encoded, _) = convert(&source, &preserve).unwrap();
		let decoded = apng::decode(&encoded).unwrap();
		assert_eq!(decoded.loop_count, 5);
		assert_eq!(decoded.frames, animation.frames);
		assert_eq!(convert(&source, &parse_args(b"in.apng out.png").unwrap()), Err(Error::InvalidOptions));
		let (frame, _) = convert(&source, &parse_args(b"--frame 1 in.apng out.png").unwrap()).unwrap();
		let frame = png::decode_rgba(&frame).unwrap();
		assert_eq!(frame.pixels[7], 128);
	}

	#[test]
	fn preserves_gif_timing_loop_and_explicit_frame_rule() {
		let first = pix::RgbaImage::new(2, 1, alloc::vec![255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
		let second = pix::RgbaImage::new(1, 1, alloc::vec![0, 255, 0, 255]).unwrap();
		let animation = pix::Animation::new(
			2,
			1,
			2,
			alloc::vec![
				pix::Frame { image: first, x: 0, y: 0, duration_ms: 20, blend: pix::Blend::Over, disposal: pix::Disposal::Keep },
				pix::Frame { image: second, x: 1, y: 0, duration_ms: 30, blend: pix::Blend::Over, disposal: pix::Disposal::Previous }
			],
		)
		.unwrap();
		let source = gif::encode(&animation).unwrap();
		assert_eq!(convert(&source, &parse_args(b"in.gif out.png").unwrap()), Err(Error::InvalidOptions));
		let (encoded, info) = convert(&source, &parse_args(b"--loop 7 --quality 100 in.gif out.gif").unwrap()).unwrap();
		let decoded = gif::decode(&encoded).unwrap();
		assert_eq!(decoded.loop_count, 7);
		assert_eq!(decoded.frames, animation.frames);
		assert_eq!(info.quality, Some(100));

		let (webp_bytes, webp_info) = convert(&source, &parse_args(b"--loop 7 --lossless --compression 100 in.gif out.webp").unwrap()).unwrap();
		let webp_animation = webp::decode_animation(&webp_bytes).unwrap();
		assert_eq!(webp_animation.loop_count, 7);
		assert_eq!(webp_animation.frames.iter().map(|frame| frame.duration_ms).collect::<Vec<_>>(), alloc::vec![20, 30]);
		assert_eq!(webp_info.mode, Some(Mode::Lossless));
		let (still_bytes, _) = convert(&source, &parse_args(b"--frame 1 --lossless --compression 100 in.gif out.webp").unwrap()).unwrap();
		assert!(webp::decode_animation(&still_bytes).is_err());
	}
}

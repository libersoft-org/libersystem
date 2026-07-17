#![no_std]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatProfile {
	pub format: Format,
	pub capabilities: Capabilities,
	pub default_quality: Option<u8>,
	pub default_compression: Option<u8>,
	pub default_mode: Option<Mode>,
	pub lossy_quality: Option<u8>,
	pub lossy_compression: Option<u8>,
}

pub const FORMAT_PROFILES: &[FormatProfile] = &[
	FormatProfile { format: Format::Apng, capabilities: Capabilities { quality: false, compression: true, lossless_mode: false, lossy_mode: false, animation: true, alpha: true }, default_quality: None, default_compression: Some(50), default_mode: None, lossy_quality: None, lossy_compression: None },
	FormatProfile { format: Format::Bmp, capabilities: Capabilities { quality: true, compression: false, lossless_mode: false, lossy_mode: false, animation: false, alpha: false }, default_quality: None, default_compression: None, default_mode: None, lossy_quality: None, lossy_compression: None },
	FormatProfile { format: Format::Gif, capabilities: Capabilities { quality: true, compression: false, lossless_mode: false, lossy_mode: false, animation: true, alpha: true }, default_quality: Some(100), default_compression: None, default_mode: None, lossy_quality: None, lossy_compression: None },
	FormatProfile { format: Format::Ico, capabilities: Capabilities { quality: false, compression: true, lossless_mode: false, lossy_mode: false, animation: false, alpha: true }, default_quality: None, default_compression: Some(50), default_mode: None, lossy_quality: None, lossy_compression: None },
	FormatProfile { format: Format::Icns, capabilities: Capabilities { quality: false, compression: true, lossless_mode: false, lossy_mode: false, animation: false, alpha: true }, default_quality: None, default_compression: Some(50), default_mode: None, lossy_quality: None, lossy_compression: None },
	FormatProfile { format: Format::Jpeg, capabilities: Capabilities { quality: true, compression: false, lossless_mode: false, lossy_mode: true, animation: false, alpha: false }, default_quality: Some(90), default_compression: None, default_mode: Some(Mode::Lossy), lossy_quality: Some(90), lossy_compression: None },
	FormatProfile { format: Format::Png, capabilities: Capabilities { quality: true, compression: true, lossless_mode: false, lossy_mode: false, animation: false, alpha: true }, default_quality: None, default_compression: Some(50), default_mode: None, lossy_quality: None, lossy_compression: None },
	FormatProfile { format: Format::Pcx, capabilities: Capabilities { quality: true, compression: false, lossless_mode: false, lossy_mode: false, animation: false, alpha: false }, default_quality: None, default_compression: None, default_mode: None, lossy_quality: None, lossy_compression: None },
	FormatProfile { format: Format::Ppm, capabilities: Capabilities { quality: false, compression: false, lossless_mode: false, lossy_mode: false, animation: false, alpha: false }, default_quality: None, default_compression: None, default_mode: None, lossy_quality: None, lossy_compression: None },
	FormatProfile { format: Format::Qoi, capabilities: Capabilities { quality: false, compression: false, lossless_mode: false, lossy_mode: false, animation: false, alpha: true }, default_quality: None, default_compression: None, default_mode: None, lossy_quality: None, lossy_compression: None },
	FormatProfile { format: Format::Tga, capabilities: Capabilities { quality: false, compression: false, lossless_mode: false, lossy_mode: false, animation: false, alpha: true }, default_quality: None, default_compression: None, default_mode: None, lossy_quality: None, lossy_compression: None },
	FormatProfile { format: Format::WebP, capabilities: Capabilities { quality: true, compression: true, lossless_mode: true, lossy_mode: true, animation: true, alpha: true }, default_quality: None, default_compression: Some(100), default_mode: Some(Mode::Lossless), lossy_quality: Some(90), lossy_compression: Some(100) },
];

pub fn profile(format: Format) -> &'static FormatProfile {
	FORMAT_PROFILES.iter().find(|profile| profile.format == format).expect("complete format profile table")
}

pub const fn capabilities(format: Format) -> Capabilities {
	FORMAT_PROFILES[format as usize].capabilities
}

pub fn help_text() -> String {
	let mut help = String::from("Usage: imgconv [options] <input> <output>\n\nOptions:\n  --format <name>       Output format (must match output suffix)\n  --force               Replace an existing destination\n  --resize <WxH>        Resize output within image geometry limits\n  --filter <name>       nearest or bilinear (default: bilinear)\n  --frame <index>       Extract one composited animation frame\n  --loop <count>        Override animation loop count\n  --quality <0..100>    Palette/lossy quality where supported\n  --compression <0..100> Encoder effort where supported\n  --lossless            Select lossless WebP mode\n  --lossy               Select lossy JPEG/WebP mode\n  --help                 Show this help\n\nOutput profiles:\n");
	for profile in FORMAT_PROFILES {
		let caps = profile.capabilities;
		let _ = write!(help, "  {:<5} options:", profile.format.name());
		if caps.quality {
			help.push_str(" quality");
		}
		if caps.compression {
			help.push_str(" compression");
		}
		if caps.lossless_mode {
			help.push_str(" lossless");
		}
		if caps.lossy_mode {
			help.push_str(" lossy");
		}
		if caps.animation {
			help.push_str(" animation");
		}
		if !caps.quality && !caps.compression && !caps.lossless_mode && !caps.lossy_mode && !caps.animation {
			help.push_str(" none");
		}
		help.push_str("; defaults:");
		if let Some(mode) = profile.default_mode {
			help.push_str(match mode {
				Mode::Lossless => " mode=lossless",
				Mode::Lossy => " mode=lossy",
			});
		}
		if let Some(quality) = profile.default_quality {
			let _ = write!(help, " quality={quality}");
		}
		if let Some(compression) = profile.default_compression {
			let _ = write!(help, " compression={compression}");
		}
		if profile.default_mode.is_none() && profile.default_quality.is_none() && profile.default_compression.is_none() {
			help.push_str(" none");
		}
		if profile.default_mode != Some(Mode::Lossy) && (profile.lossy_quality.is_some() || profile.lossy_compression.is_some()) {
			help.push_str("; lossy defaults:");
			if let Some(quality) = profile.lossy_quality {
				let _ = write!(help, " quality={quality}");
			}
			if let Some(compression) = profile.lossy_compression {
				let _ = write!(help, " compression={compression}");
			}
		}
		help.push('\n');
	}
	help
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
	let profile = profile(format);
	mode = mode.or(profile.default_mode);
	if mode == Some(Mode::Lossy) {
		quality = quality.or(profile.lossy_quality);
		compression = compression.or(profile.lossy_compression);
	} else {
		quality = quality.or(profile.default_quality);
		compression = compression.or(profile.default_compression);
	}
	let caps = profile.capabilities;
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
		if config.format == Format::Apng && animation.background != [0; 4] || config.format == Format::Gif && !matches!(animation.background[3], 0 | 255) {
			animation = canonicalize_animation(&animation)?;
		}
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
	} else if looks_like_pcx(input) {
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
	if !input.starts_with(b"\x89PNG\r\n\x1a\n") {
		return false;
	}
	let mut cursor = 8usize;
	while let Some(header) = input.get(cursor..cursor + 8) {
		let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
		let Some(end) = cursor.checked_add(12).and_then(|end| end.checked_add(length)) else {
			return false;
		};
		if end > input.len() {
			return false;
		}
		match &header[4..8] {
			b"acTL" => return true,
			b"IDAT" | b"IEND" => return false,
			_ => cursor = end,
		}
	}
	false
}

fn looks_like_pcx(input: &[u8]) -> bool {
	let Some(header) = input.get(..128) else {
		return false;
	};
	let width = u16::from_le_bytes([header[8], header[9]]).checked_sub(u16::from_le_bytes([header[4], header[5]]));
	let height = u16::from_le_bytes([header[10], header[11]]).checked_sub(u16::from_le_bytes([header[6], header[7]]));
	header[0] == 0x0a && header[1] == 5 && header[2] == 1 && header[3] == 8 && width.is_some() && height.is_some() && matches!(header[65], 1 | 3) && u16::from_le_bytes([header[66], header[67]]) > width.unwrap_or(0)
}

fn composite_frame(animation: &pix::Animation, target: usize) -> Result<pix::RgbaImage, Error> {
	if target >= animation.frames.len() {
		return Err(Error::InvalidOptions);
	}
	let mut compositor = pix::Compositor::new_with_background(animation.width, animation.height, animation.background).map_err(map_pix_error)?;
	for (index, frame) in animation.frames.iter().enumerate() {
		let displayed = compositor.render(frame).map_err(map_pix_error)?;
		if index == target {
			return Ok(displayed);
		}
	}
	Err(Error::InvalidOptions)
}

fn canonicalize_animation(animation: &pix::Animation) -> Result<pix::Animation, Error> {
	let mut compositor = pix::Compositor::new_with_background(animation.width, animation.height, animation.background).map_err(map_pix_error)?;
	let mut frames = Vec::new();
	frames.try_reserve_exact(animation.frames.len()).map_err(|_| Error::TooLarge)?;
	for frame in &animation.frames {
		frames.push(pix::Frame { image: compositor.render(frame).map_err(map_pix_error)?, x: 0, y: 0, duration_ms: frame.duration_ms, blend: pix::Blend::Source, disposal: pix::Disposal::Keep });
	}
	pix::Animation::new(animation.width, animation.height, animation.loop_count, frames).map_err(map_pix_error)
}

fn format_from_path(path: &str) -> Option<Format> {
	let suffix = path.rsplit_once('.')?.1.as_bytes();
	Format::parse(suffix)
}

fn looks_like_tga(input: &[u8]) -> bool {
	input.len() >= 18 && input[1] == 0 && matches!(input[2], 2 | 10) && u16::from_le_bytes([input[12], input[13]]) != 0 && u16::from_le_bytes([input[14], input[15]]) != 0 && matches!(input[16], 24 | 32) && input[17] & 0xc0 == 0 && 18usize.checked_add(input[0] as usize).is_some_and(|start| start <= input.len())
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
		assert_eq!(parse_args(b"--lossless --compression 50 in.png out.webp").unwrap().compression, Some(50));
		let lossy_webp = parse_args(b"--lossy --quality 80 in.png out.webp").unwrap();
		assert_eq!((lossy_webp.mode, lossy_webp.quality, lossy_webp.compression), (Some(Mode::Lossy), Some(80), Some(100)));
		assert_eq!(parse_args(b"--lossy in.png out.webp").unwrap().quality, Some(90));
		assert_eq!(parse_args(b"--quality 80 in.png out.webp"), Err(Error::UnsupportedOption));
		assert_eq!(parse_args(b"--lossless --quality 80 in.png out.webp"), Err(Error::UnsupportedOption));
		assert_eq!(parse_args(b"--lossy --compression 0 in.png out.webp").unwrap().compression, Some(0));
		assert_eq!(parse_args(b"--format bmp in.png out.png"), Err(Error::InvalidOptions));
	}

	#[test]
	fn help_and_parser_defaults_follow_format_profiles() {
		let help = help_text();
		assert!(help.starts_with("Usage: imgconv [options] <input> <output>\n"));
		assert_eq!(FORMAT_PROFILES.len(), 12);
		for profile in FORMAT_PROFILES {
			assert_eq!(capabilities(profile.format), profile.capabilities);
			assert!(help.contains(&alloc::format!("  {:<5} options:", profile.format.name())));
		}
		assert!(help.contains("WebP  options: quality compression lossless lossy animation; defaults: mode=lossless compression=100; lossy defaults: quality=90 compression=100"));
		for (output, expected) in [
			("out.apng", (None, Some(50), None)),
			("out.gif", (Some(100), None, None)),
			("out.jpg", (Some(90), None, Some(Mode::Lossy))),
			("out.png", (None, Some(50), None)),
			("out.webp", (None, Some(100), Some(Mode::Lossless))),
		] {
			let config = parse_args(alloc::format!("in.bmp {output}").as_bytes()).unwrap();
			assert_eq!((config.quality, config.compression, config.mode), expected);
		}
		let lossy = parse_args(b"--lossy in.bmp out.webp").unwrap();
		assert_eq!((lossy.quality, lossy.compression, lossy.mode), (Some(90), Some(100), Some(Mode::Lossy)));
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
	fn structurally_distinguishes_static_png_payload_and_tga_id_from_apng_and_pcx() {
		let pixel = pix::RgbaImage::new(1, 1, alloc::vec![b'a', b'c', b'T', b'L']).unwrap();
		let png = png::encode_rgba(&pixel, png::EncodeOptions { compression: 0 }).unwrap();
		assert!(png.windows(4).any(|window| window == b"acTL"));
		assert_eq!(decode_frame(&png, 0).unwrap(), (Format::Png, pixel.clone()));

		let mut tga = tga::encode(&pixel, tga::EncodeOptions { rle: false }).unwrap();
		tga[0] = 10;
		tga.splice(18..18, *b"0123456789");
		assert_eq!(decode_frame(&tga, 0).unwrap(), (Format::Tga, pixel));
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
	fn converts_png_to_lossless_webp_effort_range() {
		let source = png::decode_rgba(include_bytes!("../../../volume/sample.png")).unwrap();
		for compression in [0, 1, 25, 50, 75, 99, 100] {
			let arguments = alloc::format!("--lossless --compression {compression} in.png out.webp");
			let (encoded, info) = convert(include_bytes!("../../../volume/sample.png"), &parse_args(arguments.as_bytes()).unwrap()).unwrap();
			assert_eq!(webp::decode(&encoded).unwrap(), source);
			assert_eq!((info.mode, info.compression), (Some(Mode::Lossless), Some(compression)));
		}
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
	fn webp_background_is_composited_when_apng_cannot_represent_it() {
		let background = [7, 17, 27, 255];
		let animation = pix::Animation::new_with_background(
			2,
			1,
			background,
			2,
			alloc::vec![
				pix::Frame { image: pix::RgbaImage::new(1, 1, alloc::vec![255, 0, 0, 255]).unwrap(), x: 0, y: 0, duration_ms: 0, blend: pix::Blend::Source, disposal: pix::Disposal::Background },
				pix::Frame { image: pix::RgbaImage::new(1, 1, alloc::vec![0, 255, 0, 255]).unwrap(), x: 1, y: 0, duration_ms: 30, blend: pix::Blend::Source, disposal: pix::Disposal::Keep },
			],
		)
		.unwrap();
		let mut source_compositor = pix::Compositor::new_with_background(2, 1, background).unwrap();
		let expected: Vec<_> = animation.frames.iter().map(|frame| source_compositor.render(frame).unwrap()).collect();
		let source = webp::encode_animation(&animation, 100).unwrap();
		let (encoded, _) = convert(&source, &parse_args(b"in.webp out.apng").unwrap()).unwrap();
		let decoded = apng::decode(&encoded).unwrap();
		assert_eq!(decoded.frames.iter().map(|frame| frame.duration_ms).collect::<Vec<_>>(), alloc::vec![0, 30]);
		let mut target_compositor = pix::Compositor::new(decoded.width, decoded.height).unwrap();
		let actual: Vec<_> = decoded.frames.iter().map(|frame| target_compositor.render(frame).unwrap()).collect();
		assert_eq!(actual, expected);
		let (still, _) = convert(&source, &parse_args(b"--frame 1 in.webp out.png").unwrap()).unwrap();
		assert_eq!(png::decode_rgba(&still).unwrap(), expected[1]);
		let (encoded, _) = convert(&source, &parse_args(b"--quality 100 in.webp out.gif").unwrap()).unwrap();
		let decoded = gif::decode(&encoded).unwrap();
		assert_eq!(decoded.background, background);
		let mut target_compositor = pix::Compositor::new_with_background(decoded.width, decoded.height, decoded.background).unwrap();
		let actual: Vec<_> = decoded.frames.iter().map(|frame| target_compositor.render(frame).unwrap()).collect();
		assert_eq!(actual, expected);
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

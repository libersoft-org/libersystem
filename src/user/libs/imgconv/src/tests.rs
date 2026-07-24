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
fn every_profile_driven_option_combination_matches_capabilities() {
	fn suffix(format: Format) -> &'static str {
		match format {
			Format::Apng => "apng",
			Format::Bmp => "bmp",
			Format::Gif => "gif",
			Format::Ico => "ico",
			Format::Icns => "icns",
			Format::Jpeg => "jpg",
			Format::Png => "png",
			Format::Pcx => "pcx",
			Format::Ppm => "ppm",
			Format::Qoi => "qoi",
			Format::Tga => "tga",
			Format::WebP => "webp",
		}
	}

	let mut accepted = 0usize;
	let mut rejected = 0usize;
	for profile in FORMAT_PROFILES {
		let output = alloc::format!("out.{}", suffix(profile.format));
		let quality = if profile.format == Format::WebP { alloc::format!("--lossy --quality 17 in.png {output}") } else { alloc::format!("--quality 17 in.png {output}") };
		match parse_args(quality.as_bytes()) {
			Ok(config) if profile.capabilities.quality => {
				assert_eq!(config.quality, Some(17), "{} quality value", profile.format.name());
				accepted += 1;
			}
			Err(Error::UnsupportedOption) if !profile.capabilities.quality => rejected += 1,
			result => panic!("{} quality capability mismatch: {result:?}", profile.format.name()),
		}

		let compression = alloc::format!("--compression 17 in.png {output}");
		match parse_args(compression.as_bytes()) {
			Ok(config) if profile.capabilities.compression => {
				assert_eq!(config.compression, Some(17), "{} compression value", profile.format.name());
				accepted += 1;
			}
			Err(Error::UnsupportedOption) if !profile.capabilities.compression => rejected += 1,
			result => panic!("{} compression capability mismatch: {result:?}", profile.format.name()),
		}

		for (argument, mode, supported) in [("--lossless", Mode::Lossless, profile.capabilities.lossless_mode), ("--lossy", Mode::Lossy, profile.capabilities.lossy_mode)] {
			let arguments = alloc::format!("{argument} in.png {output}");
			match parse_args(arguments.as_bytes()) {
				Ok(config) if supported => {
					assert_eq!(config.mode, Some(mode), "{} {argument} mode", profile.format.name());
					accepted += 1;
				}
				Err(Error::UnsupportedOption) if !supported => rejected += 1,
				result => panic!("{} {argument} capability mismatch: {result:?}", profile.format.name()),
			}
		}

		let loop_count = alloc::format!("--loop 7 in.png {output}");
		match parse_args(loop_count.as_bytes()) {
			Ok(config) if profile.capabilities.animation => {
				assert_eq!(config.loop_count, Some(7), "{} loop count", profile.format.name());
				accepted += 1;
			}
			Err(Error::UnsupportedOption) if !profile.capabilities.animation => rejected += 1,
			result => panic!("{} animation capability mismatch: {result:?}", profile.format.name()),
		}
	}
	assert_eq!((accepted, rejected), (17, 43));
	assert_eq!(parse_args(b"--quality 17 in.png out.webp"), Err(Error::UnsupportedOption), "WebP quality requires explicit lossy mode");
	assert_eq!(parse_args(b"--lossless --quality 17 in.png out.webp"), Err(Error::UnsupportedOption), "lossless WebP rejects quality");
}

#[test]
fn converts_staged_bmp_png_and_resizes() {
	let png_config = parse_args(b"--compression 100 --resize 4x4 in.bmp out.png").unwrap();
	let (encoded, info) = convert(include_bytes!("../../bmp/tests/data/external-rgb24.bmp"), &png_config).unwrap();
	let decoded = png::decode_rgba(&encoded).unwrap();
	assert_eq!((decoded.width, decoded.height), (4, 4));
	assert_eq!(info.input_format, Format::Bmp);
	let bmp_config = parse_args(b"in.png out.bmp").unwrap();
	let (encoded, _) = convert(include_bytes!("../../png/tests/data/external-adam7-rgb.png"), &bmp_config).unwrap();
	assert_eq!(bmp::decode_rgba(&encoded).unwrap().pixels.len(), 1_380);
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
fn distinguishes_unknown_from_corrupt_recognized_formats() {
	assert_eq!(sniff_format(b"not an image"), None);
	assert_eq!(decode_frame(b"not an image", 0), Err(Error::UnsupportedFormat));
	for (name, input, format) in [
		("BMP", b"BM".as_slice(), Format::Bmp),
		("GIF", b"GIF89a".as_slice(), Format::Gif),
		("ICO", b"\0\0\x01\0".as_slice(), Format::Ico),
		("ICNS", b"icns".as_slice(), Format::Icns),
		("JPEG", b"\xff\xd8".as_slice(), Format::Jpeg),
		("PNG", b"\x89PNG\r\n\x1a\n".as_slice(), Format::Png),
		("PPM", b"P6 ".as_slice(), Format::Ppm),
		("PCX", b"\x0a\x05\x01\x08".as_slice(), Format::Pcx),
		("QOI", b"qoif".as_slice(), Format::Qoi),
		("WebP", b"RIFF\0\0\0\0WEBP".as_slice(), Format::WebP),
		("TGA", b"\0\0\x02".as_slice(), Format::Tga),
	] {
		assert_eq!(sniff_format(input), Some(format), "{name} family classification");
		assert_eq!(decode_frame(input, 0), Err(Error::InvalidImage), "{name} corrupt classification");
	}

	let mut apng = include_bytes!("../../apng/tests/data/external-animation.png").to_vec();
	let control = apng.windows(4).position(|window| window == b"acTL").unwrap();
	apng[control + 12] ^= 1;
	assert_eq!(sniff_format(&apng), Some(Format::Apng));
	assert_eq!(decode_frame(&apng, 0), Err(Error::InvalidImage));

	let mut tga = tga::encode(&pix::RgbaImage::new(1, 1, alloc::vec![1, 2, 3, 255]).unwrap(), tga::EncodeOptions { rle: false }).unwrap();
	tga[17] |= 0x40;
	assert_eq!(sniff_format(&tga), Some(Format::Tga));
	assert_eq!(decode_frame(&tga, 0), Err(Error::InvalidImage));
}

#[test]
fn converts_opaque_bmp_to_explicit_indexed_png() {
	let source = bmp::decode_rgba(include_bytes!("../../bmp/tests/data/external-rgb24.bmp")).unwrap();
	let config = parse_args(b"--quality 0 --compression 100 in.bmp out.png").unwrap();
	let (encoded, info) = convert(include_bytes!("../../bmp/tests/data/external-rgb24.bmp"), &config).unwrap();
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
	let source = png::decode_rgba(include_bytes!("../../png/tests/data/external-adam7-rgb.png")).unwrap();
	for (arguments, expected) in [
		(b"--compression 100 in.png out.ico".as_slice(), Format::Ico),
		(b"in.png out.pcx".as_slice(), Format::Pcx),
		(b"in.png out.ppm".as_slice(), Format::Ppm),
		(b"in.png out.qoi".as_slice(), Format::Qoi),
		(b"in.png out.tga".as_slice(), Format::Tga),
	] {
		let config = parse_args(arguments).unwrap();
		let converted = convert(include_bytes!("../../png/tests/data/external-adam7-rgb.png"), &config);
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
	let source = bmp::decode_rgba(include_bytes!("../../bmp/tests/data/external-rgb24.bmp")).unwrap();
	let config = parse_args(b"--quality 100 in.bmp out.jpg").unwrap();
	let (encoded, info) = convert(include_bytes!("../../bmp/tests/data/external-rgb24.bmp"), &config).unwrap();
	let decoded = jpeg::decode(&encoded).unwrap();
	assert_eq!((decoded.width, decoded.height), (source.width, source.height));
	assert_eq!(info.quality, Some(100));
}

#[test]
fn converts_png_to_lossless_webp_effort_range() {
	let source = png::decode_rgba(include_bytes!("../../png/tests/data/external-adam7-rgb.png")).unwrap();
	for compression in [0, 1, 25, 50, 75, 99, 100] {
		let arguments = alloc::format!("--lossless --compression {compression} in.png out.webp");
		let (encoded, info) = convert(include_bytes!("../../png/tests/data/external-adam7-rgb.png"), &parse_args(arguments.as_bytes()).unwrap()).unwrap();
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
	let (encoded, info) = convert(include_bytes!("../../png/tests/data/external-adam7-rgb.png"), &config).unwrap();
	let decoded = icns::decode(&encoded).unwrap();
	assert_eq!((decoded.width, decoded.height), (128, 128));
	assert_eq!(info.output_format, Format::Icns);
}

#[test]
fn converts_resized_png_to_classic_rle_icns() {
	let config = parse_args(b"--resize 32x32 --compression 100 in.png out.icns").unwrap();
	let (encoded, info) = convert(include_bytes!("../../png/tests/data/external-adam7-rgb.png"), &config).unwrap();
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

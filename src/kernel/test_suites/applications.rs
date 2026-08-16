use super::*;

tagged_test!(imgview_interactions, [Imgview, Image, Display, Input, Process, Service, Storage], id = "kernel.applications.imgview_interactions", covers = ["bin.imgview", "display-proto", "input-proto", "pix", "surface"]);
fn imgview_interactions() {
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let imgview_elf = program_elf(&package, volume, b"imgview").expect("imgview tool");
	let source = pix::RgbaImage::new(2, 2, alloc::vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]).expect("source image");
	let source_bmp = bmp::encode_rgba(&source).expect("encode source BMP");
	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);
	let media_image = fat16_image(&[(*b"SOURCE  BMP", source_bmp.as_slice())], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	run_imgview_harness_with_exit(imgview_elf, b"vol://media/SOURCE.BMP", &viewer_surface(&source), &mut system, &mut media, ImgviewExit::ZoomAndHold);
	run_imgview_harness_with_exit(imgview_elf, b"vol://media/SOURCE.BMP", &viewer_surface(&source), &mut system, &mut media, ImgviewExit::KeyEscape);
	run_imgview_harness_with_exit(imgview_elf, b"vol://media/SOURCE.BMP", &viewer_surface(&source), &mut system, &mut media, ImgviewExit::RawEscape);
}

// The licoview and licoedit harnesses that stood here are gone, and their coverage moved to
// `boot/scenarios/terminal-lifecycle.toml`, which takes all three terminal applications
// through the same life against a real terminal rather than a synthetic one. The scenario is
// exercised on every target, not only the one a persistent instance runs on:
// `just scenario-cold aarch64 boot/scenarios/terminal-lifecycle.toml` passes, and the riscv64
// form with it, so nothing narrowed when they were removed.
//
// The `lico` harness below stays, deliberately. This item asks for one focused cold end-to-end
// sample rather than every fast scenario duplicated in the kernel image, and this is it: it is
// the richest of the three, and it keeps the whole path - package, loader, services, terminal -
// covered by something that boots cold in the kernel suite and needs no development profile.

tagged_test!(lico_switches_panels_and_restores_the_terminal, [Lico, Process, Service, Storage], id = "kernel.applications.lico_switches_panels_and_restores_the_terminal", covers = ["bin.lico", "keys", "lico"]);
fn lico_switches_panels_and_restores_the_terminal() {
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let lico_elf = program_elf(&package, volume, b"lico").expect("lico tool");
	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);
	run_lico_harness(lico_elf, &mut system);
}

tagged_test!(lico_restores_the_terminal_when_it_is_interrupted, [Lico, Process, Service, Storage], id = "kernel.applications.lico_restores_the_terminal_when_it_is_interrupted", covers = ["bin.lico", "keys", "lico"]);
fn lico_restores_the_terminal_when_it_is_interrupted() {
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let lico_elf = program_elf(&package, volume, b"lico").expect("lico tool");
	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);
	run_lico_interrupt_harness(lico_elf, &mut system);
}

tagged_test!(imgconv_cross_volume_and_failed_overwrite_preserve_destination, [Image, Service, Storage, Process, Filesystem], id = "kernel.applications.imgconv_cross_volume_and_failed_overwrite_preserve_destination", covers = ["bin.imgconv", "imgconv", "storage", "volume-client"]);
fn imgconv_cross_volume_and_failed_overwrite_preserve_destination() {
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let imgconv_elf = program_elf(&package, volume, b"imgconv").expect("imgconv tool");
	let imgview_elf = program_elf(&package, volume, b"imgview").expect("imgview tool");
	let source = pix::RgbaImage::new(2, 2, alloc::vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]).expect("source image");
	let source_bmp = bmp::encode_rgba(&source).expect("encode source BMP");
	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);

	let media_image = fat16_image(&[(*b"SOURCE  BMP", source_bmp.as_slice())], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let help = run_volume_tool(imgconv_elf, b"--help", &mut system, &mut media);
	assert!(help.starts_with(b"Usage: imgconv [options] <input> <output>\n\nOptions:\n"));
	assert!(help.windows(b"WebP  options: quality compression lossless lossy animation; defaults: mode=lossless compression=100".len()).any(|window| window == b"WebP  options: quality compression lossless lossy animation; defaults: mode=lossless compression=100"));
	let line = run_volume_tool(imgconv_elf, b"--quality 100 vol://media/SOURCE.BMP vol://system/CROSS.BMP", &mut system, &mut media);
	assert!(line.starts_with(b"imgconv: BMP 2x2 -> BMP 2x2 quality=100 bytes="));
	let converted = system.open(b"vol://system/CROSS.BMP", 0xc2055).expect("cross-volume BMP opens");
	assert_eq!(bmp::decode_rgba(&converted).expect("cross-volume BMP decodes"), source);
	run_imgview_help_harness(imgview_elf, &mut system, &mut media);
	run_imgview_harness(imgview_elf, b"vol://system/CROSS.BMP", &viewer_surface(&source), &mut system, &mut media);

	let transparent_png = include_bytes!("../../user/libs/image/png/tests/data/external-rgba16.png");
	let transparent = png::decode_rgba(transparent_png).expect("decode transparent viewer fixture");
	let animation_webp = include_bytes!("../../user/libs/image/webp/tests/data/external-animation.webp");
	let viewer_image = fat16_image(&[(*b"ALPHA   PNG", transparent_png.as_slice()), (*b"ANIM    WEB", animation_webp)], false);
	let mut viewer_media = StorageHarness::start(storage_elf, b"FATBLOCK", &viewer_image, viewer_image.len() as u64);
	run_imgview_harness(imgview_elf, b"vol://media/ALPHA.PNG", &viewer_surface(&transparent), &mut system, &mut viewer_media);
	let animation_first = webp::decode(animation_webp).expect("composited WebP frame 0");
	run_imgview_harness(imgview_elf, b"vol://media/ANIM.WEB", &viewer_surface(&animation_first), &mut system, &mut viewer_media);

	let collision_pixel = pix::RgbaImage::new(1, 1, alloc::vec![17, 34, 51, 255]).expect("TGA collision pixel");
	let mut collision_tga = tga::encode(&collision_pixel, tga::EncodeOptions { rle: false }).expect("encode TGA collision");
	collision_tga[0] = 10;
	collision_tga.splice(18..18, *b"0123456789");
	let classification_image = fat16_image(&[(*b"UNKNOWN BIN", b"not an image"), (*b"BAD     PNG", b"\x89PNG\r\n\x1a\n"), (*b"COLLIDE TGA", &collision_tga)], false);
	let mut classification_media = StorageHarness::start(storage_elf, b"FATBLOCK", &classification_image, classification_image.len() as u64);
	let unknown = run_volume_tool(imgconv_elf, b"vol://media/UNKNOWN.BIN vol://media/UNKNOWN.BMP", &mut system, &mut classification_media);
	assert_eq!(unknown, b"imgconv: unsupported image format\n");
	let corrupt = run_volume_tool(imgconv_elf, b"vol://media/BAD.PNG vol://media/BAD.BMP", &mut system, &mut classification_media);
	assert_eq!(corrupt, b"imgconv: invalid or corrupt image\n");
	let collision = run_volume_tool(imgconv_elf, b"vol://media/COLLIDE.TGA vol://media/COLLIDE.BMP", &mut system, &mut classification_media);
	assert!(collision.starts_with(b"imgconv: TGA 1x1 -> BMP 1x1 bytes="));
	let collision_output = classification_media.open(b"vol://media/COLLIDE.BMP", 0xc0111de).expect("collision output opens");
	assert_eq!(bmp::decode_rgba(&collision_output).expect("collision output decodes"), collision_pixel);
	run_imgview_harness(imgview_elf, b"vol://media/COLLIDE.TGA", &viewer_surface(&collision_pixel), &mut system, &mut classification_media);

	let line = run_volume_tool(imgconv_elf, b"--lossless --compression 50 vol://media/SOURCE.BMP vol://system/CROSSL.WEBP", &mut system, &mut media);
	assert!(line.starts_with(b"imgconv: BMP 2x2 -> WebP 2x2 mode=lossless compression=50 bytes="));
	let converted = system.open(b"vol://system/CROSSL.WEBP", 0xc2057).expect("cross-volume lossless WebP opens");
	assert_eq!(webp::decode(&converted).expect("cross-volume lossless WebP decodes"), source);

	let line = run_volume_tool(imgconv_elf, b"--lossy --quality 100 --compression 100 vol://media/SOURCE.BMP vol://system/CROSS.WEBP", &mut system, &mut media);
	assert!(line.starts_with(b"imgconv: BMP 2x2 -> WebP 2x2 mode=lossy quality=100 compression=100 bytes="));
	let converted = system.open(b"vol://system/CROSS.WEBP", 0xc2056).expect("cross-volume WebP opens");
	assert_eq!(&converted[..4], b"RIFF", "lossy WebP uses the canonical RIFF container");
	assert_eq!(&converted[8..12], b"WEBP", "lossy WebP uses the canonical WEBP form type");
	assert_eq!(&converted[12..16], b"VP8 ", "opaque lossy WebP uses a simple VP8 chunk");
	let decoded = webp::decode(&converted).expect("cross-volume lossy WebP decodes");
	assert_eq!((decoded.width, decoded.height), (source.width, source.height));
	// Lossy WebP is 4:2:0, so a 2x2 image carries exactly one chroma sample: the average of all
	// four pixels. These four average to neutral grey by construction, so the colour cannot
	// survive and asserting a bounded RGB error here asked for the arithmetically impossible -
	// which is what this assertion did, and why it had never passed. libwebp encodes the same
	// input to the same greys, so the encoder is right and the assertion was wrong.
	// Luma is full resolution and is what this size can carry, so that is what is bounded.
	let luma = |pixel: &[u8]| -> i64 { (16839 * i64::from(pixel[0]) + 33059 * i64::from(pixel[1]) + 6420 * i64::from(pixel[2]) + (16 << 16) + (1 << 15)) >> 16 };
	let squared_error: u64 = decoded.pixels.chunks_exact(4).zip(source.pixels.chunks_exact(4)).map(|(actual, expected)| (luma(actual) - luma(expected)).unsigned_abs().pow(2)).sum();
	assert!(squared_error <= u64::from(source.width) * u64::from(source.height) * 64, "governed 2x2 lossy WebP exceeds its bounded luma error");

	let previous = b"previous destination";
	let full_image = fat16_image(&[(*b"SOURCE  BMP", source_bmp.as_slice()), (*b"KEEP    BMP", previous)], true);
	let mut full_media = StorageHarness::start(storage_elf, b"FATBLOCK", &full_image, full_image.len() as u64);
	let failure = run_volume_tool(imgconv_elf, b"--force --resize 64x64 vol://media/SOURCE.BMP vol://media/KEEP.BMP", &mut system, &mut full_media);
	assert_eq!(failure, b"imgconv: cannot write output\n");
	assert_eq!(full_media.open(b"vol://media/KEEP.BMP", 0xfa11), Some(previous.to_vec()), "failed overwrite preserves the previous destination byte-for-byte");
}

tagged_test!(audioconv_converts_across_volumes_and_never_writes_a_failed_conversion, [Audio, Service, Storage, Process, Filesystem], id = "kernel.applications.audioconv_converts_across_volumes_and_never_writes_a_failed_conversion", covers = ["adpcm", "aiff", "audioconv", "bin.audioconv", "flac", "pcm", "storage", "wav", "wavpack"]);
fn audioconv_converts_across_volumes_and_never_writes_a_failed_conversion() {
	// The real `audioconv.lsexe`, launched with a volume bundle and nothing else - no AudioService,
	// no device authority - converting between two StorageService volumes. What makes this worth
	// booting for is the last two cases: a conversion that cannot be written, and one that would
	// replace a destination it was not told it could. Neither may leave the destination changed.
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let audioconv_elf = program_elf(&package, volume, b"audioconv").expect("audioconv tool");

	// A quarter of a second of mono at 8 kHz, built here with the same encoder the tool uses, so
	// the fixture is a real WAV rather than bytes that happen to parse.
	let samples = governed_audio_fixture(2_000);
	let source_wav = {
		let format = pcm::Format::new(8_000, 1).expect("the fixture rate is one `Format` names");
		let mut encoder = wav::encode::Encoder::new(pcm::encode::VecSink::new(1 << 20), format, wav::encode::Output::Pcm { bits: 16 }).expect("the fixture encoder starts");
		encoder.push(&samples).expect("the fixture encodes");
		encoder.finish().expect("the fixture closes").0.into_bytes()
	};

	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);
	let media_image = fat16_image(&[(*b"SOURCE  WAV", source_wav.as_slice()), (*b"JUNK    BIN", b"not audio at all")], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);

	let help = run_volume_tool(audioconv_elf, b"--help", &mut system, &mut media);
	assert!(help.starts_with(b"usage: audioconv [options] <input> <output>\n"), "the help leads with its usage");
	for profile in [b"FLAC".as_slice(), b"AIFF".as_slice(), b"WAV-IMA".as_slice(), b"Ogg Vorbis".as_slice()] {
		assert!(help.windows(profile.len()).any(|window| window == profile), "the help omits a profile the table lists");
	}

	// Lossless, across volumes: media in, system out, and the samples must be the ones that went in.
	let line = run_volume_tool(audioconv_elf, b"vol://media/SOURCE.WAV vol://system/CROSS.FLAC", &mut system, &mut media);
	assert!(line.starts_with(b"audioconv: WAV 8000Hz/1ch/2000fr -> FLAC 8000Hz/1ch/2000fr duration=250ms bytes="), "unexpected report: {}", alloc::string::String::from_utf8_lossy(&line));
	assert!(line.ends_with(b" metadata=stripped\n"), "the report does not say what it dropped");
	let written = system.open(b"vol://system/CROSS.FLAC", 0xaad10).expect("the cross-volume FLAC opens");
	let decoded = decode_flac_samples(&written);
	assert_eq!(decoded, samples, "the lossless conversion did not survive the round trip");

	// And the other lossless container, resampled and remixed on the way, so the transforms are
	// exercised through the real tool rather than only in the library's own tests.
	let line = run_volume_tool(audioconv_elf, b"--rate 16000 --channels 2 vol://media/SOURCE.WAV vol://system/CROSS.AIFF", &mut system, &mut media);
	assert!(line.starts_with(b"audioconv: WAV 8000Hz/1ch/2000fr -> AIFF 16000Hz/2ch/4000fr duration=250ms bytes="), "unexpected report: {}", alloc::string::String::from_utf8_lossy(&line));
	let written = system.open(b"vol://system/CROSS.AIFF", 0xaad11).expect("the cross-volume AIFF opens");
	let converted = aiff::Aiff::parse(&written).expect("the AIFF parses");
	assert_eq!(converted.metadata().rate, 16_000);
	assert_eq!(converted.metadata().channels, 2);
	assert_eq!(converted.metadata().frames, 4_000);

	// The other lossless codec, and the one whose header carries no size that is only known at the
	// end - so it is the one this tool could stream to a pipe if it had one.
	let line = run_volume_tool(audioconv_elf, b"vol://media/SOURCE.WAV vol://system/CROSS.WV", &mut system, &mut media);
	assert!(line.starts_with(b"audioconv: WAV 8000Hz/1ch/2000fr -> WavPack 8000Hz/1ch/2000fr duration=250ms bytes="), "unexpected report: {}", alloc::string::String::from_utf8_lossy(&line));
	let written = system.open(b"vol://system/CROSS.WV", 0xaad16).expect("the cross-volume WavPack opens");
	assert_eq!(decode_wavpack_samples(&written), samples, "the WavPack conversion was not lossless");

	// A format nothing writes yet says so in those words, and does not create the destination.
	let unwritten = run_volume_tool(audioconv_elf, b"vol://media/SOURCE.WAV vol://system/CROSS.OGG", &mut system, &mut media);
	assert_eq!(unwritten, b"audioconv: writing that format is not implemented yet\n");
	assert_eq!(system.open(b"vol://system/CROSS.OGG", 0xaad12), None, "a refused profile still created its destination");

	// Not audio at all, and a destination whose suffix names nothing.
	let junk = run_volume_tool(audioconv_elf, b"vol://media/JUNK.BIN vol://system/JUNK.FLAC", &mut system, &mut media);
	assert_eq!(junk, b"audioconv: unsupported audio format\n");
	let nameless = run_volume_tool(audioconv_elf, b"vol://media/SOURCE.WAV vol://system/OUT.BIN", &mut system, &mut media);
	assert_eq!(nameless, b"audioconv: unsupported audio format\n");

	// An existing destination is left alone unless `--force` says otherwise.
	let refused = run_volume_tool(audioconv_elf, b"vol://media/SOURCE.WAV vol://system/CROSS.FLAC", &mut system, &mut media);
	assert_eq!(refused, b"audioconv: destination exists (use --force)\n");
	assert_eq!(system.open(b"vol://system/CROSS.FLAC", 0xaad13).map(|bytes| decode_flac_samples(&bytes)), Some(samples.clone()), "a refused overwrite changed the destination");
	let forced = run_volume_tool(audioconv_elf, b"--force --compression 100 vol://media/SOURCE.WAV vol://system/CROSS.FLAC", &mut system, &mut media);
	assert!(forced.starts_with(b"audioconv: WAV 8000Hz/1ch/2000fr -> FLAC"), "--force did not convert");
	assert_eq!(system.open(b"vol://system/CROSS.FLAC", 0xaad14).map(|bytes| decode_flac_samples(&bytes)), Some(samples.clone()), "the forced overwrite is not the same audio");

	// A destination volume with no room. The conversion succeeds and the write does not, and what
	// was already there must survive byte for byte - the whole reason the tool converts before it
	// opens the destination.
	// WavPack, because a FAT name has three characters of suffix and `.flac` needs four - the one
	// place in this scenario where the medium decides which codec the case is written in.
	let previous = b"previous destination";
	let full_image = fat16_image(&[(*b"SOURCE  WAV", source_wav.as_slice()), (*b"KEEP    WV ", previous)], true);
	let mut full_media = StorageHarness::start(storage_elf, b"FATBLOCK", &full_image, full_image.len() as u64);
	let failure = run_volume_tool(audioconv_elf, b"--force vol://media/SOURCE.WAV vol://media/KEEP.WV", &mut system, &mut full_media);
	assert_eq!(failure, b"audioconv: cannot write output\n");
	assert_eq!(full_media.open(b"vol://media/KEEP.WV", 0xaad15), Some(previous.to_vec()), "a failed write did not preserve the previous destination");
}

// A deterministic quarter-second that a predictor can actually predict: a wandering tone rather
// than noise, so a lossless conversion of it is a test of the coder and not of a memcpy.
fn governed_audio_fixture(frames: usize) -> alloc::vec::Vec<i16> {
	let mut samples = alloc::vec::Vec::with_capacity(frames);
	let mut phase = 0i64;
	let mut velocity = 900i64;
	let mut state = 0x4f1b_a7c3u32;
	for _ in 0..frames {
		state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
		velocity -= phase / 48;
		velocity += ((state >> 20) as i64 & 0x1f) - 16;
		phase = (phase + velocity).clamp(-26_000, 26_000);
		samples.push(phase as i16);
	}
	samples
}

fn decode_wavpack_samples(bytes: &[u8]) -> alloc::vec::Vec<i16> {
	let file = wavpack::WavPack::parse(bytes).expect("the written WavPack parses");
	let mut decoder = file.decoder();
	let mut samples = alloc::vec::Vec::new();
	let mut chunk = alloc::vec::Vec::new();
	while decoder.read_i16_le(512, &mut chunk).expect("the written WavPack decodes") != 0 {
		for pair in chunk.chunks_exact(2) {
			samples.push(i16::from_le_bytes([pair[0], pair[1]]));
		}
	}
	samples
}

fn decode_flac_samples(bytes: &[u8]) -> alloc::vec::Vec<i16> {
	let file = flac::Flac::parse(bytes).expect("the written FLAC parses");
	let mut decoder = file.decoder();
	let mut samples = alloc::vec::Vec::new();
	let mut chunk = alloc::vec::Vec::new();
	while decoder.read_i16_le(512, &mut chunk).expect("the written FLAC decodes") != 0 {
		for pair in chunk.chunks_exact(2) {
			samples.push(i16::from_le_bytes([pair[0], pair[1]]));
		}
	}
	samples
}

tagged_test!(imgconv_governed_working_set_is_measured, [Image, Memory, Process, Service, Storage], id = "kernel.applications.imgconv_governed_working_set_is_measured", covers = ["bin.imgconv", "imgconv", "png", "webp"]);
fn imgconv_governed_working_set_is_measured() {
	use object::domain::{Domain, UNLIMITED};
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	// Mirrors `IMGCONV_MEMORY_LIMIT` in `permission_manager.rs`, which is where the production
	// value and the reasoning behind it live. A kernel test cannot read a userspace service's
	// constant, so this is a copy - and a copy that drifts asserts against a quota nobody
	// enforces, which is why the two carry each other's name.
	const IMGCONV_MEMORY_LIMIT: u64 = 128 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let imgconv_elf = program_elf(&package, volume, b"imgconv").expect("imgconv tool");
	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);
	let source = pix::RgbaImage::new(2, 2, alloc::vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]).expect("source image");
	let source_bmp = bmp::encode_rgba(&source).expect("encode source BMP");

	let media_image = fat16_image(&[(*b"SOURCE  BMP", source_bmp.as_slice())], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let full_hd_domain = Domain::new_child(&sched::root_domain(), IMGCONV_MEMORY_LIMIT, UNLIMITED, UNLIMITED).expect("a live parent takes a child");
	let (full_hd, full_hd_peak) = run_volume_tool_in(full_hd_domain, imgconv_elf, b"--resize 1920x1080 --compression 100 vol://media/SOURCE.BMP vol://media/FHD.PNG", &mut system, &mut media);
	assert!(full_hd.starts_with(b"imgconv: BMP 2x2 -> PNG 1920x1080 compression=100 bytes="));
	let full_hd_output = media.open(b"vol://media/FHD.PNG", 0xf1080).expect("1080p output opens");
	let full_hd_image = png::decode_rgba(&full_hd_output).expect("1080p output decodes");
	assert_eq!((full_hd_image.width, full_hd_image.height), (1920, 1080));

	let media_image = fat16_image(&[(*b"SOURCE  BMP", source_bmp.as_slice())], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let ultra_hd_domain = Domain::new_child(&sched::root_domain(), IMGCONV_MEMORY_LIMIT, UNLIMITED, UNLIMITED).expect("a live parent takes a child");
	let (ultra_hd, ultra_hd_peak) = run_volume_tool_in(ultra_hd_domain, imgconv_elf, b"--resize 3840x2160 --compression 100 vol://media/SOURCE.BMP vol://media/UHD.PNG", &mut system, &mut media);
	assert!(ultra_hd.starts_with(b"imgconv: BMP 2x2 -> PNG 3840x2160 compression=100 bytes="));
	let ultra_hd_output = media.open(b"vol://media/UHD.PNG", 0xf2160).expect("4K output opens");
	let ultra_hd_image = png::decode_rgba(&ultra_hd_output).expect("4K output decodes");
	assert_eq!((ultra_hd_image.width, ultra_hd_image.height), (3840, 2160));

	let animation = include_bytes!("../../user/libs/image/webp/tests/data/external-animation.webp");
	let media_image = fat16_image(&[(*b"ANIM    WEB", animation)], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let animation_domain = Domain::new_child(&sched::root_domain(), IMGCONV_MEMORY_LIMIT, UNLIMITED, UNLIMITED).expect("a live parent takes a child");
	let (animation_line, animation_peak) = run_volume_tool_in(animation_domain, imgconv_elf, b"vol://media/ANIM.WEB vol://media/ANIM.GIF", &mut system, &mut media);
	assert!(animation_line.starts_with(b"imgconv: WebP 23x15 -> GIF 23x15 quality=100 bytes="));
	let animation_output = media.open(b"vol://media/ANIM.GIF", 0xa11).expect("animation output opens");
	let converted_animation = gif::decode(&animation_output).expect("animation output decodes");
	assert_eq!((converted_animation.width, converted_animation.height, converted_animation.frames.len()), (23, 15, 2));
	assert!(full_hd_peak > 1920 * 1080 * 4, "1080p peak includes more than the final RGBA buffer");
	assert!(ultra_hd_peak > 3840 * 2160 * 4, "4K peak includes more than the final RGBA buffer");
	assert!(ultra_hd_peak > full_hd_peak, "4K conversion has a larger whole-process peak");
	assert!(ultra_hd_peak < IMGCONV_MEMORY_LIMIT, "measured 4K conversion fits the production quota");

	let previous = b"preserved after quota failure";
	let media_image = fat16_image(&[(*b"SOURCE  BMP", source_bmp.as_slice()), (*b"KEEP    PNG", previous)], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let limited_domain = Domain::new_child(&sched::root_domain(), 80 * 1024 * 1024, UNLIMITED, UNLIMITED).expect("a live parent takes a child");
	let (failure, limited_peak) = run_volume_tool_result(limited_domain, imgconv_elf, b"--force --resize 3840x2160 --compression 100 vol://media/SOURCE.BMP vol://media/KEEP.PNG", &mut system, &mut media);
	assert_eq!(failure, Some(b"imgconv: out of memory\n".to_vec()), "quota failure reports a typed diagnostic");
	assert_eq!(media.open(b"vol://media/KEEP.PNG", 0xfa17), Some(previous.to_vec()), "quota failure preserves the previous destination byte-for-byte");
	assert!(limited_peak <= 80 * 1024 * 1024, "quota failure never exceeds its Domain limit");

	// The shipped corpus, which is what the production quota actually has to hold and what the
	// original measurement left out. Every case above upscales from a 2x2 BMP or a 23x15 WebP,
	// so the decoded INPUT costs nothing and the peak is essentially one output buffer. The
	// system volume ships one image - `wallpapers/logo.webp`, a 3840x2160 lossy WebP - and
	// converting it needs the decoded input AND the output at once, which is the term a
	// synthetic upscale can never show. `imgconv` could not convert it inside 96 MiB.
	// Measured in a deliberately generous Domain rather than in the production one, so that
	// this reports how much the conversion NEEDS instead of only that the quota refused it.
	// An `out of memory` line is the least useful possible answer to "what should the quota
	// be" - it is what the original 96 MiB produced here, and it says nothing about by how
	// much. The production statement is then a comparison against a number in hand.
	const MEASUREMENT_CEILING: u64 = 512 * 1024 * 1024;
	// 15 MB of media against a 2.2 MB output, so capacity cannot be what decides this. The
	// default 2.5 MB is close enough to the output size to be a suspect, and FAT16 caps at
	// 65,524 clusters - asking for 65,536 produces an image the driver will not accept, which
	// looks like the same `cannot write output` and is not.
	let media_image = fat16_image_with_clusters(&[], false, 30_000);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let corpus_domain = Domain::new_child(&sched::root_domain(), MEASUREMENT_CEILING, UNLIMITED, UNLIMITED).expect("a live parent takes a child");
	let (wallpaper, wallpaper_peak) = run_volume_tool_in(corpus_domain, imgconv_elf, b"vol://system/wallpapers/logo.webp vol://media/LOGO.PNG", &mut system, &mut media);
	assert!(wallpaper.starts_with(b"imgconv: WebP 3840x2160 -> PNG 3840x2160"), "the shipped wallpaper converts, got {:?}", core::str::from_utf8(&wallpaper));
	assert!(wallpaper_peak > ultra_hd_peak, "a 4K input costs more than a 4K output alone - decoded input and output are live together");
	assert!(wallpaper_peak < IMGCONV_MEMORY_LIMIT, "the shipped corpus must fit the production quota: measured {} bytes against a {} byte limit", wallpaper_peak, IMGCONV_MEMORY_LIMIT);

	serial_println!("imgconv governed memory: 1920x1080={} bytes, 3840x2160={} bytes, animation={} bytes, shipped 4K wallpaper={} bytes", full_hd_peak, ultra_hd_peak, animation_peak, wallpaper_peak);
}

tagged_test!(wasi_host_runs_a_component, [Component, Service], id = "kernel.applications.wasi_host_runs_a_component", covers = ["bin.wasi_host", "wasm"]);
fn wasi_host_runs_a_component() {
	// The wasi_host (a ring-3 process) loads an embedded Wasm component and runs it
	// on the `wasm` runtime. The component's only import, `liber.read`, is wired by
	// the host to read the one granted file (vol://system/hello.txt) through
	// StorageService into the component's linear memory - a WASI-style world: the
	// component has no other capability and can reach nothing it was not given. The
	// bytes the component read must equal the file straight from the volume, proving
	// a Wasm component performed a capability-gated operation via a host import
	// mapped to a native service.
	let (expected, status, actual) = run_wasi_scenario().expect("the wasi scenario should run");
	assert!(!expected.is_empty(), "the granted file should not be empty");
	assert_eq!(actual, expected, "the component read the granted file's bytes through the host import");
	// AND THE STATUS SAYS SO, which is the half the report used to drop. `count.max(0)` turned a
	// refusal into zero bytes, and a successful read of an empty file is also zero bytes - so the
	// supervisor could not tell "you may not" from "there was nothing there". The status is the
	// byte count on success and a negative status on refusal, and the two are now different
	// messages rather than the same empty one.
	assert_eq!(status, expected.len() as i32, "the report's status is the count the component read, not a sign the payload has to be guessed from");
}

tagged_test!(the_hosts_report_tells_an_empty_file_from_a_refusal, [Component, Service], id = "kernel.applications.the_hosts_report_tells_an_empty_file_from_a_refusal", covers = ["bin.wasi_host", "wasm"]);
fn the_hosts_report_tells_an_empty_file_from_a_refusal() {
	// THE TWO SHAPES `wasi_host_runs_a_component` CANNOT REACH, and the two the status word exists
	// for. That test runs against the real system volume, where the granted file exists and is not
	// empty - one of three answers the protocol can give, and the only one covered. The other two
	// produce an IDENTICAL payload (none) and are distinguished only by the status: a refusal is
	// negative, and a successful read of an empty file is zero.
	//
	// Before the status existed the host sent `count.max(0)` bytes and nothing else, so a
	// supervisor received an empty message either way and could not tell "you may not" from "there
	// was nothing there". Four rounds of work went into separating those two answers and the last
	// hop undid it; these are the two cases that say it stays undone.
	//
	// The volumes are built here rather than staged, through the archive writer `abi` already
	// carries - so this needs no build change and cannot drift from what the system volume happens
	// to contain.

	// An empty granted file: status ZERO, empty payload, and NOT an error.
	let empty = run_wasi_scenario_over(&[(b"hello.txt", b"")]).expect("the scenario runs against an empty granted file");
	assert_eq!(empty.status, 0, "an empty file read successfully is a count of zero, not a refusal");
	assert!(empty.payload.is_empty(), "and there is nothing after the status");

	// The granted file ABSENT: the volume answers and says no, which reaches the guest as a
	// negative status and reaches the supervisor as the same negative status.
	let missing = run_wasi_scenario_over(&[(b"other.txt", b"something else")]).expect("the scenario runs against a volume without the granted file");
	assert!(missing.status < 0, "a granted file that is not there is a refusal, not a zero-byte read (status {})", missing.status);
	assert!(missing.payload.is_empty(), "and a refusal carries no payload");

	// AND THE TWO ARE DIFFERENT ANSWERS, which is the entire point and the thing the old report
	// could not express.
	assert_ne!(empty.status, missing.status, "an empty file and a refusal must not arrive as the same report");
}

tagged_test!(powerbox_grants_a_picked_file_to_a_component, [Component, Service], id = "kernel.applications.powerbox_grants_a_picked_file_to_a_component", covers = ["kernel", "services"]);
fn powerbox_grants_a_picked_file_to_a_component() {
	// A Wasm component with NO filesystem access of its own runs under wasi_host,
	// which holds only a FilePicker client. The component's read import goes through
	// the picker, which (standing in for the user's choice) opens the chosen file
	// over StorageService and hands back exactly that file as a handle<file>
	// capability; the host reads it into the component's memory. The bytes must equal
	// the picked file straight from the volume - the component gained access to
	// exactly one user-picked file, and to nothing else (the powerbox pattern).
	let (expected, actual) = run_powerbox_scenario().expect("the powerbox scenario should run");
	assert!(!expected.is_empty(), "the picked file should not be empty");
	assert_eq!(actual, expected, "the component read the user-picked file through the picker");
}

tagged_test!(permission_manager_enforces_static_and_dynamic_probe_policy, [Service, Process, PermissionService], id = "kernel.applications.permission_manager_enforces_static_and_dynamic_probe_policy", covers = ["kernel", "services"]);
fn permission_manager_enforces_static_and_dynamic_probe_policy() {
	let result = run_permission_scenario(PermissionScenario::Probes).expect("the permission probe scenario should run");
	assert!(!result.expected.is_empty(), "the granted file should not be empty");
	assert_eq!(result.probe_read, result.expected, "the sandboxed component read its one granted file through the storage grant");
	assert_eq!(result.probe_summary.as_slice(), b"storage=grant log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny app-assets=deny", "sandbox_probe was granted exactly its manifest - storage and log - and denied every other capability in the vocabulary");
	assert_eq!(result.request_read.as_slice(), b"storage denied", "request_probe's undeclared storage request was refused by the headless policy default");
	assert_eq!(result.request_summary.as_slice(), b"storage=deny log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny app-assets=deny storage=deny(dynamic)", "request_probe's static grants and dynamic denial were recorded independently");
}

tagged_test!(permission_manager_runs_tools_with_minimal_grants, [Service, Process, PermissionService], id = "kernel.applications.permission_manager_runs_tools_with_minimal_grants", covers = ["kernel", "services"]);
fn permission_manager_runs_tools_with_minimal_grants() {
	let result = run_permission_scenario(PermissionScenario::GovernedTools).expect("the governed tool scenario should run");
	assert_eq!(result.date_read.len(), 21, "date rendered a 20-byte ISO-8601 UTC instant and newline");
	assert_eq!(result.date_read[4], b'-', "date separates the year and month");
	assert_eq!(result.date_read[7], b'-', "date separates the month and day");
	assert_eq!(result.date_read[10], b'T', "date separates the date and time");
	assert_eq!(result.date_read[13], b':', "date separates the hour and minute");
	assert_eq!(result.date_read[16], b':', "date separates the minute and second");
	assert_eq!(result.date_read[19], b'Z', "date reports UTC");
	assert_eq!(result.date_read[20], b'\n', "date ended its stdout line");
	assert_eq!(result.date_summary.as_slice(), b"storage=deny log=deny network=deny device=deny config=deny time=grant audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny app-assets=deny", "date received only its time grant");
	assert_eq!(result.cat_read, result.expected, "cat printed its file through the storage grant");
	assert_eq!(result.ip_read.as_slice(), b"net0: 10.0.2.15  mac 52:54:00:12:34:56  mtu 1500  gateway 10.0.2.2\n", "ip rendered state from its typed NetworkService grant");
	assert_eq!(result.ip_summary.as_slice(), b"storage=deny log=deny network=grant device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny app-assets=deny", "ip received only its network grant");
}

tagged_test!(permission_manager_mints_scoped_application_grants, [Service, Process, PermissionService], id = "kernel.applications.permission_manager_mints_scoped_application_grants", covers = ["kernel", "services"]);
fn permission_manager_mints_scoped_application_grants() {
	let result = run_permission_scenario(PermissionScenario::ScopedGrants).expect("the scoped application grant scenario should run");
	assert_eq!(result.graphics_read.as_slice(), b"graphics grants\n", "the graphics probe received process-bound display, key-only input and playback-only audio grants");
	assert!(result.graphics_start_ns != 0, "the governed app cold-start path is measured");
}

tagged_test!(component_host_runs_an_sdk_component, [Component, Service, Slow], id = "kernel.applications.component_host_runs_an_sdk_component", covers = ["bin.component_host", "liber_component", "wasm"]);
fn component_host_runs_an_sdk_component() {
	// component_host (a ring-3 process) loads a real Wasm component - built by the Rust
	// SDK and served from storage as vol://system/components/liber_component/app.wasm, not embedded in the kernel
	// image - and runs it. Its three imports are resolved by name and wired to two
	// typed services with no ambient authority: `read` / `write` to StorageService,
	// `log` to LogService. The component reads its one granted file, upper-cases it,
	// logs the result through LogService, writes it back, and returns the count; the
	// host also calls the component's float `score` export. The bytes the component
	// produced must equal the upper-cased granted file (a real SDK component performed a
	// capability-gated filesystem read and transformed it on the interpreter), the log
	// grant must have been reached (the second typed service was wired - no ambient
	// authority), and score(10) must be 17 (the float path on genuine toolchain output).
	let run = run_component_scenario().expect("the component scenario should run");
	assert!(!run.expected.is_empty(), "the granted file should not be empty");
	assert_eq!(run.report.output, run.expected, "the component read, transformed, and returned its granted file's bytes through the host imports");

	// AND THE WRITE IS REFUSED HERE, which is what this scenario's volume can honestly say.
	//
	// This test asserted that the component's bytes had been WRITTEN, and it could not have failed:
	// it compared the copy the host took on the way INTO the write import, which never goes near a
	// filesystem. Reading the file back afterwards - the only thing that proves a write - showed
	// the truth immediately: this scenario hands StorageService a `RAMDISK`, which mounts as
	// `ArchiveFs` over a mapped PKGARCH1 image, and an archive is read-only. Nothing was ever
	// written, in any run, since the test was written.
	//
	// So this is the READ-ONLY GRANT case, and it is worth having as one: the volume refuses,
	// `write_file` answers `None`, the host maps that to `STATUS_DENIED`, the guest reports it, and
	// the count comes back through the export's return value. That is the whole error model end to
	// end, on the path where "the write was empty" and "you are not allowed to write" used to be
	// the same answer.
	//
	// The POSITIVE case is `kernel.volume_layout.fresh_seeded_system_volume_...`, which runs the
	// same component against a real seeded LiberFS volume and reads `out.txt` back off it.
	assert!(run.report.readback.is_empty(), "a read-only grant persists nothing, so there is nothing to read back");
	assert_eq!(run.report.count, -1, "the component reported STATUS_DENIED: its write was refused rather than silently lost");
	assert!(run.report.logged, "the component reached its LogService grant - the second typed service was wired with no ambient authority");
	assert_eq!(run.report.score, 17, "the component's float `score` export computed trunc(10 * 1.5 + 2.0) on real toolchain output");
	// The case where truncation and flooring disagree: -3 * 1.5 + 2.0 is -2.5, which truncates to -2
	// and floors to -3. `score(10)` is 17 either way, so it could never have told them apart - and
	// the export's comment said `floor` while the cast truncated, which is how that stayed unnoticed.
	assert_eq!(run.report.score_negative, -2, "the float-to-int conversion rounds toward zero, as the cast does");
}

tagged_test!(a_redirection_is_a_governed_pipeline_stage_and_the_consumer_holds_no_storage, [Service, Process, PermissionService], id = "kernel.applications.a_redirection_is_a_governed_pipeline_stage_and_the_consumer_holds_no_storage", covers = ["kernel", "services"]);
fn a_redirection_is_a_governed_pipeline_stage_and_the_consumer_holds_no_storage() {
	// `cat < hello.txt` expands to `redirect_in hello.txt | cat` before anything is launched, and
	// this drives the expansion's product: `redirect_in hello.txt | readln`.
	//
	// WHAT IT PROVES IS THE DESIGN RATHER THAN THE FEATURE. `redirect_in` was authorized against its
	// own manifest, opened the file with the `volumes` grant that manifest gives it, and pushed the
	// bytes down an edge the broker allocated. The consumer holds no storage capability of any kind
	// - `readln`'s manifest grants nothing at all - so bytes reaching it can only have come through
	// the pipe. A redirection implemented inside the shell would have had to hand the child a file
	// capability or pump it from a process that has one; this is the reason it is not.
	let result = run_permission_scenario(PermissionScenario::GovernedTools).expect("the governed tool scenario should run");
	assert!(!result.redirect_read.is_empty(), "the redirected pipeline produced output at all");
	// `readln` prefixes each line it reads with `in> `, which is what tells the file's bytes
	// arriving through the pipe from bytes reaching the terminal some other way. Asserted as "the
	// prefix immediately precedes content" rather than as an exact buffer, for the reason the
	// pipeline test beside this one gives: a producer is entitled to split its writes.
	assert!(result.redirect_read.windows(4).any(|window| window == b"in> "), "readln read what redirect_in sent and echoed it behind its own prefix, got {:?}", core::str::from_utf8(&result.redirect_read));
	// And what came through is the FILE, not a diagnostic: `hello.txt` is the volume's seeded
	// greeting, which the storage tests read through the service, so its own text is what to look
	// for rather than anything this test could have produced.
	assert!(result.redirect_read.windows(8).any(|window| window == b"Hello fr"), "the bytes are the file's, so a redirect_in that failed to open it cannot pass this: {:?}", core::str::from_utf8(&result.redirect_read));
}

// `2>&1` IS THE SAME PIPELINE WITH ONE FLAG, AND THE DIAGNOSTIC CHANGES SIDES.
//
// `a_failing_stage_reports_to_the_terminal_and_not_into_the_pipe` proves `cat ::not-a-path | readln`
// keeps the error off the stream. This is that request byte for byte with `merge-errors` set on the
// producer, and the assertion is the opposite one: the sentence now arrives at the consumer, behind
// `readln`'s `in> ` prefix. A pair, because either half alone would pass with the routing stuck.
//
// THE FLAG EXISTS BECAUSE THE SHELL CANNOT NAME THE ANSWER. `2>&1` asks for "wherever my output
// goes", and for a non-final stage that is an edge the broker allocates inside this transaction.
// The shell has no handle to it and never will - so unlike `<` and `>`, which become programs
// holding their own grant, this one has to be a request the broker interprets.
tagged_test!(a_migrated_stream_tool_reads_a_pipeline_the_way_it_reads_a_path, [Service, Process, PermissionService], id = "kernel.applications.a_migrated_stream_tool_reads_a_pipeline_the_way_it_reads_a_path", covers = ["kernel", "services"]);
fn a_migrated_stream_tool_reads_a_pipeline_the_way_it_reads_a_path() {
	// `redirect_in motd.txt | wc`, where `wc` was given NO PATH AND NO VOLUME ARGUMENT.
	//
	// Before the migration that line was a usage error: every one of these tools refused when it
	// got no path, because a path was the only input it had. What makes this pass is `Source`
	// answering "stdin" when there is a stdin - and the only thing that tells it so is the presence
	// of the endpoint, which is a capability the launch either carried or did not.
	let result = run_permission_scenario(PermissionScenario::GovernedTools).expect("the governed tool scenario should run");
	let counted: &[u8] = &result.stream_reads[2];
	assert!(!counted.is_empty(), "the counting pipeline produced output at all");
	// `motd.txt` is two lines, and `wc` prints the line count first. A `wc` that read nothing
	// prints a leading zero and one that lost a window prints a one, so the digit is the assertion.
	assert!(counted.starts_with(b"2 "), "wc counted the whole stream: {:?}", core::str::from_utf8(counted));
}

tagged_test!(a_consumer_that_stops_early_ends_the_pipeline_instead_of_hanging_it, [Service, Process, PermissionService], id = "kernel.applications.a_consumer_that_stops_early_ends_the_pipeline_instead_of_hanging_it", covers = ["kernel", "services"]);
fn a_consumer_that_stops_early_ends_the_pipeline_instead_of_hanging_it() {
	// `redirect_in motd.txt | head -n 1` - the broken-pipe case, and the thing it really pins is
	// that the run ENDS. `head` takes its line and drops its source, which closes the read end;
	// the producer discovers it at its next write and stops. Nothing in `head` asks for that - it
	// falls out of owning the endpoint - and if it did not work the producer would sit blocked on
	// a consumer that is gone and this scenario would never return.
	let result = run_permission_scenario(PermissionScenario::GovernedTools).expect("the governed tool scenario should run");
	let taken: &[u8] = &result.stream_reads[1];
	assert!(taken.windows(5).any(|window| window == b"MOTD:"), "head printed the first line: {:?}", core::str::from_utf8(taken));
	// AND NOT THE SECOND. A `head` that ignored its count would print the whole file, which on a
	// two-line input is the difference between a limit that works and one that is never reached.
	assert!(!taken.windows(5).any(|window| window == b"Files"), "head stopped at one line: {:?}", core::str::from_utf8(taken));
}

tagged_test!(a_fan_out_stage_with_an_unwritable_destination_still_carries_the_stream, [Service, Process, PermissionService], id = "kernel.applications.a_fan_out_stage_with_an_unwritable_destination_still_carries_the_stream", covers = ["kernel", "services"]);
fn a_fan_out_stage_with_an_unwritable_destination_still_carries_the_stream() {
	// `redirect_in motd.txt | tee teed.txt | wc` on a volume that is a READ-ONLY ARCHIVE.
	//
	// So the destination cannot be opened, which is exactly the case `tee` documents a decision
	// for: a destination that fails is named on stderr and abandoned, and the stream the rest of
	// the line is built on carries on. The alternative - a failed destination ending the pipeline -
	// would mean `cmd | tee log | grep x` silently became a different command whenever the log
	// could not be written.
	//
	// Three stages, so it is also the multi-stage transaction: two edges allocated in one request,
	// and the middle stage both reading and writing.
	let result = run_permission_scenario(PermissionScenario::GovernedTools).expect("the governed tool scenario should run");
	let counted: &[u8] = &result.stream_reads[0];
	// The destination really was refused - otherwise this test would be measuring the ordinary
	// three-stage case and calling it the failure case. The diagnostic shares the channel because
	// a stage's stderr is a duplicate of the caller's terminal, which is this test's channel.
	assert!(counted.windows(23).any(|window| window == b"cannot open for writing"), "tee's destination was refused: {:?}", core::str::from_utf8(counted));
	// AND THE STREAM STILL ARRIVED. `motd.txt` is two lines of 83 bytes, so this is the whole file
	// through three stages with the middle one's destination unusable.
	assert!(counted.windows(10).any(|window| window == b"2 13 83 83"), "the bytes reached the far end anyway: {:?}", core::str::from_utf8(counted));
}

tagged_test!(a_typed_line_goes_through_the_real_shell_and_comes_back_as_a_pipeline, [Service, Process, PermissionService, Shell], id = "kernel.applications.a_typed_line_goes_through_the_real_shell_and_comes_back_as_a_pipeline", covers = ["kernel", "services"]);
fn a_typed_line_goes_through_the_real_shell_and_comes_back_as_a_pipeline() {
	// THE LAYER EVERY OTHER TEST HERE SKIPS. The pipeline tests beside this one are hand-written
	// requests to PermissionManager, which is the transaction and not the shell: the parse, the
	// redirection expansion, the launch and the status report are the half above it - and that is
	// where this milestone's defects were. A refused line ran as an ordinary command; a redirection
	// target that expanded to nothing became a stage with no argument; a foreground pipeline was
	// relayed rather than made a job. None of those are reachable from a broker request.
	//
	// So this spawns the real `shell` binary against the services already running in the scenario,
	// types `redirect_in motd.txt | wc` at its console as one line, and reads what comes back.
	// `motd.txt` is two lines of 83 bytes, so a leading count of 2 is the whole chain working:
	// lexed, expanded into stages, launched as one transaction through the broker, the bytes
	// carried down an edge, and the last stage's output printed on the terminal the shell was
	// given.
	let result = run_permission_scenario(PermissionScenario::GovernedTools).expect("the governed tool scenario should run");
	let out: &[u8] = &result.shell_read;
	let says = |needle: &[u8]| out.windows(needle.len()).any(|window| window == needle);
	assert!(!out.is_empty(), "the shell answered a typed line at all");
	assert!(!out.starts_with(b"<the shell"), "the shell started: {:?}", core::str::from_utf8(out));
	// `redirect_in motd.txt | wc`: two lines of 83 bytes, so this one string is the whole chain -
	// lexed, expanded into stages, launched as one transaction, carried down an edge, printed on
	// the terminal the shell was given.
	assert!(says(b"2 13 83 83"), "a two-stage pipeline ran: {:?}", core::str::from_utf8(out));
	// `cat ::not-a-path 2>&1 | wc`: the producer fails and its diagnostic follows its OUTPUT rather
	// than going to the terminal, so `wc` counts it - a non-zero count where the failing-stage test
	// beside this one gets zero.
	assert!(says(b"1 4 44 44"), "2>&1 folded the diagnostic into the stream: {:?}", core::str::from_utf8(out));
	// THE THREE REFUSALS, each of which used to be RUN as an ordinary command with its operator as
	// a literal argument. Matched on a distinctive phrase of each sentence rather than the whole,
	// so rewording the message does not break the test while dropping it does.
	assert!(says(b"only descriptors 1 and 2"), "`3>&1` is refused by name: {:?}", core::str::from_utf8(out));
	assert!(says(b"change THIS shell"), "a state-mutating builtin in a pipeline is refused: {:?}", core::str::from_utf8(out));
	assert!(says(b"expanded to nothing"), "a redirection target that vanished is refused: {:?}", core::str::from_utf8(out));
	// `cat < motd.txt > copied.txt` on a READ-ONLY archive: the destination cannot be opened, and
	// what this pins is that the refusal reaches the person rather than the pipe.
	assert!(says(b"cannot open for writing"), "an unwritable destination is reported: {:?}", core::str::from_utf8(out));
	// `redirect_in motd.txt | tee teed.txt | wc` TYPED AT THE PROMPT: a three-stage line whose
	// middle stage is the tool P02M0101 asks for, on a volume where its destination cannot be
	// opened. Both halves of `tee`'s documented policy are visible in one row - the destination is
	// named as refused, and the stream carries on to the far end.
	assert!(says(b"2 13 83 83"), "tee passed the stream on through a typed line: {:?}", core::str::from_utf8(out));
}

tagged_test!(a_command_word_on_its_own_runs_the_command, [Service, Process, PermissionService, Shell], id = "kernel.applications.a_command_word_on_its_own_runs_the_command", covers = ["kernel", "services"]);
fn a_command_word_on_its_own_runs_the_command() {
	// REPORTED FROM THE OUTSIDE: `lico` said it was not a command. It is in the tool table, in the
	// help table and in completion, and typing its name got "unknown command" - because the
	// argument-taking shapes matched a command word FOLLOWED BY A SPACE and nothing else, so the
	// name alone matched no shape and fell through to the error at the bottom.
	//
	// It is worst for a tool whose argument is optional, which is exactly the reported one:
	// `lico [DIRECTORY]` opens the working directory when given none, so the ordinary way to start
	// the file manager was the one spelling that did not work. The interactive shapes cannot be
	// typed here - they take the terminal, and this would be reading their redraws - so this drives
	// the same matcher through `which`, whose bare form prints its own usage.
	let result = run_permission_scenario(PermissionScenario::GovernedTools).expect("the governed tool scenario should run");
	let out: &[u8] = &result.shell_read;
	let says = |needle: &[u8]| out.windows(needle.len()).any(|window| window == needle);
	assert!(!says(b"unknown command: which"), "a command word on its own is not an unknown command: {:?}", core::str::from_utf8(out));
	// AND IT RAN, which the absence of an error does not show on its own - a line silently dropped
	// looks identical. This is the tool's own output, so it was launched.
	assert!(says(b"which: usage:"), "the bare command word launched the tool, which answered for itself: {:?}", core::str::from_utf8(out));
}

tagged_test!(merging_the_error_stream_sends_a_stages_diagnostics_down_its_own_edge, [Service, Process, PermissionService], id = "kernel.applications.merging_the_error_stream_sends_a_stages_diagnostics_down_its_own_edge", covers = ["kernel", "services"]);
fn merging_the_error_stream_sends_a_stages_diagnostics_down_its_own_edge() {
	let result = run_permission_scenario(PermissionScenario::GovernedTools).expect("the governed tool scenario should run");
	assert!(result.merged_read.windows(4).any(|window| window == b"in> "), "the consumer read something: `readln` only prints its prefix for a line it took off its input, got {:?}", core::str::from_utf8(&result.merged_read));
	assert!(result.merged_read.windows(4).any(|window| window == b"cat:"), "and what it read is the PRODUCER'S DIAGNOSTIC, which without the flag goes to the terminal instead: {:?}", core::str::from_utf8(&result.merged_read));
}

tagged_test!(a_governed_pipeline_starts_as_one_transaction_and_carries_data, [Service, Process, PermissionService], id = "kernel.applications.a_governed_pipeline_starts_as_one_transaction_and_carries_data", covers = ["kernel", "services"]);
fn a_governed_pipeline_starts_as_one_transaction_and_carries_data() {
	// `echo hello | readln` through PermissionManager: two stages, each authorized against
	// its own manifest, the edge between them allocated by the broker, and both released
	// together. `readln` prefixes what it reads with `in> `, so this distinguishes a consumer
	// that actually read its producer's bytes from a producer whose output merely reached the
	// terminal - which is what a pipeline that was never really wired would look like.
	let result = run_permission_scenario(PermissionScenario::GovernedTools).expect("the governed tool scenario should run");
	assert!(result.pipeline_started, "the broker started the two-stage pipeline");
	// Asserted as "the prefix immediately precedes the payload" rather than as an exact
	// buffer. `echo` writes its text and its newline as separate messages, so `readln` sees
	// two lines and prefixes both; pinning the whole byte sequence would tie this test to how
	// a producer happens to split its writes, which is not what a pipeline promises.
	assert!(result.pipeline_read.windows(9).any(|window| window == b"in> hello"), "readln read echo's bytes through the broker-allocated edge and echoed them behind its own prefix, got {:?}", core::str::from_utf8(&result.pipeline_read));

	// A failing PRODUCER reports to the terminal, and the pipe carries nothing.
	//
	// Every stage but the last writes into an edge, so before the error endpoint existed a stage's
	// diagnostic went to stdout - the edge - and the consumer read it as input. The two assertions
	// are the whole distinction: the message arrives, and it does NOT arrive relayed.
	assert!(result.diagnostic_read.windows(12).any(|window| window == b"cannot open\n"), "a failing stage's diagnostic reaches the terminal, got {:?}", core::str::from_utf8(&result.diagnostic_read));
	assert!(!result.diagnostic_read.windows(8).any(|window| window == b"in> cat:"), "and it went to the terminal rather than down the pipe, where the consumer would have echoed it: {:?}", core::str::from_utf8(&result.diagnostic_read));
}

tagged_test!(a_volume_stats_renames_truncates_and_touches, [Service, Storage, Filesystem], id = "kernel.applications.a_volume_stats_renames_truncates_and_touches", covers = ["libermemfs", "storage"]);
fn a_volume_stats_renames_truncates_and_touches() {
	// The four path verbs P02M0101 adds to the volume contract, over a memory volume that
	// implements all four. Every one of them answered "not implemented" before this milestone, and
	// every caller that wanted a file's size read its whole parent directory to find out.
	let (_volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let mut vol = StorageHarness::start_memory(storage_elf, b"TMPVOL", 65536);

	assert!(vol.write(b"vol://tmp/report", b"0123456789", 0x8101), "seed a file");
	// ONE FILE'S FACTS, without listing its parent.
	assert_eq!(vol.stat(b"vol://tmp/report", 0x8102), Some((10, false)), "stat answers the file's size and that it is not a directory");
	assert_eq!(vol.stat(b"vol://tmp/missing", 0x8103), None, "and a path that is not there is a failure rather than a zero-length file");

	// TRUNCATE both ways: shorter drops the tail, longer zero-extends - and the zeros are the
	// promise, so the extension is read back rather than assumed.
	assert!(vol.truncate(b"vol://tmp/report", 4, 0x8104), "shrinking is accepted");
	assert_eq!(vol.open(b"vol://tmp/report", 0x8105), Some(b"0123".to_vec()), "and the tail is gone");
	assert!(vol.truncate(b"vol://tmp/report", 8, 0x8106), "growing is accepted");
	assert_eq!(vol.open(b"vol://tmp/report", 0x8107), Some(b"0123\0\0\0\0".to_vec()), "and the file grew with zeros, not with whatever memory held");

	// RENAME moves the entry and refuses to destroy: `to` existing is the caller's decision to
	// make with `remove`, not something a rename does quietly.
	assert!(vol.rename(b"vol://tmp/report", b"vol://tmp/final", 0x8108), "a rename within one volume is accepted");
	assert_eq!(vol.stat(b"vol://tmp/report", 0x8109), None, "the old name is gone");
	assert_eq!(vol.open(b"vol://tmp/final", 0x810a), Some(b"0123\0\0\0\0".to_vec()), "and the new name holds the bytes, which were never copied");
	assert!(vol.write(b"vol://tmp/occupied", b"mine", 0x810b), "seed an occupied destination");
	assert!(!vol.rename(b"vol://tmp/final", b"vol://tmp/occupied", 0x810c), "a rename over an existing file is refused");
	assert_eq!(vol.open(b"vol://tmp/occupied", 0x810d), Some(b"mine".to_vec()), "and the file that was in the way is untouched");

	// TOUCH creates only when asked to. The two callers want different things and the flag is how
	// they say which; a silent creation is the failure this refuses.
	assert!(!vol.touch(b"vol://tmp/absent", false, 0x810e), "touch without create over a missing file is a failure");
	assert_eq!(vol.stat(b"vol://tmp/absent", 0x810f), None, "and it created nothing");
	assert!(vol.touch(b"vol://tmp/absent", true, 0x8110), "touch with create makes the file");
	assert_eq!(vol.stat(b"vol://tmp/absent", 0x8111), Some((0, false)), "empty, and it is there");
	assert!(vol.touch(b"vol://tmp/final", true, 0x8112), "touch over an existing file is accepted");
	assert_eq!(vol.open(b"vol://tmp/final", 0x8113), Some(b"0123\0\0\0\0".to_vec()), "and changes not one byte of it");
}

tagged_test!(a_volume_reads_windows_watches_changes_and_publishes_transactions, [Service, Storage, Filesystem], id = "kernel.applications.a_volume_reads_windows_watches_changes_and_publishes_transactions", covers = ["libermemfs", "storage"]);
fn a_volume_reads_windows_watches_changes_and_publishes_transactions() {
	use object::channel::Message;
	let (_volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let mut vol = StorageHarness::start_memory(storage_elf, b"TMPVOL", 65536);
	assert!(vol.write(b"vol://tmp/log", b"abcdefghij", 0x8201), "seed a file");

	// A WINDOW, not the whole file. `open` maps everything, which is no shape at all for a file
	// that does not fit; this is the call `head`, `tail` and `hexdump` are written against.
	assert_eq!(vol.read_window(b"vol://tmp/log", 0, 4, 0x8202), Some(b"abcd".to_vec()), "the first window");
	assert_eq!(vol.read_window(b"vol://tmp/log", 4, 4, 0x8203), Some(b"efgh".to_vec()), "a window from the middle");
	// A SHORT ANSWER IS THE END, not a failure: a reader that loops on this call learns where the
	// file ends from the length it got back rather than by asking a second question.
	assert_eq!(vol.read_window(b"vol://tmp/log", 8, 100, 0x8204), Some(b"ij".to_vec()), "a window that runs past the end delivers what is there");
	assert_eq!(vol.read_window(b"vol://tmp/log", 10, 4, 0x8205), Some(alloc::vec::Vec::new()), "and a window at the end delivers nothing, successfully");
	assert_eq!(vol.read_window(b"vol://tmp/nothing", 0, 4, 0x8206), None, "a path that is not there is still a failure");

	// WATCH: changes that pass through this service, as they happen.
	let events = vol.watch(b"vol://tmp/log", 0x8207).expect("a watch on an existing file is accepted");
	assert!(vol.watch(b"vol://tmp/never", 0x8208).is_none(), "a watch on a path that is not there is refused rather than held forever");
	assert!(vol.write(b"vol://tmp/log", b"changed", 0x8209), "change the watched file");
	for _ in 0..64 {
		vol.pump();
	}
	let event = events.recv().expect("the watcher was told about the change");
	// [seq u32][path lp][kind u8][size u64]
	let path_len = le_u16(&event.bytes, 4) as usize;
	assert_eq!(&event.bytes[6..6 + path_len], b"vol://tmp/log", "the event names the path that changed");
	assert_eq!(event.bytes.get(6 + path_len), Some(&1), "and says it was modified rather than created");
	// A watcher of one file hears nothing about another, which is what makes the stream bounded by
	// what the client asked for rather than by how busy the volume is.
	assert!(vol.write(b"vol://tmp/unrelated", b"other", 0x820a), "change an unwatched file");
	for _ in 0..64 {
		vol.pump();
	}
	assert!(events.recv().is_err(), "a watcher of one path is not told about another");

	// THE TRANSACTIONAL WRITER: nothing is visible until commit, which is what makes a safe save
	// safe and what every client that patches a header needs.
	let session = vol.open_writer(b"vol://tmp/doc", false, 0x8301).expect("a writer session opens");
	let mut write = alloc::vec::Vec::new();
	write.extend_from_slice(&1u16.to_le_bytes());
	write.extend_from_slice(&0x8302u32.to_le_bytes());
	write.extend_from_slice(&5u16.to_le_bytes());
	write.extend_from_slice(b"HDR..");
	assert!(vol.writer_op(&session, write, 0x8302).is_some(), "the session takes bytes");
	assert_eq!(vol.stat(b"vol://tmp/doc", 0x8303), None, "and NOTHING is published while the session is open");
	// A positioned write patches what was staged - the audio-header case, done without rewriting
	// the file or inventing a temporary name.
	let mut patch = alloc::vec::Vec::new();
	patch.extend_from_slice(&2u16.to_le_bytes());
	patch.extend_from_slice(&0x8304u32.to_le_bytes());
	patch.extend_from_slice(&3u64.to_le_bytes());
	patch.extend_from_slice(&2u16.to_le_bytes());
	patch.extend_from_slice(b"ok");
	assert!(vol.writer_op(&session, patch, 0x8304).is_some(), "a positioned write patches the staged bytes");
	let mut commit = alloc::vec::Vec::new();
	commit.extend_from_slice(&5u16.to_le_bytes());
	commit.extend_from_slice(&0x8305u32.to_le_bytes());
	assert!(vol.writer_op(&session, commit, 0x8305).is_some(), "the commit publishes");
	assert_eq!(vol.open(b"vol://tmp/doc", 0x8306), Some(b"HDRok".to_vec()), "and the published file is what the session staged, patch and all");

	// A SESSION THAT NEVER COMMITS LEAVES THE FILE EXACTLY AS IT WAS. This is the property the
	// transaction exists for: a client that dies half way through a save does not truncate the
	// file it was saving.
	assert!(vol.write(b"vol://tmp/keep", b"original", 0x8401), "seed a destination");
	let abandoned = vol.open_writer(b"vol://tmp/keep", false, 0x8402).expect("a second session opens");
	let mut clobber = alloc::vec::Vec::new();
	clobber.extend_from_slice(&1u16.to_le_bytes());
	clobber.extend_from_slice(&0x8403u32.to_le_bytes());
	clobber.extend_from_slice(&7u16.to_le_bytes());
	clobber.extend_from_slice(b"clobber");
	assert!(vol.writer_op(&abandoned, clobber, 0x8403).is_some(), "it takes bytes");
	// One session per path, because two publishing in an order neither chose means the loser's
	// work disappears at the moment it reported success.
	assert!(vol.open_writer(b"vol://tmp/keep", false, 0x8404).is_none(), "a second session over the same path is refused while the first is open");
	core::mem::drop(abandoned);
	for _ in 0..64 {
		vol.pump();
	}
	assert_eq!(vol.open(b"vol://tmp/keep", 0x8405), Some(b"original".to_vec()), "and closing the session without committing left the file as it was");

	// APPEND stages what the file already holds, so the session extends rather than replaces.
	let appender = vol.open_writer(b"vol://tmp/keep", true, 0x8406).expect("an append session opens");
	let mut more = alloc::vec::Vec::new();
	more.extend_from_slice(&1u16.to_le_bytes());
	more.extend_from_slice(&0x8407u32.to_le_bytes());
	more.extend_from_slice(&5u16.to_le_bytes());
	more.extend_from_slice(b" more");
	assert!(vol.writer_op(&appender, more, 0x8407).is_some(), "the append session takes bytes");
	let mut finish = alloc::vec::Vec::new();
	finish.extend_from_slice(&5u16.to_le_bytes());
	finish.extend_from_slice(&0x8408u32.to_le_bytes());
	assert!(vol.writer_op(&appender, finish, 0x8408).is_some(), "and commits");
	assert_eq!(vol.open(b"vol://tmp/keep", 0x8409), Some(b"original more".to_vec()), "which extended the file rather than replacing it");
	let _ = Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new(), 0);
}

tagged_test!(the_command_tools_run_governed_and_read_in_windows, [Service, Process, PermissionService, Storage], id = "kernel.applications.the_command_tools_run_governed_and_read_in_windows", covers = ["bin.cut", "bin.grep", "bin.head", "bin.hexdump", "bin.pwd", "bin.sort", "bin.tail", "bin.wc", "bin.which", "cli", "storage", "volume-client"]);
fn the_command_tools_run_governed_and_read_in_windows() {
	// Three of P02M0101's tools launched by PermissionManager, each with exactly the grants its
	// manifest declares, printing to a stdout the launcher forwarded.
	//
	// `pwd` IS THE INTERESTING ONE: it holds no capability at all and still answers, because a
	// working directory is data the launch context carries rather than authority a service grants.
	// A `pwd` that had to ask a volume where it was could be refused, and would disagree with the
	// shell prompt whenever the two asked at different moments.
	//
	// `wc` and `head` prove the other half: they read `hello.txt` through the BOUNDED `volume.read`
	// window rather than mapping the whole file, which is what makes them usable on a file that
	// does not fit in memory.
	let result = run_permission_scenario(PermissionScenario::GovernedTools).expect("the governed tool scenario should run");
	let printed = alloc::string::String::from_utf8_lossy(&result.command_read).into_owned();
	assert!(printed.contains("vol://system"), "pwd printed the working directory it inherited, got {printed:?}");
	// hello.txt is one line of text; `wc` reports lines, words, bytes and scalars followed by the
	// path it counted. Asserting the path and the byte count together is what distinguishes a real
	// count from a tool that printed a row of zeros.
	let expected_bytes = result.expected.len();
	let counted = alloc::format!("{expected_bytes}");
	assert!(printed.contains(&counted), "wc reported hello.txt's {expected_bytes} bytes, got {printed:?}");
	// `head -n 1` prints the file's first line, which for this one-line fixture is the whole of it.
	let first_line = core::str::from_utf8(&result.expected).unwrap_or("").lines().next().unwrap_or("");
	assert!(!first_line.is_empty(), "the fixture has a first line to print");
	assert!(printed.contains(first_line), "head printed the file's first line, got {printed:?}");

	// The rest of the family, each proving the thing it exists for rather than merely running:
	// `hexdump` renders an offset column, `which` resolves a command word to the artifact the
	// launcher would use, `find` walks and matches by name, `grep` counts matching lines, `cut`
	// takes a character range, and `tree` counts what it walked.
	assert!(printed.contains("00000000"), "hexdump rendered its offset column, got {printed:?}");
	// `which` resolves through the PATH the launcher passed in the environment - and it resolves
	// with `stat`, which is why the archive backend had to learn to answer one without a listing.
	// THE RESOLUTION IS A LINE OF ITS OWN, and the assertion says so. `contains` passed against
	// the tool's own diagnostic - "which: tried vol://system/bin/cat.lsexe (absent)" carries the
	// same substring - which is a test that agrees with a tool that found nothing.
	assert!(printed.lines().any(|line| line == "vol://system/bin/cat.lsexe"), "which resolved `cat` through PATH, got {printed:?}");
	// `-c 1-3` is three CHARACTERS of the first line, which for this fixture is its prefix; a `cut`
	// that dropped the range would print the whole line and this would still pass, so the assertion
	// is that the prefix appears WITHOUT the rest on its own line.
	let prefix: alloc::string::String = first_line.chars().take(3).collect();
	assert!(printed.lines().any(|line| line == prefix), "cut printed the first three characters as their own line, got {printed:?}");
	// `grep -c` counts matching lines rather than printing them, and the fixture has one.
	assert!(printed.lines().any(|line| line == "1"), "grep counted the matching line, got {printed:?}");
}

tagged_test!(a_fat_volume_accepts_a_write_as_its_first_operation, [Service, Storage, Filesystem], id = "kernel.applications.a_fat_volume_accepts_a_write_as_its_first_operation", covers = ["fat", "liberfs", "storage"]);
fn a_fat_volume_accepts_a_write_as_its_first_operation() {
	// The FAT backing mounts lazily, and the destination validation added with the write-stream
	// work read `self.fs` directly instead of mounting - so `write vol://media/file` answered
	// `NotFound` when it was the FIRST thing asked of the volume, and started working only after a
	// `list`, `read` or `status` had mounted it as a side effect. A volume that works depending on
	// what you did before it is the kind of fault that gets reported as "sometimes".
	//
	// The order is the whole test: write FIRST, read afterwards.
	let (_volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	// `false`: the image keeps free clusters. `true` fills them, which is how the failed-overwrite
	// test above makes a write fail on purpose - and is what made the first version of this test
	// fail for a reason that had nothing to do with mounting.
	let image = fat16_image(&[(*b"SEED    TXT", b"seed")], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &image, image.len() as u64);
	assert!(media.write(b"vol://media/FIRST.TXT", b"written first", 0x7f01), "a write is accepted as the volume's first operation");
	assert_eq!(media.open(b"vol://media/FIRST.TXT", 0x7f02), Some(b"written first".to_vec()), "and the bytes are there");
}

tagged_test!(the_memory_volumes_serve_files_and_keep_nothing_across_a_restart, [Service, Storage, Filesystem], id = "kernel.applications.the_memory_volumes_serve_files_and_keep_nothing_across_a_restart", covers = ["libermemfs", "storage"]);
fn the_memory_volumes_serve_files_and_keep_nothing_across_a_restart() {
	let (_volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");

	// vol://tmp - capped. It mounts whatever the memory situation, holds only what is stored,
	// and refuses the write that would cross its limit.
	let mut tmp = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	assert!(tmp.write(b"vol://tmp/hello", b"from memory", 0x7001), "a memory volume is writable");
	assert_eq!(tmp.open(b"vol://tmp/hello", 0x7002), Some(b"from memory".to_vec()), "what was written reads back");
	assert!(tmp.write(b"vol://tmp/hello", b"replaced", 0x7003), "rewriting replaces rather than appends");
	assert_eq!(tmp.open(b"vol://tmp/hello", 0x7004), Some(b"replaced".to_vec()));
	assert!(!tmp.write(b"vol://tmp/big", &alloc::vec![b'x'; 8192], 0x7005), "a write past the cap is refused");

	// The property no other volume has, and the one a reader would otherwise assume is broken:
	// a restart leaves NOTHING. Every disk-backed volume in this suite reads its files back.
	let mut tmp = tmp.restart(storage_elf);
	assert_eq!(tmp.open(b"vol://tmp/hello", 0x7006), None, "a memory volume is empty after a restart");

	// A write of nearly the whole capacity, through the PUBLIC path - a transferred buffer, the
	// way every real client writes.
	//
	// This is what the reserved policy is for, and until the service stopped copying the payload
	// into its own heap before calling the filesystem it could not be relied on: the copy sat
	// beside the reservation, so writing 4 KiB into a 4 KiB volume needed 8 KiB and the guarantee
	// the volume had taken was unreachable from outside. The service now lends the mapped buffer
	// and the filesystem releases before it copies.
	let mut nearly_full = StorageHarness::start_memory(storage_elf, b"RAMVOL", 4096);
	let payload = alloc::vec![b'p'; 4096 - b"whole".len()];
	assert!(nearly_full.write(b"vol://ram/whole", &payload, 0x7201), "a reserved volume takes a write that fills it");
	assert_eq!(nearly_full.open(b"vol://ram/whole", 0x7202).map(|bytes| bytes.len()), Some(payload.len()), "and reads it back whole");
	// One byte more than the volume holds is refused, not accepted into memory it does not have.
	assert!(!nearly_full.write(b"vol://ram/more", b"x", 0x7203), "a full reserved volume refuses the next write");

	let mut streamed = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	assert!(streamed.write(b"vol://tmp/f", b"original contents", 0x7301), "seed the destination");
	assert!(streamed.write_stream(b"vol://tmp/f", &[b"new ", b"contents"], 0x7302, None), "a stream that closes cleanly is written");
	assert_eq!(streamed.open(b"vol://tmp/f", 0x7303), Some(b"new contents".to_vec()), "and the bytes that arrive are the bytes that are stored");

	// A stream that ends ABNORMALLY must not replace the destination with what arrived first.
	//
	// This is the property the backend fixed as its very first defect - a write that cannot be
	// completed leaves the file as it was - and the streaming layer lost again: every abnormal
	// ending looked like the sender finishing, so the prefix already collected was written and
	// the call reported success. Out of memory is the ending that motivated the fix and cannot be
	// forced from a kernel test, because the service grows its own heap; an oversized chunk
	// reaches the same decision and is the reachable substitute.
	assert!(!streamed.write_stream(b"vol://tmp/f", &[b"partial"], 0x7304, Some(8192)), "a stream that ends abnormally is refused");

	// A medium that cannot be written refuses the stream BEFORE it accepts anything.
	//
	// This used to be impossible to express: the backend answered a single `Option<Result<usize>>`
	// where `None` meant "no cheap limit", "read-only" and "cannot validate the path" all at once,
	// so a read-only volume fell through to the caller's fallback ceiling and took up to 64 MiB
	// into memory before anything refused it. The backend now answers with a PLAN, and a refusal
	// is one of the answers.
	//
	// The sender holds its end OPEN and sends nothing, so a reply can only be a decision about the
	// destination. An empty stream would not test this: dropping the sender ends it cleanly and the
	// refusal then comes from the write at the end, exactly as it did before the plan existed.
	// A sender that drips one byte at a time, always inside the idle window, is still ended.
	//
	// The idle deadline is rebuilt after every chunk, so this sender is NEVER idle - it renewed
	// its window forever and held the service with it. The total deadline, fixed when the request
	// arrives and immune to anything the sender does, is what ends it.
	let mut slow = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	assert!(slow.stream_slowloris(b"vol://tmp/drip", 0x7306, 400, 4), "a sender that stays just inside the idle window is still given up on");

	assert_eq!(streamed.open(b"vol://tmp/f", 0x7305), Some(b"new contents".to_vec()), "and the destination still holds what it held - no prefix was written");

	// A path that cannot be written is refused before any of it is collected.
	assert!(!streamed.write_stream(b"vol://tmp/missing/f", &[b"x"], 0x7306, None), "an absent parent is refused, not collected");

	// The stream cases live in their own test.
	//
	// They were part of the scenario above until it grew past what an emulated architecture can run
	// inside the suite's thirty-minute budget: five service instances, two clock jumps and two
	// fixture-image builds, on top of everything the original test already did. Both aarch64 and
	// riscv64 timed out on it. Splitting is the fix rather than raising the watchdog, which would
	// only move the same wall further away.
}

tagged_test!(the_memory_volumes_bound_and_answer_streams, [Service, Storage, Filesystem], id = "kernel.applications.the_memory_volumes_bound_and_answer_streams", covers = ["libermemfs", "storage"]);
fn the_memory_volumes_bound_and_answer_streams() {
	let (_volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	// THE POINT OF THE PENDING-STREAM MODEL: another client is served while a stream is open.
	//
	// Receiving a stream synchronously meant one client held the service for the whole transfer -
	// every other client, every volume, the admin endpoint - and three rounds of review answered
	// that with a deadline, which bounds the harm rather than removing it. With the stream as a
	// pending operation the loop returns after every chunk, so this read is answered while the
	// stream is still open and unfinished.
	let mut concurrent = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	assert!(concurrent.write(b"vol://tmp/other", b"served", 0x7801), "seed a file for the second request");
	let sender = concurrent.stream_pending(b"vol://tmp/slow", 0x7802);
	assert_eq!(concurrent.open(b"vol://tmp/other", 0x7803), Some(b"served".to_vec()), "a second request is answered while a stream is pending");
	assert!(concurrent.stream_finish(sender, &[b"done"], 0x7802), "and the stream still completes afterwards");
	assert_eq!(concurrent.open(b"vol://tmp/slow", 0x7804), Some(b"done".to_vec()), "with the bytes that were streamed");

	// A sender that opens a stream and says nothing is given up on.
	//
	// The bound is a deadline, and until the harness could move the guest clock this could only be
	// waited out - a hundred thousand scheduler passes advance it by a few hundred ticks, so a
	// thirty-second bound was about a million pumps away. The sender is held OPEN throughout:
	// closing it would end the stream cleanly and test nothing.
	let mut silent = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	assert!(silent.stream_idle_until_deadline(b"vol://tmp/quiet", 0x7701, 8_000), "a stream that says nothing is dropped once its deadline passes");
	assert!(silent.write(b"vol://tmp/after", b"x", 0x7702), "and the service serves the next client");

	// A client that vanishes mid-stream takes its pending write with it.
	//
	// The pending write remembered the channel to answer on as a bare handle, and nothing tied the
	// two together: removing the client left the write collecting, holding the one pending slot and
	// the volume's memory, ready to commit a file for a caller that no longer existed and to answer
	// through a handle that had been closed. Today that send only fails; the day handle numbers are
	// reused it would answer the wrong client. The proof is that the NEXT stream gets the slot.
	let mut orphan = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	assert!(orphan.stream_orphaned_by_client(b"vol://tmp/orphan", 0x7b01), "a pending write is given up with the client that asked for it");
	assert_eq!(orphan.open(b"vol://tmp/orphan", 0x7b03), None, "and nothing was committed for a caller that had gone");

	// A client that asks and never listens does not get to hold the service.
	//
	// Every answer is bounded now, not only the typed dispatch: the heartbeat, both `CONNECT`
	// forms and the two refusals answered through an unbounded send, so a queue full of unread
	// `PONG`s stopped everyone. The clock is moved past the reply deadline, because the bound is a
	// deadline and waiting one out honestly would take about a million scheduler passes.
	let mut flood = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	assert!(flood.heartbeat_flood_from_silent_client(0x7c01, 2_000), "a client that never reads its heartbeat replies is dropped, not waited for");

	// And the client that CANNOT be dropped, whose next request is a listing.
	//
	// Bounding each answer stopped the permanent stop and left starvation behind it: a client that
	// stopped reading still has a queue full of requests, and each one cost the whole service
	// another reply deadline. A subclient is dropped on its first stall and its backlog goes with
	// it; the root is never dropped, because closing it ends the service - so a root that stopped
	// listening held everyone up, one deadline per queued request. The waiting is what costs other
	// people, so a client known not to be reading is answered once and abandoned.
	let mut listing_flood = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	assert!(listing_flood.root_lists_after_filling_its_reply_queue(0x7c11, 2_000), "a client that stopped reading must cost its own progress and nobody else's");

	// The client table has a ceiling, and reaching it is a refusal rather than an abort.
	//
	// There was neither: every `CONNECT` pushed into an unbounded `Vec` through an infallible
	// allocation, so any holder of the service capability could grow it until the handle table or
	// the heap gave out - and the allocator's answer to running out is to end the process. A
	// service hardened against one client's stalls all the way through this milestone could still
	// be killed by a client that simply asks a lot.
	let mut crowd = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	// The attempt bound comes from the same stated limit the service derives its ceiling from, so
	// the two cannot drift apart. It was 160, chosen when the ceiling was a hand-picked 64; the
	// ceiling is now `MAX_WAIT_SET_MEMBERS - 2`, and a fixed 160 would simply never reach it.
	let granted = crowd.connect_until_refused(abi::MAX_WAIT_SET_MEMBERS + 8).expect("the table has a ceiling and the service says so");
	assert!(granted > 16, "the ceiling is far above what the system actually opens: {granted}");
	assert!(granted <= abi::MAX_WAIT_SET_MEMBERS, "and no higher than the wait set can hold: {granted}");
	assert!(crowd.write(b"vol://tmp/after-crowd", b"x", 0x7c21), "and the service is still serving after refusing");

	// A stream handle the service cannot wait on is refused before it reaches the wait set.
	//
	// READ but no WAIT: every wait on it fails immediately, and a loop that retries on error spins
	// until the deadline serving nobody. The refusal has to happen where it costs a parse.
	let mut rights = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	assert!(rights.stream_with_rights(b"vol://tmp/nowait", 0x7a01, object::rights::Rights::READ), "a stream handle without WAIT is refused");
	assert!(rights.write(b"vol://tmp/after", b"x", 0x7a02), "and the service is still serving afterwards");

	// A refused write must not cost the service a handle.
	//
	// The ordinary write takes a buffer capability, and validation used to happen BEFORE the guard
	// that closes it, so every `?` on the way out left one behind. Nothing visible happens: the
	// service keeps answering, one handle poorer each time, until its table is full and every later
	// request fails for a reason unrelated to what caused it. Only a count taken before and after
	// shows it.
	let mut leaky = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	assert!(leaky.write(b"vol://tmp/real", b"x", 0x7501), "a good write still works");
	let before = leaky.handle_count();
	for i in 0..32u32 {
		// A path inside a FILE, which cannot be a directory: refused after the buffer arrives.
		assert!(!leaky.write(b"vol://tmp/real/under-a-file", b"y", 0x7510 + i), "a bad write is refused");
	}
	assert_eq!(leaky.handle_count(), before, "thirty-two refused writes cost the service no handles");
	assert!(leaky.write(b"vol://tmp/second", b"z", 0x7540), "and the service still works afterwards");

	let mut readonly = StorageHarness::start_archive(storage_elf);
	assert!(readonly.stream_refused_before_sending(b"vol://system/anything", 0x7305), "a stream to a read-only volume is refused before the sender offers a byte");

	// vol://ram - reserved. Same filesystem, same operations; the difference is only that its
	// memory was taken at mount, which is why a write cannot fail for want of memory here.
	let mut ram = StorageHarness::start_memory(storage_elf, b"RAMVOL", 4096);
	assert!(ram.write(b"vol://ram/state", b"reserved", 0x7101), "the reserved volume is writable");
	assert_eq!(ram.open(b"vol://ram/state", 0x7102), Some(b"reserved".to_vec()));
	assert_eq!(ram.open(b"vol://tmp/hello", 0x7103), None, "the two volumes are separate");
}

// The stream cases that cost real time under emulation.
//
// Tagged `Slow`, so the default run skips them and `--tags slow` includes them. They are here
// because they are expensive by construction rather than by accident: eighty round trips to fill a
// channel queue, two service instances each copying a multi-megabyte volume image, and eight
// connect-and-hang-up cycles. On x86_64 that is seconds; under TCG it is most of the suite's
// thirty-minute budget, and it timed out aarch64 and riscv64 twice - once as part of a larger test
// and once on its own.
//
// Splitting by COST rather than by subject is deliberate. The cheap half of these cases proves the
// same properties on every architecture; the expensive half proves them again at a scale only a
// fast target can afford.
tagged_test!(the_memory_volumes_bound_expensive_streams, [Service, Storage, Filesystem, Slow], id = "kernel.applications.the_memory_volumes_bound_expensive_streams", covers = ["libermemfs", "storage"]);
fn the_memory_volumes_bound_expensive_streams() {
	let (_volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");

	// An ordinary write LARGER than the streaming ceiling reaches the disk.
	//
	// A 16 MiB default was once returned for every backend that did not answer for a path, so a
	// write bigger than that was refused for a limit invented on behalf of streaming - by a volume
	// with room for it. The backend answers for itself now, and only a STREAM is bounded, because
	// there the service holds the bytes before the filesystem sees them.
	//
	// Affordable because the volume is EMPTY: formatting costs a B-tree walk per file, not per
	// megabyte, so a 32 MiB volume with nothing in it is as cheap to build as a small one.
	{
		let mut big = StorageHarness::start_empty(storage_elf, 32 * 1024 * 1024);
		let payload = alloc::vec![b'w'; 17 * 1024 * 1024];
		assert!(big.write(b"vol://system/large", &payload, 0x7b01), "a 17 MiB write is not refused for a 16 MiB limit that belongs to nothing");
		assert_eq!(big.open(b"vol://system/large", 0x7b02).map(|bytes| bytes.len()), Some(payload.len()), "and it reads back whole");
	}

	// A listing nobody reads must not stop the service either.
	//
	// Past the channel's 64-message queue the send blocks, and an unbounded one held StorageService
	// there permanently - the same defect as a silent sender, in the direction that had no bound at
	// all. Eighty entries so the queue actually fills: with two, the send never blocks and the test
	// would pass with the bound removed.
	let mut listed = StorageHarness::start_memory(storage_elf, b"TMPVOL", 16 * 1024);
	for i in 0..80u32 {
		let mut path = b"vol://tmp/f".to_vec();
		path.extend_from_slice(alloc::format!("{i}").as_bytes());
		assert!(listed.write(&path, b"x", 0x7710 + i), "seed entry {i}");
	}
	let idle_consumer = listed.list_without_reading(b"vol://tmp", 0x7780);
	assert!(idle_consumer.is_some(), "the listing hands back a consumer");
	// NO clock advance. The listing is produced between passes of the serve loop, so a consumer
	// that stops reading costs one pass rather than the service - it does not have to be given up
	// on first. An earlier version of this test needed the clock moved past the send deadline,
	// which is what "bounded" looked like before it became "not blocking".
	assert_eq!(listed.open(b"vol://tmp/f0", 0x7781), Some(b"x".to_vec()), "the service serves other clients while a listing goes unread");
	drop(idle_consumer);

	// A live import that cannot be completed is refused, not served in part.
	//
	// The medium is read-only, so its volume is copied into memory at boot; a copy that fails half
	// way leaves a system missing executables. Reporting that as healthy is worse than not coming
	// up at all, because nothing downstream can tell the difference. The whole image comes up; the
	// same image cut short does not.
	{
		let volume = volume_package_bytes().expect("volume package module not found");
		let whole = StorageHarness::fixture_image(volume);
		assert!(StorageHarness::live_volume_comes_up(storage_elf, &whole), "a complete live volume mounts");
		let cut = whole.len() * 3 / 5;
		assert!(!StorageHarness::live_volume_comes_up(storage_elf, &whole[..cut]), "one cut short is refused rather than served with holes in it");
	}

	// A client that hangs up between asking for a listing and being answered.
	//
	// The service mints a consumer and hands it over; that send was unchecked, so when the client
	// was gone the handle leaked AND the producer kept a live peer nobody would read, which blocked
	// the next send on it forever. Both are invisible from outside - the service simply stops - so
	// the test is that it still answers afterwards, and that it costs no handles.
	let mut hangup = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
	assert!(hangup.write(b"vol://tmp/here", b"still here", 0x7901), "seed a file");
	let before = hangup.handle_count();
	// Three rather than eight: each is a connect, a request and five hundred pumps, and under
	// emulation that adds up. One would prove the leak; three prove it is not a fluke.
	for i in 0..3u32 {
		hangup.list_then_hang_up(b"vol://tmp", 0x7910 + i);
	}
	assert_eq!(hangup.open(b"vol://tmp/here", 0x7920), Some(b"still here".to_vec()), "the service survives a client that hangs up mid-listing");
	// EXACTLY the same count. A tolerance here would have admitted one leak per hang-up, which is
	// the whole defect - the first version of this assertion allowed +8 for eight hang-ups and
	// passed with the fix removed.
	assert_eq!(hangup.handle_count(), before, "and leaks no handle when the client is gone");
}

tagged_test!(a_services_round_trip_against_its_client_count, [Service, Storage, Stress], id = "kernel.applications.a_services_round_trip_against_its_client_count", covers = ["kernel"]);
fn a_services_round_trip_against_its_client_count() {
	// The measurement P02M0117 asked for FIRST, before anything touches the serve loop.
	//
	// What that milestone fixes is a slope: `wait_any` takes a fresh array of handles on every call,
	// so the kernel registers a waiter on every channel in it and takes them all out again - once
	// per pass, for as long as the service runs. The cost of answering one client therefore grows
	// with how many others are connected.
	//
	// It was known by its symptom rather than its size: finding a client ceiling a test could reach
	// took several attempts, and `MAX_CLIENTS` ended up at 64 because that is where the service is
	// still brisk. This prints the number instead. It does not ASSERT a slope - a wall-clock figure
	// on an emulated guest is not something to fail a build over, and this milestone has already
	// retired three tests that asserted numbers they did not own - it puts the three figures in the
	// log so a change to the loop can be judged against them rather than guessed at.
	let (_volume, package) = scenario_packages().expect("boot modules should be present");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage_service.lsexe");
	for crowd in [4usize, 32, 62, 254] {
		let mut harness = StorageHarness::start_memory(storage_elf, b"TMPVOL", 4096);
		let ns = harness.round_trip_ns_with_crowd(crowd, 20);
		crate::serial_println!("storage-roundtrip-perf: clients={crowd} ns-per-round-trip={ns}");
	}
}

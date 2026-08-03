use super::*;

tagged_test!(imgview_interactions, [Imgview, Image, Display, Input, Process, Service, Storage]);
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

tagged_test!(lico_switches_panels_and_restores_the_terminal, [Lico, Process, Service, Storage]);
fn lico_switches_panels_and_restores_the_terminal() {
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let lico_elf = program_elf(&package, volume, b"lico").expect("lico tool");
	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);
	run_lico_harness(lico_elf, &mut system);
}

tagged_test!(imgconv_cross_volume_and_failed_overwrite_preserve_destination, [Image, Service, Storage, Process, Filesystem]);
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
	let help = run_imgconv_harness(imgconv_elf, b"--help", &mut system, &mut media);
	assert!(help.starts_with(b"Usage: imgconv [options] <input> <output>\n\nOptions:\n"));
	assert!(help.windows(b"WebP  options: quality compression lossless lossy animation; defaults: mode=lossless compression=100".len()).any(|window| window == b"WebP  options: quality compression lossless lossy animation; defaults: mode=lossless compression=100"));
	let line = run_imgconv_harness(imgconv_elf, b"--quality 100 vol://media/SOURCE.BMP vol://system/CROSS.BMP", &mut system, &mut media);
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
	let unknown = run_imgconv_harness(imgconv_elf, b"vol://media/UNKNOWN.BIN vol://media/UNKNOWN.BMP", &mut system, &mut classification_media);
	assert_eq!(unknown, b"imgconv: unsupported image format\n");
	let corrupt = run_imgconv_harness(imgconv_elf, b"vol://media/BAD.PNG vol://media/BAD.BMP", &mut system, &mut classification_media);
	assert_eq!(corrupt, b"imgconv: invalid or corrupt image\n");
	let collision = run_imgconv_harness(imgconv_elf, b"vol://media/COLLIDE.TGA vol://media/COLLIDE.BMP", &mut system, &mut classification_media);
	assert!(collision.starts_with(b"imgconv: TGA 1x1 -> BMP 1x1 bytes="));
	let collision_output = classification_media.open(b"vol://media/COLLIDE.BMP", 0xc0111de).expect("collision output opens");
	assert_eq!(bmp::decode_rgba(&collision_output).expect("collision output decodes"), collision_pixel);
	run_imgview_harness(imgview_elf, b"vol://media/COLLIDE.TGA", &viewer_surface(&collision_pixel), &mut system, &mut classification_media);

	let line = run_imgconv_harness(imgconv_elf, b"--lossless --compression 50 vol://media/SOURCE.BMP vol://system/CROSSL.WEBP", &mut system, &mut media);
	assert!(line.starts_with(b"imgconv: BMP 2x2 -> WebP 2x2 mode=lossless compression=50 bytes="));
	let converted = system.open(b"vol://system/CROSSL.WEBP", 0xc2057).expect("cross-volume lossless WebP opens");
	assert_eq!(webp::decode(&converted).expect("cross-volume lossless WebP decodes"), source);

	let line = run_imgconv_harness(imgconv_elf, b"--lossy --quality 100 --compression 100 vol://media/SOURCE.BMP vol://system/CROSS.WEBP", &mut system, &mut media);
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
	let failure = run_imgconv_harness(imgconv_elf, b"--force --resize 64x64 vol://media/SOURCE.BMP vol://media/KEEP.BMP", &mut system, &mut full_media);
	assert_eq!(failure, b"imgconv: cannot write output\n");
	assert_eq!(full_media.open(b"vol://media/KEEP.BMP", 0xfa11), Some(previous.to_vec()), "failed overwrite preserves the previous destination byte-for-byte");
}

tagged_test!(imgconv_governed_working_set_is_measured, [Image, Memory, Process, Service, Storage]);
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
	let full_hd_domain = Domain::new_child(&sched::root_domain(), IMGCONV_MEMORY_LIMIT, UNLIMITED, UNLIMITED);
	let (full_hd, full_hd_peak) = run_imgconv_harness_in(full_hd_domain, imgconv_elf, b"--resize 1920x1080 --compression 100 vol://media/SOURCE.BMP vol://media/FHD.PNG", &mut system, &mut media);
	assert!(full_hd.starts_with(b"imgconv: BMP 2x2 -> PNG 1920x1080 compression=100 bytes="));
	let full_hd_output = media.open(b"vol://media/FHD.PNG", 0xf1080).expect("1080p output opens");
	let full_hd_image = png::decode_rgba(&full_hd_output).expect("1080p output decodes");
	assert_eq!((full_hd_image.width, full_hd_image.height), (1920, 1080));

	let media_image = fat16_image(&[(*b"SOURCE  BMP", source_bmp.as_slice())], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let ultra_hd_domain = Domain::new_child(&sched::root_domain(), IMGCONV_MEMORY_LIMIT, UNLIMITED, UNLIMITED);
	let (ultra_hd, ultra_hd_peak) = run_imgconv_harness_in(ultra_hd_domain, imgconv_elf, b"--resize 3840x2160 --compression 100 vol://media/SOURCE.BMP vol://media/UHD.PNG", &mut system, &mut media);
	assert!(ultra_hd.starts_with(b"imgconv: BMP 2x2 -> PNG 3840x2160 compression=100 bytes="));
	let ultra_hd_output = media.open(b"vol://media/UHD.PNG", 0xf2160).expect("4K output opens");
	let ultra_hd_image = png::decode_rgba(&ultra_hd_output).expect("4K output decodes");
	assert_eq!((ultra_hd_image.width, ultra_hd_image.height), (3840, 2160));

	let animation = include_bytes!("../../user/libs/image/webp/tests/data/external-animation.webp");
	let media_image = fat16_image(&[(*b"ANIM    WEB", animation)], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);
	let animation_domain = Domain::new_child(&sched::root_domain(), IMGCONV_MEMORY_LIMIT, UNLIMITED, UNLIMITED);
	let (animation_line, animation_peak) = run_imgconv_harness_in(animation_domain, imgconv_elf, b"vol://media/ANIM.WEB vol://media/ANIM.GIF", &mut system, &mut media);
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
	let limited_domain = Domain::new_child(&sched::root_domain(), 80 * 1024 * 1024, UNLIMITED, UNLIMITED);
	let (failure, limited_peak) = run_imgconv_harness_result(limited_domain, imgconv_elf, b"--force --resize 3840x2160 --compression 100 vol://media/SOURCE.BMP vol://media/KEEP.PNG", &mut system, &mut media);
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
	let corpus_domain = Domain::new_child(&sched::root_domain(), MEASUREMENT_CEILING, UNLIMITED, UNLIMITED);
	let (wallpaper, wallpaper_peak) = run_imgconv_harness_in(corpus_domain, imgconv_elf, b"vol://system/wallpapers/logo.webp vol://media/LOGO.PNG", &mut system, &mut media);
	assert!(wallpaper.starts_with(b"imgconv: WebP 3840x2160 -> PNG 3840x2160"), "the shipped wallpaper converts, got {:?}", core::str::from_utf8(&wallpaper));
	assert!(wallpaper_peak > ultra_hd_peak, "a 4K input costs more than a 4K output alone - decoded input and output are live together");
	assert!(wallpaper_peak < IMGCONV_MEMORY_LIMIT, "the shipped corpus must fit the production quota: measured {} bytes against a {} byte limit", wallpaper_peak, IMGCONV_MEMORY_LIMIT);

	serial_println!("imgconv governed memory: 1920x1080={} bytes, 3840x2160={} bytes, animation={} bytes, shipped 4K wallpaper={} bytes", full_hd_peak, ultra_hd_peak, animation_peak, wallpaper_peak);
}

tagged_test!(wasi_host_runs_a_component, [Component, Service]);
fn wasi_host_runs_a_component() {
	// The wasi_host (a ring-3 process) loads an embedded Wasm component and runs it
	// on the `wasm` runtime. The component's only import, `liber.read`, is wired by
	// the host to read the one granted file (vol://system/hello.txt) through
	// StorageService into the component's linear memory - a WASI-style world: the
	// component has no other capability and can reach nothing it was not given. The
	// bytes the component read must equal the file straight from the volume, proving
	// a Wasm component performed a capability-gated operation via a host import
	// mapped to a native service.
	let (expected, actual) = run_wasi_scenario().expect("the wasi scenario should run");
	assert!(!expected.is_empty(), "the granted file should not be empty");
	assert_eq!(actual, expected, "the component read the granted file's bytes through the host import");
}

tagged_test!(powerbox_grants_a_picked_file_to_a_component, [Component, Service]);
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

tagged_test!(permission_manager_enforces_static_and_dynamic_probe_policy, [Service, Process, PermissionService]);
fn permission_manager_enforces_static_and_dynamic_probe_policy() {
	let result = run_permission_scenario(PermissionScenario::Probes).expect("the permission probe scenario should run");
	assert!(!result.expected.is_empty(), "the granted file should not be empty");
	assert_eq!(result.probe_read, result.expected, "the sandboxed component read its one granted file through the storage grant");
	assert_eq!(result.probe_summary.as_slice(), b"storage=grant log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny", "sandbox_probe was granted exactly its manifest - storage and log - and denied every other capability in the vocabulary");
	assert_eq!(result.request_read.as_slice(), b"storage denied", "request_probe's undeclared storage request was refused by the headless policy default");
	assert_eq!(result.request_summary.as_slice(), b"storage=deny log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny storage=deny(dynamic)", "request_probe's static grants and dynamic denial were recorded independently");
}

tagged_test!(permission_manager_runs_tools_with_minimal_grants, [Service, Process, PermissionService]);
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
	assert_eq!(result.date_summary.as_slice(), b"storage=deny log=deny network=deny device=deny config=deny time=grant audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny", "date received only its time grant");
	assert_eq!(result.cat_read, result.expected, "cat printed its file through the storage grant");
	assert_eq!(result.ip_read.as_slice(), b"net0: 10.0.2.15  mac 52:54:00:12:34:56  mtu 1500  gateway 10.0.2.2\n", "ip rendered state from its typed NetworkService grant");
	assert_eq!(result.ip_summary.as_slice(), b"storage=deny log=deny network=grant device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny", "ip received only its network grant");
}

tagged_test!(permission_manager_mints_scoped_application_grants, [Service, Process, PermissionService]);
fn permission_manager_mints_scoped_application_grants() {
	let result = run_permission_scenario(PermissionScenario::ScopedGrants).expect("the scoped application grant scenario should run");
	assert_eq!(result.graphics_read.as_slice(), b"graphics grants\n", "the graphics probe received process-bound display, key-only input and playback-only audio grants");
	assert!(result.graphics_start_ns != 0, "the governed app cold-start path is measured");
}

tagged_test!(component_host_runs_an_sdk_component, [Component, Service, Slow]);
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
	let (expected, content, logged, score) = run_component_scenario().expect("the component scenario should run");
	assert!(!expected.is_empty(), "the granted file should not be empty");
	assert_eq!(content, expected, "the component read, transformed, and returned its granted file's bytes through the host imports");
	assert!(logged, "the component reached its LogService grant - the second typed service was wired with no ambient authority");
	assert_eq!(score, 17, "the component's float `score` export computed floor(10 * 1.5 + 2.0) on real toolchain output");
}

tagged_test!(a_governed_pipeline_starts_as_one_transaction_and_carries_data, [Service, Process, PermissionService]);
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
}

tagged_test!(the_memory_volumes_serve_files_and_keep_nothing_across_a_restart, [Service, Storage, Filesystem]);
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
	assert_eq!(streamed.open(b"vol://tmp/f", 0x7305), Some(b"new contents".to_vec()), "and the destination still holds what it held - no prefix was written");

	// A path that cannot be written is refused before any of it is collected.
	assert!(!streamed.write_stream(b"vol://tmp/missing/f", &[b"x"], 0x7306, None), "an absent parent is refused, not collected");

	// vol://ram - reserved. Same filesystem, same operations; the difference is only that its
	// memory was taken at mount, which is why a write cannot fail for want of memory here.
	let mut ram = StorageHarness::start_memory(storage_elf, b"RAMVOL", 4096);
	assert!(ram.write(b"vol://ram/state", b"reserved", 0x7101), "the reserved volume is writable");
	assert_eq!(ram.open(b"vol://ram/state", 0x7102), Some(b"reserved".to_vec()));
	assert_eq!(ram.open(b"vol://tmp/hello", 0x7103), None, "the two volumes are separate");
}

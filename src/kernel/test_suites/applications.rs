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
// `harness/scenarios/terminal-lifecycle.toml`, which takes all three terminal applications
// through the same life against a real terminal rather than a synthetic one. The scenario is
// exercised on every target, not only the one a persistent instance runs on:
// `./lab.sh scenario-cold aarch64 harness/scenarios/terminal-lifecycle.toml` passes, and the riscv64
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

tagged_test!(licoedit_publishes_what_was_typed_through_the_transactional_writer, [Lico, Process, Service, Storage, Filesystem], id = "kernel.applications.licoedit_publishes_what_was_typed_through_the_transactional_writer", covers = ["bin.licoedit", "lico", "libermemfs", "storage", "volume-client"]);
fn licoedit_publishes_what_was_typed_through_the_transactional_writer() {
	// THE SAVE, END TO END, WHICH NOTHING HAD EVER RUN.
	//
	// `licoedit` publishes through the transactional writer: staged in the service, visible under
	// the file's name only at `commit`, so an editor that dies half way leaves the file as it was.
	// That is the right design and it was never exercised - the terminal-lifecycle scenario opens
	// the editor and leaves with `F10` - which is how the writer could be broken for every program
	// in the system, by an argument-register mismatch in the client stub, and stay broken.
	//
	// The refusing half of the same path is covered by the manager's copy onto a full volume. This
	// is the half that has to WORK: bytes typed at the terminal reach the medium under the name
	// they were typed into.
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let editor_elf = program_elf(&package, volume, b"licoedit").expect("licoedit tool");
	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);
	let mut scratch = StorageHarness::start_memory(storage_elf, b"TMPVOL", 16 * 1024);
	assert!(scratch.write(b"vol://tmp/note.txt", b"before", 0x7b01), "the file starts with something to replace");

	let (terminal, terminal_child) = object::channel::Channel::create();
	let process = spawn_terminal_app_on(editor_elf, terminal_child, b"SELECTED_FILE", b"vol://tmp/note.txt", &mut system, b"vol://tmp", &mut [(b"TMP", &mut scratch)]);

	// Wait for the file to be on the screen, which is the editor saying it read the volume.
	let mut opened = false;
	for _ in 0..200_000 {
		system.pump();
		scratch.pump();
		while let Ok(message) = terminal.recv() {
			opened |= message.bytes.windows(b"before".len()).any(|window| window == b"before");
		}
		if opened {
			break;
		}
	}
	assert!(opened, "licoedit opens the file it was launched over");

	// Type at the end of the line, then `^S`. The cursor starts at the beginning, so `End` first -
	// this is about the SAVE, and a test that typed into the middle would be about the buffer.
	lico_type(&terminal, b"\x1b[F");
	lico_type(&terminal, b" and after");
	lico_type(&terminal, b"\x13");
	let saved = lico_await(&terminal, b"saved", &mut [&mut system, &mut scratch]);
	assert!(saved.is_some(), "the editor reports the save");

	// AND THE BYTES ARE ON THE VOLUME, read back through the service rather than believed from the
	// status line. `^S` says "saved" from the moment the commit returns, so a commit that published
	// nothing would say exactly the same thing.
	lico_type(&terminal, b"\x1b[21~");
	for _ in 0..200_000 {
		system.pump();
		scratch.pump();
		while terminal.recv().is_ok() {}
		if process.is_terminated() {
			break;
		}
	}
	assert!(process.is_terminated(), "licoedit exits on F10");
	assert_eq!(scratch.open(b"vol://tmp/note.txt", 0x7b02), Some(b"before and after".to_vec()), "what was typed is what the volume holds");
}

tagged_test!(lico_exits_on_a_pointer_press_delivered_by_the_real_console, [Lico, Process, Service, Storage, Console, Display, Mouse], id = "kernel.applications.lico_exits_on_a_pointer_press_delivered_by_the_real_console", covers = ["bin.lico", "keys", "lico", "term"]);
fn lico_exits_on_a_pointer_press_delivered_by_the_real_console() {
	// THE POINTER EXITS, THROUGH THE TERMINAL THAT MAKES THE REPORTS.
	//
	// The other `lico` tests hand the program a plain channel and write escape sequences into it by
	// hand, which measures the program's decoder against the test's own idea of the encoding. A
	// mouse report is not the test's to invent: ConsoleService builds it from a raw device event,
	// the grid geometry, and the mouse modes the program asked for by printing them - and if the
	// console's cell arithmetic and the program's disagree by one, every click lands on the wrong
	// control while both halves pass their own tests.
	//
	// So there is no escape sequence anywhere below. What goes in is what the pointer driver sends:
	// a position over the normalized span and a button bit. Everything from there to `lico`'s input
	// is the real path, which is also what makes this the test that proves the program ever enabled
	// tracking at all - the console reports NOTHING to a program that did not.
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	// `lico`'s layout is fixed at 80 columns and puts its function-key row at cell row 20, so the
	// grid has to be at least that. 80x24 at the console's 8x16 font is 640x384.
	const COLS: usize = 80;
	const ROWS: usize = 24;
	// The clickable function-key row, and the two labels on it this test presses. `lico` divides
	// the row into nine-column slots over the keys 1, 3, 4, 5, 6, 7, 8, 9, 10 - so slot 8 is F10
	// (exit) and slot 7 is F9 (the menu). Named by CELL, which is what a mouse report carries.
	const FKEY_ROW: usize = 20;
	const F10_COLUMN: usize = 75;
	const F9_COLUMN: usize = 67;

	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let lico_elf = program_elf(&package, volume, b"lico").expect("lico tool");
	let display_elf = program_elf(&package, volume, b"display_service").expect("display service");
	let console_elf = program_elf(&package, volume, b"console_service").expect("console service");
	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);
	let mut console = ConsoleHarness::start(display_elf, console_elf, COLS, ROWS);

	// FIRST: THE FUNCTION-KEY ROW. `lico` on VT 1, holding the channel a program holds.
	let program = console.program.clone();
	let process = spawn_lico_on(lico_elf, program, &mut system, b"vol://system", &mut []);
	assert!(console.settle(&mut system), "lico draws its first frame on the real terminal");

	// A press inside the panel, which moves the selection and must NOT end anything. It is the
	// control for the two that follow: without it, a test where every click exits would pass.
	console.click(&mut system, 10, 6);
	assert!(!process.is_terminated(), "a press inside the panel does not end the program");

	console.click(&mut system, F10_COLUMN, FKEY_ROW);
	for _ in 0..2_000 {
		system.pump();
		console.pump();
		if process.is_terminated() {
			break;
		}
	}
	assert!(process.is_terminated(), "a press on the F10 label exits");

	// SECOND: THE MENU'S OWN QUIT, opened by pointer and chosen by pointer. A separate instance,
	// because the first one is gone - and this is the exit that goes through two clicks, so a
	// console that reported only the first press in a session would be caught here.
	let program = console.program.clone();
	let menu = spawn_lico_on(lico_elf, program, &mut system, b"vol://system", &mut []);
	assert!(console.settle(&mut system), "the second instance draws");
	console.click(&mut system, F9_COLUMN, FKEY_ROW);
	assert!(!menu.is_terminated(), "opening the menu by pointer does not end the program");
	// `Quit` is the thirteenth row of the menu, drawn from cell row 3 - so cell row 15.
	console.click(&mut system, 10, 15);
	for _ in 0..2_000 {
		system.pump();
		console.pump();
		if menu.is_terminated() {
			break;
		}
	}
	assert!(menu.is_terminated(), "a press on the menu's Quit row exits");
}

tagged_test!(lico_names_a_full_volume_when_a_copy_runs_out_of_room, [Lico, Process, Service, Storage, Filesystem], id = "kernel.applications.lico_names_a_full_volume_when_a_copy_runs_out_of_room", covers = ["bin.lico", "lico", "libermemfs", "storage", "volume-client"]);
fn lico_names_a_full_volume_when_a_copy_runs_out_of_room() {
	// THE CASE THAT NEEDS TWO WRITABLE VOLUMES, and could not be written until there were two:
	// everything inside one volume shares its space, so a destination that runs out of room while
	// the source still has plenty only exists ACROSS a boundary.
	//
	// What it measures is the whole chain, one end to the other: a memory filesystem that knows it
	// is out of blocks, a service that puts the right word on the wire, and a file manager that
	// tells the person WHICH of the things that can go wrong went wrong. Each link dropped it
	// before this test - the service answered `again` and the manager counted refusals without
	// keeping a single one of their reasons - and each of the three looks identical from the two
	// others' side.
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let lico_elf = program_elf(&package, volume, b"lico").expect("lico tool");
	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);

	// The source, with room to spare, and a destination that holds one of the two files on it.
	//
	// `small.txt` is deliberately larger than `WRITER_CHUNK`, so the copy that SUCCEEDS spans more
	// than one request and covers the chunking loop; `big.txt` is larger than the whole destination.
	let mut source = StorageHarness::start_memory(storage_elf, b"RAMVOL", 64 * 1024);
	assert!(source.write(b"vol://ram/big.txt", &alloc::vec![b'b'; 16 * 1024], 0x7a01), "the source volume takes the file that will not fit");
	assert!(source.write(b"vol://ram/small.txt", &alloc::vec![b's'; 6 * 1024], 0x7a06), "and the one that will");
	let mut destination = StorageHarness::start_memory(storage_elf, b"TMPVOL", 16 * 1024);
	assert!(destination.write(b"vol://tmp/keep.txt", b"keep me", 0x7a02), "the destination starts with a file of its own");

	let (process, terminal, initial) = {
		let (process, terminal, output) = start_lico_with(lico_elf, &mut system, b"vol://ram", &mut [(b"RAM", &mut source), (b"TMP", &mut destination)]);
		let initial: alloc::vec::Vec<u8> = output.iter().flat_map(|line| line.iter().copied()).collect();
		(process, terminal, initial)
	};
	assert!(initial.windows(b">vol://ram".len()).any(|window| window == b">vol://ram"), "the panel opens on the granted source volume");
	assert!(initial.windows(b"big.txt".len()).any(|window| window == b"big.txt"), "and lists the file to be copied");

	// FIRST THE COPY THAT WORKS, because a suite that only ever saw a refusal would pass with the
	// whole path broken - which is the state this case was written in.
	//
	// Tag by NAME rather than by cursor position, for the same reason the delete case does: the
	// test should not depend on where the cursor landed or how the panel is sorted. F5 opens the
	// copy prompt pre-filled with the OTHER panel's directory - the same volume here, so it is
	// erased before the destination is typed.
	let copy_to_tmp = |terminal: &alloc::sync::Arc<object::channel::Channel>, name: &[u8]| {
		lico_type(terminal, b"+");
		lico_type(terminal, name);
		lico_type(terminal, b"\r");
		lico_type(terminal, b"\x1b[15~");
		for _ in 0..40 {
			lico_type(terminal, b"\x7f");
		}
		lico_type(terminal, b"vol://tmp");
		lico_type(terminal, b"\r");
	};

	copy_to_tmp(&terminal, b"small.txt");
	let done = lico_await(&terminal, b" refused", &mut [&mut system, &mut source, &mut destination]).expect("the first copy job reaches its report");
	let done = done.rsplit(|byte| *byte == b'\n').next().unwrap_or(&done).to_vec();
	assert!(done.windows(b"1 done, 0 refused".len()).any(|window| window == b"1 done, 0 refused"), "a copy that fits is done rather than refused: {:?}", core::str::from_utf8(&done));

	// UNTAG IT AGAIN, or the second operation copies both.
	lico_type(&terminal, b"-");
	lico_type(&terminal, b"small.txt");
	lico_type(&terminal, b"\r");
	copy_to_tmp(&terminal, b"big.txt");

	// A DIFFERENT NEEDLE for the second job, because the status line STAYS on the screen: every
	// frame drawn after the first copy still carries "1 done, 0 refused", so waiting for the same
	// text again would match the answer to the previous question.
	let settled = lico_await(&terminal, b"1 refused", &mut [&mut system, &mut source, &mut destination]).expect("the copy job reaches its report");
	let report = settled.rsplit(|byte| *byte == b'\n').next().unwrap_or(&settled);
	assert!(report.windows(b"0 done, 1 refused".len()).any(|window| window == b"0 done, 1 refused"), "the copy is refused rather than reported done: {:?}", core::str::from_utf8(report));
	assert!(report.windows(b"the volume is full".len()).any(|window| window == b"the volume is full"), "and the report NAMES the reason a person can act on: {:?}", core::str::from_utf8(report));

	// Exit before reading the volumes back. `lico` holds the same channel ends this harness does,
	// so a query issued while it runs is a second speaker on one wire.
	lico_type(&terminal, b"\x1b[21~");
	for _ in 0..200_000 {
		system.pump();
		source.pump();
		destination.pump();
		while terminal.recv().is_ok() {}
		if process.is_terminated() {
			break;
		}
	}
	assert!(process.is_terminated(), "lico exits on F10");

	// WHAT THE REPORT PROMISED, CHECKED RATHER THAN TAKEN: nothing half-written under the
	// destination's name, and the file that was already there untouched. A copy that published a
	// partial file would be the worst outcome of the three, and it is the one the status line
	// cannot show.
	assert_eq!(destination.open(b"vol://tmp/small.txt", 0x7a07).map(|bytes| bytes.len()), Some(6 * 1024), "the copy that fitted is on the destination, whole");
	assert_eq!(destination.open(b"vol://tmp/big.txt", 0x7a03), None, "the refused copy left nothing under the destination name");
	assert_eq!(destination.open(b"vol://tmp/keep.txt", 0x7a04), Some(b"keep me".to_vec()), "and the file the destination already held is intact");
	assert_eq!(source.open(b"vol://ram/big.txt", 0x7a05).map(|bytes| bytes.len()), Some(16 * 1024), "the source is untouched");
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

tagged_test!(audioconv_writes_a_lossy_container_that_decodes_back, [Audio, Service, Storage, Process, Filesystem], id = "kernel.applications.audioconv_writes_a_lossy_container_that_decodes_back", covers = ["audioconv", "bin.audioconv", "ogg", "pcm", "vorbis"]);
fn audioconv_writes_a_lossy_container_that_decodes_back() {
	// The lossy row of the matrix, and a test OF ITS OWN rather than another conversion inside the
	// lossless one. The reason is the watchdog: that test already cost 681 seconds on riscv64 under
	// emulation, three quarters of the window in which a test must finish, and a fifth conversion
	// pushed it past. Two tests that each finish say more than one that is stopped, and the split is
	// along the line the milestone draws anyway - lossless round-trips sample-exactly, lossy does
	// not and is judged differently.
	//
	// WHAT IS COMPARED IS NOT THE SAMPLES. A lossy stream is judged by being a stream this tree's
	// own decoder accepts, at the shape that was asked for, long enough to be the track. A container
	// that parsed and decoded to nothing would pass a "does it parse" check and fail this one.
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let audioconv_elf = program_elf(&package, volume, b"audioconv").expect("audioconv tool");

	let samples = governed_audio_fixture(2_000);
	let source_wav = {
		let format = pcm::Format::new(8_000, 1).expect("the fixture rate is one `Format` names");
		let mut encoder = wav::encode::Encoder::new(pcm::encode::VecSink::new(1 << 20), format, wav::encode::Output::Pcm { bits: 16 }).expect("the fixture encoder starts");
		encoder.push(&samples).expect("the fixture encodes");
		encoder.finish().expect("the fixture closes").0.into_bytes()
	};
	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);
	let media_image = fat16_image(&[(*b"SOURCE  WAV", source_wav.as_slice())], false);
	let mut media = StorageHarness::start(storage_elf, b"FATBLOCK", &media_image, media_image.len() as u64);

	let line = run_volume_tool(audioconv_elf, b"vol://media/SOURCE.WAV vol://system/CROSS.OGG", &mut system, &mut media);
	assert!(line.starts_with(b"audioconv: WAV 8000Hz/1ch/2000fr -> Ogg Vorbis 8000Hz/1ch/2000fr duration=250ms bytes="), "unexpected report: {}", alloc::string::String::from_utf8_lossy(&line));
	let written = system.open(b"vol://system/CROSS.OGG", 0xaad12).expect("the cross-volume Ogg opens");
	let stream = vorbis::Vorbis::parse(&written).expect("the Ogg this encoder wrote is one this decoder reads");
	assert_eq!(stream.metadata().rate, 8_000);
	assert_eq!(stream.metadata().channels, 1);
	let mut decoded = alloc::vec::Vec::new();
	let read = stream.decoder().read_i16_le(2_000, &mut decoded).expect("the Ogg decodes");
	assert!(read > 1_500, "the Ogg decoded {read} frames of a two-thousand-frame track");

	// AND THE OTHER LOSSY CONTAINER. MPEG-1 Layer III names 32, 44.1 and 48 kHz and nothing else,
	// so the 8 kHz fixture has to be resampled on the way - which is the tool's own `--rate` rather
	// than something the encoder does quietly.
	let line = run_volume_tool(audioconv_elf, b"--rate 44100 vol://media/SOURCE.WAV vol://system/CROSS.MP3", &mut system, &mut media);
	assert!(line.starts_with(b"audioconv: WAV 8000Hz/1ch/2000fr -> MP3 44100Hz/1ch/11025fr duration=250ms bytes="), "unexpected report: {}", alloc::string::String::from_utf8_lossy(&line));
	let written = system.open(b"vol://system/CROSS.MP3", 0xaad14).expect("the cross-volume MP3 opens");
	let stream = mp3::Mp3::parse(&written).expect("the MP3 this encoder wrote is one this decoder reads");
	assert_eq!(stream.metadata().rate, 44_100);
	assert_eq!(stream.metadata().channels, 1);
	let mut decoded = alloc::vec::Vec::new();
	let read = stream.decoder().read_i16_le(11_025, &mut decoded).expect("the MP3 decodes");
	assert!(read > 8_000, "the MP3 decoded {read} frames of an eleven-thousand-frame track");

	// A RATE THE FORMAT CANNOT CARRY IS REFUSED, and nothing is written. Resampling to whatever the
	// encoder could manage would be changing the audio without being asked.
	let refused = run_volume_tool(audioconv_elf, b"vol://media/SOURCE.WAV vol://system/RATE.MP3", &mut system, &mut media);
	assert_eq!(refused, b"audioconv: the output format does not support that option or sample rate\n");
	assert_eq!(system.open(b"vol://system/RATE.MP3", 0xaad15), None, "a refused rate still created its destination");
}

tagged_test!(audiorec_records_a_capture_stream_and_never_publishes_a_failed_one, [Audio, AudioService, Service, Storage, Process, Filesystem], id = "kernel.applications.audiorec_records_a_capture_stream_and_never_publishes_a_failed_one", covers = ["audiorec", "bin.audiorec", "pcm", "storage", "wav"]);
fn audiorec_records_a_capture_stream_and_never_publishes_a_failed_one() {
	// The real `audiorec.lsexe`, the real AudioService, and a harness standing in for the
	// virtio-snd driver - so everything between the device and the file on the volume is the code
	// that runs on a machine with a microphone. What the harness supplies is the one thing a test
	// machine cannot: a period with KNOWN SAMPLES in it, which is what makes "the recording is the
	// audio that arrived" an assertion rather than a hope.
	//
	// The signal is constant IN TIME AND DIFFERENT PER CHANNEL, which is what makes it able to catch
	// something. Resampling a constant gives the constant back, so every sample in the finished file
	// is one known value - and because the two channels differ, a mono recording distinguishes their
	// AVERAGE from either of them. A downmix that quietly kept the left channel would pass against a
	// signal that is the same on both, which is exactly the test that measures nothing.
	use object::channel::{Channel, Message};
	use object::rights::Rights;
	const SYSTEM_CAPACITY: u64 = 64 * 1024 * 1024;
	const LEFT: i16 = 12_000;
	const RIGHT: i16 = 4_000;
	const MONO: i16 = 8_000;
	// One device period: 512 stereo frames of signed-16-bit little-endian, which is what
	// driver.virtio-snd hands back for a capture command.
	const PERIOD_BYTES: usize = 2_048;

	let (volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let audio_elf = program_elf(&package, volume, b"audio_service").expect("audio_service");
	let audiorec_elf = program_elf(&package, volume, b"audiorec").expect("audiorec tool");
	let mut system = StorageHarness::start_system(storage_elf, b"BLOCK", volume, SYSTEM_CAPACITY);

	// AudioService, with this test as its driver.
	let (audio_boot_kernel, audio_boot_user) = Channel::create();
	let (service_server, _service_client) = Channel::create();
	let (snd_host, snd_service) = Channel::create();
	let (audio_admin, admin) = Channel::create();
	let _audio_service = spawn_dynamic_test_process(sched::root_domain(), audio_elf, audio_boot_user);
	send_cap(&audio_boot_kernel, b"SND", snd_service, Rights::ALL).expect("the driver channel");
	send_cap(&audio_boot_kernel, b"ADMIN", admin, Rights::ALL).expect("the admin channel");
	send_cap(&audio_boot_kernel, b"SERVE", service_server, Rights::SEND | Rights::RECEIVE | Rights::WAIT | Rights::TRANSFER).expect("the serve channel");
	sched::run_until_idle();
	assert_eq!(&audio_boot_kernel.recv().expect("AudioService online report").bytes[..], b"AudioService: online");

	// THE CAPTURE GRANT, WHICH IS NOT THE PLAYBACK ONE. `open-captures` is op 2 on `audio-admin`;
	// what it mints may record and may not make a sound, which the next block checks.
	let capture_grant = |corr: u32| -> alloc::sync::Arc<Channel> {
		let mut request = alloc::vec::Vec::new();
		request.extend_from_slice(&2u16.to_le_bytes());
		request.extend_from_slice(&corr.to_le_bytes());
		audio_admin.send(Message::new(request, alloc::vec::Vec::new())).expect("open-captures request");
		sched::run_until_idle();
		let reply = audio_admin.recv().expect("open-captures reply");
		assert_eq!(le_u32(&reply.bytes, 0), corr);
		assert_eq!(reply.bytes[4], 1, "the launcher may mint a capture connection");
		reply.caps.first().expect("the capture connection").object().into_any_arc().downcast::<Channel>().expect("the grant is a channel")
	};

	// A capture grant is not a playback grant. `beep` is op 1 and `open-stream` is op 2 on `audio`;
	// both are refused on a connection minted for recording.
	{
		let scoped = capture_grant(60);
		let mut denied_beep = alloc::vec::Vec::new();
		denied_beep.extend_from_slice(&1u16.to_le_bytes());
		denied_beep.extend_from_slice(&61u32.to_le_bytes());
		denied_beep.extend_from_slice(&440u16.to_le_bytes());
		denied_beep.extend_from_slice(&10u32.to_le_bytes());
		scoped.send(Message::new(denied_beep, alloc::vec::Vec::new())).expect("scoped beep request");
		let mut denied_stream = alloc::vec::Vec::new();
		denied_stream.extend_from_slice(&2u16.to_le_bytes());
		denied_stream.extend_from_slice(&62u32.to_le_bytes());
		denied_stream.extend_from_slice(&48_000u32.to_le_bytes());
		denied_stream.push(2);
		scoped.send(Message::new(denied_stream, alloc::vec::Vec::new())).expect("scoped open-stream request");
		sched::run_until_idle();
		assert_eq!(scoped.recv().expect("scoped beep denial").bytes[4], 0, "a recorder may not make a sound");
		assert_eq!(scoped.recv().expect("scoped open-stream denial").bytes[4], 0, "nor open a playback stream");
	}

	// Run `audiorec` to completion, answering every capture command the service sends the driver
	// with one period of `TONE`. Returns what the tool printed.
	let record = |arguments: &[u8], system: &mut StorageHarness| -> alloc::vec::Vec<u8> {
		let grant = capture_grant(70);
		let (bootstrap, child) = Channel::create();
		let (stdout, child_stdout) = Channel::create();
		let process = spawn_dynamic_test_process(sched::root_domain(), audiorec_elf, child);
		send_cap(&bootstrap, b"STDOUT", child_stdout, Rights::ALL).expect("the tool's stdout");
		bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new())).expect("endpoint run terminator");
		bootstrap.send(Message::new(launch_context(arguments, b"vol://system"), alloc::vec::Vec::new())).expect("the tool's arguments");
		send_cap(&bootstrap, b"SYSTEM", system.client.clone(), Rights::ALL).expect("the tool's system volume");
		for tag in [b"MEDIA".as_slice(), b"ISO".as_slice(), b"UDF".as_slice(), b"USB".as_slice(), b"RAM".as_slice(), b"TMP".as_slice()] {
			bootstrap.send(Message::new(tag.to_vec(), alloc::vec::Vec::new())).expect("an absent volume");
		}
		bootstrap.send(Message::new(b"READY".to_vec(), alloc::vec::Vec::new())).expect("volume bundle terminator");
		send_cap(&bootstrap, b"AUDIO_CAPTURE", grant, Rights::ALL).expect("the tool's capture grant");

		let mut line = None;
		for _ in 0..200_000 {
			system.pump();
			// The driver's side of the protocol: a one-byte message is a command, `1` asks for a
			// period and `2` ends the stream. Anything else here would be a playback period, which
			// this test never produces.
			if let Ok(command) = snd_host.recv() {
				match command.bytes.as_slice() {
					[1] => {
						let mut period = alloc::vec::Vec::with_capacity(PERIOD_BYTES);
						while period.len() < PERIOD_BYTES {
							period.extend_from_slice(&LEFT.to_le_bytes());
							period.extend_from_slice(&RIGHT.to_le_bytes());
						}
						snd_host.send(Message::new(period, alloc::vec::Vec::new())).expect("a captured period");
					}
					[2] => snd_host.send(Message::new(b"OK".to_vec(), alloc::vec::Vec::new())).expect("the capture stop ACK"),
					other => panic!("the service sent the driver something that is not a capture command: {other:?}"),
				}
			}
			if line.is_none()
				&& let Ok(message) = stdout.recv()
			{
				line = Some(message.bytes);
			}
			if line.is_some() && process.is_terminated() {
				break;
			}
			sched::run_until_idle();
		}
		assert!(process.is_terminated(), "the recorder exits");
		line.expect("the recorder prints a result")
	};

	// A second of 8 kHz mono, which is the interesting shape: both a downmix and a downsample.
	let report = record(b"-r 8000 -c 1 -s 1 vol://system/TAKE.WAV", &mut system);
	// THE TWO NUMBERS THAT ARE NOT ROUND, ASSERTED RATHER THAN GLOSSED:
	//
	// `dropped=22` is the tail of the period that straddles the one-second mark. A device period is
	// 512 frames at 48 kHz, which is about 85 frames at 8 kHz, and 8000 is not a multiple of 85 - so
	// the last period carries 22 frames past the length that was asked for and they are not written.
	// It is a real two-and-three-quarter milliseconds of captured audio that did not reach the file,
	// which is why the tool says so instead of rounding it away.
	//
	// `peak=172` is the largest buffer the TOOL ever holds: one converted period, 86 mono frames of
	// two bytes. Not 2048, which is the device period AudioService converts - the recorder never
	// sees one. That is the whole claim behind "an hour of audio costs one period of memory".
	assert_eq!(&report[..], b"audiorec: 8000Hz/1ch/16-bit 8000fr duration=1000ms bytes=16044 dropped=22 peak=172\n", "unexpected report: {}", alloc::string::String::from_utf8_lossy(&report));

	let written = system.open(b"vol://system/TAKE.WAV", 0xaad20).expect("the recording is on the volume");
	let parsed = wav::Wav::parse(&written).expect("the recording is a WAV file this tree can read");
	assert_eq!(parsed.metadata().rate, 8_000);
	assert_eq!(parsed.metadata().channels, 1);
	assert_eq!(parsed.metadata().bits_per_sample, 16);
	assert_eq!(parsed.metadata().frames, 8_000, "the RIFF and data lengths were patched to what was recorded");
	let mut decoded = alloc::vec::Vec::new();
	let frames = parsed.decoder().read_i16_le(8_000, &mut decoded).expect("the recording decodes");
	assert_eq!(frames, 8_000);
	assert!(decoded.chunks_exact(2).all(|sample| i16::from_le_bytes([sample[0], sample[1]]) == MONO), "a mono recording of a stereo source is the average of the two channels");

	// AND THE OTHER SHAPE: 48 kHz stereo, where nothing is remixed and nothing is resampled, so the
	// interleaving is what is under test. A file whose channels were swapped, doubled or collapsed
	// decodes into something other than the pair that arrived.
	let stereo_report = record(b"-r 48000 -c 2 -s 1 vol://system/STEREO.WAV", &mut system);
	assert!(stereo_report.starts_with(b"audiorec: 48000Hz/2ch/16-bit 48000fr duration=1000ms bytes=192044 "), "unexpected report: {}", alloc::string::String::from_utf8_lossy(&stereo_report));
	let stereo_written = system.open(b"vol://system/STEREO.WAV", 0xaad22).expect("the stereo recording is on the volume");
	let stereo_parsed = wav::Wav::parse(&stereo_written).expect("the stereo recording parses");
	assert_eq!(stereo_parsed.metadata().channels, 2);
	assert_eq!(stereo_parsed.metadata().frames, 48_000);
	let mut stereo_decoded = alloc::vec::Vec::new();
	stereo_parsed.decoder().read_i16_le(48_000, &mut stereo_decoded).expect("the stereo recording decodes");
	assert!(stereo_decoded.chunks_exact(4).all(|frame| i16::from_le_bytes([frame[0], frame[1]]) == LEFT && i16::from_le_bytes([frame[2], frame[3]]) == RIGHT), "the stereo recording is not the pair of channels that arrived");

	// A DESTINATION THAT EXISTS IS NOT OVERWRITTEN, and the refusal happens before the microphone
	// is ever opened - so a mistyped path costs nothing and destroys nothing.
	let refused = record(b"-r 8000 -c 1 -s 1 vol://system/TAKE.WAV", &mut system);
	assert_eq!(&refused[..], b"audiorec: destination exists (use --force)\n");
	let again = system.open(b"vol://system/TAKE.WAV", 0xaad21).expect("the first recording is still there");
	assert_eq!(again, written, "a refused run changed the file it refused to write");
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

tagged_test!(permission_manager_enforces_static_and_dynamic_probe_policy, [Service, Process, PermissionService], id = "kernel.applications.permission_manager_enforces_static_and_dynamic_probe_policy", covers = ["bin.permission_manager", "kernel", "services"]);
fn permission_manager_enforces_static_and_dynamic_probe_policy() {
	declare_permission_cohort("kernel.applications.permission_manager_enforces_static_and_dynamic_probe_policy", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("the permission probe scenario should run");
	assert!(!result.expected.is_empty(), "the granted file should not be empty");
	assert_eq!(result.probe_read, result.expected, "the sandboxed component read its one granted file through the storage grant");
	assert_eq!(result.probe_summary.as_slice(), b"storage=grant log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny audio-capture=deny app-assets=deny", "sandbox_probe was granted exactly its manifest - storage and log - and denied every other capability in the vocabulary");
	assert_eq!(result.request_read.as_slice(), b"storage denied", "request_probe's undeclared storage request was refused by the headless policy default");
	assert_eq!(result.request_summary.as_slice(), b"storage=deny log=grant network=deny device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny audio-capture=deny app-assets=deny storage=deny(dynamic)", "request_probe's static grants and dynamic denial were recorded independently");
}

tagged_test!(permission_manager_runs_tools_with_minimal_grants, [Service, Process, PermissionService], id = "kernel.applications.permission_manager_runs_tools_with_minimal_grants", covers = ["bin.permission_manager", "kernel", "services"]);
fn permission_manager_runs_tools_with_minimal_grants() {
	declare_permission_cohort("kernel.applications.permission_manager_runs_tools_with_minimal_grants", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("the governed tool scenario should run");
	assert_eq!(result.date_read.len(), 21, "date rendered a 20-byte ISO-8601 UTC instant and newline");
	assert_eq!(result.date_read[4], b'-', "date separates the year and month");
	assert_eq!(result.date_read[7], b'-', "date separates the month and day");
	assert_eq!(result.date_read[10], b'T', "date separates the date and time");
	assert_eq!(result.date_read[13], b':', "date separates the hour and minute");
	assert_eq!(result.date_read[16], b':', "date separates the minute and second");
	assert_eq!(result.date_read[19], b'Z', "date reports UTC");
	assert_eq!(result.date_read[20], b'\n', "date ended its stdout line");
	assert_eq!(result.date_summary.as_slice(), b"storage=deny log=deny network=deny device=deny config=deny time=grant audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny audio-capture=deny app-assets=deny", "date received only its time grant");
	assert_eq!(result.cat_read, result.expected, "cat printed its file through the storage grant");
	assert_eq!(result.ip_read.as_slice(), b"net0: 10.0.2.15  mac 52:54:00:12:34:56  mtu 1500  gateway 10.0.2.2\n", "ip rendered state from its typed NetworkService grant");
	assert_eq!(result.ip_summary.as_slice(), b"storage=deny log=deny network=grant device=deny config=deny time=deny audio=deny input=deny graph=deny resource=deny process=deny permission=deny supervisor=deny volumes=deny services=deny usb=deny display=deny input-keys=deny audio-stream=deny audio-capture=deny app-assets=deny", "ip received only its network grant");
}

tagged_test!(permission_manager_mints_scoped_application_grants, [Service, Process, PermissionService], id = "kernel.applications.permission_manager_mints_scoped_application_grants", covers = ["bin.permission_manager", "kernel", "services"]);
fn permission_manager_mints_scoped_application_grants() {
	declare_permission_cohort("kernel.applications.permission_manager_mints_scoped_application_grants", PermissionCohort::Scoped);
	let result = permission_scenario_result(PermissionCohort::Scoped).expect("the scoped application grant scenario should run");
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

tagged_test!(a_redirection_is_a_governed_pipeline_stage_and_the_consumer_holds_no_storage, [Service, Process, PermissionService], id = "kernel.applications.a_redirection_is_a_governed_pipeline_stage_and_the_consumer_holds_no_storage", covers = ["bin.permission_manager", "kernel", "services"]);
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
	declare_permission_cohort("kernel.applications.a_redirection_is_a_governed_pipeline_stage_and_the_consumer_holds_no_storage", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("the governed tool scenario should run");
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
tagged_test!(a_migrated_stream_tool_reads_a_pipeline_the_way_it_reads_a_path, [Service, Process, PermissionService], id = "kernel.applications.a_migrated_stream_tool_reads_a_pipeline_the_way_it_reads_a_path", covers = ["bin.permission_manager", "kernel", "services"]);
fn a_migrated_stream_tool_reads_a_pipeline_the_way_it_reads_a_path() {
	// `redirect_in motd.txt | wc`, where `wc` was given NO PATH AND NO VOLUME ARGUMENT.
	//
	// Before the migration that line was a usage error: every one of these tools refused when it
	// got no path, because a path was the only input it had. What makes this pass is `Source`
	// answering "stdin" when there is a stdin - and the only thing that tells it so is the presence
	// of the endpoint, which is a capability the launch either carried or did not.
	declare_permission_cohort("kernel.applications.a_migrated_stream_tool_reads_a_pipeline_the_way_it_reads_a_path", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("the governed tool scenario should run");
	let counted: &[u8] = &result.stream_reads[2];
	assert!(!counted.is_empty(), "the counting pipeline produced output at all");
	// `motd.txt` is two lines, and `wc` prints the line count first. A `wc` that read nothing
	// prints a leading zero and one that lost a window prints a one, so the digit is the assertion.
	assert!(counted.starts_with(b"2 "), "wc counted the whole stream: {:?}", core::str::from_utf8(counted));
}

tagged_test!(a_consumer_that_stops_early_ends_the_pipeline_instead_of_hanging_it, [Service, Process, PermissionService], id = "kernel.applications.a_consumer_that_stops_early_ends_the_pipeline_instead_of_hanging_it", covers = ["bin.permission_manager", "kernel", "services"]);
fn a_consumer_that_stops_early_ends_the_pipeline_instead_of_hanging_it() {
	// `redirect_in motd.txt | head -n 1` - the broken-pipe case, and the thing it really pins is
	// that the run ENDS. `head` takes its line and drops its source, which closes the read end;
	// the producer discovers it at its next write and stops. Nothing in `head` asks for that - it
	// falls out of owning the endpoint - and if it did not work the producer would sit blocked on
	// a consumer that is gone and this scenario would never return.
	declare_permission_cohort("kernel.applications.a_consumer_that_stops_early_ends_the_pipeline_instead_of_hanging_it", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("the governed tool scenario should run");
	let taken: &[u8] = &result.stream_reads[1];
	assert!(taken.windows(5).any(|window| window == b"MOTD:"), "head printed the first line: {:?}", core::str::from_utf8(taken));
	// AND NOT THE SECOND. A `head` that ignored its count would print the whole file, which on a
	// two-line input is the difference between a limit that works and one that is never reached.
	assert!(!taken.windows(5).any(|window| window == b"Files"), "head stopped at one line: {:?}", core::str::from_utf8(taken));
}

tagged_test!(a_fan_out_stage_with_an_unwritable_destination_still_carries_the_stream, [Service, Process, PermissionService], id = "kernel.applications.a_fan_out_stage_with_an_unwritable_destination_still_carries_the_stream", covers = ["bin.permission_manager", "kernel", "services"]);
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
	declare_permission_cohort("kernel.applications.a_fan_out_stage_with_an_unwritable_destination_still_carries_the_stream", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("the governed tool scenario should run");
	let counted: &[u8] = &result.stream_reads[0];
	// The destination really was refused - otherwise this test would be measuring the ordinary
	// three-stage case and calling it the failure case. The diagnostic shares the channel because
	// a stage's stderr is a duplicate of the caller's terminal, which is this test's channel.
	assert!(counted.windows(23).any(|window| window == b"cannot open for writing"), "tee's destination was refused: {:?}", core::str::from_utf8(counted));
	// AND THE STREAM STILL ARRIVED. `motd.txt` is two lines of 83 bytes, so this is the whole file
	// through three stages with the middle one's destination unusable.
	assert!(counted.windows(10).any(|window| window == b"2 13 83 83"), "the bytes reached the far end anyway: {:?}", core::str::from_utf8(counted));
}

tagged_test!(a_typed_line_goes_through_the_real_shell_and_comes_back_as_a_pipeline, [Service, Process, PermissionService, Shell], id = "kernel.applications.a_typed_line_goes_through_the_real_shell_and_comes_back_as_a_pipeline", covers = ["bin.permission_manager", "kernel", "services"]);
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
	declare_permission_cohort("kernel.applications.a_typed_line_goes_through_the_real_shell_and_comes_back_as_a_pipeline", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("the governed tool scenario should run");
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

tagged_test!(a_command_word_on_its_own_runs_the_command, [Service, Process, PermissionService, Shell], id = "kernel.applications.a_command_word_on_its_own_runs_the_command", covers = ["bin.permission_manager", "kernel", "services"]);
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
	declare_permission_cohort("kernel.applications.a_command_word_on_its_own_runs_the_command", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("the governed tool scenario should run");
	let out: &[u8] = &result.shell_read;
	let says = |needle: &[u8]| out.windows(needle.len()).any(|window| window == needle);
	assert!(!says(b"unknown command: which"), "a command word on its own is not an unknown command: {:?}", core::str::from_utf8(out));
	// AND IT RAN, which the absence of an error does not show on its own - a line silently dropped
	// looks identical. This is the tool's own output, so it was launched.
	assert!(says(b"which: usage:"), "the bare command word launched the tool, which answered for itself: {:?}", core::str::from_utf8(out));
}

tagged_test!(merging_the_error_stream_sends_a_stages_diagnostics_down_its_own_edge, [Service, Process, PermissionService], id = "kernel.applications.merging_the_error_stream_sends_a_stages_diagnostics_down_its_own_edge", covers = ["bin.permission_manager", "kernel", "services"]);
fn merging_the_error_stream_sends_a_stages_diagnostics_down_its_own_edge() {
	declare_permission_cohort("kernel.applications.merging_the_error_stream_sends_a_stages_diagnostics_down_its_own_edge", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("the governed tool scenario should run");
	assert!(result.merged_read.windows(4).any(|window| window == b"in> "), "the consumer read something: `readln` only prints its prefix for a line it took off its input, got {:?}", core::str::from_utf8(&result.merged_read));
	assert!(result.merged_read.windows(4).any(|window| window == b"cat:"), "and what it read is the PRODUCER'S DIAGNOSTIC, which without the flag goes to the terminal instead: {:?}", core::str::from_utf8(&result.merged_read));
}

tagged_test!(a_governed_pipeline_starts_as_one_transaction_and_carries_data, [Service, Process, PermissionService], id = "kernel.applications.a_governed_pipeline_starts_as_one_transaction_and_carries_data", covers = ["bin.permission_manager", "kernel", "services"]);
fn a_governed_pipeline_starts_as_one_transaction_and_carries_data() {
	// `echo hello | readln` through PermissionManager: two stages, each authorized against
	// its own manifest, the edge between them allocated by the broker, and both released
	// together. `readln` prefixes what it reads with `in> `, so this distinguishes a consumer
	// that actually read its producer's bytes from a producer whose output merely reached the
	// terminal - which is what a pipeline that was never really wired would look like.
	declare_permission_cohort("kernel.applications.a_governed_pipeline_starts_as_one_transaction_and_carries_data", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("the governed tool scenario should run");
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
	let _ = Message::new(alloc::vec::Vec::new(), alloc::vec::Vec::new());
}

tagged_test!(
	the_command_tools_run_governed_and_read_in_windows,
	[Service, Process, PermissionService, Storage],
	id = "kernel.applications.the_command_tools_run_governed_and_read_in_windows",
	covers = [
		"bin.cut",
		"bin.grep",
		"bin.head",
		"bin.hexdump",
		"bin.permission_manager",
		"bin.pwd",
		"bin.sort",
		"bin.tail",
		"bin.wc",
		"bin.which",
		"cli",
		"storage",
		"volume-client"
	]
);
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
	declare_permission_cohort("kernel.applications.the_command_tools_run_governed_and_read_in_windows", PermissionCohort::Base);
	let result = permission_scenario_result(PermissionCohort::Base).expect("the governed tool scenario should run");
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

tagged_test!(a_full_volume_says_it_is_full_rather_than_asking_for_a_retry, [Service, Storage, Filesystem], id = "kernel.applications.a_full_volume_says_it_is_full_rather_than_asking_for_a_retry", covers = ["libermemfs", "storage"]);
fn a_full_volume_says_it_is_full_rather_than_asking_for_a_retry() {
	// The test beside this one already proves a full volume REFUSES the next write. What it cannot
	// see is what the refusal says, because it reads a bool - and for every volume in this system
	// the answer was `again`, "try again, it may work later", which on a volume with no room left
	// is advice that leads nowhere. `fs-core` has told `no-space` and `no-memory` apart since it
	// existed; the service boundary flattened both into one retry, with a comment saying it did so
	// because the protocol had no finer word. IDL-006 gave it every one of them.
	let (_volume, package) = scenario_packages().expect("scenario packages");
	let storage_elf = package.lookup(b"storage_service.lsexe").expect("storage service");
	let mut ram = StorageHarness::start_memory(storage_elf, b"RAMVOL", 4096);

	assert!(ram.write(b"vol://ram/kept", b"written before the volume filled up", 0x7401), "the volume takes a first write");
	let payload = alloc::vec![b'f'; 4096];
	assert_eq!(ram.write_result(b"vol://ram/toobig", &payload, 0x7402), Some(Err(NO_SPACE)), "a write past the end of the volume is refused AS OUT OF SPACE");

	// AND NOTHING ELSE CHANGED. A refusal that took the earlier file with it, or left the service
	// unable to answer, would be a worse failure than the wrong error code - and both are what an
	// out-of-space path is most likely to get wrong, because it fails halfway through its work.
	assert_eq!(ram.open(b"vol://ram/kept", 0x7403), Some(b"written before the volume filled up".to_vec()), "the write that came before is intact");
	assert_eq!(ram.open(b"vol://ram/toobig", 0x7404), None, "the write that was refused left nothing behind");
	assert!(ram.write(b"vol://ram/small", b"x", 0x7405), "the service still takes a write that fits");

	// The name is not the reason. An unusable name is `invalid`, not `no-space`, and a mapping that
	// answered `no-space` for everything would pass every assertion above.
	let overlong: alloc::vec::Vec<u8> = b"vol://ram/".iter().copied().chain(core::iter::repeat(b'n').take(300)).collect();
	assert_eq!(ram.write_result(&overlong, b"x", 0x7406), Some(Err(INVALID)), "a name too long for the volume is still refused as a bad name");
}

// The two `base.error` codes this file asserts on, as the bytes they travel as. Named here because
// `8` in an assertion says nothing about what it means.
const NO_SPACE: u8 = 8;
const INVALID: u8 = 2;

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

// P02M0139: the fixture boundary itself. These drive the REAL state machine on cells of their own,
// so they never poison a class a cohort consumer will ask for - which is why they can live in the
// ordinary suite and run in every full sweep rather than only when someone remembers them.
//
// They are not members of the permission cohort and declare no cohort: what they exercise is the
// machine, not PermissionManager.
tagged_test!(an_injected_fixture_setup_failure_stays_failed_for_the_whole_run, [Service, Process, PermissionService], id = "kernel.applications.an_injected_fixture_setup_failure_stays_failed_for_the_whole_run", covers = ["kernel"]);
fn an_injected_fixture_setup_failure_stays_failed_for_the_whole_run() {
	for cohort in [PermissionCohort::Base, PermissionCohort::Scoped] {
		let cell = permission_fixture_injection_cell(cohort);
		let first = permission_result_for_regression(cell, || Err("injected setup failure"));
		assert_eq!(first.err(), Some("injected setup failure"), "the requesting test is told what went wrong, verbatim");
		// AND THE NEXT CONSUMER GETS THE SAME ANSWER. A second attempt would be retry-shopping: the
		// setup that failed is the one every consumer of this class needs, and a different result
		// from a second try is not evidence about anything. The builder that must not run again is
		// proved by handing this one a builder that would succeed.
		let second = permission_result_for_regression(cell, || Ok(empty_permission_result()));
		assert_eq!(second.err(), Some("injected setup failure"), "a failed class stays failed and does not rebuild");
	}
}

tagged_test!(a_consumer_that_collides_with_the_builder_is_refused_rather_than_given_a_partial_result, [Service, Process, PermissionService], id = "kernel.applications.a_consumer_that_collides_with_the_builder_is_refused_rather_than_given_a_partial_result", covers = ["kernel"]);
fn a_consumer_that_collides_with_the_builder_is_refused_rather_than_given_a_partial_result() {
	let cell = permission_fixture_collision_cell();
	// The collision, forced from inside the builder: while this one is building, the cell is
	// `Building`, and a second consumer arriving now must be refused rather than handed anything.
	let mut collided = None;
	let built = permission_result_for_regression(cell, || {
		collided = Some(permission_result_for_regression(cell, || Ok(empty_permission_result())));
		Ok(empty_permission_result())
	});
	let collided = collided.expect("the inner consumer ran while the cell was Building");
	assert!(collided.is_err(), "a consumer arriving mid-build is refused, never given a partly initialized result");
	// AND THE BUILDER STILL REACHES A TERMINAL STATE. Stranding `Building` is the failure mode this
	// machine is shaped to avoid: every later consumer would be refused forever.
	assert!(built.is_ok(), "the builder published its result despite the collision");
	let after = permission_result_for_regression(cell, || Err("this builder must not run"));
	assert!(after.is_ok(), "the cell is Ready afterwards, not stranded in Building");
}

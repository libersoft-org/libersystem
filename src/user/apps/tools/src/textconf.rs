// textconf - the text and render conformance run, executed inside a booted guest.
//
// WHY THIS IS A PROGRAM AND NOT A HOST TEST. "The host tests pass on all three architectures" has no
// executable meaning here: a host test runs on the machine that built the image, once, on one
// architecture. What the three-architecture claim is ABOUT is the stack running on the target - the
// same shaping, the same layout, the same rasteriser arithmetic - and the only way to make that claim
// checkable is to run it there. So this is a staged executable, launched under its own permission
// manifest, and what it prints is the verdict the suite reads.
//
// AND IT IS GOVERNED, which is the second half. It holds ONE capability - `font-catalogue` - and
// reaches every face through it: list the installed faces, ask for the length of the one it wants,
// create a memory object, narrow a handle down to map and write, and have the catalogue fill it.
// A conformance program that opened a file would be testing a stack that no application can use.
//
// TWO PARTS, AND THE SECOND IS THE ONE THE CLAIM IS ABOUT.
//
//   the staged face   one Latin string through the last-resort face, at a size where every edge
//                     lands on a pixel boundary and at one where none does (`textconf/latin.rs`)
//   the corpus        the authored conformance cases - Arabic in its joining forms, Devanagari
//                     reordered, Thai decomposed and stacked, and a paragraph of two directions -
//                     shaped and laid out by the stages the host gate drives, compared EXACTLY with the
//                     expectations that gate reads, and drawn (`textconf/shaping.rs`)
//
// The last-resort face is staged in every image; the conformance faces are staged in none. The
// suite that runs this program installs them, with their declarations, into the font directory of
// the volume it runs against, and this program finds each one by the digest its pin states - so a
// corpus face that is missing, or is not the face the expectations were written for, is a failure
// and never a quietly smaller run.
//
// THE ORACLE IS THE GEOMETRY AND NOT A PREVIOUS RUN, in both parts: every glyph drawn here is made of
// axis-aligned rectangles, so the coverage of each pixel is computed from the outline the face
// declares. See `textconf/raster.rs`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use font_client::FontClient;
use font_parse::Face;
use proto::generated::liber::font::v1 as font;
use proto::system::LaunchContext;
use rt::*;

// THE AUTHORED CASES, which the host gate `text-corpus` includes from here as well.
#[path = "textconf/corpus.rs"]
mod corpus;
#[path = "textconf/latin.rs"]
mod latin;
// The corpus pins, generated beside this program by `font-gen`.
#[path = "textconf/pins.rs"]
mod pins;
#[path = "textconf/raster.rs"]
mod raster;
#[path = "textconf/shaping.rs"]
mod shaping;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	inherit_stdout(bootstrap);
	let _context: Option<LaunchContext> = recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode);
	// THE ONE CAPABILITY. A conformance program holding a volume client would be proving something
	// about a stack nothing else can reach.
	let catalogue: u64 = match recv_tagged(bootstrap, &mut buf, b"FONT") {
		Some(handle) => handle,
		None => {
			print(b"textconf: FAILED no font catalogue capability\n");
			exit();
		}
	};

	let bytes: Vec<u8> = match load_face(catalogue, &Wanted::Family(latin::FAMILY)) {
		Ok(bytes) => bytes,
		Err(problem) => {
			print(format!("textconf: FAILED {problem}\n").as_bytes());
			exit();
		}
	};
	print(format!("textconf: loaded {} bytes of '{}' through the catalogue\n", bytes.len(), latin::FAMILY).as_bytes());

	let face = match Face::open(&bytes, 0) {
		Ok(face) => face,
		Err(error) => {
			print(format!("textconf: FAILED the staged face does not open: {error:?}\n").as_bytes());
			exit();
		}
	};

	let mut failures = 0usize;
	for (size, exact) in [(latin::ALIGNED_SIZE, true), (latin::FRACTIONAL_SIZE, false)] {
		match latin::run(&face, &bytes, size, exact, &mut |line| say(line)) {
			Ok(report) => say(&report),
			Err(problem) => {
				print(format!("textconf: FAILED at {size} px: {problem}\n").as_bytes());
				failures += 1;
			}
		}
	}

	let summary = match scripts(catalogue) {
		Ok(summary) => summary,
		Err(problem) => {
			print(format!("textconf: FAILED {problem}\n").as_bytes());
			failures += 1;
			String::new()
		}
	};
	if failures == 0 {
		print(format!("textconf: PASSED the Latin corpus renders to the analytic oracle at both sizes, and {summary}\n").as_bytes());
	}
	exit();
}

/// One line of this program's verdict.
fn say(line: &str) {
	print(format!("textconf: {line}\n").as_bytes());
}

/// THE SCRIPT CORPUS: every case `corpus.rs` names for the guest, shaped, compared and drawn.
///
/// A CASE THAT FAILS IS REPORTED AND THE REST STILL RUN, because one wrong answer says less than a
/// list of them - and the verdict line says how many failed. A face that cannot be reached is the end
/// of this part: nothing after it would be about the corpus.
fn scripts(catalogue: u64) -> Result<String, String> {
	let pinned = shaping::pins(pins::PINS)?;

	// THE FACES THE CASES NAME, each reached through the catalogue by the digest its pin states - the
	// identity the catalogue issues is the digest of the file, so this is the face the expectations
	// were written for or it is nothing.
	let mut files: Vec<&str> = Vec::new();
	for name in corpus::GUEST_CASES {
		let case = corpus::CASES.iter().find(|case| case.name == *name).ok_or_else(|| format!("the corpus has no case named '{name}'"))?;
		if !files.contains(&case.face) {
			files.push(case.face);
		}
	}
	for name in corpus::GUEST_PARAGRAPHS {
		let paragraph = corpus::PARAGRAPHS.iter().find(|paragraph| paragraph.name == *name).ok_or_else(|| format!("the corpus has no paragraph named '{name}'"))?;
		if !files.contains(&paragraph.face) {
			files.push(paragraph.face);
		}
	}
	let mut loaded: Vec<(&shaping::Pinned<'_>, Vec<u8>)> = Vec::new();
	for file in files {
		let pin = pinned.iter().find(|pin| pin.file == file).ok_or_else(|| format!("the pins name no face {file}"))?;
		let bytes = load_face(catalogue, &Wanted::Digest(&pin.digest)).map_err(|problem| format!("{file}: {problem}"))?;
		say(&format!("loaded {} bytes of {file} through the catalogue, the face its pin names", bytes.len()));
		loaded.push((pin, bytes));
	}
	let mut installed: Vec<shaping::Installed<'_>> = Vec::new();
	for (pin, bytes) in &loaded {
		let face = Face::open(bytes, 0).map_err(|error| format!("{} does not open: {error:?}", pin.file))?;
		let identity = font_contract::FaceRef { face: font_contract::FaceIdentity { file: font_contract::face::FileIdentity(pin.digest), index: 0 }, generation: font_contract::face::Generation(1) };
		installed.push(shaping::Installed { file: pin.file, glyphs: &pin.glyphs, face, identity });
	}
	let installed_for = |file: &str| installed.iter().find(|face| face.file == file).ok_or_else(|| format!("{file} was not reached"));

	let mut failed = 0usize;
	let mut cases = 0usize;
	for name in corpus::GUEST_CASES {
		let Some(case) = corpus::CASES.iter().find(|case| case.name == *name) else { continue };
		let face = installed_for(case.face)?;
		let outcome = (|| -> Result<(), String> {
			let buffer = shaping::shape(face, case)?;
			shaping::compare(face, &buffer, case.expect)?;
			shaping::refuses_the_wrong_answer(face, case, &buffer)?;
			let script = shaping::script_of(case.text)?;
			say(&format!("shaped '{name}' ({}) -> {} glyph(s) exactly as authored, and a wrong glyph, cluster, advance, offset or count is refused", core::str::from_utf8(&script).unwrap_or("?"), buffer.len()));
			for (size, exact) in [(shaping::ALIGNED_SIZE, true), (shaping::FRACTIONAL_SIZE, false), (shaping::SHARED_SIZE, false)] {
				let report = shaping::draw_case(face, case, &buffer, size, exact, &mut refusals_only).map_err(|problem| format!("at {size} px: {problem}"))?;
				say(&format!("drawn '{name}' at {size} px: {report}"));
			}
			Ok(())
		})();
		match outcome {
			Ok(()) => cases += 1,
			Err(problem) => {
				say(&format!("case '{name}' does not conform: {problem}"));
				failed += 1;
			}
		}
	}
	let mut paragraphs = 0usize;
	for name in corpus::GUEST_PARAGRAPHS {
		let Some(paragraph) = corpus::PARAGRAPHS.iter().find(|paragraph| paragraph.name == *name) else { continue };
		let face = installed_for(paragraph.face)?;
		let outcome = (|| -> Result<(), String> {
			let visual = shaping::lay_out(face, paragraph)?;
			shaping::compare_paragraph(face, paragraph, &visual)?;
			shaping::paragraph_refuses_the_wrong_answer(face, paragraph, &visual)?;
			say(&format!("laid out '{name}' -> level {}, {} line(s) in the authored order with the authored glyphs, and a reversed order or a line's wrong glyphs is refused", paragraph.paragraph_level, visual.order.len()));
			for (size, exact) in [(shaping::ALIGNED_SIZE, true), (shaping::FRACTIONAL_SIZE, false), (shaping::SHARED_SIZE, false)] {
				let report = shaping::draw_paragraph(face, paragraph, &visual, size, exact, &mut refusals_only).map_err(|problem| format!("at {size} px: {problem}"))?;
				say(&format!("drawn '{name}' at {size} px: {report}"));
			}
			Ok(())
		})();
		match outcome {
			Ok(()) => paragraphs += 1,
			Err(problem) => {
				say(&format!("paragraph '{name}' does not conform: {problem}"));
				failed += 1;
			}
		}
	}
	if failed > 0 {
		return Err(format!("{failed} of the corpus case(s) do not conform"));
	}
	Ok(format!("{cases} script case(s) and {paragraphs} paragraph(s) shape, lay out and render exactly as authored"))
}

/// What the glyph provider says while a corpus case is drawn, kept to what is worth a line: a form it
/// refused. Every corpus glyph has an outline, so a refusal is the only news.
fn refusals_only(line: &str) {
	if line.contains("refusing") {
		say(line);
	}
}

/// Which face to ask the catalogue for.
enum Wanted<'w> {
	/// By the family its declaration states - the staged face, which nothing pins.
	Family(&'w str),
	/// By the digest of its bytes - a corpus face, which the pins name exactly.
	Digest(&'w [u8; 32]),
}

/// The face's bytes, through the catalogue and through nothing else.
fn load_face(catalogue: u64, wanted: &Wanted<'_>) -> Result<Vec<u8>, String> {
	let mut client = FontClient::new(catalogue);
	let faces: Vec<font::FaceRecord> = match client.list() {
		Some(Ok(faces)) => faces,
		Some(Err(error)) => return Err(format!("the catalogue refused the listing ({error:?})")),
		None => return Err(String::from("no answer from the font catalogue")),
	};
	let record = match wanted {
		Wanted::Family(family) => faces.iter().find(|face| face.family == *family).ok_or_else(|| format!("the catalogue lists {} face(s) and none is '{family}'", faces.len()))?,
		Wanted::Digest(digest) => faces.iter().find(|face| face.identity.digest.as_slice() == digest.as_slice()).ok_or_else(|| format!("the catalogue lists {} face(s) and none is the face its pin names - the suite installs the corpus faces, and a face that is not the pinned one is a different test", faces.len()))?,
	};
	let info = match client.resolve_info(&record.identity) {
		Some(Ok(info)) => info,
		Some(Err(error)) => return Err(format!("the catalogue refused to size the face ({error:?})")),
		None => return Err(String::from("no answer to resolve-info")),
	};
	if info.length == 0 || info.length > 8 * 1024 * 1024 {
		return Err(format!("the catalogue answered a length of {} bytes", info.length));
	}

	// THE HANDLE IS NARROWED BEFORE THE SEND, and the original is kept: `duplicate` narrows and never
	// widens, so what the catalogue receives can map and write and nothing else, and this program
	// still holds the object it created.
	let object = memory_object_create(info.length);
	if object < 0 {
		return Err(format!("a {} byte object could not be created ({object})", info.length));
	}
	let object = object as u64;
	let narrowed = duplicate(object, RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_TRANSFER);
	if narrowed < 0 {
		return Err(format!("the object handle could not be narrowed ({narrowed})"));
	}
	let written = match client.resolve_into(&record.identity, info.generation, narrowed as u64) {
		Some(Ok(font::ResolveOutcome::Filled(written))) => written,
		Some(Ok(outcome)) => return Err(format!("the catalogue did not fill the object: {outcome:?}")),
		Some(Err(error)) => return Err(format!("the catalogue refused the read ({error:?})")),
		None => return Err(String::from("no answer to resolve-into")),
	};
	if written != info.length {
		return Err(format!("the catalogue wrote {written} of {} bytes", info.length));
	}
	let Some(base) = (unsafe { map_object(object) }) else {
		return Err(String::from("the filled object could not be mapped"));
	};
	// SAFETY: the kernel mapped the whole object at `base` and it stays mapped until `unmap_object`,
	// which is below; `written` is the catalogue's own answer and is checked against the length the
	// object was created with.
	let bytes = unsafe { core::slice::from_raw_parts(base as *const u8, written as usize) }.to_vec();
	unmap_object(object);
	Ok(bytes)
}

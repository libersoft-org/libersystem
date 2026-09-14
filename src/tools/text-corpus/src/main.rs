//! THE TEXT CONFORMANCE GATE: what the shaper should have done, said in advance.
//!
//! WHAT MAKES THIS DIFFERENT FROM THE UNIT TESTS BELOW IT. A shaping unit test hands the shaper one
//! lookup and checks that it fires. That is necessary and it is not conformance: it cannot show a
//! face whose script list, feature list and lookup list must be walked in the order the format
//! defines, with several scripts in one face and positional features that must reach one letter and
//! not its neighbour. This gate reads WHOLE FACES - authored by `src/tools/font-gen`, pinned by
//! SHA-256 - and compares the shaped result against expectations written from the FACE'S OWN DESIGN
//! rather than from a previous run of the shaper.
//!
//! WHY THE EXPECTATIONS ARE LITERALS AND NOT COMPUTED. A gate that derived its expectation from the
//! face by a second implementation of the rules would be checking two shapers against each other,
//! and a gate that recorded what the shaper produced would approve whatever it does today. The
//! numbers below come from the corpus design: an advance is what the face's `hmtx` states, a kern is
//! the pair adjustment the face declares, and a mark offset is the anchor arithmetic the format
//! defines - `base anchor - mark anchor - everything the pen advanced between them`.
//!
//! THE GLYPHS ARE NAMED, NEVER NUMBERED. The pin file maps each face's glyph names to its indices,
//! so a corpus that renumbered its glyphs and still shaped correctly passes - which is right, since
//! a glyph index is internal to a face - and a corpus that shaped differently fails.
//!
//! AND IT PROVES IT REFUSES BEFORE IT APPROVES. `--self-test` corrupts a pinned face, breaks an
//! expectation and drops a glyph name, and requires each to be caught.

use bootproto::sha256;
use font_parse::Face;
use font_shape::Buffer;
use font_shape::buffer::GlyphInfo;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod cases;

/// One expected glyph: which glyph, which cluster it came from, and where it sits.
pub struct Expect {
	pub glyph: &'static str,
	pub cluster: u32,
	pub x_advance: i32,
	pub x_offset: i32,
	pub y_offset: i32,
}

/// One shaping case: a string, the face that draws it, and what must come out.
pub struct Case<'a> {
	pub name: &'static str,
	pub face: &'static str,
	pub text: &'static str,
	/// BORROWED RATHER THAN `'static` so the self-test can build a deliberately wrong one and drive
	/// it through the same comparison the real cases use. A second comparison written for the
	/// self-test would be the thing that goes stale.
	pub expect: &'a [Expect],
}

/// A face as the pin file describes it: its digest, and what its glyphs are called.
struct Pinned {
	digest: [u8; 32],
	glyphs: BTreeMap<String, u16>,
}

fn main() -> std::process::ExitCode {
	let self_test = std::env::args().any(|argument| argument == "--self-test");
	let Some(root) = repository_root() else {
		eprintln!("text-corpus: could not find the repository root");
		return std::process::ExitCode::FAILURE;
	};
	let directory = root.join("src/tests/fonts");

	let pins = match read_pins(&directory.join("corpus.pins")) {
		Ok(pins) => pins,
		Err(problem) => {
			eprintln!("text-corpus: {problem}");
			return std::process::ExitCode::FAILURE;
		}
	};

	if self_test {
		return match self_check(&directory, &pins) {
			Ok(()) => std::process::ExitCode::SUCCESS,
			Err(problem) => {
				eprintln!("text-corpus: {problem}");
				std::process::ExitCode::FAILURE
			}
		};
	}

	let mut failures = 0usize;
	let mut checked = 0usize;

	// THE PIN FIRST, BEFORE ANYTHING IS SHAPED. A corpus whose bytes drifted is a different test
	// wearing the same name, and every result below it would be about a face nobody pinned.
	let mut bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
	for (file, pinned) in &pins {
		match std::fs::read(directory.join(file)) {
			Ok(found) => {
				let digest = sha256::digest(&found);
				if digest != pinned.digest {
					eprintln!("text-corpus: {file} is not the face this corpus pins");
					eprintln!("text-corpus:   pinned  {}", hex(&pinned.digest));
					eprintln!("text-corpus:   staged  {}", hex(&digest));
					failures += 1;
					continue;
				}
				println!("text-corpus: {file} matches its pin, {} glyph(s) named", pinned.glyphs.len());
				bytes.insert(file.clone(), found);
			}
			Err(error) => {
				eprintln!("text-corpus: {file} could not be read: {error}");
				failures += 1;
			}
		}
	}
	if failures > 0 {
		eprintln!("text-corpus: regenerate with `cargo run --manifest-path src/tools/font-gen/Cargo.toml`");
		return std::process::ExitCode::FAILURE;
	}

	for case in cases::CASES {
		let Some(face_bytes) = bytes.get(case.face) else {
			eprintln!("text-corpus: {}: the case names a face the pin file does not: {}", case.name, case.face);
			failures += 1;
			continue;
		};
		let Some(pinned) = pins.get(case.face) else { continue };
		checked += 1;
		match run_case(face_bytes, pinned, case) {
			Ok(()) => println!("text-corpus: {} -> {} glyph(s) as expected", case.name, case.expect.len()),
			Err(problem) => {
				eprintln!("text-corpus: {}: {problem}", case.name);
				failures += 1;
			}
		}
	}

	// THE BIDI CASES ARE NOT SHAPING CASES and are run through the pipeline rather than the shaper:
	// what a mixed-direction paragraph is about is the VISUAL ORDER of its runs, which is decided
	// after line breaking and not by any font.
	for case in cases::PARAGRAPHS {
		checked += 1;
		match run_paragraph(case) {
			Ok(()) => println!("text-corpus: {} -> the visual order is what the levels require", case.name),
			Err(problem) => {
				eprintln!("text-corpus: {}: {problem}", case.name);
				failures += 1;
			}
		}
	}

	// AND THE VARIATION CASES ASK THE FACE, NOT THE SHAPER: an instance's advance and its outline
	// come from two different tables, and the case that matters is the one where only one of them
	// moves.
	for case in cases::INSTANCES {
		let Some(face_bytes) = bytes.get(case.face) else { continue };
		let Some(pinned) = pins.get(case.face) else { continue };
		checked += 1;
		match run_instance(face_bytes, pinned, case) {
			Ok(()) => println!("text-corpus: {} -> the instance measures what the face declares", case.name),
			Err(problem) => {
				eprintln!("text-corpus: {}: {problem}", case.name);
				failures += 1;
			}
		}
	}

	if failures > 0 {
		eprintln!("text-corpus: {failures} of {checked} case(s) did not shape as the corpus declares");
		return std::process::ExitCode::FAILURE;
	}
	println!("text-corpus: {checked} case(s) over {} pinned face(s)", pins.len());
	std::process::ExitCode::SUCCESS
}

/// Shape one case and compare it, glyph by glyph.
fn run_case(bytes: &[u8], pinned: &Pinned, case: &Case<'_>) -> Result<(), String> {
	let face = Face::open(bytes, 0).map_err(|error| format!("the face does not open: {error:?}"))?;
	let characters: Vec<char> = case.text.chars().collect();

	// THE BUFFER IS BUILT FROM THE `cmap` AND FROM NOTHING ELSE, which is the one thing a caller
	// must not get wrong: a glyph id invented here would make every expectation below a test of this
	// gate's arithmetic.
	let mut buffer = Buffer::default();
	let mut at = 0u32;
	for character in &characters {
		let glyph = face.glyph_for(*character).map_err(|error| format!("the face refused a character: {error:?}"))?.ok_or_else(|| format!("the face has no glyph for U+{:04X}, which the corpus says it covers", *character as u32))?;
		buffer.infos.push(GlyphInfo::new(glyph, at));
		buffer.positions.push(font_shape::Position::default());
		at += character.len_utf8() as u32;
	}

	font_shape::shape_run(&face, &mut buffer, &characters, *b"dflt").map_err(|error| format!("shaping refused the run: {error:?}"))?;

	if buffer.len() != case.expect.len() {
		return Err(format!("expected {} glyph(s), got {}: {}", case.expect.len(), buffer.len(), describe(&buffer, pinned)));
	}
	for (index, expect) in case.expect.iter().enumerate() {
		let wanted = *pinned.glyphs.get(expect.glyph).ok_or_else(|| format!("the case names a glyph the face does not declare: {}", expect.glyph))?;
		let info = buffer.infos[index];
		let position = buffer.positions[index];
		if info.glyph != wanted {
			return Err(format!("glyph {index} is {} where the corpus declares {}: {}", name_of(pinned, info.glyph), expect.glyph, describe(&buffer, pinned)));
		}
		if info.cluster != expect.cluster {
			return Err(format!("glyph {index} ({}) came from cluster {} where the corpus declares {}", expect.glyph, info.cluster, expect.cluster));
		}
		if position.x_advance != expect.x_advance {
			return Err(format!("glyph {index} ({}) advances {} where the corpus declares {}", expect.glyph, position.x_advance, expect.x_advance));
		}
		if position.x_offset != expect.x_offset || position.y_offset != expect.y_offset {
			return Err(format!("glyph {index} ({}) sits at ({}, {}) where the corpus declares ({}, {})", expect.glyph, position.x_offset, position.y_offset, expect.x_offset, expect.y_offset));
		}
	}
	Ok(())
}

/// A mixed-direction paragraph: the levels, and the order its clusters are drawn in.
fn run_paragraph(case: &cases::Paragraph<'_>) -> Result<(), String> {
	use text_pipeline::stages::{self, Faced, Shaped};

	let source = text_pipeline::Source::new(case.text).map_err(|error| format!("the paragraph is refused: {error:?}"))?;
	let items = stages::itemise(source);
	let levelled = stages::resolve_levels(items, case.direction);
	if levelled.levels.paragraph_level != case.paragraph_level {
		return Err(format!("the paragraph level is {} where the corpus declares {}", levelled.levels.paragraph_level, case.paragraph_level));
	}
	let shaped = Shaped { faced: Faced { levelled, faces: Vec::new(), mirrored: Vec::new() }, runs: Vec::new() };
	let measured = stages::measure(shaped).map_err(|error| format!("measuring refused: {error:?}"))?;
	let lines = stages::break_lines(measured, font_contract::Fixed266::from_pixels(1000));
	let visual = stages::reorder(lines);
	let order: Vec<usize> = visual.order.first().cloned().unwrap_or_default();
	if order != case.order {
		return Err(format!("the visual order is {order:?} where the corpus declares {:?}", case.order));
	}
	Ok(())
}

/// An instance of the variable face: what its advance and its outline measure at one coordinate.
fn run_instance(bytes: &[u8], pinned: &Pinned, case: &cases::Instance) -> Result<(), String> {
	use font_parse::Variations;
	use font_parse::glyf::{Outline, Point, PointKind};

	/// Every point of one glyph, in the order the walk produces them.
	#[derive(Default)]
	struct Recorder {
		points: Vec<Point>,
	}

	impl Outline for Recorder {
		fn point(&mut self, point: Point) -> bool {
			self.points.push(point);
			true
		}
	}

	let face = Face::open(bytes, 0).map_err(|error| format!("the face does not open: {error:?}"))?;
	let variations = Variations::of(&face).map_err(|error| format!("the variations are refused: {error:?}"))?.ok_or_else(|| "the corpus says this face varies and it carries no `fvar`".to_string())?;
	// The coordinate is in the design units `fvar` states, which the format stores in 16.16 fixed
	// point - the same units the axis's own minimum, default and maximum are in.
	let normalised = variations.normalise(0, case.coordinate << 16).map_err(|error| format!("the coordinate is refused: {error:?}"))?;
	let adjusted = variations.adjust(&face, 0, normalised).map_err(|error| format!("`avar` refused the coordinate: {error:?}"))?;
	let glyph = *pinned.glyphs.get(case.glyph).ok_or_else(|| format!("the case names a glyph the face does not declare: {}", case.glyph))?;

	let (advance, _bearing) = face.advance(glyph).map_err(|error| format!("the advance is refused: {error:?}"))?;
	let delta = font_parse::variations::advance_delta(&face, glyph, &[adjusted]).map_err(|error| format!("the advance delta is refused: {error:?}"))?;
	let measured = advance as i32 + delta;
	if measured != case.advance {
		return Err(format!("the advance at {} is {measured} where the corpus declares {}", case.coordinate, case.advance));
	}

	let mut points = [Point { x: 0, y: 0, kind: PointKind::OnCurve, ends_contour: false }; 64];
	let mut deltas = [(0i16, 0i16); 64];
	let mut touched = [false; 64];
	let scratch = font_parse::gvar::Scratch { points: &mut points, deltas: &mut deltas, touched: &mut touched };
	let mut recorder = Recorder::default();
	font_parse::outline::walk(&face, glyph, &[adjusted], scratch, &mut recorder).map_err(|error| format!("the outline is refused: {error:?}"))?;
	let point = recorder.points.get(case.point).ok_or_else(|| format!("the glyph has no point {}", case.point))?;
	if point.x != case.x {
		return Err(format!("point {} at {} is at x {} where the corpus declares {}", case.point, case.coordinate, point.x, case.x));
	}
	Ok(())
}

fn name_of(pinned: &Pinned, glyph: u16) -> String {
	pinned.glyphs.iter().find(|(_, id)| **id == glyph).map(|(name, _)| name.clone()).unwrap_or_else(|| format!("glyph {glyph}"))
}

fn describe(buffer: &Buffer, pinned: &Pinned) -> String {
	let mut out = String::from("got [");
	for (index, info) in buffer.infos.iter().enumerate() {
		if index > 0 {
			out.push_str(", ");
		}
		let position = buffer.positions[index];
		out.push_str(&format!("{}@{}+{}", name_of(pinned, info.glyph), info.cluster, position.x_advance));
		if position.x_offset != 0 || position.y_offset != 0 {
			out.push_str(&format!("({},{})", position.x_offset, position.y_offset));
		}
	}
	out.push(']');
	out
}

fn hex(digest: &[u8; 32]) -> String {
	let mut out = String::new();
	for byte in digest {
		out.push_str(&format!("{byte:02x}"));
	}
	out
}

fn read_pins(path: &Path) -> Result<BTreeMap<String, Pinned>, String> {
	let text = std::fs::read_to_string(path).map_err(|error| format!("{} could not be read: {error}", path.display()))?;
	let mut pins: BTreeMap<String, Pinned> = BTreeMap::new();
	for (number, line) in text.lines().enumerate() {
		let line = line.trim();
		if line.is_empty() || line.starts_with('#') {
			continue;
		}
		let fields: Vec<&str> = line.split_whitespace().collect();
		match fields.as_slice() {
			["face", file, digest] => {
				let bytes = decode_hex(digest).ok_or_else(|| format!("line {}: {digest} is not a SHA-256", number + 1))?;
				pins.insert((*file).to_string(), Pinned { digest: bytes, glyphs: BTreeMap::new() });
			}
			["glyph", file, name, index] => {
				let index: u16 = index.parse().map_err(|_| format!("line {}: {index} is not a glyph index", number + 1))?;
				let entry = pins.get_mut(*file).ok_or_else(|| format!("line {}: {file} has no `face` line before its glyphs", number + 1))?;
				entry.glyphs.insert((*name).to_string(), index);
			}
			_ => return Err(format!("line {}: {line} is not a pin", number + 1)),
		}
	}
	if pins.is_empty() {
		return Err(format!("{} pins no faces at all", path.display()));
	}
	Ok(pins)
}

fn decode_hex(text: &str) -> Option<[u8; 32]> {
	if text.len() != 64 {
		return None;
	}
	let mut out = [0u8; 32];
	for (index, pair) in text.as_bytes().chunks(2).enumerate() {
		out[index] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
	}
	Some(out)
}

fn repository_root() -> Option<PathBuf> {
	let mut at: PathBuf = std::env::current_dir().ok()?;
	loop {
		if at.join("product.conf").is_file() && at.join("src").is_dir() {
			return Some(at);
		}
		if !at.pop() {
			return None;
		}
	}
}

/// THE GATE PROVES IT REFUSES BEFORE IT APPROVES. A checker exercised only over a corpus that
/// currently passes is not a checker: it would keep passing if its comparison quietly stopped
/// comparing, and nothing in a green run would say so.
fn self_check(directory: &Path, pins: &BTreeMap<String, Pinned>) -> Result<(), String> {
	let (file, pinned) = pins.iter().next().ok_or_else(|| "no faces are pinned".to_string())?;
	let mut bytes = std::fs::read(directory.join(file)).map_err(|error| format!("{file} could not be read: {error}"))?;

	// ONE FLIPPED BYTE IN A PINNED FACE. The digest is what makes "pinned" mean anything.
	let last = bytes.len() - 1;
	bytes[last] ^= 0x01;
	if sha256::digest(&bytes) == pinned.digest {
		return Err("a face with a flipped byte still matched its pin".to_string());
	}
	bytes[last] ^= 0x01;
	println!("text-corpus: a corrupted face does not match its pin");

	// AN EXPECTATION THAT IS WRONG, run through the same comparison the real cases use. The case
	// below is a copy of a real one with its first glyph's advance moved by one unit.
	let case = cases::CASES.first().ok_or_else(|| "no cases".to_string())?;
	let real = case.expect.first().ok_or_else(|| "a case with no expectations".to_string())?;
	let broken_expect = [Expect { glyph: real.glyph, cluster: real.cluster, x_advance: real.x_advance + 1, x_offset: real.x_offset, y_offset: real.y_offset }];
	let broken = Case { name: "a deliberately wrong advance", face: case.face, text: case.text, expect: &broken_expect };
	let face_bytes = std::fs::read(directory.join(case.face)).map_err(|error| format!("{} could not be read: {error}", case.face))?;
	let face_pins = pins.get(case.face).ok_or_else(|| format!("{} is not pinned", case.face))?;
	match run_case(&face_bytes, face_pins, &broken) {
		Ok(()) => return Err("an advance one unit out of true was accepted".to_string()),
		Err(problem) => println!("text-corpus: a wrong advance is refused: {problem}"),
	}

	// A GLYPH NAME THE FACE DOES NOT DECLARE, which is what a renamed or deleted glyph looks like.
	let absent_expect = [Expect { glyph: "no.such.glyph", cluster: real.cluster, x_advance: real.x_advance, x_offset: real.x_offset, y_offset: real.y_offset }];
	let absent = Case { name: "a glyph nothing declares", face: case.face, text: case.text, expect: &absent_expect };
	match run_case(&face_bytes, face_pins, &absent) {
		Ok(()) => return Err("a case naming a glyph the face does not declare was accepted".to_string()),
		Err(problem) => println!("text-corpus: an undeclared glyph name is refused: {problem}"),
	}

	// AND A PARAGRAPH WHOSE VISUAL ORDER IS WRONG.
	let paragraph = cases::PARAGRAPHS.first().ok_or_else(|| "no paragraphs".to_string())?;
	let mut order = paragraph.order.to_vec();
	order.reverse();
	let broken = cases::Paragraph { name: "a deliberately reversed order", text: paragraph.text, direction: paragraph.direction, paragraph_level: paragraph.paragraph_level, order: &order };
	match run_paragraph(&broken) {
		Ok(()) => return Err("a reversed visual order was accepted".to_string()),
		Err(problem) => println!("text-corpus: a wrong visual order is refused: {problem}"),
	}

	println!("text-corpus: the gate refuses a corrupted face, a wrong position, an undeclared glyph and a wrong order");
	Ok(())
}

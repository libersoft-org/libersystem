//! THE FACES THIS TREE USES, AUTHORED RATHER THAN IMPORTED.
//!
//! The font licence decision is that a face entering this tree must be PUBLIC DOMAIN OR UNLICENSE
//! and nothing else. A face is not linked code: it is redistributed verbatim in every image, so an
//! attribution or notice obligation attaches to the artifact itself rather than to a build step, and
//! the project's answer is to carry none. The faces a text stack would reach for are all outside that
//! set - Noto, Liberation, Inter and Unicode's own Last Resort are SIL OFL, DejaVu carries the
//! Bitstream Vera terms, Roboto and Droid are Apache-2.0 - so nothing could be staged and the
//! catalogue had nothing to read.
//!
//! THE ROUTE OUT IS TO AUTHOR THEM, and this tool is it. Anything this tree writes is Unlicense, so
//! the licence question does not arise. Two kinds of face come out of it:
//!
//!   * THE LAST-RESORT FACE, staged into the image, which draws a visible box for any code point no
//!     other face covers.
//!   * THE CONFORMANCE CORPUS, which is NOT staged into any image and exists to be read by the host
//!     text gate: faces carrying real `GSUB`, `GPOS`, `GDEF` and variation tables for Latin, Arabic,
//!     Hebrew, Devanagari, Thai, Khmer, emoji sequences and a variable axis. An importable face
//!     could not have served that purpose either, because a gate needs to know what the face
//!     DECLARES in order to say what the shaper should have done with it.
//!
//! EVERY FACE IS PARSED BY THE PARSER THAT WILL READ IT, in the run that writes it, because a
//! generator validating with its own writer proves only that it agrees with itself.
//!
//! `--check` regenerates and compares rather than writing, so a gate can prove the staged bytes are
//! the bytes this generator produces.

mod corpus;
mod lastresort;
mod layout;
mod variable;
mod write;

use bootproto::sha256;
use font_parse::Face;
use std::path::{Path, PathBuf};

fn main() -> std::process::ExitCode {
	let check = std::env::args().any(|argument| argument == "--check");
	let Some(root) = repository_root() else {
		eprintln!("font-gen: could not find the repository root");
		return std::process::ExitCode::FAILURE;
	};

	// What this run produces: every path, and the bytes that belong at it.
	let mut outputs: Vec<(PathBuf, Vec<u8>)> = Vec::new();

	let staged = root.join("src/volume/share/fonts");
	let face = write::build(&lastresort::spec());
	if let Err(error) = Face::open(&face, 0) {
		eprintln!("font-gen: the last-resort face this generator produced does not open: {error:?}");
		return std::process::ExitCode::FAILURE;
	}
	let declaration = build_declaration(&face);
	// AND THE SIDECAR IS PARSED BY THE CATALOGUE'S OWN VOCABULARY, for the same reason.
	let digest = sha256::digest(&face);
	if let Err(error) = service_logic::font_record::parse(&declaration, &digest) {
		eprintln!("font-gen: the declaration this generator produced is refused: {error:?}");
		return std::process::ExitCode::FAILURE;
	}
	outputs.push((staged.join("lastresort.ttf"), face));
	outputs.push((staged.join("lastresort.ttf.face"), declaration.into_bytes()));

	// THE CORPUS, WHICH IS TEST DATA AND NOT AN IMAGE INPUT. It lives beside the gate that reads it
	// and is never declared in a manifest: a face that exercises Khmer stacking has no business in a
	// system volume, and staging one would put a megabyte of test fixture in every image.
	let corpus_directory = root.join("src/tests/fonts");
	let mut digests: Vec<(String, [u8; 32])> = Vec::new();
	for authored in corpus::faces() {
		let bytes = write::build(&authored.spec);
		if let Err(error) = Face::open(&bytes, 0) {
			eprintln!("font-gen: the corpus face {} does not open: {error:?}", authored.file);
			return std::process::ExitCode::FAILURE;
		}
		digests.push((authored.file.to_string(), sha256::digest(&bytes)));
		outputs.push((corpus_directory.join(authored.file), bytes));
	}
	// THE PIN, WRITTEN BESIDE THE CORPUS. The gate reads it and refuses a face whose bytes are not
	// the ones this generator produced, so "pinned by SHA-256" is a check rather than a sentence.
	outputs.push((corpus_directory.join("corpus.pins"), corpus::pins(&digests).into_bytes()));

	if check {
		let mut drifted = Vec::new();
		for (path, wanted) in &outputs {
			match std::fs::read(path) {
				Ok(found) if found == *wanted => {}
				Ok(_) => drifted.push(format!("{} is not what this generator produces", display(&root, path))),
				Err(_) => drifted.push(format!("{} is not staged", display(&root, path))),
			}
		}
		if drifted.is_empty() {
			println!("font-gen: {} generated file(s) are exactly what this generator produces", outputs.len());
			return std::process::ExitCode::SUCCESS;
		}
		for problem in &drifted {
			eprintln!("font-gen: {problem}");
		}
		eprintln!("font-gen: run `cargo run --manifest-path src/tools/font-gen/Cargo.toml` to restage them");
		return std::process::ExitCode::FAILURE;
	}

	for (path, bytes) in &outputs {
		let Some(directory) = path.parent() else { continue };
		if let Err(error) = std::fs::create_dir_all(directory) {
			eprintln!("font-gen: {} could not be created: {error}", display(&root, directory));
			return std::process::ExitCode::FAILURE;
		}
		if let Err(error) = std::fs::write(path, bytes) {
			eprintln!("font-gen: {} could not be written: {error}", display(&root, path));
			return std::process::ExitCode::FAILURE;
		}
	}
	println!("font-gen: wrote {} generated file(s)", outputs.len());
	std::process::ExitCode::SUCCESS
}

fn display(root: &Path, path: &Path) -> String {
	path.strip_prefix(root).unwrap_or(path).display().to_string()
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

/// The declaration the catalogue reads, in the vocabulary `font_record` defines.
fn build_declaration(face: &[u8]) -> String {
	let digest = sha256::digest(face);
	let mut hex = String::new();
	for byte in digest {
		hex.push_str(&format!("{byte:02x}"));
	}
	let family = lastresort::FAMILY;
	let style = lastresort::STYLE;
	let weight = lastresort::WEIGHT_CLASS;
	format!(
		"# Authored by `src/tools/font-gen`, which is the only way a face enters this tree: the\n\
		 # font licence decision admits public domain and Unlicense only, and nothing this\n\
		 # project did not write can satisfy it. Regenerate with\n\
		 #   cargo run --manifest-path src/tools/font-gen/Cargo.toml\n\
		 family = {family}\n\
		 style = {style}\n\
		 format = truetype-glyf\n\
		 face-index = 0\n\
		 weight = {weight}\n\
		 width = normal\n\
		 slant = upright\n\
		 digest = {hex}\n"
	)
}

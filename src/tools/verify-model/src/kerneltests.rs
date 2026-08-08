// Which kernel tests exist, on which targets, and what they claim to cover.
//
// Existence comes from the COMPILED test binaries, not from the source tree, because the source
// cannot answer it: `cfg(target_arch)` gating on tagged tests is scattered through
// `test_suites/kernel.rs` (eight), `hardware.rs` (six) and `sched/tests.rs` (two) as well as the
// arch trees, so a test living in a shared file may exist on one target only. 205 compile on
// x86_64 and 196 on aarch64, and a catalog that guessed from paths would invent keys that can never
// be fresh.
//
// `tagged_test!` expands to `mod NAME { static CASE: TaggedTest }`, so every test leaves one
// `..NAME..CASE` symbol in the binary. Reading those is one `nm` per target and needs no boot.

use crate::catalog::KernelTest;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

pub struct Discovery {
	pub tests: Vec<KernelTest>,
	// Targets whose test binary was not found. The planner cannot scope what it cannot enumerate,
	// so these fall open to the whole suite rather than to nothing.
	pub missing_targets: Vec<String>,
	// Tests with no `covers` declaration. They are always selected - which is correct and is also
	// the migration path: annotating them is what makes the suite scopeable, and until a test is
	// annotated the model refuses to guess that it can be skipped.
	pub unannotated: usize,
}

pub fn discover(repo_root: &Path, architectures: &[&str]) -> Result<Discovery, String> {
	let declarations = scan_source(repo_root)?;
	let mut per_test: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
	let mut missing_targets = Vec::new();
	for architecture in architectures {
		let mut found = false;
		for binary in test_binaries(repo_root, architecture) {
			let names = symbols(&binary)?;
			// A candidate with no descriptors is not this target's test binary. `build.sh --part
			// kernel` writes the ORDINARY kernel into the same `deps/` directory under the same
			// `kernel-<hash>` shape, and it is often the newest file there - so taking the newest
			// unconditionally silently reported that the target has no tests at all, which produced
			// no variants, no items and no warning. Under-selection with a clean bill.
			if names.is_empty() {
				continue;
			}
			for name in names {
				per_test.entry(name).or_default().insert((*architecture).to_string());
			}
			found = true;
			break;
		}
		if !found {
			missing_targets.push((*architecture).to_string());
		}
	}

	let mut tests = Vec::new();
	let mut unannotated = 0;
	for (name, architectures) in per_test {
		let covers = declarations.get(&name).cloned().unwrap_or_default();
		if covers.is_empty() {
			unannotated += 1;
		}
		tests.push(KernelTest { name, architectures: architectures.into_iter().collect(), covers });
	}
	tests.sort();
	Ok(Discovery { tests, missing_targets, unannotated })
}

// Every plausible candidate, newest first. The caller takes the first that actually carries test
// descriptors, because sharing a directory with the ordinary kernel build means the newest file is
// not reliably the right one.
fn test_binaries(repo_root: &Path, architecture: &str) -> Vec<std::path::PathBuf> {
	let triple = match architecture {
		"x86_64" => "x86_64-unknown-none",
		"aarch64" => "aarch64-unknown-none",
		"riscv64" => "riscv64gc-unknown-none-elf",
		_ => return Vec::new(),
	};
	let deps = repo_root.join(".build/cargo/kernel").join(triple).join("debug/deps");
	let Ok(entries) = fs::read_dir(&deps) else { return Vec::new() };
	let mut candidates: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
	for entry in entries.flatten() {
		let path = entry.path();
		let name = entry.file_name().to_string_lossy().into_owned();
		if !name.starts_with("kernel-") || name.ends_with(".d") || path.is_dir() {
			continue;
		}
		let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else { continue };
		candidates.push((modified, path));
	}
	// Newest first: several accumulate as the kernel is rebuilt, and an older one would report a
	// catalog for a tree that is gone.
	candidates.sort_by(|left, right| right.0.cmp(&left.0));
	candidates.into_iter().map(|(_, path)| path).collect()
}

fn symbols(binary: &Path) -> Result<Vec<String>, String> {
	let output = Command::new("nm").arg(binary).output().map_err(|error| format!("nm {}: {error}", binary.display()))?;
	if !output.status.success() {
		return Err(format!("nm {} failed: {}", binary.display(), String::from_utf8_lossy(&output.stderr).trim()));
	}
	let text = String::from_utf8_lossy(&output.stdout);
	let mut names = BTreeSet::new();
	for line in text.lines() {
		let Some(symbol) = line.split_whitespace().last() else { continue };
		if !symbol.ends_with("CASE") {
			continue;
		}
		if let Some(name) = test_name_from_symbol(symbol) {
			names.insert(name);
		}
	}
	Ok(names.into_iter().collect())
}

// Pull the test's own module name out of a v0-mangled path ending in `..4CASE`.
//
// The grammar's path components are length-prefixed identifiers, so the parse is a loop rather
// than a pattern: skip the crate disambiguator (`Cs<base62>_`), then read `<len><identifier>`
// until the string runs out. The component before `CASE` is the module `tagged_test!` created,
// which carries the test's name.
fn test_name_from_symbol(symbol: &str) -> Option<String> {
	let rest = symbol.strip_prefix("_R")?;
	let start = rest.find("Cs").and_then(|index| rest[index..].find('_').map(|offset| index + offset + 1))?;
	let bytes = rest.as_bytes();
	let mut cursor = start;
	let mut components: Vec<String> = Vec::new();
	while cursor < bytes.len() {
		let digits_start = cursor;
		while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
			cursor += 1;
		}
		if cursor == digits_start {
			// Not a length prefix: a grammar token we do not need to understand. Everything the
			// caller wants has already been collected by this point.
			break;
		}
		let length: usize = rest[digits_start..cursor].parse().ok()?;
		if cursor + length > bytes.len() {
			break;
		}
		components.push(rest[cursor..cursor + length].to_string());
		cursor += length;
	}
	let last = components.pop()?;
	if last != "CASE" {
		return None;
	}
	components.pop()
}

// `covers` as the source declares it.
//
// Optional on purpose. Making it mandatory would mean annotating 212 call sites before any of this
// runs, and a test with no declaration is not skipped - it is always selected. That way the model
// is correct from the first commit and gets cheaper as tests are annotated, rather than being
// wrong until the last one is.
fn scan_source(repo_root: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
	let mut declarations = BTreeMap::new();
	scan_dir(&repo_root.join("src/kernel"), &mut declarations)?;
	Ok(declarations)
}

fn scan_dir(dir: &Path, out: &mut BTreeMap<String, Vec<String>>) -> Result<(), String> {
	let Ok(entries) = fs::read_dir(dir) else { return Ok(()) };
	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_dir() {
			scan_dir(&path, out)?;
		} else if path.extension().is_some_and(|extension| extension == "rs") {
			let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
			for (name, covers) in parse_declarations(&text) {
				out.insert(name, covers);
			}
		}
	}
	Ok(())
}

// `tagged_test!(name, [Tags], covers = [alpha, beta])`, with the covers clause optional.
pub fn parse_declarations(text: &str) -> Vec<(String, Vec<String>)> {
	let mut found = Vec::new();
	let mut rest = text;
	while let Some(index) = rest.find("tagged_test!(") {
		rest = &rest[index + "tagged_test!(".len()..];
		let Some(end) = matching_paren(rest) else { break };
		let arguments = &rest[..end];
		rest = &rest[end..];
		let Some(name) = arguments.split(',').next().map(str::trim) else { continue };
		if name.is_empty() || !name.chars().all(|character| character.is_ascii_alphanumeric() || character == '_') {
			continue;
		}
		let covers = match arguments.find("covers") {
			Some(at) => {
				let tail = &arguments[at..];
				match (tail.find('['), tail.find(']')) {
					(Some(open), Some(close)) if close > open => tail[open + 1..close].split(',').map(str::trim).map(|item| item.trim_matches('"')).filter(|item| !item.is_empty()).map(str::to_string).collect(),
					_ => Vec::new(),
				}
			}
			None => Vec::new(),
		};
		found.push((name.to_string(), covers));
	}
	found
}

fn matching_paren(text: &str) -> Option<usize> {
	let mut depth = 1usize;
	for (index, character) in text.char_indices() {
		match character {
			'(' => depth += 1,
			')' => {
				depth -= 1;
				if depth == 0 {
					return Some(index);
				}
			}
			_ => {}
		}
	}
	None
}

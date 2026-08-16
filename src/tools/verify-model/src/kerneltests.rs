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
	// What each test's body was SEEN to reach: programs it launches by name, crates it calls into.
	// Observational, never a selection primitive - it exists to check `covers` declarations and to
	// report the other direction for a person to read.
	pub touches: BTreeMap<String, BTreeSet<String>>,
	// Targets whose test binary was not found. The planner cannot scope what it cannot enumerate,
	// so these fall open to the whole suite rather than to nothing.
	pub missing_targets: Vec<String>,
	// Tests with no `covers` declaration. They are always selected - which is correct and is also
	// the migration path: annotating them is what makes the suite scopeable, and until a test is
	// annotated the model refuses to guess that it can be skipped.
	pub unannotated: usize,
	// Ids the source declares that no built binary carries. Reported rather than refused: the
	// ordinary cause is a binary older than the source, which is the state every edit passes
	// through. The reason it is worth saying at all is that a test in this list cannot be selected
	// and does not run - the model knows only what was built - so a declaration that stays here
	// across a rebuild is a test nobody is running.
	pub declared_not_built: Vec<String>,
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

	let mut touches = scan_touches(repo_root)?;
	// Every one of these runs INSIDE the kernel, which is a reach no launch expresses. Without it a
	// unit test covering `kernel` fails the reachability gate for want of a `program_elf` call.
	//
	// It does not make the gate vacuous, because reach excludes `link.static.dev`: the kernel
	// dev-depends on eleven codecs to build its scenario fixtures, and a test claiming to cover one
	// of those still has to name it.
	for reached in touches.values_mut() {
		reached.insert(String::from("kernel"));
	}
	for name in per_test.keys() {
		touches.entry(name.clone()).or_default().insert(String::from("kernel"));
	}
	let mut tests = Vec::new();
	let mut unannotated = 0;
	for (name, architectures) in per_test {
		// A test in the built binary with no declaration in the source is a test whose identity the
		// model does not know - so it cannot be named in an exact selection, and naming it by its
		// function name is what this whole change exists to stop. Refuse rather than invent one.
		let Some(declaration) = declarations.get(&name) else {
			return Err(format!("kernel test `{name}` is in the built suite and has no `tagged_test!` declaration in `src/kernel` - the model cannot name what it cannot identify"));
		};
		if declaration.covers.is_empty() {
			unannotated += 1;
		}
		tests.push(KernelTest { name, id: declaration.id.clone(), architectures: architectures.into_iter().collect(), covers: declaration.covers.clone() });
	}
	tests.sort();
	let built: BTreeSet<&str> = tests.iter().map(|test| test.id.as_str()).collect();
	let mut declared_not_built: Vec<String> = declarations.values().map(|declaration| declaration.id.clone()).filter(|id| !built.contains(id.as_str())).collect();
	declared_not_built.sort();
	declared_not_built.dedup();
	Ok(Discovery { tests, touches, missing_targets, unannotated, declared_not_built })
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
fn scan_source(repo_root: &Path) -> Result<BTreeMap<String, Declaration>, String> {
	let mut declarations = BTreeMap::new();
	let mut seen = BTreeMap::new();
	scan_dir(&repo_root.join("src/kernel"), &mut declarations, &mut seen)?;
	Ok(declarations)
}

fn scan_dir(dir: &Path, out: &mut BTreeMap<String, Declaration>, seen: &mut BTreeMap<String, String>) -> Result<(), String> {
	let Ok(entries) = fs::read_dir(dir) else { return Ok(()) };
	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_dir() {
			scan_dir(&path, out, seen)?;
		} else if path.extension().is_some_and(|extension| extension == "rs") {
			let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
			for (name, id, covers) in parse_declarations(&text) {
				if id.is_empty() {
					return Err(format!("{}: `tagged_test!({name}, ..)` carries no `id = \"..\"`, and the identity of a test is not something this model may guess", path.display()));
				}
				// One name may be declared several times and MUST then carry one id: that is the
				// arch-gated shape, where a `#[cfg(target_arch = "..")]` variant per target
				// implements the same test three ways under one identity. Those merge, correctly.
				//
				// Two DIFFERENT ids under one function name is the other thing, and this model
				// cannot tell those apart: per-architecture presence comes from the compiled
				// binary's symbols, a symbol carries the function name, so the two would share
				// architectures and covers and the id kept would be whichever file was read last.
				//
				// Not hypothetical: `breakpoint_exception_returns` was in both the x86_64 IDT and
				// the riscv64 trap modules with different ids. Disambiguating costs a rename of the
				// function - which is exactly what an explicit `id` exists to survive.
				if let Some(first) = seen.insert(name.clone(), id.clone())
					&& first != id
				{
					return Err(format!("{}: two tests share the Rust function name `{name}` under different ids (`{first}` and `{id}`), which this model cannot tell apart - rename one function; its `id` may stay as it is", path.display()));
				}
				out.insert(name, Declaration { id, covers });
			}
		}
	}
	Ok(())
}

// What the source says about one test: its identity and what it claims to cover.
pub struct Declaration {
	pub id: String,
	pub covers: Vec<String>,
}

// Where a named CLAUSE starts inside a `tagged_test!` argument list, or None.
//
// A plain `arguments.find("covers")` finds the first occurrence of those six letters ANYWHERE, and a
// test name is part of the argument list. `the_lifecycle_guard_covers_the_whole_operation_...`
// therefore matched inside its own name, the search for the following `[` found the TAG list, and
// the test was recorded as covering `Kernel`, `Memory` and `Dma` - three components it never
// declared and cannot reach, which failed `every_annotation_in_this_tree_is_reachable` for a reason
// that had nothing to do with the annotation.
//
// A clause is the word standing alone and followed by `=`: not preceded by an identifier character,
// and the next non-space character is `=`. `id` is read the same way, because `valid`, `width` and
// any other name containing those two letters is the same trap one letter smaller.
fn clause_at(arguments: &str, clause: &str) -> Option<usize> {
	let bytes = arguments.as_bytes();
	let mut from = 0usize;
	while let Some(offset) = arguments[from..].find(clause) {
		let at = from + offset;
		let before_ok = at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
		let after = &arguments[at + clause.len()..];
		let follows_ok = after.trim_start().starts_with('=');
		if before_ok && follows_ok {
			return Some(at);
		}
		from = at + clause.len();
	}
	None
}

// `tagged_test!(name, [Tags], id = "..", covers = [alpha, beta])` -> (name, id, covers), with the
// covers clause optional and `id` required (the macro has no arm without it).
pub fn parse_declarations(text: &str) -> Vec<(String, String, Vec<String>)> {
	let mut found = Vec::new();
	let mut rest = text;
	while let Some(index) = rest.find("tagged_test!(") {
		rest = &rest[index + "tagged_test!(".len()..];
		let Some(end) = matching_paren(rest) else { break };
		let arguments = &rest[..end];
		rest = &rest[end..];
		// The name is the first ARGUMENT that is a plain identifier, and the declaration comes in two
		// shapes: one line, or several with a `#[cfg(target_arch = "..")]` attribute first. Handling
		// only the second broke the first and every single-line annotation vanished at once; handling
		// only the first skipped the arch-gated tests. Both, then, and in that order.
		let Some(name) = arguments
			.split(',')
			.map(str::trim)
			.chain(arguments.lines().map(|line| line.trim().trim_end_matches(',')))
			// Lowercase-first is what separates a test name from a TAG: names are snake_case and tags
			// are CamelCase. Without it the attributed form resolved to `Process` - the second tag in
			// the list, the first field that happened to be a bare identifier - and two real tests
			// reported as undeclared while a test by that name was invented and never matched.
			.find(|candidate| candidate.chars().next().is_some_and(|first| first.is_ascii_lowercase()) && candidate.chars().all(|character| character.is_ascii_alphanumeric() || character == '_'))
		else {
			continue;
		};
		let covers = match clause_at(arguments, "covers") {
			Some(at) => {
				let tail = &arguments[at..];
				match (tail.find('['), tail.find(']')) {
					(Some(open), Some(close)) if close > open => tail[open + 1..close].split(',').map(str::trim).map(|item| item.trim_matches('"')).filter(|item| !item.is_empty()).map(str::to_string).collect(),
					_ => Vec::new(),
				}
			}
			None => Vec::new(),
		};
		// The `id = ".."` literal, which is the test's identity everywhere outside this parser.
		//
		// Taken from the text between the first quote after `id =` and the next one, rather than by
		// splitting on commas: `covers` is a list and a future clause may be too, and an identity
		// read out of the wrong clause is worse than no identity at all.
		let id = clause_at(arguments, "id").and_then(|at| {
			let tail = &arguments[at..];
			let open = tail.find('"')?;
			let rest = &tail[open + 1..];
			let close = rest.find('"')?;
			Some(rest[..close].to_string())
		});
		found.push((name.to_string(), id.unwrap_or_default(), covers));
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

// What each test's body reaches, read from the source.
//
// Two signals, both unambiguous: a program launched by name - `program_elf(.., b"audioconv")` or
// `lookup(b"storage_service.lsexe")` - and a crate called into, `wav::encode::Encoder`. The kernel
// dev-depends on eleven codecs precisely so its scenarios can build and check their own fixtures,
// so the second signal is how a test that asserts on FLAC bytes is distinguished from one that
// merely launches something.
//
// This is the OBSERVATIONAL half of the model and it is never a selection primitive. It answers
// "could this test reach X", which is the enforceable direction of the `covers` rule; the other
// direction - "it reaches X, therefore it covers X" - is the `touches = covers` collapse the whole
// design exists to prevent, and it is emitted as a report for a person instead.
fn scan_touches(repo_root: &Path) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
	let mut direct = BTreeMap::new();
	let mut calls: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
	touch_dir(&repo_root.join("src/kernel"), &mut direct, &mut calls)?;
	Ok(close_over_helpers(direct, &calls))
}

// A test that launches nothing itself but calls `run_lico_harness` reaches whatever that reaches.
//
// Without this the gate is unusable: most scenarios delegate their setup to a harness function, so
// the direct scan sees an empty body and reports that a test covering `lico` cannot reach it. That
// was the first version's behaviour and it produced four false findings out of eight.
//
// A fixpoint rather than one hop, because harnesses call harnesses. It terminates because the set
// only grows and the function set is finite; the iteration cap is a guard against a cycle making it
// spin rather than a bound on legitimate depth.
fn close_over_helpers(direct: BTreeMap<String, BTreeSet<String>>, calls: &BTreeMap<String, BTreeSet<String>>) -> BTreeMap<String, BTreeSet<String>> {
	let mut touches = direct;
	for _ in 0..8 {
		let mut changed = false;
		let names: Vec<String> = touches.keys().cloned().collect();
		for name in names {
			let Some(called) = calls.get(&name) else { continue };
			let mut gained: BTreeSet<String> = BTreeSet::new();
			for callee in called {
				if callee == &name {
					continue;
				}
				if let Some(reached) = touches.get(callee) {
					gained.extend(reached.iter().cloned());
				}
			}
			let entry = touches.entry(name).or_default();
			let before = entry.len();
			entry.extend(gained);
			changed |= entry.len() != before;
		}
		if !changed {
			break;
		}
	}
	touches
}

fn touch_dir(dir: &Path, out: &mut BTreeMap<String, BTreeSet<String>>, calls: &mut BTreeMap<String, BTreeSet<String>>) -> Result<(), String> {
	let Ok(entries) = fs::read_dir(dir) else { return Ok(()) };
	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_dir() {
			touch_dir(&path, out, calls)?;
		} else if path.extension().is_some_and(|extension| extension == "rs") {
			let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
			for (name, reached, called) in parse_touches(&text) {
				out.entry(name.clone()).or_insert_with(BTreeSet::new).extend(reached);
				calls.entry(name).or_insert_with(BTreeSet::new).extend(called);
			}
		}
	}
	Ok(())
}

// Split a file into function bodies and attribute what each one reaches to its own name. Split on
// `\nfn ` at column zero, which is where every test function in this tree begins.
pub fn parse_touches(text: &str) -> Vec<(String, BTreeSet<String>, BTreeSet<String>)> {
	let mut found = Vec::new();
	// Function starts at ANY indentation. Splitting on a column-zero `\nfn ` missed every test
	// written inside a `mod tests { ... }`, which is how the object, memory and scheduler suites are
	// written - and those are most of the kernel's unit tests.
	let mut current: Option<(String, String)> = None;
	for line in text.lines() {
		let trimmed = line.trim_start();
		// `pub(crate) fn` AND `pub(super) fn` TOO, which this did not recognise.
		//
		// It stripped `pub fn ` or `fn ` and nothing else, so a `pub(crate) fn` was not seen as a
		// declaration at all - its body was appended to the PREVIOUS function's, the name never
		// entered the call graph, and every test that reached a program through such a helper was
		// reported as unable to reach it. Silent in both directions: the previous function gained
		// reaches it does not have, and the helper vanished.
		//
		// Found by writing a `pub(crate) fn` helper in `kernel/tests.rs` and watching the gate say
		// the test calling it could not reach `bin.wasi_host`.
		let visibility = trimmed.strip_prefix("pub(crate) ").or_else(|| trimmed.strip_prefix("pub(super) ")).or_else(|| trimmed.strip_prefix("pub ")).unwrap_or(trimmed);
		let declaration = visibility.strip_prefix("fn ").or_else(|| visibility.strip_prefix("async fn "));
		if let Some(rest) = declaration
			&& let Some(name) = rest.split(['(', '<', ' ']).next()
			&& !name.is_empty()
			&& name.chars().all(|character| character.is_ascii_alphanumeric() || character == '_')
		{
			if let Some((previous, body)) = current.take() {
				found.push((previous, reached_in(&body), called_in(&body)));
			}
			current = Some((name.to_string(), String::new()));
			continue;
		}
		if let Some((_, body)) = current.as_mut() {
			body.push_str(line);
			body.push('\n');
		}
	}
	if let Some((name, body)) = current {
		found.push((name, reached_in(&body), called_in(&body)));
	}
	found
}

// Plain function calls in a body: `run_lico_harness(...)`, `StorageHarness::start(...)`. Names only,
// resolved later against the set of functions actually scanned - anything that is not one of those
// simply never matches.
fn called_in(body: &str) -> BTreeSet<String> {
	let mut called = BTreeSet::new();
	let bytes = body.as_bytes();
	for (index, _) in body.match_indices('(') {
		let start = body[..index].rfind(|character: char| !(character.is_ascii_alphanumeric() || character == '_')).map(|at| at + 1).unwrap_or(0);
		if start >= index {
			continue;
		}
		// `Type::method(` is a call too, and the method is what was scanned.
		let name = &body[start..index];
		if name.is_empty() || bytes.get(index.saturating_sub(name.len() + 1)) == Some(&b'.') {
			continue;
		}
		called.insert(name.to_string());
	}
	called
}

fn push_program(name: &str, reached: &mut BTreeSet<String>) {
	if !name.is_empty() && name.chars().all(|character| character.is_ascii_alphanumeric() || character == '_') {
		reached.insert(format!("bin.{name}"));
	}
}

fn reached_in(body: &str) -> BTreeSet<String> {
	let mut reached = BTreeSet::new();
	// `program_elf(&package, volume, b"audioconv")` - the name is the byte string inside the call.
	let mut rest = body;
	while let Some(index) = rest.find("program_elf(") {
		rest = &rest[index + "program_elf(".len()..];
		let window = &rest[..rest.len().min(160)];
		let Some(open) = window.find("b\"") else { continue };
		let Some(close) = window[open + 2..].find('"') else { continue };
		push_program(&window[open + 2..open + 2 + close], &mut reached);
	}
	// `lookup(b"storage_service.lsexe")` - the name starts immediately, so looking for a `b"` AFTER
	// the marker finds the NEXT call's argument instead. That was the first version of this and it
	// is why the reachability gate reported that a test launching StorageService could not reach it.
	let mut rest = body;
	while let Some(index) = rest.find("lookup(b\"") {
		rest = &rest[index + "lookup(b\"".len()..];
		let Some(close) = rest.find('"') else { continue };
		if let Some(stem) = rest[..close].strip_suffix(".lsexe") {
			push_program(stem, &mut reached);
		}
	}
	// `wav::encode::Encoder` - a crate called into directly. Underscores are how Rust spells a
	// hyphenated package name, so both forms are recorded and the caller matches whichever exists.
	let bytes = body.as_bytes();
	for (index, _) in body.match_indices("::") {
		let start = body[..index].rfind(|character: char| !(character.is_ascii_alphanumeric() || character == '_')).map(|at| at + 1).unwrap_or(0);
		if start >= index || (start > 0 && bytes[start - 1] == b':') {
			continue;
		}
		let name = &body[start..index];
		if name.is_empty() || name.chars().next().is_some_and(|first| first.is_ascii_uppercase()) {
			continue;
		}
		reached.insert(name.to_string());
		if name.contains('_') {
			reached.insert(name.replace('_', "-"));
		}
	}
	reached
}

// Edge kinds that mean "the guest can get from here to there" at run time.
//
// `link.static.dev` is deliberately absent: a dev-dependency is how a test BUILDS its fixture, not
// something the running system reaches. Including it would let a test claim to cover any crate the
// kernel dev-depends on, which is all eleven codecs, for every test in the suite.
pub const RUNTIME_REACH: [&str; 7] = ["link.static", "link.dynamic", "format", "ipc", "syscall", "device", "generation"];

// The enforceable half of the `covers` rule: what a test declares must be something it can reach.
//
// The other half - "it reaches X therefore it covers X" - is NOT enforced and never will be. A
// scenario that starts StorageService in order to test `component_host` asserts nothing about
// StorageService, and inferring coverage from a launch is the `touches = covers` collapse that would
// inflate every declaration back to the full suite one honest-looking gate at a time.
pub fn unreachable_covers(test: &KernelTest, touched: &BTreeSet<String>, graph: &crate::graph::Graph) -> Vec<String> {
	let mut reachable: BTreeSet<String> = BTreeSet::new();
	for component in touched {
		if graph.contains(component) {
			reachable.extend(graph.reaches(component, &RUNTIME_REACH));
		}
	}
	test.covers.iter().filter(|component| !reachable.contains(*component)).cloned().collect()
}

// The report direction: launched and not claimed. For a person to read, never a failure.
pub fn launched_but_not_covered(test: &KernelTest, touched: &BTreeSet<String>, graph: &crate::graph::Graph) -> Vec<String> {
	if test.covers.is_empty() {
		return Vec::new();
	}
	touched.iter().filter(|component| component.starts_with("bin.") && graph.contains(component) && !test.covers.contains(component)).cloned().collect()
}

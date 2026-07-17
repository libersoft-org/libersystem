//! lsidl-gen - the LSIDL (LiberSystem Interface Definition Language) front-end.
//!
//! For now it lexes, parses, and validates the `.lsidl` files given on the
//! command line, printing a one-line summary per file and a non-zero exit code on
//! any error. Code generation (the `proto` crate, CLI/JSON/CBOR, docs, and compat
//! tests) is being added incrementally.
//!
//! Usage: `lsidl-gen [--dump] [--rust-dir <dir>] <file.lsidl>...`

mod ast;
mod codegen;
mod lexer;
mod parser;
mod resolve;
mod token;
mod validate;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

struct Source {
	path: String,
	src: String,
	file: ast::File,
}

struct Output {
	path: PathBuf,
	contents: String,
}

fn main() -> ExitCode {
	let mut dump = false;
	let mut check = false;
	let mut accept_breaking = false;
	let mut rust_dir: Option<String> = None;
	let mut docs_dir: Option<String> = None;
	let mut paths: Vec<String> = Vec::new();
	let mut args = std::env::args().skip(1);
	while let Some(a) = args.next() {
		match a.as_str() {
			"--dump" => dump = true,
			"--check" => check = true,
			"--accept-breaking" => accept_breaking = true,
			"--rust-dir" => match args.next() {
				Some(d) => rust_dir = Some(d),
				None => {
					eprintln!("lsidl-gen: --rust-dir needs a directory argument");
					return ExitCode::FAILURE;
				}
			},
			"--docs-dir" => match args.next() {
				Some(d) => docs_dir = Some(d),
				None => {
					eprintln!("lsidl-gen: --docs-dir needs a directory argument");
					return ExitCode::FAILURE;
				}
			},
			"-h" | "--help" => {
				println!("usage: lsidl-gen [--dump] [--check] [--accept-breaking] [--rust-dir <dir>] [--docs-dir <dir>] <file.lsidl>...");
				return ExitCode::SUCCESS;
			}
			_ => paths.push(a),
		}
	}
	if paths.is_empty() {
		eprintln!("lsidl-gen: no input files (usage: lsidl-gen [--dump] [--rust-dir <dir>] [--docs-dir <dir>] <file.lsidl>...)");
		return ExitCode::FAILURE;
	}

	if check && accept_breaking {
		eprintln!("lsidl-gen: --check and --accept-breaking are mutually exclusive");
		return ExitCode::FAILURE;
	}
	if process_all(&paths, dump, check, accept_breaking, rust_dir.as_deref(), docs_dir.as_deref()) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn process_all(paths: &[String], dump: bool, check: bool, accept_breaking: bool, rust_dir: Option<&str>, docs_dir: Option<&str>) -> bool {
	let mut sources = Vec::new();
	for path in paths {
		let src = match std::fs::read_to_string(path) {
			Ok(src) => src,
			Err(error) => {
				eprintln!("{path}: cannot read: {error}");
				return false;
			}
		};
		let tokens = match lexer::tokenize(&src) {
			Ok(tokens) => tokens,
			Err(error) => {
				print_error(path, &src, &error);
				return false;
			}
		};
		let file = match parser::parse(tokens) {
			Ok(file) => file,
			Err(error) => {
				print_error(path, &src, &error);
				return false;
			}
		};
		sources.push(Source { path: path.clone(), src, file });
	}
	sources.sort_by(|a, b| (a.file.package.path.join(":"), a.file.package.version, &a.path).cmp(&(b.file.package.path.join(":"), b.file.package.version, &b.path)));
	let files: Vec<ast::File> = sources.iter().map(|source| source.file.clone()).collect();
	let packages = match resolve::resolve(&files) {
		Ok(packages) => packages,
		Err(errors) => {
			for error in errors {
				print_error(&sources[error.file].path, &sources[error.file].src, &error.error);
			}
			return false;
		}
	};

	let mut valid = true;
	for package in &packages {
		for error in validate::validate_resolved(&sources[package.file].file, &package.imports) {
			print_error(&sources[package.file].path, &sources[package.file].src, &error);
			valid = false;
		}
	}
	if !valid {
		return false;
	}

	let mut rust_outputs = Vec::new();
	let mut docs_outputs = Vec::new();
	let mut destinations = HashSet::new();
	for package in &packages {
		let source = &sources[package.file];
		if let Some(dir) = rust_dir {
			let mut path = Path::new(dir).join("generated");
			for component in package.id.file_components() {
				path.push(component);
			}
			path.push(format!("v{}.rs", package.id.version));
			if !destinations.insert(path.clone()) {
				eprintln!("{}: error: generated destination collision at {}", source.path, path.display());
				return false;
			}
			let contents = match codegen::rust(&source.file, &source.path, &package.imports) {
				Ok(contents) => contents,
				Err(error) => {
					print_error(&source.path, &source.src, &error);
					return false;
				}
			};
			rust_outputs.push(Output { path, contents });
			let mut compat_path = Path::new(dir).join("generated");
			for component in package.id.file_components() {
				compat_path.push(component);
			}
			compat_path.push(format!("v{}", package.id.version));
			compat_path.push("compat.rs");
			if !destinations.insert(compat_path.clone()) {
				eprintln!("{}: error: generated destination collision at {}", source.path, compat_path.display());
				return false;
			}
			rust_outputs.push(Output { path: compat_path, contents: codegen::compat_rust(&source.file, &package.imports) });
		}
		if let Some(dir) = docs_dir {
			let mut path = Path::new(dir).to_path_buf();
			for component in package.id.file_components() {
				path.push(component);
			}
			path.push(format!("v{}.md", package.id.version));
			if !destinations.insert(path.clone()) {
				eprintln!("{}: error: generated destination collision at {}", source.path, path.display());
				return false;
			}
			docs_outputs.push(Output { path, contents: codegen::docs(&source.file, &source.path) });
			let mut abi_path = Path::new(dir).to_path_buf();
			for component in package.id.file_components() {
				abi_path.push(component);
			}
			abi_path.push(format!("v{}.abi", package.id.version));
			if !destinations.insert(abi_path.clone()) {
				eprintln!("{}: error: generated destination collision at {}", source.path, abi_path.display());
				return false;
			}
			docs_outputs.push(Output { path: abi_path, contents: codegen::abi_manifest(&source.file, &package.imports) });
		}
	}
	if let Some(dir) = rust_dir {
		if !add_module_indexes(Path::new(dir), &packages, &mut destinations, &mut rust_outputs) {
			return false;
		}
	}
	rush_outputs(&mut rust_outputs);
	rush_outputs(&mut docs_outputs);
	for output in &mut rust_outputs {
		match rustfmt(&output.contents) {
			Ok(contents) => output.contents = contents,
			Err(error) => {
				eprintln!("lsidl-gen: rustfmt failed for {}: {error}", output.path.display());
				return false;
			}
		}
	}
	if !accept_breaking {
		for output in &docs_outputs {
			if output.path.extension().and_then(|extension| extension.to_str()) == Some("abi") {
				if let Ok(previous) = std::fs::read_to_string(&output.path) {
					let breaks = breaking_abi_changes(&previous, &output.contents);
					if !breaks.is_empty() {
						eprintln!("{}: breaking ABI change(s):", output.path.display());
						for change in breaks {
							eprintln!("  removed or changed: {change}");
						}
						eprintln!("rerun with --accept-breaking for an intentional pre-release change");
						return false;
					}
				}
			}
		}
	}
	if check {
		let rust_ok = rust_dir.map(|dir| check_outputs(Path::new(dir), &rust_outputs)).unwrap_or(true);
		let docs_ok = docs_dir.map(|dir| check_outputs(Path::new(dir), &docs_outputs)).unwrap_or(true);
		return rust_ok && docs_ok;
	}
	if let Some(dir) = rust_dir {
		if let Err(error) = replace_outputs(Path::new(dir), &rust_outputs) {
			eprintln!("lsidl-gen: cannot replace Rust outputs: {error}");
			return false;
		}
	}
	if let Some(dir) = docs_dir {
		if let Err(error) = replace_outputs(Path::new(dir), &docs_outputs) {
			eprintln!("lsidl-gen: cannot replace documentation outputs: {error}");
			return false;
		}
	}
	for output in rust_outputs.iter().chain(docs_outputs.iter()) {
		println!("wrote {}", output.path.display());
	}
	for package in &packages {
		let source = &sources[package.file];
		if dump {
			println!("{:#?}", source.file);
			println!("// source bytes: {}", source.src.len());
		}
		println!("{}: {} [ok]", source.path, summary(&source.file));
	}
	true
}

fn print_error(path: &str, source: &str, error: &token::Error) {
	eprintln!("{path}:{}: error: {}", error.span, error.msg);
	if let Some(line) = source.lines().nth(error.span.line.saturating_sub(1) as usize) {
		eprintln!("  {line}");
		let column = error.span.col.saturating_sub(1) as usize;
		eprintln!("  {}^", " ".repeat(column));
	}
}

fn rustfmt(source: &str) -> Result<String, String> {
	let mut child = Command::new("rustfmt").args(["--emit", "stdout", "--edition", "2024"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|error| error.to_string())?;
	child.stdin.as_mut().ok_or_else(|| "rustfmt stdin unavailable".to_string())?.write_all(source.as_bytes()).map_err(|error| error.to_string())?;
	let output = child.wait_with_output().map_err(|error| error.to_string())?;
	if !output.status.success() {
		return Err(String::from_utf8_lossy(&output.stderr).into_owned());
	}
	String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

fn check_outputs(root: &Path, outputs: &[Output]) -> bool {
	let mut ok = true;
	let expected_paths: Vec<String> = outputs.iter().filter_map(|output| output.path.strip_prefix(root).ok()).map(|path| path.to_string_lossy().into_owned()).collect();
	for output in outputs {
		match std::fs::read_to_string(&output.path) {
			Ok(contents) if contents == output.contents => {}
			Ok(_) => {
				eprintln!("{}: generated output differs", output.path.display());
				ok = false;
			}
			Err(_) => {
				eprintln!("{}: generated output is missing", output.path.display());
				ok = false;
			}
		}
	}
	let manifest = root.join(".lsidl-generated.manifest");
	let expected_manifest = if expected_paths.is_empty() { String::new() } else { format!("{}\n", expected_paths.join("\n")) };
	if std::fs::read_to_string(&manifest).unwrap_or_default() != expected_manifest {
		eprintln!("{}: generated manifest differs or contains stale outputs", manifest.display());
		ok = false;
	}
	ok
}

fn breaking_abi_changes(old: &str, new: &str) -> Vec<String> {
	let new_lines: HashSet<&str> = new.lines().collect();
	old.lines()
		.filter(|line| !line.starts_with("meta "))
		.filter(|line| {
			if new_lines.contains(*line) {
				return false;
			}
			if line.starts_with("flags ") {
				return !new.lines().any(|candidate| compatible_flags(line, candidate));
			}
			if line.starts_with("method ") {
				return !new.lines().any(|candidate| compatible_reduced_rights(line, candidate));
			}
			true
		})
		.map(str::to_string)
		.collect()
}

fn compatible_flags(old: &str, new: &str) -> bool {
	let Some((old_head, old_names)) = old.split_once(" (") else { return false };
	let Some((new_head, new_names)) = new.split_once(" (") else { return false };
	if old_head != new_head {
		return false;
	}
	let old_names: HashSet<&str> = old_names.trim_end_matches(')').split(',').filter(|name| !name.is_empty()).collect();
	let new_names: HashSet<&str> = new_names.trim_end_matches(')').split(',').filter(|name| !name.is_empty()).collect();
	old_names.is_subset(&new_names)
}

fn compatible_reduced_rights(old: &str, new: &str) -> bool {
	if !old.starts_with("method ") || !new.starts_with("method ") || strip_rights(old) != strip_rights(new) {
		return false;
	}
	let old_rights = rights_sets(old);
	let new_rights = rights_sets(new);
	old_rights.len() == new_rights.len() && old_rights.iter().zip(new_rights).all(|(old, new)| new.is_subset(old))
}

fn strip_rights(line: &str) -> String {
	let mut out = String::new();
	let mut rest = line;
	while let Some(index) = rest.find("rights=") {
		out.push_str(&rest[..index + "rights=".len()]);
		rest = &rest[index + "rights=".len()..];
		let end = rest.find([',', ')']).unwrap_or(rest.len());
		rest = &rest[end..];
	}
	out.push_str(rest);
	out
}

fn rights_sets(line: &str) -> Vec<HashSet<&str>> {
	let mut sets = Vec::new();
	let mut rest = line;
	while let Some(index) = rest.find("rights=") {
		rest = &rest[index + "rights=".len()..];
		let end = rest.find([',', ')']).unwrap_or(rest.len());
		sets.push(rest[..end].split('+').filter(|right| !right.is_empty()).collect());
		rest = &rest[end..];
	}
	sets
}

fn rush_outputs(outputs: &mut [Output]) {
	outputs.sort_by(|a, b| a.path.cmp(&b.path));
}

fn add_module_indexes(root: &Path, packages: &[resolve::ResolvedPackage], destinations: &mut HashSet<PathBuf>, outputs: &mut Vec<Output>) -> bool {
	let generated = root.join("generated");
	let mut modules: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
	for package in packages {
		let mut parent = generated.clone();
		for (module, file) in package.id.rust_components().into_iter().zip(package.id.file_components()) {
			modules.entry(parent.join("mod.rs")).or_default().insert(module);
			parent.push(file);
		}
		modules.entry(parent.join("mod.rs")).or_default().insert(format!("v{}", package.id.version));
	}
	for (path, children) in modules {
		if !destinations.insert(path.clone()) {
			eprintln!("lsidl-gen: error: generated destination collision at {}", path.display());
			return false;
		}
		let mut contents = String::from("// @generated by lsidl-gen. Do not edit; run `just gen`.\n");
		for child in children {
			contents.push_str(&format!("pub mod {child};\n"));
		}
		outputs.push(Output { path, contents });
	}
	true
}

fn replace_outputs(root: &Path, outputs: &[Output]) -> std::io::Result<()> {
	std::fs::create_dir_all(root)?;
	let manifest = root.join(".lsidl-generated.manifest");
	let new_paths: Vec<String> = outputs.iter().filter_map(|output| output.path.strip_prefix(root).ok()).map(|path| path.to_string_lossy().into_owned()).collect();
	let new_set: HashSet<&str> = new_paths.iter().map(String::as_str).collect();
	for output in outputs {
		if let Some(parent) = output.path.parent() {
			std::fs::create_dir_all(parent)?;
		}
		let temp = output.path.with_extension(format!("lsidl-tmp-{}", std::process::id()));
		std::fs::write(&temp, &output.contents)?;
		std::fs::rename(&temp, &output.path)?;
	}
	if let Ok(previous) = std::fs::read_to_string(&manifest) {
		for stale in previous.lines().filter(|path| !path.is_empty() && !new_set.contains(*path)) {
			let stale_path = root.join(stale);
			if stale_path.is_file() {
				std::fs::remove_file(stale_path)?;
			}
		}
	}
	let manifest_text = if new_paths.is_empty() { String::new() } else { format!("{}\n", new_paths.join("\n")) };
	let temp_manifest = manifest.with_extension(format!("tmp-{}", std::process::id()));
	std::fs::write(&temp_manifest, manifest_text)?;
	std::fs::rename(temp_manifest, manifest)?;
	Ok(())
}

fn summary(file: &ast::File) -> String {
	let pkg = format!("{}@{}", file.package.path.join(":"), file.package.version);
	let mut aliases = 0;
	let mut records = 0;
	let mut enums = 0;
	let mut variants = 0;
	let mut flags = 0;
	let mut resources = 0;
	let mut interfaces = 0;
	let mut ops = 0;
	for item in &file.items {
		match item {
			ast::Item::Alias(_) => aliases += 1,
			ast::Item::Record(_) => records += 1,
			ast::Item::Enum(_) => enums += 1,
			ast::Item::Variant(_) => variants += 1,
			ast::Item::Flags(_) => flags += 1,
			ast::Item::Resource(_) => resources += 1,
			ast::Item::Interface(i) => {
				interfaces += 1;
				ops += i.methods.len();
			}
		}
	}
	format!("{pkg} - {aliases} alias(es), {records} record(s), {enums} enum(s), {variants} variant(s), {flags} flags, {resources} resource(s), {interfaces} interface(s), {ops} op(s)")
}

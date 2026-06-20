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
mod token;
mod validate;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
	let mut dump = false;
	let mut rust_dir: Option<String> = None;
	let mut paths: Vec<String> = Vec::new();
	let mut args = std::env::args().skip(1);
	while let Some(a) = args.next() {
		match a.as_str() {
			"--dump" => dump = true,
			"--rust-dir" => match args.next() {
				Some(d) => rust_dir = Some(d),
				None => {
					eprintln!("lsidl-gen: --rust-dir needs a directory argument");
					return ExitCode::FAILURE;
				}
			},
			"-h" | "--help" => {
				println!("usage: lsidl-gen [--dump] [--rust-dir <dir>] <file.lsidl>...");
				return ExitCode::SUCCESS;
			}
			_ => paths.push(a),
		}
	}
	if paths.is_empty() {
		eprintln!("lsidl-gen: no input files (usage: lsidl-gen [--dump] [--rust-dir <dir>] <file.lsidl>...)");
		return ExitCode::FAILURE;
	}

	let mut ok = true;
	for path in &paths {
		if !process(path, dump, rust_dir.as_deref()) {
			ok = false;
		}
	}
	if ok {
		ExitCode::SUCCESS
	} else {
		ExitCode::FAILURE
	}
}

// Lex, parse, and validate one file (and, when `rust_dir` is set, generate its
// Rust bindings). Returns false (and prints diagnostics to stderr) on any error.
fn process(path: &str, dump: bool, rust_dir: Option<&str>) -> bool {
	let src = match std::fs::read_to_string(path) {
		Ok(s) => s,
		Err(e) => {
			eprintln!("{path}: cannot read: {e}");
			return false;
		}
	};
	let toks = match lexer::tokenize(&src) {
		Ok(t) => t,
		Err(e) => {
			eprintln!("{path}:{}: error: {}", e.span, e.msg);
			return false;
		}
	};
	let file = match parser::parse(toks) {
		Ok(f) => f,
		Err(e) => {
			eprintln!("{path}:{}: error: {}", e.span, e.msg);
			return false;
		}
	};
	let errs = validate::validate(&file);
	if !errs.is_empty() {
		for e in &errs {
			eprintln!("{path}:{}: error: {}", e.span, e.msg);
		}
		return false;
	}
	if let Some(dir) = rust_dir {
		let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("out");
		let out_path = format!("{dir}/{stem}.rs");
		match codegen::rust(&file, path) {
			Ok(code) => {
				if let Err(e) = std::fs::write(&out_path, code) {
					eprintln!("{path}: cannot write {out_path}: {e}");
					return false;
				}
				println!("{path}: wrote {out_path}");
			}
			Err(e) => {
				eprintln!("{path}:{}: error: {}", e.span, e.msg);
				return false;
			}
		}
	}
	if dump {
		println!("{file:#?}");
	}
	println!("{path}: {} [ok]", summary(&file));
	true
}

fn summary(file: &ast::File) -> String {
	let pkg = format!("{}@{}", file.package.path.join(":"), file.package.version);
	let mut records = 0;
	let mut enums = 0;
	let mut variants = 0;
	let mut flags = 0;
	let mut resources = 0;
	let mut interfaces = 0;
	let mut ops = 0;
	for item in &file.items {
		match item {
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
	format!("{pkg} - {records} record(s), {enums} enum(s), {variants} variant(s), {flags} flags, {resources} resource(s), {interfaces} interface(s), {ops} op(s)")
}

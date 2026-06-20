//! lsidl-gen - the LSIDL (LiberSystem Interface Definition Language) front-end.
//!
//! For now it lexes, parses, and validates the `.lsidl` files given on the
//! command line, printing a one-line summary per file and a non-zero exit code on
//! any error. Code generation (the `proto` crate, CLI/JSON/CBOR, docs, and compat
//! tests) lands in a later step.
//!
//! Usage: `lsidl-gen [--dump] <file.lsidl>...`

mod ast;
mod lexer;
mod parser;
mod token;
mod validate;

#[cfg(test)]
mod tests;

use std::process::ExitCode;

fn main() -> ExitCode {
	let mut dump = false;
	let mut paths: Vec<String> = Vec::new();
	for a in std::env::args().skip(1) {
		match a.as_str() {
			"--dump" => dump = true,
			"-h" | "--help" => {
				println!("usage: lsidl-gen [--dump] <file.lsidl>...");
				return ExitCode::SUCCESS;
			}
			_ => paths.push(a),
		}
	}
	if paths.is_empty() {
		eprintln!("lsidl-gen: no input files (usage: lsidl-gen [--dump] <file.lsidl>...)");
		return ExitCode::FAILURE;
	}

	let mut ok = true;
	for path in &paths {
		if !process(path, dump) {
			ok = false;
		}
	}
	if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

// Lex, parse, and validate one file. Returns false (and prints diagnostics to
// stderr) on any error.
fn process(path: &str, dump: bool) -> bool {
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

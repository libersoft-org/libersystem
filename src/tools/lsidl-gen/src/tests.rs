//! Front-end tests: lexing, parsing, and validation.

use crate::ast;
use crate::lexer;
use crate::parser;
use crate::token::Tok;
use crate::validate;

// Parse + validate a source that is expected to be valid, returning the AST.
fn parse_ok(src: &str) -> ast::File {
	let toks = lexer::tokenize(src).expect("lex failed");
	let file = parser::parse(toks).expect("parse failed");
	let errs = validate::validate(&file);
	assert!(errs.is_empty(), "unexpected validation errors: {:?}", errs.iter().map(|e| &e.msg).collect::<Vec<_>>());
	file
}

// Collect every diagnostic (lex, parse, or validate) for a source.
fn errors(src: &str) -> Vec<String> {
	let toks = match lexer::tokenize(src) {
		Ok(t) => t,
		Err(e) => return vec![e.msg],
	};
	let file = match parser::parse(toks) {
		Ok(f) => f,
		Err(e) => return vec![e.msg],
	};
	validate::validate(&file).into_iter().map(|e| e.msg).collect()
}

fn assert_err_contains(src: &str, needle: &str) {
	let errs = errors(src);
	assert!(errs.iter().any(|m| m.contains(needle)), "expected an error containing {needle:?}, got {errs:?}");
}

fn wrap(body: &str) -> String {
	format!("package liber:system@1;\n{body}")
}

const LOG: &str = r#"
package liber:system@1;

enum error { denied, not-found, invalid, again, closed }

enum severity { trace = 0, debug = 1, info = 2, warn = 3, error = 4, fatal = 5 }

record field { key: string, value: string }

record entry {
	timestamp: u64,
	severity: severity,
	source: string,
	fields: list<field>,
}

record query {
	since: option<u64>,
	min-severity: option<severity>,
	source: option<string>,
	limit: u32,
}

interface log {
	@op(1) emit: func(e: entry) -> result<unit, error>;
	@op(2) query: func(q: query) -> result<list<entry>, error>;
	@op(3) tail: func(q: query) -> result<stream<entry>, error>;
}
"#;

#[test]
fn lexes_arrow_and_kebab() {
	let toks = lexer::tokenize("min-severity ->").unwrap();
	assert_eq!(toks[0].tok, Tok::Ident("min-severity".into()));
	assert_eq!(toks[1].tok, Tok::Arrow);
	assert_eq!(toks[2].tok, Tok::Eof);
}

#[test]
fn rejects_double_hyphen() {
	assert!(lexer::tokenize("a--b").is_err());
}

#[test]
fn parses_the_log_sample() {
	let f = parse_ok(LOG);
	assert_eq!(f.package.path, vec!["liber".to_string(), "system".to_string()]);
	assert_eq!(f.package.version, 1);
	assert_eq!(f.items.len(), 6);
	let log = f.items.iter().find_map(|i| if let ast::Item::Interface(x) = i { Some(x) } else { None }).expect("log interface");
	assert_eq!(log.name, "log");
	assert_eq!(log.methods.len(), 3);
	assert_eq!(log.methods[0].op, 1);
}

#[test]
fn accepts_trailing_commas() {
	parse_ok(&wrap("enum e { a, b, }\nrecord r { x: u8, }"));
}

#[test]
fn accepts_handle_to_resource_and_builtin_channel() {
	parse_ok(&wrap("resource file;\nenum error { x }\ninterface vol {\n@op(1) open: func() -> result<handle<file>, error>;\n@op(2) sub: func() -> result<handle<channel>, error>;\n}"));
}

#[test]
fn rejects_duplicate_opcode() {
	assert_err_contains(&wrap("enum error { x }\ninterface i { @op(1) a: func() -> result<unit, error>; @op(1) b: func() -> result<unit, error>; }"), "reuses opcode");
}

#[test]
fn rejects_opcode_zero() {
	assert_err_contains(&wrap("enum error { x }\ninterface i { @op(0) m: func() -> result<unit, error>; }"), "1..=65535");
}

#[test]
fn rejects_missing_opcode() {
	assert_err_contains(&wrap("enum error { x }\ninterface i { m: func() -> result<unit, error>; }"), "missing its `@op");
}

#[test]
fn rejects_unknown_type() {
	assert_err_contains(&wrap("record r { a: nope }"), "unknown type");
}

#[test]
fn rejects_handle_to_non_resource() {
	assert_err_contains(&wrap("record entry { a: u8 }\nenum error { x }\ninterface i { @op(1) m: func() -> result<handle<entry>, error>; }"), "to be a resource");
}

#[test]
fn rejects_unknown_right() {
	assert_err_contains(&wrap("resource file;\nenum error { x }\ninterface i { @op(1) m: func(@rights(bogus) f: handle<file>) -> result<unit, error>; }"), "unknown right");
}

#[test]
fn rejects_duplicate_type_name() {
	assert_err_contains(&wrap("record a { x: u8 }\nenum a { y }"), "already defined");
}

#[test]
fn rejects_enum_ordinal_reuse() {
	assert_err_contains(&wrap("enum e { a = 1, b = 1 }"), "reuses ordinal");
}

#[test]
fn rejects_reserved_opcode_in_use() {
	assert_err_contains(&wrap("enum error { x }\ninterface i { @reserved(1); @op(1) m: func() -> result<unit, error>; }"), "reserves opcode");
}

#[test]
fn rejects_duplicate_parameter() {
	assert_err_contains(&wrap("enum error { x }\ninterface i { @op(1) m: func(a: u8, a: u8) -> result<unit, error>; }"), "duplicate parameter");
}

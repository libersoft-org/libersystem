//! Front-end tests: lexing, parsing, and validation.

use crate::ast;
use crate::lexer;
use crate::parser;
use crate::resolve;
use crate::token::Tok;
use crate::validate;
use std::path::PathBuf;

// Parse + validate a source that is expected to be valid, returning the AST.
fn parse_ok(src: &str) -> ast::File {
	let toks = lexer::tokenize(src).expect("lex failed");
	let file = parser::parse(toks).expect("parse failed");
	let errs = validate::validate(&file);
	assert!(errs.is_empty(), "unexpected validation errors: {:?}", errs.iter().map(|e| &e.msg).collect::<Vec<_>>());
	file
}

fn parse_only(src: &str) -> ast::File {
	parser::parse(lexer::tokenize(src).expect("lex failed")).expect("parse failed")
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
fn parses_versioned_imports_with_per_name_aliases_and_spans() {
	let toks = lexer::tokenize("package liber:app@1;\nuse liber:storage@1.{error as storage-error, file};\n").unwrap();
	let file = parser::parse(toks).unwrap();
	let import = &file.uses[0];
	assert_eq!(import.path, ["liber", "storage"]);
	assert_eq!(import.version, 1);
	assert_eq!(import.names[0].name, "error");
	assert_eq!(import.names[0].alias.as_deref(), Some("storage-error"));
	assert_eq!(import.names[0].span.line, 2);
	assert_eq!(import.names[0].alias_span.unwrap().line, 2);
	assert_eq!(import.names[1].local_name(), "file");
}

#[test]
fn rejects_unversioned_imports() {
	assert_err_contains("package liber:app@1;\nuse liber:storage.{error};", "expected `@`");
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
	assert_err_contains(&wrap("enum error { x }\ninterface i { @op(1) a: func() -> result<unit, error>; @op(1) b: func() -> result<unit, error>; }"), "first declared at");
}

#[test]
fn rejects_opcode_zero() {
	assert_err_contains(&wrap("enum error { x }\ninterface i { @op(0) m: func() -> result<unit, error>; }"), "1..=65531");
}

#[test]
fn rejects_runtime_control_opcodes() {
	for op in [abi::GOODBYE_OP, abi::RESOLVE_OP, abi::HEARTBEAT_OP, abi::CONNECT_OP] {
		assert_err_contains(&wrap(&format!("enum error {{ x }}\ninterface i {{ @op({op}) m: func() -> result<unit, error>; }}")), "1..=65531");
	}
}

#[test]
fn rejects_missing_opcode() {
	assert_err_contains(&wrap("enum error { x }\ninterface i { m: func() -> result<unit, error>; }"), "missing its `@op");
}

#[test]
fn rejects_unknown_type() {
	assert_err_contains(&wrap("record r { a: nope }"), "unknown type");
	assert_err_contains(&wrap("record known { value: u8 } record r { a: knwon }"), "did you mean `known`");
}

#[test]
fn rejects_handle_to_non_resource() {
	assert_err_contains(&wrap("record entry { a: u8 }\nenum error { x }\ninterface i { @op(1) m: func() -> result<handle<entry>, error>; }"), "to be a resource");
}

#[test]
fn rejects_unknown_right() {
	assert_err_contains(&wrap("resource file;\nenum error { x }\ninterface i { @op(1) m: func(@rights(bogus) f: handle<file>) -> result<unit, error>; }"), "unknown right");
	assert_err_contains(&wrap("resource file; interface i { @op(1) m: func(@rights(reed) f: handle<file>) -> unit; }"), "did you mean `read`");
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

#[test]
fn accepts_one_handle_per_alternative() {
	parse_ok(&wrap("resource file;\nvariant choice { file(handle<file>), memory(buffer), none }\ninterface i { @op(1) m: func() -> result<handle<file>, buffer>; }"));
}

#[test]
fn rejects_multiple_handles_in_records_and_requests() {
	assert_err_contains(&wrap("resource file;\nrecord pair { a: handle<file>, b: buffer }"), "record `pair` can transfer more than one");
	assert_err_contains(&wrap("resource file;\ninterface i { @op(1) m: func(a: handle<file>, b: buffer) -> unit; }"), "request for `i.m` can transfer more than one");
}

#[test]
fn rejects_lists_of_handle_bearing_values() {
	assert_err_contains(&wrap("resource file;\nrecord item { file: handle<file> }\nrecord batch { items: list<item> }"), "record `batch` can transfer more than one");
}

#[test]
fn rejects_direct_value_recursion_but_allows_indirected_recursion() {
	assert_err_contains(&wrap("record node { next: option<node> }"), "non-indirected recursive value cycle");
	parse_ok(&wrap("record node { children: list<node> }"));
}

#[test]
fn unresolved_imported_wire_shapes_fail_closed() {
	assert_err_contains("package liber:app@1;\nuse liber:shared@1.{foreign};\ninterface i { @op(1) m: func(v: foreign) -> unit; }", "has not been resolved");
}

#[test]
fn interfaces_are_not_value_types() {
	assert_err_contains(&wrap("interface service { @op(1) ping: func() -> unit; }\nrecord bad { value: service }"), "is an interface, not a value type");
}

#[test]
fn resolver_qualifies_aliases_and_preserves_concrete_kinds() {
	let files = vec![
		parse_only("package liber:app@1; use liber:shared@1.{error as shared-error, file}; record request { e: shared-error } interface app { @op(1) open: func(f: handle<file>) -> unit; }"),
		parse_only("package liber:shared@1; enum error { again } resource file;"),
	];
	let packages = resolve::resolve(&files).expect("resolve");
	assert_eq!(packages[0].id.display(), "liber:shared@1");
	let app = packages.iter().find(|package| package.id.display() == "liber:app@1").unwrap();
	assert!(validate::validate_resolved(&files[app.file], &app.imports).is_empty());
	let rust = crate::codegen::rust(&files[app.file], "app.lsidl", &app.imports).expect("codegen");
	assert!(rust.contains("use crate::generated::liber::shared::v1::Error as SharedError;"));
}

#[test]
fn package_keywords_use_raw_modules_and_plain_paths() {
	let files = vec![parse_only("package liber:type@1; record value { number: u32 }")];
	let package = resolve::resolve(&files).unwrap().remove(0);
	assert_eq!(package.id.rust_module(), "liber::r#type::v1");
	assert_eq!(package.id.file_components(), ["liber", "type"]);
}

#[test]
fn source_docs_preserve_spans_and_emit_rust_and_markdown() {
	let file = parse_only("//! Package prose.\npackage liber:docs@1;\n// discarded\n/// Record line one.\n/// Record line two with | pipe.\nrecord sample {\n/// Field prose.\nvalue: u32,\n}\n/// Interface prose.\ninterface api {\n/// Method prose.\n@op(1) run: func(/// Parameter prose.\nvalue: sample) -> unit;\n}");
	assert_eq!(file.package_doc[0].text, " Package prose.");
	assert_eq!(file.package_doc[0].span.line, 1);
	let record = file.items.iter().find_map(|item| if let ast::Item::Record(record) = item { Some(record) } else { None }).unwrap();
	assert_eq!(record.doc.len(), 2);
	assert_eq!(record.doc[0].span.line, 4);
	let rust = crate::codegen::rust(&file, "docs.lsidl", &std::collections::HashMap::new()).unwrap();
	assert!(rust.contains("//! Package prose."));
	assert!(rust.contains("/// Record line one."));
	assert!(rust.contains("/// Field prose."));
	assert!(rust.contains("/// Method prose."));
	assert!(!rust.contains("discarded"));
	let markdown = crate::codegen::docs(&file, "docs.lsidl");
	assert!(markdown.contains("Package prose."));
	assert!(markdown.contains("Record line one. Record line two with \\| pipe."));
	assert!(markdown.contains("Field prose."));
	assert!(markdown.contains("Method prose. `value`: Parameter prose."));
}

#[test]
fn resolver_rejects_missing_versions_names_and_duplicate_package_paths() {
	let wrong_version = vec![parse_only("package liber:app@1; use liber:shared@2.{error};"), parse_only("package liber:shared@1; enum error { x }")];
	assert!(resolve::resolve(&wrong_version).unwrap_err()[0].error.msg.contains("requests `liber:shared@2`"));
	let missing_name = vec![parse_only("package liber:app@1; use liber:shared@1.{missing};"), parse_only("package liber:shared@1; enum error { x }")];
	assert!(resolve::resolve(&missing_name).unwrap_err()[0].error.msg.contains("does not export `missing`"));
	let two_versions = vec![parse_only("package liber:shared@1;"), parse_only("package liber:shared@2;")];
	assert!(resolve::resolve(&two_versions).unwrap_err()[0].error.msg.contains("already loaded"));
}

#[test]
fn resolver_rejects_package_cycles() {
	let files = vec![parse_only("package liber:a@1; use liber:b@1.{b}; record a { value: b }"), parse_only("package liber:b@1; use liber:a@1.{a}; record b { value: a }")];
	assert!(resolve::resolve(&files).unwrap_err()[0].error.msg.contains("package import cycle"));
}

#[test]
fn imported_handle_cardinality_is_checked_after_resolution() {
	let files = vec![
		parse_only("package liber:app@1; use liber:shared@1.{held}; record batch { values: list<held> }"),
		parse_only("package liber:shared@1; resource file; record held { file: handle<file> }"),
	];
	let packages = resolve::resolve(&files).expect("resolve");
	let app = packages.iter().find(|package| package.id.display() == "liber:app@1").unwrap();
	let errors = validate::validate_resolved(&files[app.file], &app.imports);
	assert!(errors.iter().any(|error| error.msg.contains("record `batch` can transfer more than one")));
}

#[test]
fn aliases_expand_through_codecs_and_reject_cycles() {
	let file = parse_only("package liber:alias@1; type koid = u64; record process { id: koid }");
	let errors = validate::validate(&file);
	assert!(errors.is_empty(), "{errors:?}");
	let rust = crate::codegen::rust(&file, "alias.lsidl", &std::collections::HashMap::new()).unwrap();
	assert!(rust.contains("pub type Koid = u64;"));
	assert!(rust.contains("w.u64(self.id)?;"));
	assert!(rust.contains("let id = r.u64()?;"));
	assert!(!rust.contains("Koid::read"));
	assert_err_contains(&wrap("type a = list<b>; type b = option<a>;"), "recursive value cycle");
}

#[test]
fn stream_helpers_carry_one_handle_per_open_and_frame() {
	let file = parse_only("package liber:stream@1; resource file; record held { file: handle<file> } interface feed { @op(1) open: func(source: handle<file>) -> stream<held>; }");
	assert!(validate::validate(&file).is_empty());
	let rust = crate::codegen::rust(&file, "stream.lsidl", &std::collections::HashMap::new()).unwrap();
	assert!(rust.contains("self.transport.call(&request, request_handle)?"));
	assert!(rust.contains("frame_handle: &mut u64"));
	assert!(rust.contains("*frame_handle = writer.handle();"));
	assert!(rust.contains("Reader::with_handle(msg, *frame_handle)"));
}

#[test]
fn evolution_metadata_is_validated_and_emitted() {
	let file = parse_only("package liber:evolution@3; @since(1) @deprecated(3) record sample { @since(2) value: u32 } enum state { @deprecated(2) old, current } interface api { @op(1) @since(2) run: func(@deprecated(3) value: sample) -> unit; }");
	let errors = validate::validate(&file);
	assert!(errors.is_empty(), "{errors:?}");
	let rust = crate::codegen::rust(&file, "evolution.lsidl", &std::collections::HashMap::new()).unwrap();
	assert!(rust.contains("Since package version 1. Deprecated since package version 3."));
	assert!(rust.contains("Since package version 2."));
	assert!(rust.contains("Parameter `value`: Deprecated since package version 3."));
	let markdown = crate::codegen::docs(&file, "evolution.lsidl");
	assert!(markdown.contains("Since package version 1. Deprecated since package version 3."));
	assert!(markdown.contains("`value`: Deprecated since package version 3."));
	assert_err_contains("package liber:e@1; @since(0) record bad {}", "must be in 1..=1");
	assert_err_contains("package liber:e@1; @deprecated(2) record bad {}", "must be in 1..=1");
	assert_err_contains("package liber:e@1; @since(1) @since(1) record bad {}", "duplicate `@since`");
}

#[test]
fn abi_manifest_classifies_breaking_and_additive_changes() {
	let base = "package liber:test@1\nrecord item(value:u32)\nenum state(a=0)\nflags mode width=u8 (read)\ninterface api\nmethod api.get op=1 (h:handle<file>:rights=read+write) -> unit\nreserved interface api 9\n";
	let additive = format!("{base}method api.new op=2 () -> unit\nmeta record item since=1 deprecated=-\n");
	assert!(crate::breaking_abi_changes(base, &additive).is_empty());
	assert!(crate::breaking_abi_changes(base, &base.replace("record item(value:u32)", "record item(value:u32,more:u32)")).iter().any(|line| line.starts_with("record ")));
	assert!(crate::breaking_abi_changes(base, &base.replace("enum state(a=0)", "enum state(a=0,b=1)")).iter().any(|line| line.starts_with("enum ")));
	assert!(crate::breaking_abi_changes(base, &base.replace("flags mode width=u8 (read)", "flags mode width=u8 (read,write)")).is_empty());
	assert!(crate::breaking_abi_changes(base, &base.replace("rights=read+write", "rights=read")).is_empty());
	assert!(crate::breaking_abi_changes(&base.replace("rights=read+write", "rights=read"), base).iter().any(|line| line.starts_with("method ")));
}

#[test]
fn pipeline_failure_and_check_mode_never_write() {
	let root = temp_dir("pipeline");
	let input = root.join("input");
	let rust = root.join("rust");
	let docs = root.join("docs");
	std::fs::create_dir_all(&input).unwrap();
	std::fs::create_dir_all(&rust).unwrap();
	std::fs::create_dir_all(&docs).unwrap();
	let invalid = input.join("invalid.lsidl");
	std::fs::write(&invalid, "package liber:invalid@1; use liber:missing@1.{value};").unwrap();
	let sentinel = rust.join("sentinel");
	std::fs::write(&sentinel, "keep").unwrap();
	assert!(!crate::process_all(&[invalid.to_string_lossy().into_owned()], false, false, false, rust.to_str(), docs.to_str(), &[], &Default::default()));
	assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "keep");
	assert!(!rust.join(".lsidl-generated.manifest").exists());

	let valid = input.join("valid.lsidl");
	std::fs::write(&valid, "//! Test package.\npackage liber:valid@1; record value { number: u32 }").unwrap();
	let paths = [valid.to_string_lossy().into_owned()];
	assert!(crate::process_all(&paths, false, false, false, rust.to_str(), docs.to_str(), &[], &Default::default()));
	assert!(crate::process_all(&paths, false, true, false, rust.to_str(), docs.to_str(), &[], &Default::default()));
	let generated = rust.join("generated/liber/valid/v1.rs");
	std::fs::write(&generated, "drift\n").unwrap();
	assert!(!crate::process_all(&paths, false, true, false, rust.to_str(), docs.to_str(), &[], &Default::default()));
	assert_eq!(std::fs::read_to_string(&generated).unwrap(), "drift\n");
	let _ = std::fs::remove_dir_all(root);
}

#[test]
fn rust_package_selection_and_external_ownership_are_explicit() {
	let root = temp_dir("package-selection");
	let input = root.join("input");
	let selected = root.join("selected");
	let external = root.join("external");
	std::fs::create_dir_all(&input).unwrap();
	let base = input.join("base.lsidl");
	let storage = input.join("storage.lsidl");
	std::fs::write(&base, "package liber:base@1; enum error { failed = 1 }").unwrap();
	std::fs::write(&storage, "package liber:storage@1; use liber:base@1.{error}; record status { error: error }").unwrap();
	let paths = [base.to_string_lossy().into_owned(), storage.to_string_lossy().into_owned()];
	let packages = ["liber:base@1".to_string()];
	assert!(crate::process_all(&paths, false, false, false, selected.to_str(), None, &packages, &Default::default()));
	assert!(selected.join("generated/liber/base/v1.rs").is_file());
	assert!(!selected.join("generated/liber/storage/v1.rs").exists());

	let owners = std::collections::BTreeMap::from([("liber:storage@1".to_string(), "storage_proto::generated::liber::storage".to_string())]);
	assert!(crate::process_all(&paths, false, false, false, external.to_str(), None, &[], &owners));
	assert!(!external.join("generated/liber/storage/v1.rs").exists());
	let index = std::fs::read_to_string(external.join("generated/liber/mod.rs")).unwrap();
	assert!(index.contains("pub use storage_proto::generated::liber::storage;"));
	let missing = ["liber:missing@1".to_string()];
	assert!(!crate::process_all(&paths, false, false, false, external.to_str(), None, &missing, &Default::default()));
	let _ = std::fs::remove_dir_all(root);
}

fn temp_dir(name: &str) -> PathBuf {
	let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
	std::env::temp_dir().join(format!("lsidl-{name}-{}-{nonce}", std::process::id()))
}

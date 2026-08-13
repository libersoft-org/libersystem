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
	assert_err_contains(&wrap("enum error { x }\ninterface i { @op(0) m: func() -> result<unit, error>; }"), "1..=65530");
}

#[test]
fn rejects_runtime_control_opcodes() {
	for op in [abi::GOODBYE_OP, abi::RESOLVE_OP, abi::HEARTBEAT_OP, abi::CONNECT_OP] {
		assert_err_contains(&wrap(&format!("enum error {{ x }}\ninterface i {{ @op({op}) m: func() -> result<unit, error>; }}")), "1..=65530");
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
fn a_schema_may_now_ask_for_more_than_one_handle() {
	// THIS TEST USED TO ASSERT THE OPPOSITE, and it was the last pin holding the 1 -> 4 handle
	// migration still: the validator refused `Many` because the typed SERVER receive took one
	// handle and the kernel dropped the rest, so a schema that could ask for three would have lost
	// two. `rt::recv_caps_blocking` and the thirteen converted call sites removed that reason, and
	// the refusal went with them - but these assertions stayed, red, describing a rule the tree no
	// longer has.
	//
	// What replaces it is not "no bound": the bound is `wire::MAX_HANDLES`, enforced where the
	// handles actually are - the writer refuses the fifth, the reader tracks occupied and consumed
	// slots, and the failure paths return every capability to its owner. A schema is not the place
	// to count them, because a `list<T>` of handle-bearing values has a length nobody knows until
	// the message is built.
	parse_ok(&wrap("resource file;\nrecord pair { a: handle<file>, b: buffer }"));
	parse_ok(&wrap("resource file;\ninterface i { @op(1) m: func(a: handle<file>, b: buffer) -> unit; }"));
	parse_ok(&wrap("resource file;\ninterface i { @op(1) m: func() -> tuple<handle<file>, handle<file>>; }"));
}

#[test]
fn a_list_of_handle_bearing_values_is_a_shape_and_not_a_refusal() {
	// The `Many` case reached through a list, which is the one the old refusal was really about: a
	// `list<item>` where `item` carries a handle has a count the schema cannot know. It is accepted
	// here and bounded at the wire, where the count exists.
	parse_ok(&wrap("resource file;\nrecord item { file: handle<file> }\nrecord batch { items: list<item> }"));
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
	// RESOLVED, so its cardinality is known - `Many`, which is a shape rather than a refusal since
	// the handle migration finished. What must still fail is the UNRESOLVED case below: a schema
	// whose imported wire shape nobody has established cannot be reasoned about at all, and that
	// one fails closed.
	assert!(errors.is_empty(), "a resolved list of handle-bearing values is a legal shape: {errors:?}");
	let unresolved = validate::validate(&files[app.file]);
	assert!(unresolved.iter().any(|error| error.msg.contains("has not been resolved")), "an unresolved imported shape still fails closed: {unresolved:?}");
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
fn stream_helpers_carry_handles_per_open_and_frame() {
	// This asserted the SINGLE-handle shape (`call(&request, request_handle)`) until 2026-08-01.
	// A message now carries a bounded LIST, so the call takes the request's handles as a slice
	// and receives the reply's through an out-parameter.
	//
	// AND SO DO THE PER-FRAME HELPERS, since 2026-08-13. They stayed singular on the argument that
	// "one stream element transfers at most one capability, a property of the frame protocol rather
	// than a limit of the transport" - which was true until the validator was relaxed and a stream
	// element could declare two. The transport carrying what the schema can express is what makes
	// that argument true again.
	let file = parse_only("package liber:stream@1; resource file; record held { file: handle<file> } interface feed { @op(1) open: func(source: handle<file>) -> stream<held>; }");
	assert!(validate::validate(&file).is_empty());
	let rust = crate::codegen::rust(&file, "stream.lsidl", &std::collections::HashMap::new()).unwrap();
	assert!(rust.contains("self.transport.call(&request, request_handles.as_slice(), &mut reply_handles)?"));
	assert!(rust.contains("frame_handles: &mut Handles"), "the frame writer takes the bounded list");
	assert!(rust.contains("*frame_handles = Handles::try_from_slice(writer.handles())?;"), "and hands back every handle the element wrote");
	assert!(rust.contains("Reader::with_handles(msg, frame_handles)"), "and the reader takes the list too");
	assert!(!rust.contains("frame_handle: &mut u64"), "no singular frame handle survives");
}

#[test]
fn a_generated_option_and_result_use_the_strict_tag() {
	// `Reader::boolean` was made strict - 0, 1, and `None` for everything else - after a finding
	// about exactly this malleability, and a wire test sweeps all 254 invalid values. The GENERATOR
	// did not learn it: `option` emitted `if r.u8()? != 0 { Some(..) }`, `result` the same shape, and
	// there was a third copy in the reply path. So a reply whose result tag was `0xff` decoded as
	// `Ok` - an error turned into a success with a garbage payload - in a tree that had just closed
	// the finding that names it.
	//
	// One rule in four spellings gets fixed in one of them. There is one now, `Reader::tag`, and this
	// asserts the generator reaches for it rather than comparing bytes itself.
	let file = parse_only("package liber:tag@1; record maybe { a: option<u32> } record either { b: result<u32, u32> } interface t { @op(1) go: func(m: maybe) -> either; }");
	assert!(validate::validate(&file).is_empty());
	let rust = crate::codegen::rust(&file, "tag.lsidl", &std::collections::HashMap::new()).unwrap();
	assert!(rust.contains("if r.tag()? { Some("), "the option tag is strict: {rust}");
	assert!(rust.contains("if r.tag()? { Ok("), "and so is the result tag");
	assert!(!rust.contains("r.u8()? != 0"), "no byte comparison survives anywhere in the generated codec");
}

#[test]
fn a_stream_frame_carries_every_capability_its_element_declares() {
	// THE SCHEMA AND THE WIRE MUST AGREE, which is what this milestone is named for and what the fix
	// for it broke one layer down. `report_cardinality` was relaxed to accept several capabilities
	// for replies and this call site went with it, while `{method}_frame` still ended with
	// `*frame_handle = writer.handle()` - the FIRST handle - and read with `Reader::with_handle`. A
	// two-capability frame would have had its second capability created, written into the writer,
	// dropped by the encoder, and decoded by the client as a placeholder.
	//
	// It was refused for a few hours and then made to work, which is the right order: close the gap,
	// then remove the reason for it.
	let file = parse_only("package liber:stream@1; resource chan; record pipe { input: handle<chan>, output: handle<chan> } interface feed { @op(1) frames: func() -> stream<pipe>; }");
	assert!(validate::validate(&file).is_empty(), "a two-capability frame is expressible now");
	let rust = crate::codegen::rust(&file, "stream.lsidl", &std::collections::HashMap::new()).unwrap();
	assert!(rust.contains("frame_handles: &mut Handles"), "and the writer takes the whole list");
	assert!(rust.contains("Reader::with_handles(msg, frame_handles)"), "and so does the reader");
	// Both capabilities are written, which is the property the placeholder used to replace.
	assert!(rust.contains("w.set_handle(self.input)?"), "the first is written");
	assert!(rust.contains("w.set_handle(self.output)?"), "and so is the second");
}

#[test]
fn a_stream_may_fail_before_it_starts() {
	// `result<stream<T>, error>` PARSED AND GENERATED NOTHING. The generator matched `Type::Stream`
	// on the return position directly, so a stream inside a result never reached that arm and the
	// value-position paths refused it with "stream is not supported in a value position" - codegen
	// then emitted `// tail (op 3) uses handle/buffer/stream; bindings deferred.` where the method
	// should be, the method vanished from the trait, and every consumer stopped compiling.
	//
	// The cost was not hypothetical: `Volume::list` answered a directory it could not read with an
	// EMPTY LISTING, which is the exact "a failure looks like an empty answer" semantics this
	// project spent a milestone removing everywhere else, and it could not be fixed from the
	// storage side while this shape emitted nothing.
	//
	// The wire shape is the ordinary result's: `corr`, a one-byte tag, and then either the
	// sub-channel handle (Ok) or the encoded error (Err). What is generated for it is a trait
	// method returning `Result<Vec<T>, E>`, an `_open` that carries that result out to the serve
	// loop, the two reply bodies the loop needs, and a client answering `Option<Result<u64, E>>`.
	let file = parse_only(LOG);
	let rust = crate::codegen::rust(&file, "log.lsidl", &std::collections::HashMap::new()).unwrap();
	assert!(!rust.contains("bindings deferred"), "nothing about this schema is deferred any more");

	// The service side: the result reaches the implementor, and `_open` hands it to the loop.
	assert!(rust.contains("fn tail(&mut self, q: Query) -> Result<Vec<Entry>, Error>;"), "the trait carries the error arm");
	assert!(rust.contains("pub fn tail_open<S: Service>(service: &mut S, request: &[u8], request_handles: &mut Handles) -> Option<(u32, Result<Vec<Entry>, Error>)>"));

	// The two reply bodies. The serve loop owns the sub-channel, so the Ok body is `corr`, the tag
	// and the handle's placeholder with the capability travelling in the reply's handle list; the
	// Err body carries the error and NO handle, so a client that decodes a failure is not also left
	// holding a channel nobody will ever write to.
	assert!(rust.contains("pub fn tail_reply_ok(corr: u32, out: &mut [u8]) -> Option<usize>"));
	assert!(rust.contains("pub fn tail_reply_err(corr: u32, error: &Error, out: &mut [u8]) -> Option<usize>"));

	// The client side, and the handle accounting on both arms of the tag. An `Ok` with anything but
	// one handle, or an `Err` with any at all, is a reply this schema cannot have produced - and
	// every one of those handles is a capability, so they are discarded rather than dropped.
	assert!(rust.contains("pub fn tail(&mut self, q: &Query) -> Option<Result<u64, Error>>"));
	assert!(rust.contains("if reply_handles.len() != 1 { return None; }"));
	assert!(rust.contains("if !reply_handles.is_empty() { return None; }"));
	assert!(rust.contains("if !matches!(decoded, Some(Ok(_))) {"));

	// The per-element framing is unchanged - the error arm is about whether the stream STARTS.
	assert!(rust.contains("pub fn tail_frame(seq: u32, item: &Entry, out: &mut [u8], frame_handles: &mut Handles) -> Option<usize>"));
	assert!(rust.contains("pub fn tail_read(msg: &[u8], frame_handles: &Handles) -> Option<Entry>"));

	// And the bare `stream<T>` form still generates what it did: no tag, no error, the handle alone.
	let bare = parse_only("package liber:stream@1; record item { n: u32 } interface feed { @op(1) open: func() -> stream<item>; }");
	let rust = crate::codegen::rust(&bare, "stream.lsidl", &std::collections::HashMap::new()).unwrap();
	assert!(rust.contains("fn open(&mut self) -> Vec<Item>;"));
	assert!(rust.contains("pub fn open(&mut self) -> Option<u64>"));
	assert!(!rust.contains("open_reply_ok"), "a stream with no error arm has no refusal body to emit");
}

#[test]
fn every_generated_dispatch_answers_the_identity_query() {
	// The query must be emitted for every interface, ahead of the typed match so no method can
	// shadow it, and it must report the package it was generated from rather than a guess.
	let file = parse_only("package liber:identity@7; enum error { x } interface api { @op(1) run: func() -> result<unit, error>; }");
	assert!(validate::validate(&file).is_empty());
	let rust = crate::codegen::rust(&file, "identity.lsidl", &std::collections::HashMap::new()).unwrap();
	assert!(rust.contains("if op == PROTOCOL_INFO_OP {"), "the identity query is emitted");
	assert!(rust.contains("w.bytes_lp(b\"liber:identity\")?;"), "it reports the colon-joined package path");
	assert!(rust.contains("w.u32(7)?;"), "it reports the declared package version");
	let query = rust.find("if op == PROTOCOL_INFO_OP {").unwrap();
	let typed = rust.find("match op {").unwrap();
	assert!(query < typed, "the query is answered before the typed match, so no @op can shadow it");
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

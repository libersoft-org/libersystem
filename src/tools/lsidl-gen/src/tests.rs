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
fn the_documented_opcode_range_is_the_one_the_validator_enforces() {
	// `docs/LSIDL.md` gave the range as `1..=65531` with `0xfffc..=0xffff` reserved, and `65531` IS
	// `PROTOCOL_INFO_OP`. A schema author following the specification would have been refused by a
	// validator reading `TYPED_OP_MAX`, with a diagnostic contradicting the document they had just
	// read - the specification handing out an opcode that collides.
	//
	// So the document's number is checked against the constant rather than trusted. This is the
	// cheap half of "make the range a generated fact": the fact stays written, and a test refuses to
	// let it drift.
	let doc = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/LSIDL.md")).expect("the specification is beside the generator");
	let stated = format!("`1..=abi::TYPED_OP_MAX`, which is `1..={}` (`{:#06x}`)", abi::TYPED_OP_MAX, abi::TYPED_OP_MAX);
	assert!(doc.contains(&stated), "the specification must state the constant's value: expected {stated:?}");
	// And every reserved opcode by name, so a reader does not have to remember a range.
	for (name, value) in [
		("PROTOCOL_INFO_OP", abi::PROTOCOL_INFO_OP),
		("GOODBYE_OP", abi::GOODBYE_OP),
		("RESOLVE_OP", abi::RESOLVE_OP),
		("HEARTBEAT_OP", abi::HEARTBEAT_OP),
		("CONNECT_OP", abi::CONNECT_OP),
	] {
		assert!(doc.contains(&format!("`{name}` (`{value:#06x}`")), "the specification must name {name} and its value {value:#06x}");
	}

	// The validator's own boundary, so the two are pinned to each other rather than each to a
	// number: the largest legal opcode is accepted and the first reserved one is refused.
	let legal = format!("resource r; interface i {{ @op({}) m: func() -> unit; }}", abi::TYPED_OP_MAX);
	assert!(errors(&wrap(&legal)).is_empty(), "the largest documented opcode is legal: {:?}", errors(&wrap(&legal)));
	let reserved = format!("resource r; interface i {{ @op({}) m: func() -> unit; }}", abi::PROTOCOL_INFO_OP);
	assert_err_contains(&wrap(&reserved), &format!("must be in 1..={}", abi::TYPED_OP_MAX));
}

#[test]
fn the_fixed_buffer_encode_refuses_to_drop_a_capability() {
	// `encode_vec` was corrected for this and `encode` was not, so the pair the generator advertises
	// as a round trip had one half that still lost the live part of a value: `encode` returns a
	// LENGTH, so a capability recorded during `write` stayed in a writer the caller drops, and
	// `OpenResult { file: 123, .. }.encode(&mut buf)` answered `Some(n)` with the handle gone.
	let file = parse_only("package liber:enc@1; resource file; record held { file: handle<file>, size: u64 } record plain { size: u64 } interface i { @op(1) m: func() -> held; }");
	assert!(validate::validate(&file).is_empty());
	let rust = crate::codegen::rust(&file, "enc.lsidl", &std::collections::HashMap::new()).unwrap();
	assert!(rust.contains("// A capability recorded here would be dropped by returning the length alone."), "the fixed-buffer encode refuses too: {rust}");
	assert!(rust.contains("if w.has_handle() { return None; }"), "and refuses by asking the writer");
	// Both halves of the pair, so neither can be corrected alone again.
	assert!(rust.contains("if !w.handles().is_empty() { return None; }"), "encode_vec still refuses");
	assert!(rust.contains("pub fn encode_message(&self) -> Option<(Vec<u8>, Handles)>"), "and the shape that carries both halves is still generated");
}

#[test]
fn the_validator_counts_capabilities_and_names_the_limit() {
	// `Many` COULD NOT TELL FOUR FROM FIVE. It meant "more than one", so a five-capability record
	// validated and the writer - which refuses past `MAX_HANDLES` - answered `None` on the fifth: a
	// schema expressing what the wire cannot carry, which is this milestone's own defect at the other
	// end of the same road. The cardinality is a NUMBER now, so the refusal can print both sides.
	let four = "resource chan; record quad { a: handle<chan>, b: handle<chan>, c: handle<chan>, d: handle<chan> } interface i { @op(1) m: func() -> quad; }";
	assert!(errors(&wrap(four)).is_empty(), "four is what a message carries: {:?}", errors(&wrap(four)));

	let five = "resource chan; record quint { a: handle<chan>, b: handle<chan>, c: handle<chan>, d: handle<chan>, e: handle<chan> } interface i { @op(1) m: func() -> quint; }";
	assert_err_contains(&wrap(five), "carries 5 capabilities and a message carries at most 4");

	// COMPOSED, not just declared side by side: two records of two are four, and three are six. The
	// count has to add up through nesting or it is a check on one shape rather than on the message.
	let pair = "resource chan; record pair { a: handle<chan>, b: handle<chan> }";
	assert!(errors(&wrap(&format!("{pair} record two {{ x: pair, y: pair }} interface i {{ @op(1) m: func() -> two; }}"))).is_empty(), "two pairs are four");
	assert_err_contains(&wrap(&format!("{pair} record three {{ x: pair, y: pair, z: pair }} interface i {{ @op(1) m: func() -> three; }}")), "carries 6 capabilities");

	// A RESULT'S ARMS ARE ALTERNATIVES, so the worst case is what has to fit rather than the sum.
	// Summing them would refuse a shape the wire carries fine.
	let arms = format!("{pair} record other {{ c: handle<chan>, d: handle<chan> }} interface i {{ @op(1) m: func() -> result<pair, other>; }}");
	assert!(errors(&wrap(&arms)).is_empty(), "one arm arrives, not both: {:?}", errors(&wrap(&arms)));

	// And the REQUEST is counted the same way as the reply - a method's parameters are side by side
	// in one message.
	assert_err_contains(&wrap("resource chan; interface i { @op(1) m: func(a: handle<chan>, b: handle<chan>, c: handle<chan>, d: handle<chan>, e: handle<chan>) -> unit; }"), "carries 5 capabilities and a message carries at most 4");
}

#[test]
fn a_list_of_handle_bearing_values_is_refused_because_a_failed_encode_cannot_give_them_back() {
	// THIS TEST USED TO ASSERT THE OPPOSITE, and the argument it carried was that the count is a
	// property of the value rather than of the schema, so the wire bounds it: `set_handle` refuses
	// past the limit and the whole message fails to encode.
	//
	// The message does fail. The capability does not come back. `set_handle` refuses at the
	// BOUNDARY, so the fifth handle never enters the writer, and the generated failure cleanup hands
	// back `writer.handles()` - the four that did. The fifth is in the caller's `Vec<u64>`, which
	// has no `Drop`, and nothing else knows it exists. "The message fails rather than losing a
	// capability" was true of the bytes.
	assert_err_contains(&wrap("resource file;\nrecord item { file: handle<file> }\nrecord batch { items: list<item> }"), "collection of capabilities");
	// And a list of values carrying NO capability is untouched, which is every list in this tree.
	parse_ok(&wrap("record item { name: string }\nrecord batch { items: list<item> }"));
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
	// RESOLVED, so its cardinality is known - and a list of capability-bearing values is refused for
	// what it is rather than for being unknown. The two refusals are what this test tells apart:
	// resolution makes the shape legible, and the shape is then judged on its merits. Before the
	// collection refusal existed this asserted `errors.is_empty()`, which proved resolution had
	// happened by the absence of any complaint - a weaker signal, since a validator that had
	// silently given up would also produce none.
	assert!(errors.iter().any(|error| error.msg.contains("collection of capabilities")), "a resolved list of handle-bearing values is refused for its shape: {errors:?}");
	assert!(!errors.iter().any(|error| error.msg.contains("has not been resolved")), "and not for being unresolved, which it is not: {errors:?}");
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
	assert!(rust.contains("pub fn tail_read(msg: &[u8], frame_handles: &mut Handles) -> Option<Entry>"));
	// AND IT SPENDS THE LIST IT WAS GIVEN. `finish` already requires every transferred handle to be
	// adopted, so a successful read leaves nothing for the caller to close and says so by clearing
	// the list - which is what lets a consumer close `frame_handles` unconditionally instead of
	// knowing whether the decode succeeded.
	assert!(rust.contains("reader.finish()?;\n\t\tframe_handles.clear();"), "a successful frame read empties the caller's handle list");

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

#[test]
fn every_generated_message_boundary_ends_with_finish() {
	// ONE RULE, ASSERTED STRUCTURALLY, because stating it in a comment on one path is what produced
	// the three that did not have it.
	//
	// `Reader::finish` answers both halves of a framing question - every byte consumed and every
	// transferred handle taken - and the ordinary reply path has called it since the finding that
	// named it. Three boundaries beside it did not: `protocol_info`'s client, the server's
	// `PROTOCOL_INFO_OP` arm, and both stream-open replies. Each was written next to the ordinary
	// path rather than through it, so each inherited the shape and not the rule.
	//
	// The schema below has every one of them in one file: a plain method, the identity query the
	// generator always emits, a bare `stream<T>` and a `result<stream<T>, E>`.
	let file = parse_only(
		"package liber:bound@1; enum error { bad } record item { n: u32 } \
		 interface svc { @op(1) plain: func(v: u32) -> u32; @op(2) feed: func() -> stream<item>; @op(3) guarded: func() -> result<stream<item>, error>; }",
	);
	assert!(validate::validate(&file).is_empty());
	let rust = crate::codegen::rust(&file, "bound.lsidl", &std::collections::HashMap::new()).unwrap();

	// Every `Reader` a generated function builds is a message being read, so every one of them has
	// to reach a `finish`. Counting is what makes this a rule rather than four assertions that a
	// fifth boundary can be added beside.
	let readers = rust.matches("Reader::new(").count() + rust.matches("Reader::with_handle_list(").count() + rust.matches("Reader::with_handles(").count();
	let finishes = rust.matches(".finish()").count();
	assert!(finishes >= readers, "every message a generated function reads must end at a boundary check: {readers} readers, {finishes} finishes\n{rust}");

	// And the three that were missing, named so a regression says which one came back.
	assert!(rust.contains("let version = r.u32()?;\n\t\t\tr.finish()?;"), "the identity query's client ends its reply: {rust}");
	assert!(rust.contains("if op == PROTOCOL_INFO_OP {\n\t\t\tr.finish()?;"), "and its server ends the request");
	assert!(rust.contains("if r.u32()? != corr || r.finish().is_none() || reply_handles.len() != 1 {"), "a bare stream-open reply ends too");
	assert!(rust.contains("let _ = r.u32()?;\n\t\t\t\t\tr.finish()?;"), "and so does the Ok arm of a guarded one");
}

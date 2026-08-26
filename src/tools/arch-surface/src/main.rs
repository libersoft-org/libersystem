// A production architecture-contract member whose whole answer is a placeholder.
//
// WHY THIS IS NOT A GREP. The sentence being enforced is "no production architecture contract member
// whose answer is `todo!`, `unimplemented!` or an unconditional placeholder panic", and the first two
// are findable by pattern while the third is not. A `panic!` in architecture code is very often
// CORRECT: a firmware value this port cannot address, a state the machine must not be in, an
// invariant whose violation means the tables lied. Banning the macro would push those refusals into
// silent fallbacks, which is the failure this tree keeps finding. What must fail is a function whose
// ENTIRE production body is one panic - a stub wearing a different macro.
//
// Telling those apart needs the item, not the line. This walks the file as tokens rather than as
// text: it knows what is inside a comment, a string or a character literal, so a brace in any of
// them does not move its depth, and it matches `#[cfg(test)]` items by their real extent instead of
// by guessing where a block ends. That is what the line-oriented filter this replaces could not do -
// it skipped whatever followed a `#[cfg(test)]` until the braces looked balanced, which is right for
// a braced module and wrong for every other shape the attribute takes.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
	let mut args = std::env::args().skip(1);
	let mut roots: Vec<PathBuf> = Vec::new();
	let mut self_test = false;
	for arg in args.by_ref() {
		match arg.as_str() {
			"--self-test" => self_test = true,
			other => roots.push(PathBuf::from(other)),
		}
	}
	if self_test {
		return match run_self_test() {
			Ok(()) => {
				println!("arch-surface: the scanner refuses every shape it is meant to and accepts every shape it is not");
				ExitCode::SUCCESS
			}
			Err(reason) => {
				eprintln!("arch-surface: SELF-TEST FAILED - {reason}");
				ExitCode::FAILURE
			}
		};
	}
	if roots.is_empty() {
		eprintln!("usage: arch-surface [--self-test] <directory>...");
		return ExitCode::from(2);
	}

	let mut files: Vec<PathBuf> = Vec::new();
	for root in &roots {
		if let Err(reason) = collect(root, &mut files) {
			eprintln!("arch-surface: cannot read {}: {reason}", root.display());
			return ExitCode::FAILURE;
		}
	}
	files.sort();

	let mut findings = 0usize;
	for file in &files {
		let text = match std::fs::read_to_string(file) {
			Ok(text) => text,
			Err(reason) => {
				eprintln!("arch-surface: cannot read {}: {reason}", file.display());
				return ExitCode::FAILURE;
			}
		};
		for finding in scan(&text) {
			eprintln!("arch-surface: {}:{}: {} - {}", file.display(), finding.line, finding.name, finding.what);
			findings += 1;
		}
	}
	if findings > 0 {
		eprintln!("arch-surface: {findings} production contract member(s) answer with a placeholder");
		return ExitCode::FAILURE;
	}
	println!("arch-surface: {} file(s), no production contract member answers with a placeholder", files.len());
	ExitCode::SUCCESS
}

// Every `.rs` file under `root` except the test files, which are tests by their name.
fn collect(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
	for entry in std::fs::read_dir(root)? {
		let entry = entry?;
		let path = entry.path();
		if path.is_dir() {
			collect(&path, out)?;
		} else if path.extension().is_some_and(|e| e == "rs") && path.file_name().is_some_and(|n| n != "tests.rs") {
			out.push(path);
		}
	}
	Ok(())
}

pub struct Finding {
	pub line: usize,
	pub name: String,
	pub what: &'static str,
}

// One token of Rust, as far as this needs to know what one is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tok {
	Word,
	Bang,
	OpenBrace,
	CloseBrace,
	OpenParen,
	CloseParen,
	OpenBracket,
	CloseBracket,
	Hash,
	Semicolon,
	// A `cfg` predicate's argument separator. `all(test, unix)` is two arguments and `all(test)` is
	// one, and telling them apart is what makes "every arm is a test arm" a question with an answer.
	Comma,
	Other,
}

struct Token {
	kind: Tok,
	text: String,
	line: usize,
}

// Split source into tokens, dropping comments and treating every literal as one opaque token.
//
// The point of doing this rather than reading lines is that a brace inside `"{"` or `// {` must not
// move the nesting depth, and a `panic!("}")` must not close the function it is inside.
fn tokenize(text: &str) -> Vec<Token> {
	let bytes = text.as_bytes();
	let mut out = Vec::new();
	let mut i = 0usize;
	let mut line = 1usize;
	while i < bytes.len() {
		let c = bytes[i];
		if c == b'\n' {
			line += 1;
			i += 1;
			continue;
		}
		if c.is_ascii_whitespace() {
			i += 1;
			continue;
		}
		// Comments, both kinds, with nesting for the block form as Rust has it.
		if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
			while i < bytes.len() && bytes[i] != b'\n' {
				i += 1;
			}
			continue;
		}
		if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
			let mut depth = 1usize;
			i += 2;
			while i < bytes.len() && depth > 0 {
				if bytes[i] == b'\n' {
					line += 1;
				}
				if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
					depth += 1;
					i += 2;
					continue;
				}
				if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
					depth -= 1;
					i += 2;
					continue;
				}
				i += 1;
			}
			continue;
		}
		// Raw strings: r"...", r#"..."#, and any number of hashes.
		if c == b'r' && i + 1 < bytes.len() && (bytes[i + 1] == b'"' || bytes[i + 1] == b'#') {
			let mut hashes = 0usize;
			let mut j = i + 1;
			while j < bytes.len() && bytes[j] == b'#' {
				hashes += 1;
				j += 1;
			}
			if j < bytes.len() && bytes[j] == b'"' {
				j += 1;
				let closing = format!("\"{}", "#".repeat(hashes));
				loop {
					if j >= bytes.len() {
						break;
					}
					if bytes[j] == b'\n' {
						line += 1;
					}
					if text[j..].starts_with(&closing) {
						j += closing.len();
						break;
					}
					j += 1;
				}
				out.push(Token { kind: Tok::Other, text: String::new(), line });
				i = j;
				continue;
			}
		}
		// Ordinary strings and byte strings.
		if c == b'"' || (c == b'b' && i + 1 < bytes.len() && bytes[i + 1] == b'"') {
			let mut j = if c == b'"' { i + 1 } else { i + 2 };
			while j < bytes.len() {
				if bytes[j] == b'\\' {
					j += 2;
					continue;
				}
				if bytes[j] == b'\n' {
					line += 1;
				}
				if bytes[j] == b'"' {
					j += 1;
					break;
				}
				j += 1;
			}
			out.push(Token { kind: Tok::Other, text: String::new(), line });
			i = j;
			continue;
		}
		// A character literal, told from a lifetime by what follows the quote.
		if c == b'\'' && i + 1 < bytes.len() {
			let escaped = bytes[i + 1] == b'\\';
			let close = if escaped { None } else { bytes.get(i + 2) };
			if escaped || close == Some(&b'\'') {
				let mut j = i + 1;
				while j < bytes.len() {
					if bytes[j] == b'\\' {
						j += 2;
						continue;
					}
					if bytes[j] == b'\'' {
						j += 1;
						break;
					}
					j += 1;
				}
				out.push(Token { kind: Tok::Other, text: String::new(), line });
				i = j;
				continue;
			}
		}
		if c.is_ascii_alphanumeric() || c == b'_' {
			let start = i;
			while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
				i += 1;
			}
			out.push(Token { kind: Tok::Word, text: text[start..i].to_string(), line });
			continue;
		}
		let kind = match c {
			b'!' => Tok::Bang,
			b'{' => Tok::OpenBrace,
			b'}' => Tok::CloseBrace,
			b'(' => Tok::OpenParen,
			b')' => Tok::CloseParen,
			b',' => Tok::Comma,
			b'[' => Tok::OpenBracket,
			b']' => Tok::CloseBracket,
			b'#' => Tok::Hash,
			b';' => Tok::Semicolon,
			_ => Tok::Other,
		};
		out.push(Token { kind, text: String::new(), line });
		i += 1;
	}
	out
}

// Every production function whose whole body is a placeholder.
pub fn scan(text: &str) -> Vec<Finding> {
	let tokens = tokenize(text);
	let mut out = Vec::new();
	let mut i = 0usize;
	while i < tokens.len() {
		// An attribute: `#[...]`, possibly `#![...]`. If it is a `cfg(test)` one, the item it
		// decorates is skipped WHOLE - whatever shape that item has.
		if tokens[i].kind == Tok::Hash {
			let (is_cfg_test, after) = read_attribute(&tokens, i);
			if is_cfg_test {
				i = skip_item(&tokens, after);
			} else {
				i = after;
			}
			continue;
		}
		if tokens[i].kind == Tok::Word && tokens[i].text == "fn" {
			let name = tokens.get(i + 1).map(|t| t.text.clone()).unwrap_or_default();
			let Some(open) = find_body(&tokens, i) else {
				i += 1;
				continue;
			};
			if let Some(what) = placeholder_body(&tokens, open) {
				out.push(Finding { line: tokens[i].line, name, what });
			}
			i = matching_brace(&tokens, open).map_or(tokens.len(), |end| end + 1);
			continue;
		}
		i += 1;
	}
	out
}

// Read `#[ ... ]` starting at `at`, answering whether it gates the item on TESTS and where it ends.
//
// THE PREDICATE, NOT THE WORDS IN IT. This answered yes for any attribute containing both `cfg` and
// `test` anywhere inside it, which is three different mistakes at once:
//
//   `#[cfg(not(test))]`                  production code, skipped as test-only
//   `#[cfg(any(test, target_arch = ..))]` production on that target, skipped as test-only
//   `#[cfg_attr(test, doc = "..")]`      production code with one attribute applied under test
//
// All three are ordinary shapes and all three took the whole item out of the scan, so a placeholder
// body inside any of them was invisible to a gate whose entire job is finding them. `cfg_attr` in
// particular is not a gate at all - it applies an ATTRIBUTE conditionally and leaves the item
// compiled either way.
//
// So the predicate is walked: `test` is a test gate, `not(..)` inverts whatever it wraps, `all`
// wants every argument, `any` wants one, and anything else is a term this scanner cannot evaluate
// and does not treat as a gate.
fn read_attribute(tokens: &[Token], at: usize) -> (bool, usize) {
	let mut i = at + 1;
	if tokens.get(i).map(|t| t.kind) == Some(Tok::Bang) {
		i += 1;
	}
	if tokens.get(i).map(|t| t.kind) != Some(Tok::OpenBracket) {
		return (false, at + 1);
	}
	let start = i;
	let mut depth = 0usize;
	while i < tokens.len() {
		match tokens[i].kind {
			Tok::OpenBracket => depth += 1,
			Tok::CloseBracket => {
				depth -= 1;
				if depth == 0 {
					break;
				}
			}
			_ => {}
		}
		i += 1;
	}
	// `#[` `cfg` `(` .. `)` `]` - the predicate is what sits between the parentheses after `cfg`.
	// Only `cfg` gates an item; `cfg_attr` conditions an attribute and the item is compiled anyway.
	let gates = tokens.get(start + 1).is_some_and(|t| t.kind == Tok::Word && t.text == "cfg") && tokens.get(start + 2).is_some_and(|t| t.kind == Tok::OpenParen) && predicate_is_test(tokens, start + 2);
	(gates, i + 1)
}

// Whether the `cfg` predicate whose opening parenthesis is at `open` is one that holds ONLY under
// test. `true` means the item does not exist in a production build and the scan may skip it.
fn predicate_is_test(tokens: &[Token], open: usize) -> bool {
	// The single term inside the parentheses, or - for `all`/`any`/`not` - the arguments it takes.
	let close = matching_paren(tokens, open);
	let Some(close) = close else { return false };
	let inner = open + 1;
	if inner >= close {
		return false;
	}
	match tokens[inner].kind {
		Tok::Word if tokens[inner].text == "test" && inner + 1 == close => true,
		Tok::Word if matches!(tokens[inner].text.as_str(), "not" | "all" | "any") && tokens.get(inner + 1).is_some_and(|t| t.kind == Tok::OpenParen) => {
			let group = inner + 1;
			let args = arguments(tokens, group);
			match tokens[inner].text.as_str() {
				// A negation is never a test gate. `not(test)` is the PRODUCTION half of a split,
				// which is the case that sent this scanner past whole production functions, and
				// `not(unix)` is not about tests at all. `not(not(test))` would be, and answering
				// `false` for it scans an item that could have been skipped - which is the safe
				// direction for a gate whose failure mode is skipping.
				"not" => false,
				// `all(test, unix)` needs `test` to hold, so it cannot be compiled without it: ONE
				// test arm makes the whole conjunction test-only.
				"all" => args.iter().any(|&a| term_is_test(tokens, a)),
				// `any(test, unix)` compiles wherever EITHER holds, so it is production code on
				// unix. Every arm has to be a test arm for the item to be absent from a production
				// build.
				_ => !args.is_empty() && args.iter().all(|&a| term_is_test(tokens, a)),
			}
		}
		_ => false,
	}
}

// One argument of an `all`/`any`/`not` list: either the bare word `test`, or a nested predicate.
fn term_is_test(tokens: &[Token], at: usize) -> bool {
	if tokens[at].kind == Tok::Word && tokens[at].text == "test" {
		return !tokens.get(at + 1).is_some_and(|t| t.kind == Tok::OpenParen);
	}
	if tokens[at].kind == Tok::Word && matches!(tokens[at].text.as_str(), "not" | "all" | "any") && tokens.get(at + 1).is_some_and(|t| t.kind == Tok::OpenParen) {
		// `predicate_is_test` reads the operator from the token before the parenthesis, which is
		// exactly what `at` is, so the same walk answers a nested list.
		return predicate_is_test(tokens, at + 1);
	}
	false
}

// Where the group opened at `open` closes.
fn matching_paren(tokens: &[Token], open: usize) -> Option<usize> {
	let mut depth = 0usize;
	for (offset, token) in tokens.iter().enumerate().skip(open) {
		match token.kind {
			Tok::OpenParen => depth += 1,
			Tok::CloseParen => {
				depth -= 1;
				if depth == 0 {
					return Some(offset);
				}
			}
			_ => {}
		}
	}
	None
}

// The index of each top-level argument inside the group opened at `open`.
fn arguments(tokens: &[Token], open: usize) -> Vec<usize> {
	let Some(close) = matching_paren(tokens, open) else { return Vec::new() };
	let mut out = Vec::new();
	let mut depth = 0usize;
	let mut expect = true;
	for (offset, token) in tokens.iter().enumerate().take(close).skip(open + 1) {
		match token.kind {
			Tok::OpenParen => depth += 1,
			Tok::CloseParen => depth -= 1,
			Tok::Comma if depth == 0 => {
				expect = true;
				continue;
			}
			_ => {}
		}
		if expect && depth == 0 {
			out.push(offset);
			expect = false;
		}
	}
	out
}

// Where the item starting at `at` ends: past its block, or past its semicolon.
fn skip_item(tokens: &[Token], at: usize) -> usize {
	let mut i = at;
	while i < tokens.len() {
		match tokens[i].kind {
			Tok::OpenBrace => return matching_brace(tokens, i).map_or(tokens.len(), |end| end + 1),
			Tok::Semicolon => return i + 1,
			// Another attribute on the same item - `#[cfg(test)] #[allow(...)] fn ...`.
			Tok::Hash => {
				let (_, after) = read_attribute(tokens, i);
				i = after;
			}
			_ => i += 1,
		}
	}
	tokens.len()
}

// The `{` that opens the body of the `fn` at `at`, or None for a signature with no body.
fn find_body(tokens: &[Token], at: usize) -> Option<usize> {
	let mut i = at;
	let mut paren = 0usize;
	let mut angle_free = true;
	while i < tokens.len() {
		match tokens[i].kind {
			Tok::OpenParen => paren += 1,
			Tok::CloseParen => paren = paren.saturating_sub(1),
			Tok::OpenBrace if paren == 0 && angle_free => return Some(i),
			Tok::Semicolon if paren == 0 => return None,
			_ => {}
		}
		// A `where` clause can carry braces in a closure bound; this tree has none, and if one
		// arrives the worst case is a body this declines to judge rather than a wrong verdict.
		if tokens[i].kind == Tok::Word && tokens[i].text == "where" {
			angle_free = true;
		}
		i += 1;
	}
	None
}

fn matching_brace(tokens: &[Token], open: usize) -> Option<usize> {
	let mut depth = 0usize;
	let mut i = open;
	while i < tokens.len() {
		match tokens[i].kind {
			Tok::OpenBrace => depth += 1,
			Tok::CloseBrace => {
				depth -= 1;
				if depth == 0 {
					return Some(i);
				}
			}
			_ => {}
		}
		i += 1;
	}
	None
}

// Whether the body opened at `open` is nothing but a placeholder call.
//
// EXACTLY ONE STATEMENT, and it is the macro. `panic!` anywhere else in a body is a refusal the
// function reaches under a condition, which is correct code and stays that way.
fn placeholder_body(tokens: &[Token], open: usize) -> Option<&'static str> {
	let end = matching_brace(tokens, open)?;
	let body = &tokens[open + 1..end];
	let mut i = 0usize;
	// `unsafe { ... }` around the whole body is the same body.
	if body.first().is_some_and(|t| t.kind == Tok::Word && t.text == "unsafe") && body.get(1).is_some_and(|t| t.kind == Tok::OpenBrace) {
		let inner_end = matching_brace(body, 1)?;
		if inner_end + 1 == body.len() {
			return placeholder_body(body, 1);
		}
	}
	let name = match body.get(i) {
		Some(t) if t.kind == Tok::Word => t.text.as_str(),
		_ => return None,
	};
	let what = match name {
		"todo" => "its whole body is todo!()",
		"unimplemented" => "its whole body is unimplemented!()",
		"panic" => "its whole body is an unconditional panic!(), which is a stub wearing a different macro",
		_ => return None,
	};
	i += 1;
	if body.get(i).map(|t| t.kind) != Some(Tok::Bang) {
		return None;
	}
	i += 1;
	if body.get(i).map(|t| t.kind) != Some(Tok::OpenParen) {
		return None;
	}
	let mut depth = 0usize;
	while i < body.len() {
		match body[i].kind {
			Tok::OpenParen => depth += 1,
			Tok::CloseParen => {
				depth -= 1;
				if depth == 0 {
					break;
				}
			}
			_ => {}
		}
		i += 1;
	}
	i += 1;
	// A trailing semicolon is allowed and nothing else is: one statement, and it is this one.
	if body.get(i).map(|t| t.kind) == Some(Tok::Semicolon) {
		i += 1;
	}
	if i == body.len() { Some(what) } else { None }
}

// The scanner refuses every shape it is meant to and accepts every shape it is not.
//
// A gate whose failure path has never run is a gate whose failure path is untested code, and this
// one replaced a filter that had stopped matching one of the three constructs its own milestone
// named. Each case below is a sentence about what the scanner must decide, not about how it decides.
fn run_self_test() -> Result<(), String> {
	let must_find: &[(&str, &str)] = &[
		("a body that is only todo!()", "pub fn live() {\n\ttodo!(\"not done\")\n}\n"),
		("a body that is only unimplemented!()", "fn live() { unimplemented!() }\n"),
		("a body that is only an unconditional panic", "pub fn entry(x: u64) -> u64 {\n\tpanic!(\"this port has no entry\")\n}\n"),
		("a panic-only body wrapped in unsafe", "pub unsafe fn entry() {\n\tunsafe { panic!(\"no\") }\n}\n"),
		("a panic-only body with a trailing semicolon", "fn entry() {\n\tpanic!(\"no\");\n}\n"),
		("a panic-only body on one line", "fn entry() { panic!(\"no\") }\n"),
		// THE THREE SHAPES THAT USED TO BE SKIPPED AS TEST-ONLY. Each is production code, and the
		// attribute filter answered yes to all of them because it looked for the words `cfg` and
		// `test` anywhere inside the attribute rather than reading the predicate.
		("a placeholder under cfg(not(test)), which is the production half of a split", "#[cfg(not(test))]\nfn entry() { todo!() }\n"),
		("a placeholder under cfg(any(test, target_arch)), which compiles on that target", "#[cfg(any(test, target_arch = \"x86_64\"))]\nfn entry() { todo!() }\n"),
		("a placeholder under cfg_attr(test, ..), which gates an attribute and not the item", "#[cfg_attr(test, doc = \"under test\")]\nfn entry() { todo!() }\n"),
	];
	let must_accept: &[(&str, &str)] = &[
		("a panic after a check, which is a refusal and not a stub", "fn entry(n: u64) -> u64 {\n\tif n == 0 {\n\t\tpanic!(\"the firmware published no interrupt files\");\n\t}\n\tn\n}\n"),
		("a panic among other statements", "fn entry() {\n\tlet n = read();\n\tpanic!(\"unreachable: {n}\")\n}\n"),
		("a brace inside a string literal", "fn entry() {\n\tlet s = \"}{\";\n\tuse_it(s)\n}\n"),
		("a brace inside a comment", "fn entry() {\n\t// }\n\tuse_it()\n}\n"),
		("a placeholder inside a cfg(test) module", "#[cfg(test)]\nmod tests {\n\tfn probe() {\n\t\ttodo!(\"a deliberate test fault\")\n\t}\n}\n"),
		("a placeholder in a cfg(test) function with no block of its own", "#[cfg(test)]\nfn probe() { todo!() }\nfn live() { work() }\n"),
		// A `cfg_attr` beside a REAL gate. The fixture used to be only this, so it passed on the
		// `#[cfg(test)]` below it and said nothing about the `cfg_attr` it was named for - which is
		// why the `must_find` list above now carries a bare one.
		("a placeholder under cfg_attr(test, ...) beside a real cfg(test)", "#[cfg_attr(test, doc = \"a fixture\")]\n#[cfg(test)]\nfn probe() { todo!() }\n"),
		("a placeholder under cfg(all(test, target_arch)), which needs test to compile at all", "#[cfg(all(test, target_arch = \"x86_64\"))]\nfn probe() { todo!() }\n"),
		("a placeholder under cfg(any(test, doc)), where every arm is a test arm", "#[cfg(any(test, test))]\nfn probe() { todo!() }\n"),
		("a trait method with no body at all", "trait Port {\n\tfn entry(&self) -> u64;\n}\n"),
		("a panic! in a nested block", "fn entry() {\n\tloop {\n\t\tpanic!(\"stop\")\n\t}\n}\n"),
	];
	let mut complaints = String::new();
	for (what, source) in must_find {
		if scan(source).is_empty() {
			let _ = write!(complaints, "\n  did not find: {what}");
		}
	}
	for (what, source) in must_accept {
		let found = scan(source);
		if !found.is_empty() {
			let _ = write!(complaints, "\n  wrongly reported: {what} (as {})", found[0].what);
		}
	}
	if complaints.is_empty() { Ok(()) } else { Err(complaints) }
}

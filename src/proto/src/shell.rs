//! The shell's line language, kept pure and host-tested so it is exercised
//! without booting.
//!
//! These are the steps that turn a raw input line into the command (or the
//! `NAME=VALUE` assignment) the shell's dispatcher routes: trim the line,
//! normalize the Linux-style `--json` / `--json-min` / `--cbor` flags to the bare
//! `json` / `json-min` / `cbor` tokens the dispatch and the tools match on, expand
//! `$NAME` / `${NAME}` references against the environment, and detect a bare
//! assignment. `parse_and_expand` is the whole read-side pipeline the REPL runs
//! before it dispatches; the pieces are public so a caller (and a test) can reach
//! them on their own.

use alloc::string::String;
use alloc::vec::Vec;

// Drop leading and trailing ASCII whitespace from a byte slice.
pub fn trim(mut s: &[u8]) -> &[u8] {
	while let [first, rest @ ..] = s {
		if first.is_ascii_whitespace() {
			s = rest;
		} else {
			break;
		}
	}
	while let [rest @ .., last] = s {
		if last.is_ascii_whitespace() {
			s = rest;
		} else {
			break;
		}
	}
	s
}

// Rewrite the Linux-style `--json` / `--json-min` / `--cbor` flag tokens to the bare
// `json` / `json-min` / `cbor` forms the dispatch arms and the tools match on - one
// canonical spelling inside, both accepted at the prompt.
pub fn normalize_flags(line: &[u8]) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::with_capacity(line.len());
	for (i, token) in line.split(|&b| b == b' ').enumerate() {
		if i > 0 {
			out.push(b' ');
		}
		match token {
			b"--json" => out.extend_from_slice(b"json"),
			b"--json-min" => out.extend_from_slice(b"json-min"),
			b"--cbor" => out.extend_from_slice(b"cbor"),
			_ => out.extend_from_slice(token),
		}
	}
	out
}

// Expand `$NAME` and `${NAME}` references in a command line against the environment cache,
// where a name is `[A-Za-z_][A-Za-z0-9_]*`. An unset name expands to nothing; a `$` not
// followed by a valid name (or an unterminated `${`) is left literal. The result is a
// fresh line the dispatcher then parses, so variables reach every command uniformly.
pub fn expand_vars(line: &[u8], vars: &[(String, String)]) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::with_capacity(line.len());
	let mut i: usize = 0;
	while i < line.len() {
		if line[i] != b'$' {
			out.push(line[i]);
			i += 1;
			continue;
		}
		// `${NAME}`: the name runs to the closing brace.
		if i + 1 < line.len() && line[i + 1] == b'{' {
			let start: usize = i + 2;
			match line[start..].iter().position(|&b: &u8| b == b'}') {
				Some(rel) => {
					push_var_value(&mut out, &line[start..start + rel], vars);
					i = start + rel + 1;
				}
				None => {
					// Unterminated `${`: leave it literal.
					out.push(b'$');
					i += 1;
				}
			}
			continue;
		}
		// `$NAME`: the name is the identifier run right after the `$`.
		let start: usize = i + 1;
		if start < line.len() && (line[start].is_ascii_alphabetic() || line[start] == b'_') {
			let mut end: usize = start + 1;
			while end < line.len() && (line[end].is_ascii_alphanumeric() || line[end] == b'_') {
				end += 1;
			}
			push_var_value(&mut out, &line[start..end], vars);
			i = end;
		} else {
			// A lone `$` (or one before a non-name): keep it literal.
			out.push(b'$');
			i += 1;
		}
	}
	out
}

// Append the value of the named variable to `out`, or nothing if it is unset.
fn push_var_value(out: &mut Vec<u8>, name: &[u8], vars: &[(String, String)]) {
	if let Some((_, value)) = vars.iter().find(|(n, _): &&(String, String)| n.as_bytes() == name) {
		out.extend_from_slice(value.as_bytes());
	}
}

// Detect a bare `NAME=VALUE` assignment. The name must be a shell identifier
// (`[A-Za-z_][A-Za-z0-9_]*`) so a command with an `=` in an argument (a URL, a flag) is
// not mistaken for one; the value is everything after the first `=` and may be empty.
// Returns the name and value byte slices, the name valid UTF-8 by construction.
pub fn parse_assignment(line: &[u8]) -> Option<(&str, &[u8])> {
	let eq: usize = line.iter().position(|&b: &u8| b == b'=')?;
	let name: &[u8] = &line[..eq];
	if name.is_empty() {
		return None;
	}
	let head: u8 = name[0];
	if !(head.is_ascii_alphabetic() || head == b'_') {
		return None;
	}
	if !name.iter().all(|&b: &u8| b.is_ascii_alphanumeric() || b == b'_') {
		return None;
	}
	let value: &[u8] = &line[eq + 1..];
	Some((core::str::from_utf8(name).ok()?, value))
}

// The read-side pipeline the REPL runs before it dispatches: trim the raw input line,
// expand its `$NAME` / `${NAME}` references against the environment, then normalize its
// flags. The result is the line the dispatcher routes (or detects a `NAME=VALUE`
// assignment in). Pure - a fresh line in, a fresh line out - so the shell's whole line
// language is exercised on the host without a running system.
pub fn parse_and_expand(raw: &[u8], vars: &[(String, String)]) -> Vec<u8> {
	let expanded: Vec<u8> = expand_vars(trim(raw), vars);
	normalize_flags(&expanded)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn env() -> Vec<(String, String)> {
		alloc::vec![(String::from("NAME"), String::from("world")), (String::from("EMPTY"), String::from("")), (String::from("PATH"), String::from("vol://system"))]
	}

	#[test]
	fn trim_strips_both_ends() {
		assert_eq!(trim(b"  hello  "), b"hello");
		assert_eq!(trim(b"\t x \n"), b"x");
		assert_eq!(trim(b""), b"");
		assert_eq!(trim(b"   "), b"");
	}

	#[test]
	fn expand_resolves_both_reference_forms() {
		let v = env();
		assert_eq!(expand_vars(b"hi $NAME", &v), b"hi world");
		assert_eq!(expand_vars(b"${NAME}!", &v), b"world!");
		assert_eq!(expand_vars(b"$NAME$NAME", &v), b"worldworld");
	}

	#[test]
	fn expand_drops_unset_and_keeps_literals() {
		let v = env();
		// an unset name expands to nothing
		assert_eq!(expand_vars(b"[$MISSING]", &v), b"[]");
		assert_eq!(expand_vars(b"[$EMPTY]", &v), b"[]");
		// a lone `$` and a `$` before a non-name stay literal
		assert_eq!(expand_vars(b"5 $ 3", &v), b"5 $ 3");
		assert_eq!(expand_vars(b"$1", &v), b"$1");
		// an unterminated `${` stays literal
		assert_eq!(expand_vars(b"${NAME", &v), b"${NAME");
	}

	#[test]
	fn normalize_rewrites_only_whole_flag_tokens() {
		assert_eq!(normalize_flags(b"lsvol --json"), b"lsvol json");
		assert_eq!(normalize_flags(b"ss --json-min"), b"ss json-min");
		assert_eq!(normalize_flags(b"graph --cbor"), b"graph cbor");
		// a `--json` embedded in a larger token is not a flag
		assert_eq!(normalize_flags(b"echo x--json"), b"echo x--json");
		assert_eq!(normalize_flags(b"echo hi"), b"echo hi");
	}

	#[test]
	fn assignment_accepts_identifiers_and_rejects_the_rest() {
		assert_eq!(parse_assignment(b"FOO=bar"), Some(("FOO", &b"bar"[..])));
		assert_eq!(parse_assignment(b"_x1=v=w"), Some(("_x1", &b"v=w"[..])));
		// an empty value is allowed
		assert_eq!(parse_assignment(b"FOO="), Some(("FOO", &b""[..])));
		// a name that is not an identifier, or an `=` inside an argument, is not an assignment
		assert_eq!(parse_assignment(b"1FOO=bar"), None);
		assert_eq!(parse_assignment(b"cat vol://a=b"), None);
		assert_eq!(parse_assignment(b"=bar"), None);
		assert_eq!(parse_assignment(b"noeq"), None);
	}

	#[test]
	fn parse_and_expand_runs_the_whole_pipeline() {
		let v = env();
		// trim, then expand, then normalize - in that order
		assert_eq!(parse_and_expand(b"  ls $PATH --json  ", &v), b"ls vol://system json");
		// an assignment survives the pipeline for the dispatcher to detect, its value expanded
		assert_eq!(parse_and_expand(b"GREETING=hi $NAME", &v), b"GREETING=hi world");
	}
}

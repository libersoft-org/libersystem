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

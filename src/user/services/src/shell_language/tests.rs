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
	let values = env();
	assert_eq!(expand_vars(b"hi $NAME", &values), b"hi world");
	assert_eq!(expand_vars(b"${NAME}!", &values), b"world!");
	assert_eq!(expand_vars(b"$NAME$NAME", &values), b"worldworld");
}

#[test]
fn expand_drops_unset_and_keeps_literals() {
	let values = env();
	assert_eq!(expand_vars(b"[$MISSING]", &values), b"[]");
	assert_eq!(expand_vars(b"[$EMPTY]", &values), b"[]");
	assert_eq!(expand_vars(b"5 $ 3", &values), b"5 $ 3");
	assert_eq!(expand_vars(b"$1", &values), b"$1");
	assert_eq!(expand_vars(b"${NAME", &values), b"${NAME");
}

#[test]
fn normalize_rewrites_only_whole_flag_tokens() {
	assert_eq!(normalize_flags(b"lsvol --json"), b"lsvol json");
	assert_eq!(normalize_flags(b"ss --json-min"), b"ss json-min");
	assert_eq!(normalize_flags(b"graph --cbor"), b"graph cbor");
	assert_eq!(normalize_flags(b"echo x--json"), b"echo x--json");
	assert_eq!(normalize_flags(b"echo hi"), b"echo hi");
}

#[test]
fn assignment_accepts_identifiers_and_rejects_the_rest() {
	assert_eq!(parse_assignment(b"FOO=bar"), Some(("FOO", &b"bar"[..])));
	assert_eq!(parse_assignment(b"_x1=v=w"), Some(("_x1", &b"v=w"[..])));
	assert_eq!(parse_assignment(b"FOO="), Some(("FOO", &b""[..])));
	assert_eq!(parse_assignment(b"1FOO=bar"), None);
	assert_eq!(parse_assignment(b"cat vol://a=b"), None);
	assert_eq!(parse_assignment(b"=bar"), None);
	assert_eq!(parse_assignment(b"noeq"), None);
}

#[test]
fn parse_and_expand_runs_the_whole_pipeline() {
	let values = env();
	assert_eq!(parse_and_expand(b"  ls $PATH --json  ", &values), b"ls vol://system json");
	assert_eq!(parse_and_expand(b"GREETING=hi $NAME", &values), b"GREETING=hi world");
}

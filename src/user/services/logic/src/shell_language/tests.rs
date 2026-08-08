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

// The property this whole design turns on: an expanded value is DATA and never syntax.
//
// Today's shell expands before it routes, which is safe only because no operator exists. The
// moment `|` means something, expanding first turns a variable's contents into structure - so
// this asserts the opposite ordering directly, with the operator characters coming from
// variables in every position that matters.
#[test]
fn an_expanded_value_never_becomes_syntax() {
	let values = alloc::vec![(String::from("PIPE"), String::from("| rm")), (String::from("REDIR"), String::from("> /etc/passwd")), (String::from("AMP"), String::from("&")),];

	// A variable holding `| rm` is one argument, not a pipe and a command.
	let p = parse_pipeline(b"echo $PIPE", &values).expect("parses");
	assert_eq!(p.stages.len(), 1, "an expanded pipe character does not create a stage");
	assert_eq!(p.stages[0].words, alloc::vec![b"echo".to_vec(), b"| rm".to_vec()]);

	// The same for a redirection: no file is named, because nothing was redirected.
	let p = parse_pipeline(b"echo $REDIR", &values).expect("parses");
	assert!(p.stages[0].redirects.is_empty(), "an expanded redirect character redirects nothing");
	assert_eq!(p.stages[0].words[1], b"> /etc/passwd".to_vec());

	// And for backgrounding, which would otherwise detach a job the user is watching.
	let p = parse_pipeline(b"echo $AMP", &values).expect("parses");
	assert!(!p.background, "an expanded ampersand does not background the job");
}

// Quoting is decided during lexing, so an operator inside quotes is a literal. If quoting were
// applied after operators were found, `echo "a | b"` would already have been split.
#[test]
fn quoting_hides_operators_from_the_lexer() {
	let none: alloc::vec::Vec<(String, String)> = alloc::vec::Vec::new();
	let p = parse_pipeline(b"echo \"a | b\"", &none).expect("parses");
	assert_eq!(p.stages.len(), 1, "a quoted pipe is not a pipe");
	assert_eq!(p.stages[0].words, alloc::vec![b"echo".to_vec(), b"a | b".to_vec()]);

	let p = parse_pipeline(b"echo a\\|b", &none).expect("parses");
	assert_eq!(p.stages.len(), 1, "an escaped pipe is not a pipe");
}

#[test]
fn a_pipeline_splits_into_stages_with_their_own_redirects() {
	let none: alloc::vec::Vec<(String, String)> = alloc::vec::Vec::new();
	let p = parse_pipeline(b"cat < in.txt | grep x | tee out.txt", &none).expect("parses");
	assert_eq!(p.stages.len(), 3);
	assert_eq!(p.stages[0].redirects, alloc::vec![Redirect::In(b"in.txt".to_vec())]);
	assert!(p.stages[1].redirects.is_empty(), "a redirect belongs to its own stage only");
	assert_eq!(p.stages[2].words, alloc::vec![b"tee".to_vec(), b"out.txt".to_vec()]);

	// Redirections keep their written order, because `> a > b` and `2>&1 > f` differ by it.
	let p = parse_pipeline(b"cmd 2>&1 > f", &none).expect("parses");
	assert_eq!(p.stages[0].redirects, alloc::vec![Redirect::ErrToOut, Redirect::Out { path: b"f".to_vec(), append: false, stderr: false }]);

	let p = parse_pipeline(b"cmd 2>> log &", &none).expect("parses");
	assert_eq!(p.stages[0].redirects, alloc::vec![Redirect::Out { path: b"log".to_vec(), append: true, stderr: true }]);
	assert!(p.background);
}

// Every refusal is checked before a caller could open a file or launch a stage, which is why
// parsing is separate from running. A truncated pipeline is a different command from the one
// that was typed, so bounds refuse rather than trim.
#[test]
fn malformed_lines_are_refused_not_repaired() {
	let none: alloc::vec::Vec<(String, String)> = alloc::vec::Vec::new();
	assert_eq!(parse_pipeline(b"", &none), Err(ParseError::Empty));
	assert_eq!(parse_pipeline(b"| b", &none), Err(ParseError::EmptyStage));
	assert_eq!(parse_pipeline(b"a |", &none), Err(ParseError::EmptyStage));
	assert_eq!(parse_pipeline(b"a | | b", &none), Err(ParseError::EmptyStage));
	assert_eq!(parse_pipeline(b"a >", &none), Err(ParseError::DanglingOperator));
	assert_eq!(parse_pipeline(b"a < ", &none), Err(ParseError::DanglingOperator));
	assert_eq!(parse_pipeline(b"echo \"unclosed", &none), Err(ParseError::UnterminatedQuote));
	assert_eq!(parse_pipeline(b"a & b", &none), Err(ParseError::DanglingOperator));
	assert_eq!(parse_pipeline(b"cmd 3>&1", &none), Err(ParseError::UnsupportedDescriptor));

	let long = alloc::vec![b'a'; MAX_LINE_BYTES + 1];
	assert_eq!(parse_pipeline(&long, &none), Err(ParseError::TooLarge));
	let many = alloc::vec![b"a |".to_vec(); MAX_STAGES + 1].concat();
	assert!(matches!(parse_pipeline(&many, &none), Err(ParseError::TooLarge) | Err(ParseError::EmptyStage)));
}

// A digit is only a descriptor when it introduces a redirection, or ordinary words starting
// with a digit would stop being words.
#[test]
fn a_leading_digit_is_only_a_descriptor_before_a_redirect() {
	let none: alloc::vec::Vec<(String, String)> = alloc::vec::Vec::new();
	let p = parse_pipeline(b"echo 2x file2", &none).expect("parses");
	assert_eq!(p.stages[0].words, alloc::vec![b"echo".to_vec(), b"2x".to_vec(), b"file2".to_vec()]);
	assert!(p.stages[0].redirects.is_empty());
}

#[test]
fn a_state_mutating_builtin_is_refused_anywhere_its_effect_would_be_discarded() {
	let none: alloc::vec::Vec<(String, String)> = alloc::vec::Vec::new();

	// The one place it works: a lone foreground command, run by the shell itself.
	let p = parse_pipeline(b"cd /tmp", &none).expect("a lone cd is the parent shell's own");
	assert_eq!(p.stages.len(), 1);
	assert_eq!(p.stages[0].words[0], b"cd".to_vec());

	// As a pipeline stage the builtin would run in a child, so the directory would silently
	// not change. Refusing beats reporting a success that did not happen.
	assert_eq!(parse_pipeline(b"cd /tmp | grep x", &none), Err(ParseError::BuiltinNotAStage));
	assert_eq!(parse_pipeline(b"ls | cd /tmp", &none), Err(ParseError::BuiltinNotAStage));
	// Backgrounded, its state dies with the job.
	assert_eq!(parse_pipeline(b"cd /tmp &", &none), Err(ParseError::BuiltinNotAStage));
	// Redirected, it is no longer the shell's own command either.
	assert_eq!(parse_pipeline(b"cd /tmp > log", &none), Err(ParseError::BuiltinNotAStage));

	// Assignments are recognised by shape, not by name: they have no command word to match.
	assert_eq!(parse_pipeline(b"NAME=value | cat", &none), Err(ParseError::BuiltinNotAStage));
	assert!(parse_pipeline(b"NAME=value", &none).is_ok(), "a lone assignment is the shell's own");

	// A path or flag that merely CONTAINS `=` is not an assignment: refusing it would break
	// ordinary commands.
	assert!(parse_pipeline(b"cmd --opt=v | cat", &none).is_ok(), "a flag with `=` is not an assignment");
	assert!(parse_pipeline(b"=leading | cat", &none).is_ok(), "an empty name is not an assignment");

	// And a command whose name merely starts like one is untouched.
	assert!(parse_pipeline(b"cdrom x | cat", &none).is_ok(), "`cdrom` is not `cd`");
}

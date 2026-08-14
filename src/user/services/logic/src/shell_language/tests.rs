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

// The words of an expansion, for the assertions that are about SHAPE rather than about `2>&1`.
// The flag has its own test; folding it into every comparison would make five assertions restate
// "and nothing merged its errors" and bury the one place where that is the point.
fn words_of(expansion: &Expansion) -> Vec<Vec<Vec<u8>>> {
	expansion.stages.iter().map(|stage| stage.words.clone()).collect()
}

#[test]
fn a_redirection_expands_into_pipeline_stages() {
	// THE WHOLE DESIGN, in one assertion. A redirection is not a property of a stage that something
	// downstream has to interpret - it is two more stages, governed like any other, and the command
	// in the middle receives one stream endpoint and no file capability at all.
	let vars: Vec<(String, String)> = Vec::new();
	let pipeline = parse_pipeline(b"cat < in.txt\n", &vars).expect("parses");
	assert_eq!(words_of(&expand_redirects(&pipeline).expect("expands")), alloc::vec![alloc::vec![b"redirect_in".to_vec(), b"in.txt".to_vec()], alloc::vec![b"cat".to_vec()]]);

	let pipeline = parse_pipeline(b"cat > out.txt\n", &vars).expect("parses");
	assert_eq!(words_of(&expand_redirects(&pipeline).expect("expands")), alloc::vec![alloc::vec![b"cat".to_vec()], alloc::vec![b"redirect_out".to_vec(), b"out.txt".to_vec()]]);

	// Append carries the flag rather than a second program: one destination, two modes.
	let pipeline = parse_pipeline(b"cat >> out.txt\n", &vars).expect("parses");
	assert_eq!(words_of(&expand_redirects(&pipeline).expect("expands")), alloc::vec![alloc::vec![b"cat".to_vec()], alloc::vec![b"redirect_out".to_vec(), b"--append".to_vec(), b"out.txt".to_vec()]]);

	// Both ends of a real pipeline, which is the shape the milestone's own example uses.
	let pipeline = parse_pipeline(b"cat < a.txt | grep hello > b.txt\n", &vars).expect("parses");
	assert_eq!(
		words_of(&expand_redirects(&pipeline).expect("expands")),
		alloc::vec![
			alloc::vec![b"redirect_in".to_vec(), b"a.txt".to_vec()],
			alloc::vec![b"cat".to_vec()],
			alloc::vec![b"grep".to_vec(), b"hello".to_vec()],
			alloc::vec![b"redirect_out".to_vec(), b"b.txt".to_vec()],
		]
	);

	// A line with no redirection is its own stages, unchanged - so the expansion is on one path
	// rather than a branch the ordinary case has to avoid.
	let pipeline = parse_pipeline(b"cat a | grep b\n", &vars).expect("parses");
	assert_eq!(words_of(&expand_redirects(&pipeline).expect("expands")), alloc::vec![alloc::vec![b"cat".to_vec(), b"a".to_vec()], alloc::vec![b"grep".to_vec(), b"b".to_vec()]]);
}

#[test]
fn a_redirection_that_cannot_mean_what_it_looks_like_is_refused() {
	// EACH REFUSAL IS A LINE THAT HAS NO HONEST READING, and each one is named rather than folded
	// into "unsupported": a user who typed `a > f | b` wants to know that the file took the output
	// and `b` got nothing, not that redirection is unavailable.
	let vars: Vec<(String, String)> = Vec::new();

	// Input on anything but the first stage: two producers for one consumer.
	let pipeline = parse_pipeline(b"a | b < f\n", &vars).expect("parses");
	assert_eq!(expand_redirects(&pipeline), Err(RedirectError::InputNotFirst));

	// Output on anything but the last: the rest of the chain would be fed nothing.
	let pipeline = parse_pipeline(b"a > f | b\n", &vars).expect("parses");
	assert_eq!(expand_redirects(&pipeline), Err(RedirectError::OutputNotLast));

	// Two destinations for one stream. A shell that picked one would be choosing for the user.
	let pipeline = parse_pipeline(b"a > x > y\n", &vars).expect("parses");
	assert_eq!(expand_redirects(&pipeline), Err(RedirectError::Duplicate));
	let pipeline = parse_pipeline(b"a < x < y\n", &vars).expect("parses");
	assert_eq!(expand_redirects(&pipeline), Err(RedirectError::Duplicate));

	// `2> path` needs the stage's errors to reach a consumer that is NOT in the chain, beside the
	// linear one - a second edge, where the transaction allocates one per `A | B` and no others.
	// Refused by name rather than run with the part the user asked for silently dropped. Note this
	// is NOT the same refusal as `2>&1`, which the next test shows working: one asks for an endpoint
	// nobody has, the other asks for one the broker already holds.
	let pipeline = parse_pipeline(b"a 2> f\n", &vars).expect("parses");
	assert_eq!(expand_redirects(&pipeline), Err(RedirectError::StderrToPathUnsupported));
}

#[test]
fn merging_the_error_stream_is_a_property_of_a_stage_and_not_a_stage_of_its_own() {
	// `2>&1` DOES NOT EXPAND. Every other redirection becomes a program because it names a FILE,
	// and a file needs authority somebody has to be granted; this one names the stage's own output,
	// which is a handle the broker allocates inside the launch transaction. So it travels as a flag
	// and adds nothing to the chain - and the shell could not expand it if it wanted to, because
	// the shell never sees the edge.
	let vars: Vec<(String, String)> = Vec::new();

	let pipeline = parse_pipeline(b"a 2>&1 | b\n", &vars).expect("parses");
	let expansion = expand_redirects(&pipeline).expect("expands");
	assert_eq!(words_of(&expansion), alloc::vec![alloc::vec![b"a".to_vec()], alloc::vec![b"b".to_vec()]], "no stage is added");
	assert_eq!(expansion.stages[0].merge_errors, true, "the stage that asked for it carries it");
	assert_eq!(expansion.stages[1].merge_errors, false, "and no other stage does");

	// THE FLAG LANDS ON THE STAGE THE USER WROTE IT ON, even when an input redirection has pushed
	// every typed stage one place along. `redirect_in` is stage 0 of the expanded line and stage 0
	// of what the user typed is `a`, and an off-by-one here would send `a`'s diagnostics nowhere
	// and merge the ones from the redirection instead.
	let pipeline = parse_pipeline(b"a < f 2>&1 | b\n", &vars).expect("parses");
	let expansion = expand_redirects(&pipeline).expect("expands");
	assert_eq!(words_of(&expansion)[0], alloc::vec![b"redirect_in".to_vec(), b"f".to_vec()]);
	assert_eq!(expansion.stages.iter().map(|s| s.merge_errors).collect::<Vec<bool>>(), alloc::vec![false, true, false]);

	// `cmd > file 2>&1`: the last stage's output IS the edge into `redirect_out`, so merging its
	// errors into that edge puts both streams in the file. Nothing here special-cases the shape -
	// it falls out of the flag meaning "wherever my output goes".
	let pipeline = parse_pipeline(b"a 2>&1 > f\n", &vars).expect("parses");
	let expansion = expand_redirects(&pipeline).expect("expands");
	assert_eq!(words_of(&expansion), alloc::vec![alloc::vec![b"a".to_vec()], alloc::vec![b"redirect_out".to_vec(), b"f".to_vec()]]);
	assert_eq!(expansion.stages[0].merge_errors, true);
	assert_eq!(expansion.stages[1].merge_errors, false, "the writer's own diagnostics still reach the terminal - they are about the file");
}

// A DETERMINISTIC BYTE SOURCE, because a fuzz that is not reproducible reports a failure nobody
// can look at twice. Xorshift64*, seeded by the caller, so a failing case is `seed` plus an index.
struct Bytes(u64);

impl Bytes {
	fn next(&mut self) -> u8 {
		let mut x = self.0;
		x ^= x >> 12;
		x ^= x << 25;
		x ^= x >> 27;
		self.0 = x;
		(x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u8
	}
}

#[test]
fn the_parser_answers_every_line_a_person_could_type_including_the_ones_they_could_not_mean() {
	// THE PROPERTY IS "ANSWERS", NOT "ACCEPTS". Every one of these lines is nonsense; what this
	// asserts is that the parser returns an answer for each rather than panicking, looping, or
	// growing - and that whatever it accepts satisfies the bounds it advertises. A shell that can
	// be made to fault by a paste from a chat window is a shell that stops being a shell.
	//
	// The alphabet is the metacharacters plus a few ordinary bytes: random bytes over the whole
	// range would spend almost every draw on characters the lexer treats identically, so the
	// interesting collisions - a quote inside a redirection inside a pipe - would essentially
	// never come up.
	const ALPHABET: &[u8] = b"|<>&2;'\"\\ \t\nabc=$\0";
	let vars: Vec<(String, String)> = alloc::vec![(String::from("HOME"), String::from("vol://system"))];
	let mut source = Bytes(0x0102_0304_0506_0708);
	for case in 0..4000 {
		let length: usize = (source.next() as usize % 24) + 1;
		let mut line: Vec<u8> = Vec::with_capacity(length);
		for _ in 0..length {
			line.push(ALPHABET[source.next() as usize % ALPHABET.len()]);
		}
		let Ok(pipeline) = parse_pipeline(&line, &vars) else { continue };
		// WHAT AN ACCEPTED LINE MUST SATISFY. These are the bounds the launcher trusts: it sizes a
		// transaction from `stages.len()` and reads `words[0]` as a program name, so a pipeline
		// that got past the parser with no words in a stage is a launcher indexing an empty vector.
		assert!(!pipeline.stages.is_empty(), "case {case}: an accepted line has at least one stage: {:?}", line);
		assert!(pipeline.stages.len() <= MAX_STAGES, "case {case}: within the stage bound: {:?}", line);
		for stage in &pipeline.stages {
			assert!(!stage.words.is_empty(), "case {case}: every accepted stage has a command word: {:?}", line);
			assert!(stage.words.len() <= MAX_WORDS_PER_STAGE, "case {case}: within the word bound: {:?}", line);
			for redirect in &stage.redirects {
				// A redirection with an empty target is a dangling operator the parser is supposed
				// to have refused. Accepting one hands `redirect_out` no destination, and the check
				// for that would then live in the tool rather than in the grammar.
				match redirect {
					Redirect::In(path) => assert!(!path.is_empty(), "case {case}: `<` has a target: {:?}", line),
					Redirect::Out { path, .. } => assert!(!path.is_empty(), "case {case}: `>` has a target: {:?}", line),
					Redirect::ErrToOut => {}
				}
			}
		}
		// And the expansion answers too. It is the half that runs after the parser has approved a
		// line, so it is reached by exactly these inputs and by nothing else.
		let Ok(expansion) = expand_redirects(&pipeline) else { continue };
		assert!(!expansion.stages.is_empty(), "case {case}: an expansion has stages: {:?}", line);
		for stage in &expansion.stages {
			assert!(!stage.words.is_empty(), "case {case}: every expanded stage has a command word: {:?}", line);
		}
	}
}

#[test]
fn a_line_that_asks_for_something_the_grammar_does_not_mean_is_refused_by_name() {
	// THE ADVERSARIAL CASES, each one a shape somebody could reasonably type and each refused for
	// a stated reason rather than reinterpreted. A shell that guesses is a shell that runs a
	// different command from the one on the screen.
	let vars: Vec<(String, String)> = Vec::new();

	// A descriptor this grammar does not implement. `3>&1` must NOT be read as `>` with a stray
	// `3`, which is what a lexer that skipped unknown digits would do - and that reading silently
	// redirects stdout on a line that asked about a descriptor the shell has never heard of.
	assert_eq!(parse_pipeline(b"a 3>&1\n", &vars), Err(ParseError::UnsupportedDescriptor));

	// A state-mutating builtin anywhere its state would be discarded. `cd x | grep y` runs `cd` in
	// a child, so the directory does not change and nothing says so.
	assert_eq!(parse_pipeline(b"cd x | grep y\n", &vars), Err(ParseError::BuiltinNotAStage));

	// Dangling operators, in both directions.
	assert_eq!(parse_pipeline(b"a >\n", &vars), Err(ParseError::DanglingOperator));
	assert_eq!(parse_pipeline(b"a <\n", &vars), Err(ParseError::DanglingOperator));
	assert_eq!(parse_pipeline(b"a |\n", &vars), Err(ParseError::EmptyStage));
	assert_eq!(parse_pipeline(b"| b\n", &vars), Err(ParseError::EmptyStage));
	assert_eq!(parse_pipeline(b"a | | b\n", &vars), Err(ParseError::EmptyStage));

	// A quote that never closes. Accepting it would swallow the rest of the line into one word,
	// which is a different command that happens to parse.
	assert_eq!(parse_pipeline(b"a 'unterminated\n", &vars), Err(ParseError::UnterminatedQuote));

	// PAST THE BOUNDS, which is where a shell either refuses or starts sizing allocations from
	// input. A line longer than the maximum, and more stages than the transaction can carry.
	let long: Vec<u8> = alloc::vec![b'a'; MAX_LINE_BYTES + 1];
	assert_eq!(parse_pipeline(&long, &vars), Err(ParseError::TooLarge));
	let mut many: Vec<u8> = Vec::new();
	for index in 0..MAX_STAGES + 4 {
		if index > 0 {
			many.extend_from_slice(b" | ");
		}
		many.push(b'a');
	}
	assert_eq!(parse_pipeline(&many, &vars), Err(ParseError::TooLarge));

	// A REDIRECTION WHOSE TARGET EXPANDED TO NOTHING. Found by the fuzz above as `a < $b` with `b`
	// unset: the operator is not dangling - there IS a word after it - and the word is empty, so
	// the redirection has no destination. Distinct from `DanglingOperator` because the two need
	// different sentences to fix.
	assert_eq!(parse_pipeline(b"a < $b\n", &vars), Err(ParseError::EmptyRedirectTarget));
	assert_eq!(parse_pipeline(b"a > $b\n", &vars), Err(ParseError::EmptyRedirectTarget));

	// AND THE ONE THAT IS NOT A REFUSAL. A NUL byte is an ordinary byte in a word: paths on this
	// system are byte strings and the volume decides what it will accept, so the grammar has no
	// business rejecting one - it would be refusing on behalf of a layer that can answer for
	// itself. Asserted so that a later "sanitize the input" change has to argue with this line.
	let pipeline = parse_pipeline(b"cat a\0b\n", &vars).expect("a NUL is a byte in a word");
	assert_eq!(pipeline.stages[0].words[1], b"a\0b".to_vec());
}

#[test]
fn expanding_a_redirection_never_hands_the_command_a_path() {
	// THE CAPABILITY CLAIM, at the only layer that can be checked without a machine: the command
	// the user typed keeps exactly the words the user typed. A path that leaked into its argument
	// list would be a path it might open - and the point of expanding `<` into a stage is that the
	// command receives a STREAM and no authority over the file behind it.
	let vars: Vec<(String, String)> = Vec::new();
	for line in [&b"cat < in.txt\n"[..], &b"cat > out.txt\n"[..], &b"cat >> out.txt\n"[..], &b"cat < in.txt | grep x > out.txt\n"[..]] {
		let pipeline = parse_pipeline(line, &vars).expect("parses");
		let expansion = expand_redirects(&pipeline).expect("expands");
		for stage in &expansion.stages {
			let name: &[u8] = &stage.words[0];
			if name == REDIRECT_IN || name == REDIRECT_OUT {
				continue;
			}
			for word in &stage.words {
				assert!(word != b"in.txt", "the command never sees the source path: {:?}", line);
				assert!(word != b"out.txt", "the command never sees the destination path: {:?}", line);
			}
		}
	}
}

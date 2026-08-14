//! Pure parsing and expansion for the shell line language.

use alloc::string::String;
use alloc::vec::Vec;

pub fn trim(mut value: &[u8]) -> &[u8] {
	while let [first, rest @ ..] = value {
		if first.is_ascii_whitespace() {
			value = rest;
		} else {
			break;
		}
	}
	while let [rest @ .., last] = value {
		if last.is_ascii_whitespace() {
			value = rest;
		} else {
			break;
		}
	}
	value
}

pub fn normalize_flags(line: &[u8]) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::with_capacity(line.len());
	for (index, token) in line.split(|&byte: &u8| byte == b' ').enumerate() {
		if index > 0 {
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

pub fn expand_vars(line: &[u8], vars: &[(String, String)]) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::with_capacity(line.len());
	let mut index: usize = 0;
	while index < line.len() {
		if line[index] != b'$' {
			out.push(line[index]);
			index += 1;
			continue;
		}
		if index + 1 < line.len() && line[index + 1] == b'{' {
			let start: usize = index + 2;
			match line[start..].iter().position(|&byte: &u8| byte == b'}') {
				Some(relative) => {
					push_var_value(&mut out, &line[start..start + relative], vars);
					index = start + relative + 1;
				}
				None => {
					out.push(b'$');
					index += 1;
				}
			}
			continue;
		}
		let start: usize = index + 1;
		if start < line.len() && (line[start].is_ascii_alphabetic() || line[start] == b'_') {
			let mut end: usize = start + 1;
			while end < line.len() && (line[end].is_ascii_alphanumeric() || line[end] == b'_') {
				end += 1;
			}
			push_var_value(&mut out, &line[start..end], vars);
			index = end;
		} else {
			out.push(b'$');
			index += 1;
		}
	}
	out
}

fn push_var_value(out: &mut Vec<u8>, name: &[u8], vars: &[(String, String)]) {
	if let Some((_, value)) = vars.iter().find(|(candidate, _): &&(String, String)| candidate.as_bytes() == name) {
		out.extend_from_slice(value.as_bytes());
	}
}

pub fn parse_assignment(line: &[u8]) -> Option<(&str, &[u8])> {
	let equals: usize = line.iter().position(|&byte: &u8| byte == b'=')?;
	let name: &[u8] = &line[..equals];
	if name.is_empty() {
		return None;
	}
	let head: u8 = name[0];
	if !(head.is_ascii_alphabetic() || head == b'_') {
		return None;
	}
	if !name.iter().all(|&byte: &u8| byte.is_ascii_alphanumeric() || byte == b'_') {
		return None;
	}
	Some((core::str::from_utf8(name).ok()?, &line[equals + 1..]))
}

pub fn parse_and_expand(raw: &[u8], vars: &[(String, String)]) -> Vec<u8> {
	let expanded: Vec<u8> = expand_vars(trim(raw), vars);
	normalize_flags(&expanded)
}

// The bounded shell grammar: a line becomes a pipeline of stages, each a command plus its
// redirections. Replaces matching whole lines against a table, which cannot express `a | b`
// and cannot say where a redirection applies.
//
// THE ORDER OF OPERATIONS IS THE SECURITY PROPERTY, and it is the reason this is a lexer
// rather than a few `split` calls. Operators are recognised on the RAW line, before any
// variable is expanded, and an expanded value is never lexed again. Today's shell expands
// first and then routes, which is harmless only because no operator exists yet: the moment
// `|` means something, `FOO='| rm x'; echo $FOO` would build a pipeline out of DATA. Every
// shell that got this wrong has the same bug, and the fix is not to escape the data but to
// never look at it as syntax.
//
// Quoting is therefore part of lexing, not of expansion: an operator inside quotes is a
// literal, so `echo "a | b"` is one argument and not a pipeline.

// Bounds, checked before anything is opened or launched. A line that would exceed one is
// refused whole rather than truncated, because a pipeline missing its last stage is a
// different command from the one that was typed.
pub const MAX_STAGES: usize = 8;
pub const MAX_WORDS_PER_STAGE: usize = 64;
pub const MAX_LINE_BYTES: usize = 4096;

// Where a stage's stream is attached. Absent means the stage keeps the terminal.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Redirect {
	// `< path`
	In(Vec<u8>),
	// `> path` and `2> path`
	Out { path: Vec<u8>, append: bool, stderr: bool },
	// `2>&1` - stderr joins whatever stdout is at this point in left-to-right order.
	ErrToOut,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Stage {
	// The command and its arguments, already variable-expanded. Never empty.
	pub words: Vec<Vec<u8>>,
	// In the order they were written, because `> a > b` and `2>&1 > f` differ by order.
	pub redirects: Vec<Redirect>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Pipeline {
	pub stages: Vec<Stage>,
	// A trailing `&`.
	pub background: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
	Empty,
	// A stage with no command word: `| b`, `a |`, `a | | b`.
	EmptyStage,
	// An operator with nothing after it: `a >`, `a | `.
	DanglingOperator,
	// A quote that never closed.
	UnterminatedQuote,
	// Past one of the bounds above.
	TooLarge,
	// A descriptor this grammar does not implement, so `3>&1` is refused rather than
	// silently treated as something else.
	UnsupportedDescriptor,
	// A redirection whose target expanded to nothing: `> $UNSET`. Distinct from a dangling
	// operator, which has no word after it at all - here there is a word and it is empty, and the
	// difference is what the user has to be told to fix it.
	EmptyRedirectTarget,
	// A builtin that mutates the shell's own state, used anywhere but as a lone foreground
	// command. `cd x | grep y` would run `cd` in a CHILD whose state dies with it, so the
	// directory silently does not change - refusing is the only honest answer.
	BuiltinNotAStage,
}

// Builtins that mutate the parent shell's persistent state. They are meaningful only in the
// shell's own process: a pipeline stage, a redirected command and a background job all run
// somewhere whose state is discarded, so the user's intent cannot be honoured there.
// `fg`/`bg` are here for the same reason - they act on the shell's job table.
const STATE_MUTATING_BUILTINS: &[&[u8]] = &[b"cd", b"unset", b"export", b"fg", b"bg"];

// Whether `word` names one. Assignments (`NAME=value`) are caught separately: they carry no
// command word at all, so they are recognised by shape rather than by name.
fn mutates_shell_state(word: &[u8]) -> bool {
	STATE_MUTATING_BUILTINS.contains(&word) || word.iter().position(|b| *b == b'=').is_some_and(|at| at > 0 && word[..at].iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_'))
}

// One lexed token from the raw line. Operators are distinguishable from words BY CONSTRUCTION:
// a word carries no information about whether its bytes looked like an operator, so no later
// stage can promote data to syntax.
#[derive(Debug, PartialEq, Eq)]
enum Token {
	Word(Vec<u8>),
	Pipe,
	Less,
	Great,
	GreatGreat,
	ErrGreat,
	ErrGreatGreat,
	ErrToOut,
	Amp,
}

// Lex the raw line. `vars` is applied to word tokens as they are completed, never to operator
// text, and the result is not re-examined for operators.
fn lex(raw: &[u8], vars: &[(String, String)]) -> Result<Vec<Token>, ParseError> {
	if raw.len() > MAX_LINE_BYTES {
		return Err(ParseError::TooLarge);
	}
	let mut tokens: Vec<Token> = Vec::new();
	let mut word: Vec<u8> = Vec::new();
	let mut has_word = false;
	let mut i = 0usize;

	// Finish the word in progress, expanding it now - after its extent was decided by the
	// quoting rules above, so an expansion cannot change where the word ends.
	fn flush(word: &mut Vec<u8>, has_word: &mut bool, tokens: &mut Vec<Token>, vars: &[(String, String)]) {
		if *has_word {
			tokens.push(Token::Word(expand_vars(word, vars)));
			word.clear();
			*has_word = false;
		}
	}

	while i < raw.len() {
		let byte = raw[i];
		match byte {
			b' ' | b'\t' => {
				flush(&mut word, &mut has_word, &mut tokens, vars);
				i += 1;
			}
			b'\'' | b'"' => {
				// Everything to the matching quote is literal, including operators. This is
				// what makes `echo "a | b"` one argument.
				let quote = byte;
				let mut j = i + 1;
				while j < raw.len() && raw[j] != quote {
					word.push(raw[j]);
					j += 1;
				}
				if j >= raw.len() {
					return Err(ParseError::UnterminatedQuote);
				}
				has_word = true;
				i = j + 1;
			}
			b'\\' if i + 1 < raw.len() => {
				word.push(raw[i + 1]);
				has_word = true;
				i += 2;
			}
			b'|' => {
				flush(&mut word, &mut has_word, &mut tokens, vars);
				tokens.push(Token::Pipe);
				i += 1;
			}
			b'<' => {
				flush(&mut word, &mut has_word, &mut tokens, vars);
				tokens.push(Token::Less);
				i += 1;
			}
			b'>' => {
				flush(&mut word, &mut has_word, &mut tokens, vars);
				if raw.get(i + 1) == Some(&b'>') {
					tokens.push(Token::GreatGreat);
					i += 2;
				} else {
					tokens.push(Token::Great);
					i += 1;
				}
			}
			b'&' => {
				flush(&mut word, &mut has_word, &mut tokens, vars);
				tokens.push(Token::Amp);
				i += 1;
			}
			// A digit only starts a descriptor form when it is followed by `>` AND is not part
			// of a longer word, so `2>x` redirects while `file2` and `2x` stay words.
			b'0'..=b'9' if !has_word && raw.get(i + 1) == Some(&b'>') => {
				let descriptor = byte;
				let (token, width) = match (descriptor, raw.get(i + 2), raw.get(i + 3)) {
					(b'2', Some(&b'&'), Some(&b'1')) => (Token::ErrToOut, 4),
					(b'2', Some(&b'>'), _) => (Token::ErrGreatGreat, 3),
					(b'2', _, _) => (Token::ErrGreat, 2),
					// `1>` is stdout, which `>` already spells; anything else is refused
					// rather than guessed at.
					(b'1', Some(&b'>'), _) => (Token::GreatGreat, 3),
					(b'1', _, _) => (Token::Great, 2),
					_ => return Err(ParseError::UnsupportedDescriptor),
				};
				tokens.push(token);
				i += width;
			}
			_ => {
				word.push(byte);
				has_word = true;
				i += 1;
			}
		}
	}
	flush(&mut word, &mut has_word, &mut tokens, vars);
	Ok(tokens)
}

// Parse a raw line into a pipeline. Every bound is checked here, before a caller opens a file
// or launches anything - which is the point of parsing separately from running.
pub fn parse_pipeline(raw: &[u8], vars: &[(String, String)]) -> Result<Pipeline, ParseError> {
	let tokens = lex(trim(raw), vars)?;
	if tokens.is_empty() {
		return Err(ParseError::Empty);
	}
	let mut stages: Vec<Stage> = Vec::new();
	let mut current = Stage { words: Vec::new(), redirects: Vec::new() };
	let mut background = false;
	let mut iter = tokens.into_iter().peekable();

	// Take the path a redirection operator applies to. A redirection with no target is a
	// dangling operator, not an empty path.
	//
	// AND A TARGET THAT EXPANDED TO NOTHING IS NEITHER. `< $HOME/notes` where `HOME` is unset
	// leaves a word that is present in the token stream and empty in content, so the operator is
	// not dangling - it simply has no destination. Found by the parser fuzz, which produced
	// `a < $b` with `b` undefined and got a redirection whose path was zero bytes.
	//
	// Refusing here rather than letting it through matters because of WHERE it would have surfaced:
	// the expansion turns it into `redirect_in` with no argument, that tool refuses, and the user
	// is told a program they never typed could not find a path they never wrote. The line is
	// ambiguous, the shell is the layer that knows it, and this is where it says so.
	fn path(iter: &mut core::iter::Peekable<alloc::vec::IntoIter<Token>>) -> Result<Vec<u8>, ParseError> {
		match iter.next() {
			Some(Token::Word(w)) if w.is_empty() => Err(ParseError::EmptyRedirectTarget),
			Some(Token::Word(w)) => Ok(w),
			_ => Err(ParseError::DanglingOperator),
		}
	}

	while let Some(token) = iter.next() {
		match token {
			Token::Word(w) => {
				if current.words.len() >= MAX_WORDS_PER_STAGE {
					return Err(ParseError::TooLarge);
				}
				current.words.push(w);
			}
			Token::Pipe => {
				if current.words.is_empty() {
					return Err(ParseError::EmptyStage);
				}
				if stages.len() + 1 >= MAX_STAGES {
					return Err(ParseError::TooLarge);
				}
				stages.push(core::mem::replace(&mut current, Stage { words: Vec::new(), redirects: Vec::new() }));
			}
			Token::Less => {
				let p = path(&mut iter)?;
				current.redirects.push(Redirect::In(p));
			}
			Token::Great => {
				let p = path(&mut iter)?;
				current.redirects.push(Redirect::Out { path: p, append: false, stderr: false });
			}
			Token::GreatGreat => {
				let p = path(&mut iter)?;
				current.redirects.push(Redirect::Out { path: p, append: true, stderr: false });
			}
			Token::ErrGreat => {
				let p = path(&mut iter)?;
				current.redirects.push(Redirect::Out { path: p, append: false, stderr: true });
			}
			Token::ErrGreatGreat => {
				let p = path(&mut iter)?;
				current.redirects.push(Redirect::Out { path: p, append: true, stderr: true });
			}
			Token::ErrToOut => current.redirects.push(Redirect::ErrToOut),
			// `&` is only the background marker, and only at the very end.
			Token::Amp => {
				if iter.peek().is_some() {
					return Err(ParseError::DanglingOperator);
				}
				background = true;
			}
		}
	}
	if current.words.is_empty() {
		return Err(if stages.is_empty() { ParseError::Empty } else { ParseError::EmptyStage });
	}
	stages.push(current);
	// A state-mutating builtin survives only as a lone foreground command with no redirection,
	// which is the one place its effect outlives the line. Everywhere else it would run in a
	// process whose state is thrown away, and reporting success there would be a lie.
	let lone_foreground = stages.len() == 1 && !background && stages[0].redirects.is_empty();
	if !lone_foreground && stages.iter().any(|stage| mutates_shell_state(&stage.words[0])) {
		return Err(ParseError::BuiltinNotAStage);
	}
	Ok(Pipeline { stages, background })
}

// The names of the two programs a redirection becomes.
//
// A REDIRECTION IS A PIPELINE STAGE. `cmd < a > b` expands to `redirect_in a | cmd | redirect_out b`
// before anything is launched, which is what makes it capability-native rather than a special case:
// the two halves are governed programs granted `volumes` and nothing else, the command in the middle
// receives one stream endpoint and no file capability at all, and every lifecycle rule the pipeline
// already has - EOF on a producer's close, broken pipe on a consumer's early exit, one ProcessGroup
// for the whole line - applies to them unchanged.
//
// The alternative is a pump inside the shell or inside the broker. Both have to hold the source or
// destination capability for as long as the child runs, which is the thing the milestone's rule
// about the child receiving only the stream endpoint exists to prevent; and a shell-side pump cannot
// serve a BACKGROUND pipeline at all, because the shell has gone back to its prompt.
pub const REDIRECT_IN: &[u8] = b"redirect_in";
pub const REDIRECT_OUT: &[u8] = b"redirect_out";
pub const APPEND_FLAG: &[u8] = b"--append";

// Why a line's redirections cannot be expanded.
#[derive(Debug, PartialEq, Eq)]
pub enum RedirectError {
	// `< path` anywhere but on the first stage. `a | b < f` asks for b's input to come from a file
	// AND from a, which is two producers for one consumer - not a thing to pick a winner for.
	InputNotFirst,
	// `> path` anywhere but on the last stage. `a > f | b` sends a's output to the file and leaves
	// b with nothing, which is a line that cannot mean what it looks like.
	OutputNotLast,
	// More than one input or more than one output on the line. `a > x > y` is two destinations for
	// one stream; a shell that picked one would be choosing for the user.
	Duplicate,
	// `2> path`: the stage's errors would have to reach a consumer that is NOT in the chain, and
	// the transaction allocates one edge per `A | B` and no others. `2>&1` is not here - it is a
	// flag on the stage, because it asks for an endpoint the broker already has.
	StderrToPathUnsupported,
}

/// One stage of an expanded line: the words to launch, and whether it asked for `2>&1`.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ExpandedStage {
	pub words: Vec<Vec<u8>>,
	pub merge_errors: bool,
}

/// A line with its redirections expanded into stages.
#[derive(Debug, PartialEq, Eq)]
pub struct Expansion {
	pub stages: Vec<ExpandedStage>,
}

// The stages a line becomes once its redirections are expanded, as `(command, arguments)` pairs in
// launch order.
//
// Returned as words rather than as `Stage`s because the expansion's product is exactly what the
// launch contract takes, and a `Stage` carrying a `redirects` list that is now always empty would
// invite somebody to look at it.
pub fn expand_redirects(pipeline: &Pipeline) -> Result<Expansion, RedirectError> {
	let last: usize = pipeline.stages.len() - 1;
	let mut input: Option<&[u8]> = None;
	let mut output: Option<(&[u8], bool)> = None;
	let mut merged: Vec<usize> = Vec::new();
	for (index, stage) in pipeline.stages.iter().enumerate() {
		for redirect in &stage.redirects {
			match redirect {
				Redirect::In(path) => {
					if index != 0 {
						return Err(RedirectError::InputNotFirst);
					}
					if input.is_some() {
						return Err(RedirectError::Duplicate);
					}
					input = Some(path);
				}
				// `2>&1` is a property of the stage rather than a stage of its own: it asks for the
				// diagnostics to go wherever the OUTPUT goes, and where that is - an edge, or the
				// terminal for the last stage - is something only the broker knows. So it travels
				// as a flag on the stage and the broker duplicates the right handle.
				Redirect::ErrToOut => merged.push(index),
				// `2> path` is not the same shape at all. It needs the stage's errors to reach a
				// `redirect_out` that is NOT in the chain - a second consumer beside the linear one -
				// and the pipeline transaction allocates one edge per `A | B` and no others. Refused
				// by name until the transaction can carry a side edge.
				Redirect::Out { stderr: true, .. } => {
					return Err(RedirectError::StderrToPathUnsupported);
				}
				Redirect::Out { path, append, stderr: false } => {
					if index != last {
						return Err(RedirectError::OutputNotLast);
					}
					if output.is_some() {
						return Err(RedirectError::Duplicate);
					}
					output = Some((path, *append));
				}
			}
		}
	}
	let mut stages: Vec<ExpandedStage> = Vec::new();
	if let Some(path) = input {
		stages.push(ExpandedStage { words: alloc::vec![REDIRECT_IN.to_vec(), path.to_vec()], merge_errors: false });
	}
	for (index, stage) in pipeline.stages.iter().enumerate() {
		stages.push(ExpandedStage { words: stage.words.clone(), merge_errors: merged.contains(&index) });
	}
	if let Some((path, append)) = output {
		let mut words: Vec<Vec<u8>> = alloc::vec![REDIRECT_OUT.to_vec()];
		if append {
			words.push(APPEND_FLAG.to_vec());
		}
		words.push(path.to_vec());
		stages.push(ExpandedStage { words, merge_errors: false });
	}
	Ok(Expansion { stages })
}

#[cfg(test)]
#[path = "shell_language/tests.rs"]
mod tests;

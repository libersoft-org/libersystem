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
	fn path(iter: &mut core::iter::Peekable<alloc::vec::IntoIter<Token>>) -> Result<Vec<u8>, ParseError> {
		match iter.next() {
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
	Ok(Pipeline { stages, background })
}

#[cfg(test)]
#[path = "shell_language/tests.rs"]
mod tests;

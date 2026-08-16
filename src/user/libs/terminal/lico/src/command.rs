//! The bottom command bar: line editing, quoting, completion and history.
//!
//! IT LAUNCHES NOTHING. What is here is the LINE - what has been typed, where the caret is, what a
//! completion would put there, what was typed before. The launch goes through the same governed
//! broker the shell uses, and the bar has no opinion about it, which is what stops this becoming a
//! second shell with a second set of rules about what a word is.
//!
//! History carries no authority either: a line recalled from it is text, re-parsed and re-checked
//! by the launcher exactly as if it had just been typed.

extern crate alloc;

use alloc::vec::Vec;

/// The most bytes one command line may hold, and the most lines the history keeps. Both refuse
/// rather than truncate: a command line silently cut short is a different command.
pub const MAX_COMMAND_BYTES: usize = 4096;
pub const MAX_HISTORY_LINES: usize = 64;

/// The editable command line.
pub struct CommandBar {
	line: Vec<u8>,
	caret: usize,
	history: Vec<Vec<u8>>,
	/// Where in the history the reader is, counted from the end. `0` is the line being typed.
	recall: usize,
	/// The line that was being typed before the reader started walking the history, so walking back
	/// to the present restores it rather than leaving the last recalled line in the bar.
	pending: Vec<u8>,
}

impl Default for CommandBar {
	fn default() -> CommandBar {
		CommandBar::new()
	}
}

impl CommandBar {
	pub fn new() -> CommandBar {
		CommandBar { line: Vec::new(), caret: 0, history: Vec::new(), recall: 0, pending: Vec::new() }
	}

	pub fn line(&self) -> &[u8] {
		&self.line
	}

	pub fn caret(&self) -> usize {
		self.caret
	}

	pub fn is_empty(&self) -> bool {
		self.line.is_empty()
	}

	pub fn clear(&mut self) {
		self.line.clear();
		self.caret = 0;
		self.recall = 0;
	}

	/// Type one byte at the caret. False when the line is full or would not grow.
	pub fn insert(&mut self, byte: u8) -> bool {
		if self.line.len() == MAX_COMMAND_BYTES || self.line.try_reserve(1).is_err() {
			return false;
		}
		self.line.insert(self.caret, byte);
		self.caret += 1;
		true
	}

	/// Insert a run - what pasting the selected name does.
	pub fn insert_slice(&mut self, bytes: &[u8]) -> bool {
		if self.line.len() + bytes.len() > MAX_COMMAND_BYTES || self.line.try_reserve(bytes.len()).is_err() {
			return false;
		}
		for (offset, &byte) in bytes.iter().enumerate() {
			self.line.insert(self.caret + offset, byte);
		}
		self.caret += bytes.len();
		true
	}

	pub fn backspace(&mut self) -> bool {
		if self.caret == 0 {
			return false;
		}
		self.caret -= 1;
		self.line.remove(self.caret);
		true
	}

	pub fn delete(&mut self) -> bool {
		if self.caret >= self.line.len() {
			return false;
		}
		self.line.remove(self.caret);
		true
	}

	pub fn left(&mut self) -> bool {
		if self.caret == 0 {
			return false;
		}
		self.caret -= 1;
		true
	}

	pub fn right(&mut self) -> bool {
		if self.caret >= self.line.len() {
			return false;
		}
		self.caret += 1;
		true
	}

	pub fn home(&mut self) -> bool {
		let moved = self.caret != 0;
		self.caret = 0;
		moved
	}

	pub fn end(&mut self) -> bool {
		let moved = self.caret != self.line.len();
		self.caret = self.line.len();
		moved
	}

	/// Move a word left. The same word rule the editor uses.
	pub fn word_left(&mut self) -> bool {
		let start = self.caret;
		while self.caret > 0 && self.line[self.caret - 1] == b' ' {
			self.caret -= 1;
		}
		while self.caret > 0 && self.line[self.caret - 1] != b' ' {
			self.caret -= 1;
		}
		self.caret != start
	}

	pub fn word_right(&mut self) -> bool {
		let start = self.caret;
		while self.caret < self.line.len() && self.line[self.caret] != b' ' {
			self.caret += 1;
		}
		while self.caret < self.line.len() && self.line[self.caret] == b' ' {
			self.caret += 1;
		}
		self.caret != start
	}

	/// Take the line and record it in the history. What Enter does.
	///
	/// A LINE THAT REPEATS THE LAST ONE IS NOT RECORDED TWICE, because a history full of the same
	/// command is a history nobody can walk back through.
	pub fn take(&mut self) -> Vec<u8> {
		let line = core::mem::take(&mut self.line);
		self.caret = 0;
		self.recall = 0;
		self.pending.clear();
		if !line.is_empty() && self.history.last().map(Vec::as_slice) != Some(line.as_slice()) {
			let mut copy: Vec<u8> = Vec::new();
			if copy.try_reserve_exact(line.len()).is_ok() && self.history.try_reserve(1).is_ok() {
				copy.extend_from_slice(&line);
				self.history.push(copy);
				if self.history.len() > MAX_HISTORY_LINES {
					self.history.remove(0);
				}
			}
		}
		line
	}

	/// Walk back through the history. The line being typed is remembered, so walking forward again
	/// returns to it rather than to an empty bar.
	pub fn recall_previous(&mut self) -> bool {
		if self.recall == self.history.len() {
			return false;
		}
		if self.recall == 0 {
			self.pending = core::mem::take(&mut self.line);
		}
		self.recall += 1;
		let entry = &self.history[self.history.len() - self.recall];
		let mut line: Vec<u8> = Vec::new();
		if line.try_reserve_exact(entry.len()).is_err() {
			self.recall -= 1;
			return false;
		}
		line.extend_from_slice(entry);
		self.line = line;
		self.caret = self.line.len();
		true
	}

	pub fn recall_next(&mut self) -> bool {
		if self.recall == 0 {
			return false;
		}
		self.recall -= 1;
		self.line = if self.recall == 0 {
			core::mem::take(&mut self.pending)
		} else {
			let entry = &self.history[self.history.len() - self.recall];
			let mut line: Vec<u8> = Vec::new();
			if line.try_reserve_exact(entry.len()).is_err() {
				return false;
			}
			line.extend_from_slice(entry);
			line
		};
		self.caret = self.line.len();
		true
	}

	pub fn history(&self) -> &[Vec<u8>] {
		&self.history
	}

	/// Complete the word under the caret from `candidates`, replacing it with the LONGEST COMMON
	/// PREFIX of everything that matches - which is what makes repeated presses converge rather
	/// than cycle through possibilities the reader has to watch go by.
	///
	/// Answers how many candidates matched, so a caller can show them when there is more than one
	/// and say nothing when there is exactly one.
	pub fn complete<'a>(&mut self, candidates: impl Iterator<Item = &'a [u8]> + Clone) -> usize {
		let (start, end) = self.word_bounds();
		let prefix: Vec<u8> = self.line[start..end].to_vec();
		let matching = candidates.clone().filter(|candidate| candidate.starts_with(&prefix));
		let count = matching.clone().count();
		if count == 0 {
			return 0;
		}
		let mut common: Vec<u8> = Vec::new();
		for candidate in matching {
			if common.is_empty() {
				if common.try_reserve_exact(candidate.len()).is_err() {
					return count;
				}
				common.extend_from_slice(candidate);
				continue;
			}
			let shared = common.iter().zip(candidate).take_while(|(a, b)| a == b).count();
			common.truncate(shared);
		}
		if common.len() <= prefix.len() {
			return count;
		}
		self.line.splice(start..end, common.iter().copied());
		self.caret = start + common.len();
		count
	}

	// The word the caret sits in, as a byte range. Words are separated by spaces OUTSIDE quotes,
	// which is what makes completing a quoted path with a space in it work at all.
	fn word_bounds(&self) -> (usize, usize) {
		let mut start = 0;
		let mut quoted = false;
		for (at, &byte) in self.line[..self.caret].iter().enumerate() {
			match byte {
				b'"' | b'\'' => quoted = !quoted,
				b' ' if !quoted => start = at + 1,
				_ => {}
			}
		}
		(start, self.caret)
	}
}

/// Why a command line could not be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseError {
	/// A quote that is never closed. Refused rather than guessed at: where the argument ends
	/// changes what the command is.
	UnterminatedQuote,
	/// Nothing but spaces.
	Empty,
	OutOfMemory,
	TooManyWords,
}

/// The most words one line may carry into a launch.
pub const MAX_WORDS: usize = 64;

/// Split a line into words, honouring single and double quotes.
///
/// THIS IS NOT A SHELL GRAMMAR AND DOES NOT WANT TO BE. There is no expansion, no operator, no
/// substitution - a variable holding `| rm x` is an argument containing those characters, because
/// nothing here ever looks at expanded text as syntax. The bar gains pipelines when it gains the
/// shell's own parser, which is what this milestone says it will do rather than inventing a second
/// one here.
pub fn split(line: &[u8]) -> Result<Vec<Vec<u8>>, ParseError> {
	let mut words: Vec<Vec<u8>> = Vec::new();
	let mut word: Vec<u8> = Vec::new();
	let mut started = false;
	let mut quote: Option<u8> = None;
	for &byte in line {
		match quote {
			Some(mark) if byte == mark => quote = None,
			Some(_) => {
				word.try_reserve(1).map_err(|_| ParseError::OutOfMemory)?;
				word.push(byte);
			}
			None if byte == b'"' || byte == b'\'' => {
				quote = Some(byte);
				started = true;
			}
			None if byte == b' ' || byte == b'\t' => {
				if started {
					if words.len() == MAX_WORDS {
						return Err(ParseError::TooManyWords);
					}
					words.try_reserve(1).map_err(|_| ParseError::OutOfMemory)?;
					words.push(core::mem::take(&mut word));
					started = false;
				}
			}
			None => {
				word.try_reserve(1).map_err(|_| ParseError::OutOfMemory)?;
				word.push(byte);
				started = true;
			}
		}
	}
	if quote.is_some() {
		return Err(ParseError::UnterminatedQuote);
	}
	if started {
		if words.len() == MAX_WORDS {
			return Err(ParseError::TooManyWords);
		}
		words.try_reserve(1).map_err(|_| ParseError::OutOfMemory)?;
		words.push(word);
	}
	if words.is_empty() {
		return Err(ParseError::Empty);
	}
	Ok(words)
}

/// What a line asks the bar to do, decided before anything is launched.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
	/// `cd <path>` - panel navigation, handled by the manager. NOT launched: a `cd` in a child
	/// changes a directory that is thrown away when the child exits, and reporting success for that
	/// is the lie this refuses to tell.
	ChangeDirectory(Vec<u8>),
	/// Run in the foreground, owning the terminal until it finishes.
	Foreground(Vec<Vec<u8>>),
	/// Run as a session job. The trailing `&`.
	Background(Vec<Vec<u8>>),
	/// A state-mutating builtin that cannot be honoured from here.
	Unsupported(Vec<u8>),
}

/// Read a typed line as a request.
///
/// THE STATE-MUTATING BUILTINS ARE REFUSED BY NAME rather than run somewhere their effect is
/// discarded - the same rule and the same reason as the shell's, which refuses `cd x | grep y`.
/// `cd` is the exception because the bar has somewhere to put it: the panel.
pub fn classify(line: &[u8]) -> Result<Request, ParseError> {
	let mut words = split(line)?;
	let background = words.last().is_some_and(|word| word.as_slice() == b"&");
	if background {
		words.pop();
		if words.is_empty() {
			return Err(ParseError::Empty);
		}
	}
	let command = words[0].clone();
	match command.as_slice() {
		b"cd" => {
			// `cd` with no argument is the root of the current volume, which the caller resolves;
			// an empty path is what says so.
			let target = words.get(1).cloned().unwrap_or_default();
			Ok(Request::ChangeDirectory(target))
		}
		b"export" | b"unset" | b"fg" | b"bg" | b"set" => Ok(Request::Unsupported(command)),
		// A shape-recognised assignment: `NAME=value` as the whole first word, which the session
		// owns and a child cannot change on its parent's behalf.
		_ if command.iter().position(|&byte| byte == b'=').is_some_and(|at| at > 0 && command[..at].iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')) => Ok(Request::Unsupported(command)),
		_ if background => Ok(Request::Background(words)),
		_ => Ok(Request::Foreground(words)),
	}
}

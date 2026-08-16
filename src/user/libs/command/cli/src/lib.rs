// The bounded primitives the command-line tools share: argument, size and range parsing, one
// deterministic glob matcher, and one chunk-at-a-time streaming vocabulary.
//
// WHY A CRATE OF ITS OWN, beside the `tools` library that already holds shared helpers: everything
// here is pure. It touches no capability, no channel and no runtime, so it builds and is TESTED ON
// THE HOST - which is the only way a matcher or a range parser gets the hostile inputs it needs.
// The helpers that must speak to a service (the volume walker, the file readers) stay in `tools`,
// where the client they need already lives, and are written in terms of what is here.
//
// Everything is bounded and iterative. A glob matcher written the obvious recursive way is a stack
// overflow with a pattern of asterisks, and a tool that reads a whole file to count its lines is a
// tool that fails on the file that matters - so the matcher backtracks with two cursors and the
// stream vocabulary hands out chunks.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Drop leading and trailing ASCII whitespace.
pub fn trim(s: &[u8]) -> &[u8] {
	let mut start: usize = 0;
	let mut end: usize = s.len();
	while start < end && s[start].is_ascii_whitespace() {
		start += 1;
	}
	while end > start && s[end - 1].is_ascii_whitespace() {
		end -= 1;
	}
	&s[start..end]
}

/// Parse an unsigned decimal integer. `None` when empty, non-decimal or past `u64`.
pub fn parse_u64(s: &[u8]) -> Option<u64> {
	if s.is_empty() {
		return None;
	}
	let mut value: u64 = 0;
	for &byte in s {
		if !byte.is_ascii_digit() {
			return None;
		}
		value = value.checked_mul(10)?.checked_add((byte - b'0') as u64)?;
	}
	Some(value)
}

/// Parse a byte size: a decimal number with an optional unit suffix.
///
/// `K`/`M`/`G`/`T` are POWERS OF TWO, and the `KiB` spelling means the same thing. That choice is
/// stated rather than assumed because the two conventions differ by seven per cent at gigabytes,
/// and a tool that truncates a file to "1G" of the wrong kind has destroyed data the caller meant
/// to keep. The decimal spellings (`KB`, `MB`) are refused rather than silently read as binary:
/// answering a caller who asked for 1000 with 1024 is the error this is avoiding.
///
/// Overflow is `None`, never a wrap: a size that does not fit is not a size.
pub fn parse_size(s: &[u8]) -> Option<u64> {
	let s = trim(s);
	if s.is_empty() {
		return None;
	}
	let digits_end: usize = s.iter().position(|byte| !byte.is_ascii_digit()).unwrap_or(s.len());
	let value: u64 = parse_u64(&s[..digits_end])?;
	let suffix: &[u8] = &s[digits_end..];
	let shift: u32 = match suffix {
		b"" | b"B" | b"b" => 0,
		b"K" | b"k" | b"KiB" | b"kiB" | b"kib" => 10,
		b"M" | b"m" | b"MiB" | b"miB" | b"mib" => 20,
		b"G" | b"g" | b"GiB" | b"giB" | b"gib" => 30,
		b"T" | b"t" | b"TiB" | b"tiB" | b"tib" => 40,
		_ => return None,
	};
	value.checked_mul(1u64.checked_shl(shift)?)
}

/// One `N`, `N-M`, `N-` or `-M` selection, in the closed form both ends inclusive.
///
/// Inclusive at BOTH ends because that is what every command-line range means to the person typing
/// it: `1-3` is three fields, not two. The half-open form is the right one inside a program and the
/// wrong one at its boundary, and converting at the boundary is this type's job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Range {
	pub start: u64,
	pub end: u64,
}

impl Range {
	pub fn contains(&self, at: u64) -> bool {
		at >= self.start && at <= self.end
	}
}

/// Parse one range. Both ends are inclusive; an open end is the largest `u64`.
///
/// REFUSED, rather than silently reordered: `5-2` is a mistake somebody made, and quietly reading
/// it as `2-5` is how a command does something other than what it was told. So is an empty side on
/// both ends (`-`), and so is a zero where the caller's numbering starts at one - which this does
/// not know, so `zero_ok` says.
pub fn parse_range(s: &[u8], zero_ok: bool) -> Option<Range> {
	let s = trim(s);
	if s.is_empty() {
		return None;
	}
	let (start, end): (u64, u64) = match s.iter().position(|&byte| byte == b'-') {
		None => {
			let only = parse_u64(s)?;
			(only, only)
		}
		Some(0) => {
			let end = parse_u64(&s[1..])?;
			(if zero_ok { 0 } else { 1 }, end)
		}
		Some(at) if at + 1 == s.len() => (parse_u64(&s[..at])?, u64::MAX),
		Some(at) => (parse_u64(&s[..at])?, parse_u64(&s[at + 1..])?),
	};
	if start > end || (!zero_ok && start == 0) {
		return None;
	}
	Some(Range { start, end })
}

/// Parse a comma-separated range list (`1,3-5,8-`), refusing the whole list if any part is bad.
///
/// THE WHOLE LIST FAILS, because a partially understood selection is worse than none: a tool that
/// dropped the part it could not read would cut different fields than it was asked to and say
/// nothing about it.
pub fn parse_ranges(s: &[u8], zero_ok: bool) -> Option<Vec<Range>> {
	let mut out: Vec<Range> = Vec::new();
	for part in s.split(|&byte| byte == b',') {
		let range = parse_range(part, zero_ok)?;
		out.try_reserve(1).ok()?;
		out.push(range);
	}
	if out.is_empty() { None } else { Some(out) }
}

/// Whether `name` matches `pattern`: `*` any run, `?` one byte, `[abc]` / `[a-z]` / `[!abc]` a set.
///
/// ITERATIVE, with one backtrack point, so a pattern of forty asterisks costs forty comparisons
/// rather than a stack. The recursive form of this function is the textbook one and it is a way to
/// crash a tool by naming a file - which is exactly the input a matcher is given.
///
/// Bytes, not characters: a `?` matches one BYTE, so it matches half of a two-byte UTF-8 character.
/// That is stated rather than fixed because the alternative - decoding every name before matching -
/// changes what `?` means for filenames that are not valid UTF-8, and those exist on the media this
/// system mounts. Callers matching human text use `*` and literals.
///
/// A malformed class (`[a`, `[]`) matches NOTHING rather than being read as a literal bracket: a
/// pattern the caller cannot have meant should fail visibly rather than quietly matching one file.
pub fn glob_match(pattern: &[u8], name: &[u8]) -> bool {
	let mut p: usize = 0;
	let mut n: usize = 0;
	// Where to resume if the current `*` turns out to have taken too little.
	let mut star: Option<usize> = None;
	let mut retry: usize = 0;
	while n < name.len() {
		let matched: bool = match pattern.get(p) {
			Some(b'*') => {
				star = Some(p);
				p += 1;
				retry = n;
				continue;
			}
			Some(b'?') => {
				p += 1;
				n += 1;
				continue;
			}
			Some(b'[') => match class_match(pattern, p, name[n]) {
				Some((matches, next)) => {
					if matches {
						p = next;
						n += 1;
						continue;
					}
					false
				}
				// A malformed class matches nothing, and backtracking into a `*` would let it
				// match after all.
				None => return false,
			},
			Some(&literal) => {
				if literal == name[n] {
					p += 1;
					n += 1;
					continue;
				}
				false
			}
			None => false,
		};
		if matched {
			continue;
		}
		// The one backtrack: the last `*` takes one more byte and we try again from there.
		match star {
			Some(at) => {
				p = at + 1;
				retry += 1;
				n = retry;
			}
			None => return false,
		}
	}
	// Trailing asterisks may match the empty rest; anything else may not.
	while pattern.get(p) == Some(&b'*') {
		p += 1;
	}
	p == pattern.len()
}

// One `[...]` class against one byte: whether it matched, and where the pattern continues.
// `None` is a malformed class - unterminated, or empty.
fn class_match(pattern: &[u8], open: usize, byte: u8) -> Option<(bool, usize)> {
	let mut at: usize = open + 1;
	let negated: bool = pattern.get(at) == Some(&b'!') || pattern.get(at) == Some(&b'^');
	if negated {
		at += 1;
	}
	let first: usize = at;
	let mut matched: bool = false;
	loop {
		let &current = pattern.get(at)?;
		// A `]` in the first position is a literal `]`, which is the only way to write one.
		if current == b']' && at > first {
			break;
		}
		// `a-z`, unless the `-` is the last byte before the `]`, where it is a literal.
		if pattern.get(at + 1) == Some(&b'-') && pattern.get(at + 2).is_some_and(|&end| end != b']') {
			let low = current;
			let high = *pattern.get(at + 2)?;
			// A reversed range matches nothing rather than being reordered, for the reason
			// `parse_range` refuses one.
			if low <= high && byte >= low && byte <= high {
				matched = true;
			}
			at += 3;
			continue;
		}
		if current == byte {
			matched = true;
		}
		at += 1;
	}
	if at == first {
		// `[]` and `[!]` - nothing between the brackets.
		return None;
	}
	Some((matched != negated, at + 1))
}

/// The kind of thing a `--option` argument turned out to be.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arg<'a> {
	/// `--name` or `--name=value`.
	Long(&'a [u8], Option<&'a [u8]>),
	/// One letter from a `-abc` cluster.
	Short(u8),
	/// Anything else, including `-` (which conventionally means standard input).
	Value(&'a [u8]),
	/// Everything after a bare `--` is a value whatever it looks like.
	Separator,
}

/// Split one argument into what it is, without deciding what it means.
///
/// A CLUSTER IS NOT SPLIT HERE: `-abc` returns `Short(b'a')` and the caller asks for the rest with
/// `short_cluster`, because a parser that returned three arguments from one would have to allocate
/// and this one does not.
pub fn classify(argument: &[u8]) -> Arg<'_> {
	if argument == b"--" {
		return Arg::Separator;
	}
	if let Some(rest) = argument.strip_prefix(b"--") {
		return match rest.iter().position(|&byte| byte == b'=') {
			Some(at) => Arg::Long(&rest[..at], Some(&rest[at + 1..])),
			None => Arg::Long(rest, None),
		};
	}
	if argument.len() > 1 && argument[0] == b'-' && argument[1] != b'-' {
		return Arg::Short(argument[1]);
	}
	Arg::Value(argument)
}

/// The letters of a `-abc` cluster, empty for anything else.
pub fn short_cluster(argument: &[u8]) -> &[u8] {
	if argument.len() > 1 && argument[0] == b'-' && argument[1] != b'-' { &argument[1..] } else { &[] }
}

/// A source of bytes that is read a chunk at a time.
///
/// The point of the trait is that the tools are written against it rather than against a file: a
/// pager, a counter and a search all consume bytes in order and none of them needs to know whether
/// the bytes come from a volume window, a pipe or a buffer in a test. It is the backpressure
/// boundary as well - `next_chunk` is a REQUEST, so a consumer that is not ready simply does not
/// ask, and nothing accumulates behind it.
pub trait ChunkSource {
	/// The next chunk, or an empty slice at the end of the stream. `Err` is a real failure, which
	/// is different from the end and must not be reported as one.
	fn next_chunk(&mut self) -> Result<&[u8], ChunkError>;
}

/// Why a chunk could not be produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChunkError {
	/// The source is gone: a closed channel, an unmounted volume.
	Unavailable,
	/// The source refused this read and may not refuse the next one.
	Again,
	/// The bytes could not be held.
	OutOfMemory,
}

/// A `ChunkSource` over bytes already in memory, for tests and for callers that have the whole
/// thing already.
pub struct SliceSource<'a> {
	bytes: &'a [u8],
	chunk: usize,
	at: usize,
}

impl<'a> SliceSource<'a> {
	pub fn new(bytes: &'a [u8], chunk: usize) -> SliceSource<'a> {
		SliceSource { bytes, chunk: chunk.max(1), at: 0 }
	}
}

impl ChunkSource for SliceSource<'_> {
	fn next_chunk(&mut self) -> Result<&[u8], ChunkError> {
		let end: usize = core::cmp::min(self.at + self.chunk, self.bytes.len());
		let chunk: &[u8] = &self.bytes[self.at..end];
		self.at = end;
		Ok(chunk)
	}
}

/// Split a stream into lines without holding more than one line at a time.
///
/// ONE LINE IS BOUNDED TOO, by `limit`. A stream with no newline in it is otherwise a way to grow a
/// tool's memory without bound by handing it a file - and "the input is a text file" is an
/// assumption, not a fact, on a system that mounts other people's media. Crossing the limit is an
/// error rather than a split, because a line silently cut in half is a wrong answer that looks
/// right.
pub struct Lines<S: ChunkSource> {
	source: S,
	held: Vec<u8>,
	limit: usize,
	// Bytes of the current chunk not yet handed out.
	pending: Vec<u8>,
	at: usize,
	done: bool,
}

/// What `Lines::next_line` produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineOutcome {
	/// A line, without its terminator. The bytes are in `Lines::line`.
	Line,
	/// The stream ended. Any bytes before the end without a terminator were the last line and were
	/// reported before this.
	End,
	/// The stream failed, or one line grew past the limit.
	Failed(ChunkError),
	/// One line grew past `limit` - a distinct answer from a failed source, because the input is
	/// intact and it is the tool's bound that was reached.
	TooLong,
}

impl<S: ChunkSource> Lines<S> {
	pub fn new(source: S, limit: usize) -> Lines<S> {
		Lines { source, held: Vec::new(), limit: limit.max(1), pending: Vec::new(), at: 0, done: false }
	}

	/// The bytes of the line most recently produced.
	pub fn line(&self) -> &[u8] {
		&self.held
	}

	/// Produce the next line.
	pub fn next_line(&mut self) -> LineOutcome {
		self.held.clear();
		loop {
			// Everything already in hand, up to the next newline.
			while self.at < self.pending.len() {
				let byte: u8 = self.pending[self.at];
				self.at += 1;
				if byte == b'\n' {
					return LineOutcome::Line;
				}
				if self.held.len() == self.limit {
					return LineOutcome::TooLong;
				}
				if self.held.try_reserve(1).is_err() {
					return LineOutcome::Failed(ChunkError::OutOfMemory);
				}
				self.held.push(byte);
			}
			if self.done {
				// A trailing line without a newline is still a line; the end comes after it.
				return if self.held.is_empty() { LineOutcome::End } else { LineOutcome::Line };
			}
			match self.source.next_chunk() {
				Ok(chunk) if chunk.is_empty() => {
					self.done = true;
					self.pending.clear();
					self.at = 0;
				}
				Ok(chunk) => {
					self.pending.clear();
					if self.pending.try_reserve(chunk.len()).is_err() {
						return LineOutcome::Failed(ChunkError::OutOfMemory);
					}
					self.pending.extend_from_slice(chunk);
					self.at = 0;
				}
				Err(e) => return LineOutcome::Failed(e),
			}
		}
	}
}

/// The last `n` lines of a stream, held in a ring so the input's size does not decide the memory.
///
/// This is what `tail` is: a bounded window over an unbounded stream. Keeping every line and
/// slicing the end is the obvious implementation and it is one file away from failing.
pub struct LastLines {
	lines: Vec<Vec<u8>>,
	want: usize,
	next: usize,
	filled: usize,
}

impl LastLines {
	pub fn new(want: usize) -> LastLines {
		LastLines { lines: Vec::new(), want, next: 0, filled: 0 }
	}

	/// Offer one line. `false` means it could not be held, which the caller reports rather than
	/// silently producing a shorter answer.
	pub fn push(&mut self, line: &[u8]) -> bool {
		if self.want == 0 {
			return true;
		}
		let mut owned: Vec<u8> = Vec::new();
		if owned.try_reserve_exact(line.len()).is_err() {
			return false;
		}
		owned.extend_from_slice(line);
		if self.lines.len() < self.want {
			if self.lines.try_reserve(1).is_err() {
				return false;
			}
			self.lines.push(owned);
			self.filled = self.lines.len();
			self.next = self.lines.len() % self.want;
			return true;
		}
		self.lines[self.next] = owned;
		self.next = (self.next + 1) % self.want;
		true
	}

	/// The held lines, oldest first.
	pub fn lines(&self) -> impl Iterator<Item = &[u8]> {
		let start: usize = if self.filled < self.want { 0 } else { self.next };
		(0..self.filled).map(move |i| self.lines[(start + i) % self.filled.max(1)].as_slice())
	}
}

/// A bounded set of lines held for sorting: the bytes in ONE buffer, and an index beside them.
///
/// Not `Vec<Vec<u8>>`. Two reasons, and the second is the one that decided it: a line per
/// allocation costs an allocator round trip and a header per line, and sorting moves the boxes
/// rather than the index. The flat form sorts an index of two integers and leaves every byte where
/// it was.
///
/// It is BOUNDED at both ends: a maximum number of lines and a maximum total size, both refused
/// rather than grown, because the caller that fills this cannot know how large the input is until
/// it has read it.
pub struct LineBuffer {
	bytes: Vec<u8>,
	index: Vec<(usize, usize)>,
	max_lines: usize,
	max_bytes: usize,
}

/// Why a line could not be held.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HoldError {
	/// The caller's ceiling was reached. The input is intact; this buffer will not take more.
	Full,
	OutOfMemory,
}

impl LineBuffer {
	pub fn new(max_lines: usize, max_bytes: usize) -> LineBuffer {
		LineBuffer { bytes: Vec::new(), index: Vec::new(), max_lines, max_bytes }
	}

	pub fn len(&self) -> usize {
		self.index.len()
	}

	pub fn is_empty(&self) -> bool {
		self.index.is_empty()
	}

	/// Hold one line, or say why it could not be.
	pub fn push(&mut self, line: &[u8]) -> Result<(), HoldError> {
		if self.index.len() >= self.max_lines || self.bytes.len().saturating_add(line.len()) > self.max_bytes {
			return Err(HoldError::Full);
		}
		self.bytes.try_reserve(line.len()).map_err(|_| HoldError::OutOfMemory)?;
		self.index.try_reserve(1).map_err(|_| HoldError::OutOfMemory)?;
		let at: usize = self.bytes.len();
		self.bytes.extend_from_slice(line);
		self.index.push((at, line.len()));
		Ok(())
	}

	/// One held line by position.
	pub fn line(&self, at: usize) -> &[u8] {
		match self.index.get(at) {
			Some(&(start, len)) => &self.bytes[start..start + len],
			None => &[],
		}
	}

	/// Order the lines by a comparison over their bytes. The sort is STABLE, so lines that compare
	/// equal keep the order they arrived in - which is what makes a second sort by another key
	/// meaningful rather than arbitrary.
	pub fn sort_by<F: FnMut(&[u8], &[u8]) -> core::cmp::Ordering>(&mut self, mut compare: F) {
		let bytes: &[u8] = &self.bytes;
		self.index.sort_by(|a, b| compare(&bytes[a.0..a.0 + a.1], &bytes[b.0..b.0 + b.1]));
	}

	/// The held lines in their current order.
	pub fn lines(&self) -> impl Iterator<Item = &[u8]> {
		self.index.iter().map(move |&(start, len)| &self.bytes[start..start + len])
	}
}

/// A bounded regular expression, compiled once and matched with a STEP BUDGET.
///
/// WHY A BUDGET RATHER THAN A CLEVERER ENGINE. Backtracking is the simple implementation and it is
/// the one with the pathological cases: `(a*)*b` against a run of `a` is the classic way to make a
/// search take longer than the machine has. A budget makes every match TERMINATE - a pattern that
/// needs more steps than it allows is REFUSED as too complex rather than run until somebody notices,
/// and refusing is an answer a caller can act on where a hang is not.
///
/// THE SYNTAX IS SMALL AND SAID OUT LOUD, because a half-understood pattern is a wrong answer that
/// looks right: literal bytes, `.` for any byte, `*` `+` `?` on the item before them, `[abc]`
/// `[^abc]` `[a-z]` classes, `^` and `$` anchors, and `\` to escape any of these. No groups, no
/// alternation, no repetition counts - each is an addition behind this same interface rather than a
/// different thing, and none of them is needed to find a word in a file.
#[derive(Debug)]
pub struct Regex {
	items: Vec<Item>,
	anchored_start: bool,
	anchored_end: bool,
}

/// One compiled item: what to match, and how many times.
#[derive(Debug)]
struct Item {
	kind: Kind,
	min: u8,
	/// `u32::MAX` for `*` and `+`.
	max: u32,
}

#[derive(Debug)]
enum Kind {
	Byte(u8),
	Any,
	/// A class as a 256-bit set, so `[a-z0-9_]` costs the same to test as `[a]`.
	Class {
		set: [u64; 4],
		negated: bool,
	},
}

/// Why a pattern could not be compiled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegexError {
	/// A repetition with nothing before it - `*ab`.
	NothingToRepeat,
	/// A class that is never closed.
	UnterminatedClass,
	/// A trailing backslash, which escapes nothing.
	DanglingEscape,
	/// Past the item bound.
	TooComplex,
	OutOfMemory,
}

/// The most items one pattern may compile to, and the most steps one match may take.
pub const MAX_REGEX_ITEMS: usize = 128;
pub const MAX_REGEX_STEPS: u32 = 200_000;

impl Regex {
	pub fn compile(pattern: &[u8]) -> Result<Regex, RegexError> {
		let mut items: Vec<Item> = Vec::new();
		let mut at = 0;
		let anchored_start = pattern.first() == Some(&b'^');
		if anchored_start {
			at = 1;
		}
		let mut anchored_end = false;
		while at < pattern.len() {
			// `$` is an anchor only at the very end; anywhere else it is a literal dollar, which is
			// what somebody searching for a price typed.
			if pattern[at] == b'$' && at + 1 == pattern.len() {
				anchored_end = true;
				break;
			}
			let kind = match pattern[at] {
				b'\\' => {
					at += 1;
					let byte = *pattern.get(at).ok_or(RegexError::DanglingEscape)?;
					Kind::Byte(byte)
				}
				b'.' => Kind::Any,
				b'[' => {
					let (kind, next) = compile_class(pattern, at)?;
					at = next;
					kind
				}
				b'*' | b'+' | b'?' => return Err(RegexError::NothingToRepeat),
				byte => Kind::Byte(byte),
			};
			at += 1;
			let (min, max) = match pattern.get(at) {
				Some(b'*') => {
					at += 1;
					(0, u32::MAX)
				}
				Some(b'+') => {
					at += 1;
					(1, u32::MAX)
				}
				Some(b'?') => {
					at += 1;
					(0, 1)
				}
				_ => (1, 1),
			};
			if items.len() == MAX_REGEX_ITEMS {
				return Err(RegexError::TooComplex);
			}
			items.try_reserve(1).map_err(|_| RegexError::OutOfMemory)?;
			items.push(Item { kind, min, max });
		}
		Ok(Regex { items, anchored_start, anchored_end })
	}

	/// The first match at or after `from`, as `(start, end)`. `None` when there is none, or when the
	/// match ran out of steps - which is refused rather than reported as "not found", so a caller
	/// can tell a pattern that is too expensive from a file that does not contain it.
	pub fn find(&self, haystack: &[u8], from: usize) -> Option<(usize, usize)> {
		let mut start = from.min(haystack.len());
		loop {
			let mut steps = MAX_REGEX_STEPS;
			if let Some(end) = self.match_here(haystack, start, 0, &mut steps) {
				if !self.anchored_end || end == haystack.len() {
					return Some((start, end));
				}
			}
			// AN ANCHORED PATTERN IS TRIED ONCE. `^ab` that does not match at the start does not
			// match anywhere, and sliding it along would be answering a different question.
			if self.anchored_start || start >= haystack.len() {
				return None;
			}
			start += 1;
		}
	}

	/// Whether the whole of `text` is this pattern.
	pub fn matches(&self, text: &[u8]) -> bool {
		let mut steps = MAX_REGEX_STEPS;
		self.match_here(text, 0, 0, &mut steps) == Some(text.len())
	}

	// Match `items[index..]` at `at`, answering where the match ended. GREEDY WITH BACKTRACKING:
	// each repetition takes as much as it can and gives back one at a time, which is what makes
	// `.*b` find the LAST `b` and `.*?` unnecessary.
	fn match_here(&self, haystack: &[u8], at: usize, index: usize, steps: &mut u32) -> Option<usize> {
		if *steps == 0 {
			return None;
		}
		*steps -= 1;
		let Some(item) = self.items.get(index) else { return Some(at) };
		let mut taken = 0u32;
		let mut cursor = at;
		while taken < item.max && cursor < haystack.len() && item.kind.admits(haystack[cursor]) {
			cursor += 1;
			taken += 1;
		}
		while taken + 1 > item.min as u32 || (taken == 0 && item.min == 0) {
			if let Some(end) = self.match_here(haystack, at + taken as usize, index + 1, steps) {
				return Some(end);
			}
			if taken == 0 {
				return None;
			}
			taken -= 1;
		}
		None
	}
}

impl Kind {
	fn admits(&self, byte: u8) -> bool {
		match self {
			Kind::Byte(wanted) => *wanted == byte,
			Kind::Any => true,
			Kind::Class { set, negated } => {
				let present = set[(byte >> 6) as usize] & (1u64 << (byte & 63)) != 0;
				present != *negated
			}
		}
	}
}

// Compile `[...]` starting at `at`, answering the kind and the index of its closing bracket.
fn compile_class(pattern: &[u8], at: usize) -> Result<(Kind, usize), RegexError> {
	let mut set = [0u64; 4];
	let mut index = at + 1;
	let negated = pattern.get(index) == Some(&b'^');
	if negated {
		index += 1;
	}
	// A `]` FIRST IS A LITERAL `]`, which is the convention and is what somebody writing a class of
	// brackets needs - there is no other way to say it without an escape.
	let mut first = true;
	while index < pattern.len() {
		let byte = pattern[index];
		if byte == b']' && !first {
			return Ok((Kind::Class { set, negated }, index));
		}
		first = false;
		let byte = if byte == b'\\' {
			index += 1;
			*pattern.get(index).ok_or(RegexError::DanglingEscape)?
		} else {
			byte
		};
		// A RANGE, when a `-` sits between two members and is not the last character.
		if pattern.get(index + 1) == Some(&b'-') && pattern.get(index + 2).is_some_and(|end| *end != b']') {
			let end = pattern[index + 2];
			let (low, high) = if byte <= end { (byte, end) } else { (end, byte) };
			for value in low..=high {
				set[(value >> 6) as usize] |= 1u64 << (value & 63);
			}
			index += 3;
			continue;
		}
		set[(byte >> 6) as usize] |= 1u64 << (byte & 63);
		index += 1;
	}
	Err(RegexError::UnterminatedClass)
}

#[cfg(test)]
mod tests;

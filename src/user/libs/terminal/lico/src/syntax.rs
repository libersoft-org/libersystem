//! Versioned declarative syntax descriptions and incremental highlighting.
//!
//! A descriptor is valid UTF-8 text. Its first non-comment line is `lico-syntax 1`.
//! Subsequent lines use whitespace-separated tokens; literals cannot contain spaces and
//! use `\\`, `\"`, `\n`, `\r`, or `\t` escapes. The supported directives are:
//!
//! ```text
//! name rust
//! glob *.rs
//! first-line #!
//! style plain
//! max-nesting 4
//! context root plain
//! line root // comment
//! open root /* comment comment
//! close comment */
//! escape string \\\
//! keyword root fn keyword
//! ```
//!
//! No directive can execute code, name a path, or request a capability. All literal
//! matchers must make progress, and the parser bounds every descriptor resource before a
//! caller can begin highlighting.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;
pub const MAX_STYLES: usize = 32;
pub const MAX_CONTEXTS: usize = 32;
pub const MAX_RULES: usize = 256;
pub const MAX_TOKEN_BYTES: usize = 128;
pub const MAX_NESTING: usize = 8;
const MAX_LINES: usize = 1024;

pub type StyleId = u8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenSpan {
	pub start: usize,
	pub end: usize,
	pub style: StyleId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HighlightResult {
	pub spans: usize,
	pub truncated: bool,
	pub nesting_limited: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxError {
	TooLarge,
	InvalidUtf8,
	InvalidHeader,
	UnsupportedVersion,
	InvalidDirective,
	InvalidValue,
	MissingName,
	MissingGlob,
	MissingRoot,
	MissingNesting,
	InvalidNesting,
	TokenTooLong,
	TooManyStyles,
	TooManyContexts,
	TooManyRules,
	TooManyLines,
	DuplicateStyle,
	DuplicateContext,
	DuplicateRecognition,
	ConflictingRule,
	UnknownStyle,
	UnknownContext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxMatchKind {
	Filename,
	FirstLine,
}

#[derive(Clone, Copy)]
pub struct SyntaxSelection<'a> {
	pub descriptor: &'a SyntaxDescriptor,
	pub kind: SyntaxMatchKind,
}

#[derive(Clone)]
struct Context {
	name: String,
	style: StyleId,
}

#[derive(Clone)]
enum Rule {
	Line { context: u8, literal: Vec<u8>, style: StyleId },
	Open { context: u8, literal: Vec<u8>, target: u8, style: StyleId },
	Close { context: u8, literal: Vec<u8> },
	Escape { context: u8, literal: Vec<u8> },
	Keyword { context: u8, literal: Vec<u8>, style: StyleId },
}

impl Rule {
	fn context(&self) -> u8 {
		match self {
			Rule::Line { context, .. } | Rule::Open { context, .. } | Rule::Close { context, .. } | Rule::Escape { context, .. } | Rule::Keyword { context, .. } => *context,
		}
	}

	fn literal(&self) -> &[u8] {
		match self {
			Rule::Line { literal, .. } | Rule::Open { literal, .. } | Rule::Close { literal, .. } | Rule::Escape { literal, .. } | Rule::Keyword { literal, .. } => literal,
		}
	}
}

#[derive(Clone, Eq, PartialEq)]
enum PendingKind {
	Line(String),
	Open { target: String, style: String },
	Close,
	Escape,
	Keyword(String),
}

#[derive(Clone, Eq, PartialEq)]
struct PendingRule {
	context: String,
	literal: Vec<u8>,
	kind: PendingKind,
}

/// A fully validated descriptor. Style names remain stable descriptors of theme roles.
#[derive(Clone)]
pub struct SyntaxDescriptor {
	name: String,
	globs: Vec<String>,
	first_lines: Vec<Vec<u8>>,
	styles: Vec<String>,
	contexts: Vec<Context>,
	rules: Vec<Rule>,
	root: u8,
	max_nesting: u8,
}

impl SyntaxDescriptor {
	pub fn name(&self) -> &str {
		&self.name
	}

	pub fn style_name(&self, style: StyleId) -> Option<&str> {
		self.styles.get(style as usize).map(String::as_str)
	}

	pub fn context_count(&self) -> usize {
		self.contexts.len()
	}

	pub fn rule_count(&self) -> usize {
		self.rules.len()
	}

	pub fn max_nesting(&self) -> usize {
		self.max_nesting as usize
	}

	pub fn initial_state(&self) -> LineState {
		LineState::for_descriptor(self)
	}

	fn filename_score(&self, name: &[u8]) -> Option<usize> {
		self.globs.iter().filter(|glob| glob_matches(glob.as_bytes(), name)).map(|glob| glob.bytes().filter(|byte| *byte != b'*' && *byte != b'?').count()).max()
	}

	fn first_line_score(&self, first_line: &[u8]) -> Option<usize> {
		self.first_lines.iter().filter(|prefix| first_line.starts_with(prefix)).map(Vec::len).max()
	}

	/// Highlight one logical line, preserving block/string context in `state`.
	pub fn highlight_line(&self, state: &mut LineState, line: &[u8], spans: &mut [TokenSpan]) -> HighlightResult {
		state.ensure_descriptor(self);
		let mut count = 0;
		let mut truncated = false;
		let mut nesting_limited = false;
		let mut offset = 0;
		while offset < line.len() {
			let context = state.current();
			if let Some(rule) = self.best_rule(context, line, offset, is_escape).cloned() {
				let end = (offset + rule.literal().len() + scalar_width(&line[offset + rule.literal().len()..])).min(line.len());
				emit(spans, &mut count, offset, end, self.context_style(context), &mut truncated);
				offset = end;
				continue;
			}
			if let Some(rule) = self.best_rule(context, line, offset, is_close).cloned() {
				let end = offset + rule.literal().len();
				emit(spans, &mut count, offset, end, self.context_style(context), &mut truncated);
				state.pop();
				offset = end;
				continue;
			}
			if let Some(rule) = self.best_rule(context, line, offset, is_open).cloned() {
				let Rule::Open { literal, target, style, .. } = rule else { unreachable!() };
				let end = offset + literal.len();
				emit(spans, &mut count, offset, end, style, &mut truncated);
				if !state.push(target, self.max_nesting) {
					nesting_limited = true;
				}
				offset = end;
				continue;
			}
			if let Some(rule) = self.best_rule(context, line, offset, is_line).cloned() {
				let Rule::Line { style, .. } = rule else { unreachable!() };
				emit(spans, &mut count, offset, line.len(), style, &mut truncated);
				break;
			}
			if let Some(rule) = self.best_rule(context, line, offset, is_keyword).cloned() {
				let Rule::Keyword { literal, style, .. } = rule else { unreachable!() };
				if keyword_boundary(line, offset, literal.len()) {
					let end = offset + literal.len();
					emit(spans, &mut count, offset, end, style, &mut truncated);
					offset = end;
					continue;
				}
			}
			let end = offset + scalar_width(&line[offset..]);
			emit(spans, &mut count, offset, end, self.context_style(context), &mut truncated);
			offset = end;
		}
		HighlightResult { spans: count, truncated, nesting_limited }
	}

	fn context_style(&self, context: u8) -> StyleId {
		self.contexts[context as usize].style
	}

	fn best_rule<'a, F: Fn(&Rule) -> bool>(&'a self, context: u8, line: &[u8], offset: usize, accept: F) -> Option<&'a Rule> {
		self.rules.iter().filter(|rule| rule.context() == context && accept(rule) && line[offset..].starts_with(rule.literal())).max_by_key(|rule| rule.literal().len())
	}
}

/// Lexical state retained by the viewer/editor's incremental line cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineState {
	stack: [u8; MAX_NESTING],
	depth: u8,
}

impl LineState {
	pub const fn new() -> LineState {
		LineState { stack: [0; MAX_NESTING], depth: 0 }
	}

	pub fn reset(&mut self, descriptor: &SyntaxDescriptor) {
		*self = Self::for_descriptor(descriptor);
	}

	pub fn depth(&self) -> usize {
		self.depth as usize
	}

	fn for_descriptor(descriptor: &SyntaxDescriptor) -> LineState {
		let mut state = LineState::new();
		state.stack[0] = descriptor.root;
		state.depth = 1;
		state
	}

	fn ensure_descriptor(&mut self, descriptor: &SyntaxDescriptor) {
		if self.depth == 0 || self.depth as usize > descriptor.max_nesting() || self.stack[0] != descriptor.root || self.stack[..self.depth as usize].iter().any(|&context| context as usize >= descriptor.contexts.len()) {
			self.reset(descriptor);
		}
	}

	fn current(&self) -> u8 {
		self.stack[self.depth as usize - 1]
	}

	fn push(&mut self, context: u8, max_nesting: u8) -> bool {
		if self.depth as usize >= MAX_NESTING || self.depth >= max_nesting {
			return false;
		}
		self.stack[self.depth as usize] = context;
		self.depth += 1;
		true
	}

	fn pop(&mut self) {
		if self.depth > 1 {
			self.depth -= 1;
		}
	}
}

impl Default for LineState {
	fn default() -> Self {
		Self::new()
	}
}

/// Parse and validate a descriptor before it can affect highlighting.
pub fn parse_descriptor(bytes: &[u8]) -> Result<SyntaxDescriptor, SyntaxError> {
	if bytes.len() > MAX_DESCRIPTOR_BYTES {
		return Err(SyntaxError::TooLarge);
	}
	let source = core::str::from_utf8(bytes).map_err(|_| SyntaxError::InvalidUtf8)?;
	let mut header = false;
	let mut lines = 0;
	let mut name = None;
	let mut globs = Vec::new();
	let mut first_lines = Vec::new();
	let mut styles = Vec::new();
	let mut contexts: Vec<(String, String)> = Vec::new();
	let mut pending = Vec::new();
	let mut max_nesting = None;
	for raw in source.split('\n') {
		let line = raw.strip_suffix('\r').unwrap_or(raw).trim();
		// A COMMENT IS `#` WITH OR WITHOUT TEXT AFTER IT. The rule required the space, so a bare
		// `#` - the line every multi-paragraph comment uses to separate its paragraphs - fell
		// through to the header check and the file was refused as `InvalidHeader`, naming a line
		// that was right there and correct. A format whose comment syntax has a trap in it is a
		// format people write invalid files in.
		if line.is_empty() || line == "#" || line.starts_with("# ") {
			continue;
		}
		lines += 1;
		if lines > MAX_LINES {
			return Err(SyntaxError::TooManyLines);
		}
		let fields: Vec<&str> = line.split_ascii_whitespace().collect();
		if !header {
			if fields.as_slice() == ["lico-syntax", "1"] {
				header = true;
				continue;
			}
			if fields.first() == Some(&"lico-syntax") {
				return Err(SyntaxError::UnsupportedVersion);
			}
			return Err(SyntaxError::InvalidHeader);
		}
		let directive = *fields.first().ok_or(SyntaxError::InvalidDirective)?;
		match directive {
			"name" => {
				exact(&fields, 2)?;
				if name.replace(fields[1].to_string()).is_some() || !identifier(fields[1]) {
					return Err(SyntaxError::InvalidValue);
				}
			}
			"glob" => {
				exact(&fields, 2)?;
				let glob = fields[1].to_string();
				if !literal(&decode_literal(fields[1])?) {
					return Err(SyntaxError::InvalidValue);
				}
				if globs.iter().any(|existing: &String| existing == &glob) {
					return Err(SyntaxError::DuplicateRecognition);
				}
				globs.push(glob);
			}
			"first-line" => {
				exact(&fields, 2)?;
				let prefix = decode_literal(fields[1])?;
				if !literal(&prefix) {
					return Err(SyntaxError::InvalidValue);
				}
				if first_lines.iter().any(|existing: &Vec<u8>| existing == &prefix) {
					return Err(SyntaxError::DuplicateRecognition);
				}
				first_lines.push(prefix);
			}
			"style" => {
				exact(&fields, 2)?;
				if styles.len() == MAX_STYLES {
					return Err(SyntaxError::TooManyStyles);
				}
				if !identifier(fields[1]) {
					return Err(SyntaxError::InvalidValue);
				}
				if styles.iter().any(|existing: &String| existing == fields[1]) {
					return Err(SyntaxError::DuplicateStyle);
				}
				styles.push(fields[1].to_string());
			}
			"max-nesting" => {
				exact(&fields, 2)?;
				if max_nesting.replace(parse_nesting(fields[1])?).is_some() {
					return Err(SyntaxError::InvalidNesting);
				}
			}
			"context" => {
				exact(&fields, 3)?;
				if contexts.len() == MAX_CONTEXTS {
					return Err(SyntaxError::TooManyContexts);
				}
				if !identifier(fields[1]) || !identifier(fields[2]) {
					return Err(SyntaxError::InvalidValue);
				}
				if contexts.iter().any(|(existing, _)| existing == fields[1]) {
					return Err(SyntaxError::DuplicateContext);
				}
				contexts.push((fields[1].to_string(), fields[2].to_string()));
			}
			"line" => {
				exact(&fields, 4)?;
				push_pending(&mut pending, PendingRule { context: fields[1].to_string(), literal: decode_literal(fields[2])?, kind: PendingKind::Line(fields[3].to_string()) })?;
			}
			"open" => {
				exact(&fields, 5)?;
				push_pending(&mut pending, PendingRule { context: fields[1].to_string(), literal: decode_literal(fields[2])?, kind: PendingKind::Open { target: fields[3].to_string(), style: fields[4].to_string() } })?;
			}
			"close" => {
				exact(&fields, 3)?;
				push_pending(&mut pending, PendingRule { context: fields[1].to_string(), literal: decode_literal(fields[2])?, kind: PendingKind::Close })?;
			}
			"escape" => {
				exact(&fields, 3)?;
				push_pending(&mut pending, PendingRule { context: fields[1].to_string(), literal: decode_literal(fields[2])?, kind: PendingKind::Escape })?;
			}
			"keyword" => {
				exact(&fields, 4)?;
				push_pending(&mut pending, PendingRule { context: fields[1].to_string(), literal: decode_literal(fields[2])?, kind: PendingKind::Keyword(fields[3].to_string()) })?;
			}
			_ => return Err(SyntaxError::InvalidDirective),
		}
	}
	if !header {
		return Err(SyntaxError::InvalidHeader);
	}
	let name = name.ok_or(SyntaxError::MissingName)?;
	if globs.is_empty() {
		return Err(SyntaxError::MissingGlob);
	}
	let max_nesting = max_nesting.ok_or(SyntaxError::MissingNesting)?;
	let root = contexts.iter().position(|(context, _)| context == "root").ok_or(SyntaxError::MissingRoot)?;
	let mut compiled_contexts = Vec::new();
	for (name, style) in contexts {
		compiled_contexts.push(Context { name, style: style_id(&styles, &style)? });
	}
	let mut rules = Vec::new();
	for rule in pending {
		rules.push(compile_rule(rule, &compiled_contexts, &styles)?);
	}
	Ok(SyntaxDescriptor { name, globs, first_lines, styles, contexts: compiled_contexts, rules, root: root as u8, max_nesting })
}

/// Select by filename before first line. Longer literal matches win; equal matches use
/// the descriptor name so installation order cannot affect syntax selection.
pub fn select_descriptor<'a>(descriptors: &'a [SyntaxDescriptor], name: &[u8], first_line: &[u8]) -> Option<SyntaxSelection<'a>> {
	select(descriptors, |descriptor| descriptor.filename_score(name)).map(|descriptor| SyntaxSelection { descriptor, kind: SyntaxMatchKind::Filename }).or_else(|| select(descriptors, |descriptor| descriptor.first_line_score(first_line)).map(|descriptor| SyntaxSelection { descriptor, kind: SyntaxMatchKind::FirstLine }))
}

fn select<'a, F: Fn(&SyntaxDescriptor) -> Option<usize>>(descriptors: &'a [SyntaxDescriptor], score: F) -> Option<&'a SyntaxDescriptor> {
	let mut best: Option<(&SyntaxDescriptor, usize)> = None;
	for descriptor in descriptors {
		let Some(candidate) = score(descriptor) else { continue };
		if best.is_none_or(|(current, score)| candidate > score || (candidate == score && descriptor.name.as_bytes() < current.name.as_bytes())) {
			best = Some((descriptor, candidate));
		}
	}
	best.map(|(descriptor, _)| descriptor)
}

fn exact(fields: &[&str], expected: usize) -> Result<(), SyntaxError> {
	if fields.len() == expected { Ok(()) } else { Err(SyntaxError::InvalidDirective) }
}

fn parse_nesting(value: &str) -> Result<u8, SyntaxError> {
	let mut parsed: u16 = 0;
	if value.is_empty() {
		return Err(SyntaxError::InvalidNesting);
	}
	for byte in value.bytes() {
		if !byte.is_ascii_digit() {
			return Err(SyntaxError::InvalidNesting);
		}
		parsed = parsed.checked_mul(10).and_then(|current| current.checked_add((byte - b'0') as u16)).ok_or(SyntaxError::InvalidNesting)?;
	}
	if parsed == 0 || parsed as usize > MAX_NESTING {
		return Err(SyntaxError::InvalidNesting);
	}
	Ok(parsed as u8)
}

fn push_pending(rules: &mut Vec<PendingRule>, rule: PendingRule) -> Result<(), SyntaxError> {
	if !identifier(&rule.context) || !literal(&rule.literal) {
		return Err(SyntaxError::InvalidValue);
	}
	if rules.len() == MAX_RULES {
		return Err(SyntaxError::TooManyRules);
	}
	if rules.iter().any(|existing| existing.context == rule.context && existing.literal == rule.literal) {
		return Err(SyntaxError::ConflictingRule);
	}
	rules.push(rule);
	Ok(())
}

fn compile_rule(rule: PendingRule, contexts: &[Context], styles: &[String]) -> Result<Rule, SyntaxError> {
	let context = context_id(contexts, &rule.context)?;
	match rule.kind {
		PendingKind::Line(style) => Ok(Rule::Line { context, literal: rule.literal, style: style_id(styles, &style)? }),
		PendingKind::Open { target, style } => Ok(Rule::Open { context, literal: rule.literal, target: context_id(contexts, &target)?, style: style_id(styles, &style)? }),
		PendingKind::Close => {
			if contexts[context as usize].name == "root" {
				return Err(SyntaxError::InvalidValue);
			}
			Ok(Rule::Close { context, literal: rule.literal })
		}
		PendingKind::Escape => Ok(Rule::Escape { context, literal: rule.literal }),
		PendingKind::Keyword(style) => Ok(Rule::Keyword { context, literal: rule.literal, style: style_id(styles, &style)? }),
	}
}

fn context_id(contexts: &[Context], name: &str) -> Result<u8, SyntaxError> {
	contexts.iter().position(|context| context.name == name).map(|index| index as u8).ok_or(SyntaxError::UnknownContext)
}

fn style_id(styles: &[String], name: &str) -> Result<StyleId, SyntaxError> {
	styles.iter().position(|style| style == name).map(|index| index as StyleId).ok_or(SyntaxError::UnknownStyle)
}

fn identifier(value: &str) -> bool {
	!value.is_empty() && value.len() <= MAX_TOKEN_BYTES && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn literal(value: &[u8]) -> bool {
	!value.is_empty() && value.len() <= MAX_TOKEN_BYTES && !value.contains(&0)
}

fn decode_literal(token: &str) -> Result<Vec<u8>, SyntaxError> {
	let mut output = Vec::new();
	let bytes = token.as_bytes();
	let mut offset = 0;
	while offset < bytes.len() {
		if bytes[offset] != b'\\' {
			output.push(bytes[offset]);
			offset += 1;
			continue;
		}
		offset += 1;
		let escape = *bytes.get(offset).ok_or(SyntaxError::InvalidValue)?;
		output.push(match escape {
			b'\\' => b'\\',
			b'"' => b'"',
			b'n' => b'\n',
			b'r' => b'\r',
			b't' => b'\t',
			_ => return Err(SyntaxError::InvalidValue),
		});
		offset += 1;
		if output.len() > MAX_TOKEN_BYTES {
			return Err(SyntaxError::TokenTooLong);
		}
	}
	Ok(output)
}

fn is_escape(rule: &Rule) -> bool {
	matches!(rule, Rule::Escape { .. })
}

fn is_close(rule: &Rule) -> bool {
	matches!(rule, Rule::Close { .. })
}

fn is_open(rule: &Rule) -> bool {
	matches!(rule, Rule::Open { .. })
}

fn is_line(rule: &Rule) -> bool {
	matches!(rule, Rule::Line { .. })
}

fn is_keyword(rule: &Rule) -> bool {
	matches!(rule, Rule::Keyword { .. })
}

fn emit(spans: &mut [TokenSpan], count: &mut usize, start: usize, end: usize, style: StyleId, truncated: &mut bool) {
	if start == end {
		return;
	}
	if let Some(last) = count.checked_sub(1).and_then(|index| spans.get_mut(index)) {
		if last.end == start && last.style == style {
			last.end = end;
			return;
		}
	}
	if *count == spans.len() {
		*truncated = true;
		return;
	}
	spans[*count] = TokenSpan { start, end, style };
	*count += 1;
}

fn scalar_width(bytes: &[u8]) -> usize {
	let Some(&first) = bytes.first() else { return 0 };
	let width = match first {
		0xc2..=0xdf => 2,
		0xe0..=0xef => 3,
		0xf0..=0xf4 => 4,
		_ => 1,
	};
	if width <= bytes.len() { width } else { 1 }
}

fn keyword_boundary(line: &[u8], start: usize, len: usize) -> bool {
	let before = start.checked_sub(1).and_then(|index| line.get(index)).copied();
	let after = line.get(start + len).copied();
	!before.is_some_and(word_byte) && !after.is_some_and(word_byte)
}

fn word_byte(byte: u8) -> bool {
	byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

fn glob_matches(pattern: &[u8], value: &[u8]) -> bool {
	let mut pattern_index = 0;
	let mut value_index = 0;
	let mut star = None;
	let mut restart = 0;
	while value_index < value.len() {
		if pattern_index < pattern.len() && (pattern[pattern_index] == b'?' || pattern[pattern_index].eq_ignore_ascii_case(&value[value_index])) {
			pattern_index += 1;
			value_index += 1;
		} else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
			star = Some(pattern_index);
			pattern_index += 1;
			restart = value_index;
		} else if let Some(star_index) = star {
			pattern_index = star_index + 1;
			restart += 1;
			value_index = restart;
		} else {
			return false;
		}
	}
	while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
		pattern_index += 1;
	}
	pattern_index == pattern.len()
}

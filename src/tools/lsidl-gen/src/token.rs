//! Tokens, source spans, and the shared error type for the LSIDL front-end.

use std::fmt;

// A 1-based line/column position in a source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
	pub line: u32,
	pub col: u32,
}

impl fmt::Display for Span {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}:{}", self.line, self.col)
	}
}

// A front-end error (lexing, parsing, or validation), tied to a source span.
#[derive(Clone, Debug)]
pub struct Error {
	pub span: Span,
	pub msg: String,
}

impl Error {
	pub fn new(span: Span, msg: impl Into<String>) -> Error {
		Error { span, msg: msg.into() }
	}
}

// The lexical tokens of LSIDL. Keywords are not distinguished here; the parser
// recognizes them as ordinary identifiers by position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
	Ident(String),
	Num(u64),
	LBrace,
	RBrace,
	LParen,
	RParen,
	Lt,
	Gt,
	Colon,
	Semi,
	Comma,
	Dot,
	Eq,
	At,
	Arrow,
	DocComment(String), // /// declaration/member doc
	PackageDoc(String), // //! package doc
	Eof,
}

impl fmt::Display for Tok {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Tok::Ident(s) => write!(f, "`{s}`"),
			Tok::Num(n) => write!(f, "`{n}`"),
			Tok::LBrace => f.write_str("`{`"),
			Tok::RBrace => f.write_str("`}`"),
			Tok::LParen => f.write_str("`(`"),
			Tok::RParen => f.write_str("`)`"),
			Tok::Lt => f.write_str("`<`"),
			Tok::Gt => f.write_str("`>`"),
			Tok::Colon => f.write_str("`:`"),
			Tok::Semi => f.write_str("`;`"),
			Tok::Comma => f.write_str("`,`"),
			Tok::Dot => f.write_str("`.`"),
			Tok::Eq => f.write_str("`=`"),
			Tok::At => f.write_str("`@`"),
			Tok::Arrow => f.write_str("`->`"),
			Tok::DocComment(_) => f.write_str("doc comment"),
			Tok::PackageDoc(_) => f.write_str("package doc"),
			Tok::Eof => f.write_str("end of file"),
		}
	}
}

// A token paired with the span where it begins.
#[derive(Clone, Debug)]
pub struct Token {
	pub tok: Tok,
	pub span: Span,
}

//! The LSIDL lexer: source text -> a flat token stream.

use crate::token::{Error, Span, Tok, Token};

struct Lexer<'a> {
	b: &'a [u8],
	i: usize,
	line: u32,
	col: u32,
}

impl<'a> Lexer<'a> {
	fn span(&self) -> Span {
		Span { line: self.line, col: self.col }
	}

	// The byte `k` positions ahead, or 0 (treated as end-of-input) past the end.
	fn at(&self, k: usize) -> u8 {
		let j = self.i + k;
		if j < self.b.len() { self.b[j] } else { 0 }
	}

	fn bump(&mut self) -> u8 {
		let c = self.at(0);
		self.i += 1;
		if c == b'\n' {
			self.line += 1;
			self.col = 1;
		} else {
			self.col += 1;
		}
		c
	}

	// Skip whitespace and `//` / `/* */` comments; error on an unterminated block.
	// Returns any doc comment (/// or //!) tokens that should be preserved.
	fn skip_trivia(&mut self) -> Result<Vec<Token>, Error> {
		let mut docs = Vec::new();
		loop {
			let c = self.at(0);
			if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' {
				self.bump();
			} else if c == b'/' && self.at(1) == b'/' && self.at(2) == b'/' {
				// /// doc comment
				let span = self.span();
				self.bump();
				self.bump();
				self.bump();
				let start = self.i;
				while self.at(0) != 0 && self.at(0) != b'\n' {
					self.bump();
				}
				let text = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
				docs.push(Token { tok: Tok::DocComment(text), span });
			} else if c == b'/' && self.at(1) == b'/' && self.at(2) == b'!' {
				// //! package doc
				let span = self.span();
				self.bump();
				self.bump();
				self.bump();
				let start = self.i;
				while self.at(0) != 0 && self.at(0) != b'\n' {
					self.bump();
				}
				let text = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
				docs.push(Token { tok: Tok::PackageDoc(text), span });
			} else if c == b'/' && self.at(1) == b'/' {
				// ordinary comment, discarded
				while self.at(0) != 0 && self.at(0) != b'\n' {
					self.bump();
				}
			} else if c == b'/' && self.at(1) == b'*' {
				let open = self.span();
				self.bump();
				self.bump();
				loop {
					if self.at(0) == 0 {
						return Err(Error::new(open, "unterminated block comment"));
					}
					if self.at(0) == b'*' && self.at(1) == b'/' {
						self.bump();
						self.bump();
						break;
					}
					self.bump();
				}
			} else {
				return Ok(docs);
			}
		}
	}

	// Lex a kebab-case identifier: a lowercase letter, then lowercase letters,
	// digits, and single interior hyphens (a `-` is only consumed when followed by
	// a letter or digit, so a trailing or doubled hyphen ends the identifier).
	fn ident(&mut self) -> Token {
		let span = self.span();
		let start = self.i;
		loop {
			let c = self.at(0);
			if c.is_ascii_lowercase() || c.is_ascii_digit() {
				self.bump();
			} else if c == b'-' && (self.at(1).is_ascii_lowercase() || self.at(1).is_ascii_digit()) {
				self.bump();
			} else {
				break;
			}
		}
		let text = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
		Token { tok: Tok::Ident(text), span }
	}

	fn number(&mut self) -> Result<Token, Error> {
		let span = self.span();
		let start = self.i;
		while self.at(0).is_ascii_digit() {
			self.bump();
		}
		let text = std::str::from_utf8(&self.b[start..self.i]).unwrap();
		let n: u64 = text.parse().map_err(|_| Error::new(span, format!("integer literal `{text}` is too large")))?;
		Ok(Token { tok: Tok::Num(n), span })
	}
}

// Tokenize a whole source string, ending with a single `Eof` token.
pub fn tokenize(src: &str) -> Result<Vec<Token>, Error> {
	let mut lx = Lexer { b: src.as_bytes(), i: 0, line: 1, col: 1 };
	let mut out: Vec<Token> = Vec::new();
	loop {
		let docs = lx.skip_trivia()?;
		out.extend(docs);
		let span = lx.span();
		let c = lx.at(0);
		if c == 0 {
			out.push(Token { tok: Tok::Eof, span });
			return Ok(out);
		}
		match c {
			b'{' => {
				lx.bump();
				out.push(Token { tok: Tok::LBrace, span });
			}
			b'}' => {
				lx.bump();
				out.push(Token { tok: Tok::RBrace, span });
			}
			b'(' => {
				lx.bump();
				out.push(Token { tok: Tok::LParen, span });
			}
			b')' => {
				lx.bump();
				out.push(Token { tok: Tok::RParen, span });
			}
			b'<' => {
				lx.bump();
				out.push(Token { tok: Tok::Lt, span });
			}
			b'>' => {
				lx.bump();
				out.push(Token { tok: Tok::Gt, span });
			}
			b':' => {
				lx.bump();
				out.push(Token { tok: Tok::Colon, span });
			}
			b';' => {
				lx.bump();
				out.push(Token { tok: Tok::Semi, span });
			}
			b',' => {
				lx.bump();
				out.push(Token { tok: Tok::Comma, span });
			}
			b'.' => {
				lx.bump();
				out.push(Token { tok: Tok::Dot, span });
			}
			b'=' => {
				lx.bump();
				out.push(Token { tok: Tok::Eq, span });
			}
			b'@' => {
				lx.bump();
				out.push(Token { tok: Tok::At, span });
			}
			b'-' => {
				if lx.at(1) == b'>' {
					lx.bump();
					lx.bump();
					out.push(Token { tok: Tok::Arrow, span });
				} else {
					return Err(Error::new(span, "unexpected `-` (use `->` for returns; identifiers may not start or end with `-`)"));
				}
			}
			_ if c.is_ascii_lowercase() => out.push(lx.ident()),
			_ if c.is_ascii_digit() => out.push(lx.number()?),
			_ => return Err(Error::new(span, format!("unexpected character `{}`", c as char))),
		}
	}
}

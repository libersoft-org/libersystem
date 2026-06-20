//! Recursive-descent parser: a token stream -> an `ast::File`.
//!
//! Keywords are ordinary identifiers recognized by position, so the lexer needs
//! no keyword table. Parsing fails fast on the first syntax error.

use crate::ast::*;
use crate::token::{Error, Span, Tok, Token};

// A parsed annotation, e.g. `@op(1)` or `@rights(read, write)`.
struct Ann {
	name: String,
	args: Vec<Arg>,
	span: Span,
}

enum Arg {
	Name(String),
	Num(u64),
}

struct Parser {
	toks: Vec<Token>,
	pos: usize,
}

// Parse a token stream into a file AST.
pub fn parse(toks: Vec<Token>) -> Result<File, Error> {
	let mut p = Parser { toks, pos: 0 };
	p.file()
}

impl Parser {
	fn peek(&self) -> &Tok {
		&self.toks[self.pos].tok
	}

	fn span(&self) -> Span {
		self.toks[self.pos].span
	}

	fn is(&self, t: &Tok) -> bool {
		self.peek() == t
	}

	fn is_kw(&self, kw: &str) -> bool {
		matches!(self.peek(), Tok::Ident(s) if s == kw)
	}

	fn bump(&mut self) -> Token {
		let t = self.toks[self.pos].clone();
		if self.pos + 1 < self.toks.len() {
			self.pos += 1;
		}
		t
	}

	fn eat(&mut self, want: &Tok) -> Result<(), Error> {
		if self.peek() == want {
			self.bump();
			Ok(())
		} else {
			Err(Error::new(self.span(), format!("expected {want}, found {}", self.peek())))
		}
	}

	fn ident(&mut self) -> Result<(String, Span), Error> {
		let span = self.span();
		match self.peek() {
			Tok::Ident(s) => {
				let s = s.clone();
				self.bump();
				Ok((s, span))
			}
			other => Err(Error::new(span, format!("expected an identifier, found {other}"))),
		}
	}

	// Require a specific keyword identifier (e.g. `func`).
	fn keyword(&mut self, kw: &str) -> Result<Span, Error> {
		let span = self.span();
		if self.is_kw(kw) {
			self.bump();
			Ok(span)
		} else {
			Err(Error::new(span, format!("expected `{kw}`, found {}", self.peek())))
		}
	}

	fn number(&mut self) -> Result<(u64, Span), Error> {
		let span = self.span();
		match self.peek() {
			Tok::Num(n) => {
				let n = *n;
				self.bump();
				Ok((n, span))
			}
			other => Err(Error::new(span, format!("expected a number, found {other}"))),
		}
	}

	fn file(&mut self) -> Result<File, Error> {
		let package = self.package()?;
		let mut uses = Vec::new();
		while self.is_kw("use") {
			uses.push(self.use_decl()?);
		}
		let mut items = Vec::new();
		while !self.is(&Tok::Eof) {
			items.push(self.item()?);
		}
		Ok(File { package, uses, items })
	}

	fn package(&mut self) -> Result<Package, Error> {
		let span = self.keyword("package")?;
		let mut path = vec![self.ident()?.0];
		while self.is(&Tok::Colon) {
			self.bump();
			path.push(self.ident()?.0);
		}
		self.eat(&Tok::At)?;
		let (version, vsp) = self.number()?;
		let version = u32::try_from(version).map_err(|_| Error::new(vsp, "package version must fit in u32"))?;
		self.eat(&Tok::Semi)?;
		Ok(Package { path, version, span })
	}

	fn use_decl(&mut self) -> Result<Use, Error> {
		let span = self.keyword("use")?;
		let mut path = vec![self.ident()?.0];
		while self.is(&Tok::Colon) {
			self.bump();
			path.push(self.ident()?.0);
		}
		self.eat(&Tok::Dot)?;
		self.eat(&Tok::LBrace)?;
		let mut names = vec![self.ident()?.0];
		while self.is(&Tok::Comma) {
			self.bump();
			if self.is(&Tok::RBrace) {
				break;
			}
			names.push(self.ident()?.0);
		}
		self.eat(&Tok::RBrace)?;
		self.eat(&Tok::Semi)?;
		Ok(Use { path, names, span })
	}

	fn annotations(&mut self) -> Result<Vec<Ann>, Error> {
		let mut anns = Vec::new();
		while self.is(&Tok::At) {
			self.bump();
			let (name, span) = self.ident()?;
			let mut args = Vec::new();
			if self.is(&Tok::LParen) {
				self.bump();
				if !self.is(&Tok::RParen) {
					loop {
						args.push(self.ann_arg()?);
						if self.is(&Tok::Comma) {
							self.bump();
						} else {
							break;
						}
					}
				}
				self.eat(&Tok::RParen)?;
			}
			anns.push(Ann { name, args, span });
		}
		Ok(anns)
	}

	fn ann_arg(&mut self) -> Result<Arg, Error> {
		match self.peek() {
			Tok::Ident(s) => {
				let s = s.clone();
				self.bump();
				Ok(Arg::Name(s))
			}
			Tok::Num(n) => {
				let n = *n;
				self.bump();
				Ok(Arg::Num(n))
			}
			other => Err(Error::new(self.span(), format!("expected an annotation argument, found {other}"))),
		}
	}

	fn item(&mut self) -> Result<Item, Error> {
		let anns = self.annotations()?;
		reject_anns_except(&anns, &["since"])?;
		let span = self.span();
		let (kw, _) = self.ident()?;
		match kw.as_str() {
			"record" => Ok(Item::Record(self.record()?)),
			"enum" => Ok(Item::Enum(self.enum_decl()?)),
			"variant" => Ok(Item::Variant(self.variant()?)),
			"flags" => Ok(Item::Flags(self.flags()?)),
			"resource" => Ok(Item::Resource(self.resource()?)),
			"interface" => Ok(Item::Interface(self.interface()?)),
			other => Err(Error::new(span, format!("expected a type or interface declaration, found `{other}`"))),
		}
	}

	fn record(&mut self) -> Result<Record, Error> {
		let (name, span) = self.ident()?;
		self.eat(&Tok::LBrace)?;
		let mut fields = Vec::new();
		while !self.is(&Tok::RBrace) {
			let anns = self.annotations()?;
			reject_anns_except(&anns, &["since"])?;
			let (fname, fsp) = self.ident()?;
			self.eat(&Tok::Colon)?;
			let ty = self.ty()?;
			fields.push(Field { name: fname, ty, span: fsp });
			if self.is(&Tok::Comma) {
				self.bump();
			} else {
				break;
			}
		}
		self.eat(&Tok::RBrace)?;
		Ok(Record { name, fields, span })
	}

	fn enum_decl(&mut self) -> Result<Enum, Error> {
		let (name, span) = self.ident()?;
		self.eat(&Tok::LBrace)?;
		let mut cases = Vec::new();
		let mut reserved = Vec::new();
		while !self.is(&Tok::RBrace) {
			let anns = self.annotations()?;
			// A lone `@reserved(n)` with no following identifier reserves an ordinal.
			if anns.len() == 1 && anns[0].name == "reserved" && (self.is(&Tok::Comma) || self.is(&Tok::RBrace)) {
				reserved.push(reserved_value(&anns[0])?);
			} else {
				reject_anns_except(&anns, &["since"])?;
				let (cname, csp) = self.ident()?;
				let ordinal = if self.is(&Tok::Eq) {
					self.bump();
					let (n, nsp) = self.number()?;
					Some(u32::try_from(n).map_err(|_| Error::new(nsp, "enum ordinal must fit in u32"))?)
				} else {
					None
				};
				cases.push(EnumCase { name: cname, ordinal, span: csp });
			}
			if self.is(&Tok::Comma) {
				self.bump();
			} else {
				break;
			}
		}
		self.eat(&Tok::RBrace)?;
		Ok(Enum { name, cases, reserved, span })
	}

	fn variant(&mut self) -> Result<Variant, Error> {
		let (name, span) = self.ident()?;
		self.eat(&Tok::LBrace)?;
		let mut cases = Vec::new();
		while !self.is(&Tok::RBrace) {
			let anns = self.annotations()?;
			reject_anns_except(&anns, &["since"])?;
			let (cname, csp) = self.ident()?;
			let payload = if self.is(&Tok::LParen) {
				self.bump();
				let t = self.ty()?;
				self.eat(&Tok::RParen)?;
				Some(t)
			} else {
				None
			};
			cases.push(VarCase { name: cname, payload, span: csp });
			if self.is(&Tok::Comma) {
				self.bump();
			} else {
				break;
			}
		}
		self.eat(&Tok::RBrace)?;
		Ok(Variant { name, cases, span })
	}

	fn flags(&mut self) -> Result<Flags, Error> {
		let (name, span) = self.ident()?;
		self.eat(&Tok::LBrace)?;
		let mut flags = Vec::new();
		while !self.is(&Tok::RBrace) {
			flags.push(self.ident()?.0);
			if self.is(&Tok::Comma) {
				self.bump();
			} else {
				break;
			}
		}
		self.eat(&Tok::RBrace)?;
		Ok(Flags { name, flags, span })
	}

	fn resource(&mut self) -> Result<Resource, Error> {
		let (name, span) = self.ident()?;
		self.eat(&Tok::Semi)?;
		Ok(Resource { name, span })
	}

	fn interface(&mut self) -> Result<Interface, Error> {
		let (name, span) = self.ident()?;
		self.eat(&Tok::LBrace)?;
		let mut methods = Vec::new();
		let mut reserved = Vec::new();
		while !self.is(&Tok::RBrace) {
			let anns = self.annotations()?;
			// `@reserved(n);` retires an opcode.
			if anns.len() == 1 && anns[0].name == "reserved" && self.is(&Tok::Semi) {
				reserved.push(reserved_value(&anns[0])?);
				self.bump();
				continue;
			}
			methods.push(self.method(anns)?);
		}
		self.eat(&Tok::RBrace)?;
		Ok(Interface { name, methods, reserved, span })
	}

	fn method(&mut self, anns: Vec<Ann>) -> Result<Method, Error> {
		reject_anns_except(&anns, &["op", "since"])?;
		let op = self.require_op(&anns)?;
		let (name, span) = self.ident()?;
		self.eat(&Tok::Colon)?;
		self.keyword("func")?;
		self.eat(&Tok::LParen)?;
		let mut params = Vec::new();
		if !self.is(&Tok::RParen) {
			loop {
				params.push(self.param()?);
				if self.is(&Tok::Comma) {
					self.bump();
				} else {
					break;
				}
			}
		}
		self.eat(&Tok::RParen)?;
		self.eat(&Tok::Arrow)?;
		let ret = self.ty()?;
		self.eat(&Tok::Semi)?;
		Ok(Method { name, op, params, ret, span })
	}

	fn require_op(&self, anns: &[Ann]) -> Result<u32, Error> {
		let ops: Vec<&Ann> = anns.iter().filter(|a| a.name == "op").collect();
		match ops.as_slice() {
			[] => Err(Error::new(self.span(), "method is missing its `@op(n)` opcode")),
			[a] => {
				if a.args.len() != 1 {
					return Err(Error::new(a.span, "`@op` takes exactly one number, e.g. `@op(1)`"));
				}
				match &a.args[0] {
					Arg::Num(n) => u32::try_from(*n).map_err(|_| Error::new(a.span, "opcode must fit in u32")),
					Arg::Name(_) => Err(Error::new(a.span, "`@op` takes a number, not a name")),
				}
			}
			_ => Err(Error::new(ops[1].span, "method has more than one `@op`")),
		}
	}

	fn param(&mut self) -> Result<Param, Error> {
		let anns = self.annotations()?;
		reject_anns_except(&anns, &["rights", "since"])?;
		let rights = collect_rights(&anns);
		let (name, span) = self.ident()?;
		self.eat(&Tok::Colon)?;
		let ty = self.ty()?;
		Ok(Param { name, ty, rights, span })
	}

	fn ty(&mut self) -> Result<Type, Error> {
		let (name, _) = self.ident()?;
		let t = match name.as_str() {
			"bool" => Type::Bool,
			"u8" => Type::U8,
			"u16" => Type::U16,
			"u32" => Type::U32,
			"u64" => Type::U64,
			"i8" => Type::I8,
			"i16" => Type::I16,
			"i32" => Type::I32,
			"i64" => Type::I64,
			"f32" => Type::F32,
			"f64" => Type::F64,
			"string" => Type::String,
			"unit" => Type::Unit,
			"buffer" => Type::Buffer,
			"option" => Type::Option(Box::new(self.generic1()?)),
			"list" => Type::List(Box::new(self.generic1()?)),
			"stream" => Type::Stream(Box::new(self.generic1()?)),
			"tuple" => {
				self.eat(&Tok::Lt)?;
				let mut elems = vec![self.ty()?];
				while self.is(&Tok::Comma) {
					self.bump();
					elems.push(self.ty()?);
				}
				self.eat(&Tok::Gt)?;
				Type::Tuple(elems)
			}
			"result" => {
				self.eat(&Tok::Lt)?;
				let ok = self.ty()?;
				self.eat(&Tok::Comma)?;
				let err = self.ty()?;
				self.eat(&Tok::Gt)?;
				Type::Result(Box::new(ok), Box::new(err))
			}
			"handle" => {
				self.eat(&Tok::Lt)?;
				let (res, _) = self.ident()?;
				self.eat(&Tok::Gt)?;
				Type::Handle(res)
			}
			_ => Type::Named(name),
		};
		Ok(t)
	}

	fn generic1(&mut self) -> Result<Type, Error> {
		self.eat(&Tok::Lt)?;
		let t = self.ty()?;
		self.eat(&Tok::Gt)?;
		Ok(t)
	}
}

fn reject_anns_except(anns: &[Ann], allowed: &[&str]) -> Result<(), Error> {
	for a in anns {
		if !allowed.contains(&a.name.as_str()) {
			return Err(Error::new(a.span, format!("annotation `@{}` is not allowed here", a.name)));
		}
	}
	Ok(())
}

fn reserved_value(a: &Ann) -> Result<u32, Error> {
	if a.args.len() != 1 {
		return Err(Error::new(a.span, "`@reserved` takes exactly one number, e.g. `@reserved(3)`"));
	}
	match &a.args[0] {
		Arg::Num(n) => u32::try_from(*n).map_err(|_| Error::new(a.span, "reserved value must fit in u32")),
		Arg::Name(_) => Err(Error::new(a.span, "`@reserved` takes a number, not a name")),
	}
}

fn collect_rights(anns: &[Ann]) -> Vec<String> {
	let mut rights = Vec::new();
	for a in anns.iter().filter(|a| a.name == "rights") {
		for arg in &a.args {
			if let Arg::Name(s) = arg {
				rights.push(s.clone());
			}
		}
	}
	rights
}

// The WebAssembly binary parser: it reads the module preamble and the sections the
// runtime needs (types, imports, functions, memory, exports, code) into a
// [`Module`]. Unknown or unsupported sections (custom, tables, globals, data, ...)
// are skipped by their declared size, so a module may carry them as long as the
// runtime does not need them.

use crate::module::{Export, ExportKind, Func, FuncType, Import, Module, ValType};
use alloc::string::String;
use alloc::vec::Vec;

// A parse failure with a short static reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError(pub &'static str);

// A cursor over the module bytes with the LEB128 + name readers wasm uses.
struct Reader<'a> {
	buf: &'a [u8],
	pos: usize,
}

impl<'a> Reader<'a> {
	fn new(buf: &'a [u8]) -> Reader<'a> {
		Reader { buf, pos: 0 }
	}

	fn done(&self) -> bool {
		self.pos >= self.buf.len()
	}

	fn byte(&mut self) -> Result<u8, ParseError> {
		let b: u8 = *self.buf.get(self.pos).ok_or(ParseError("unexpected end of module"))?;
		self.pos += 1;
		Ok(b)
	}

	fn bytes(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
		if self.pos + n > self.buf.len() {
			return Err(ParseError("unexpected end of module"));
		}
		let s: &[u8] = &self.buf[self.pos..self.pos + n];
		self.pos += n;
		Ok(s)
	}

	// Unsigned LEB128, capped at 32 bits.
	fn u32(&mut self) -> Result<u32, ParseError> {
		let mut result: u32 = 0;
		let mut shift: u32 = 0;
		loop {
			let b: u8 = self.byte()?;
			result |= ((b & 0x7f) as u32) << shift;
			if b & 0x80 == 0 {
				return Ok(result);
			}
			shift += 7;
			if shift >= 32 {
				return Err(ParseError("LEB128 overflow"));
			}
		}
	}

	// A length-prefixed UTF-8 name.
	fn name(&mut self) -> Result<String, ParseError> {
		let n: usize = self.u32()? as usize;
		let s: &[u8] = self.bytes(n)?;
		core::str::from_utf8(s).map(String::from).map_err(|_| ParseError("invalid UTF-8 in name"))
	}
}

fn val_type(b: u8) -> Result<ValType, ParseError> {
	match b {
		0x7f => Ok(ValType::I32),
		0x7e => Ok(ValType::I64),
		_ => Err(ParseError("unsupported value type")),
	}
}

// Parse a module's bytes into a [`Module`], or fail with the first error.
pub fn parse(bytes: &[u8]) -> Result<Module, ParseError> {
	let mut r: Reader = Reader::new(bytes);
	if r.bytes(4)? != b"\0asm" {
		return Err(ParseError("bad magic"));
	}
	if r.bytes(4)? != [1, 0, 0, 0] {
		return Err(ParseError("unsupported version"));
	}
	let mut m: Module = Module::default();
	while !r.done() {
		let id: u8 = r.byte()?;
		let size: usize = r.u32()? as usize;
		let end: usize = r.pos + size;
		if end > r.buf.len() {
			return Err(ParseError("section runs past end of module"));
		}
		match id {
			1 => parse_types(&mut r, &mut m)?,
			2 => parse_imports(&mut r, &mut m)?,
			3 => parse_functions(&mut r, &mut m)?,
			5 => parse_memory(&mut r, &mut m)?,
			7 => parse_exports(&mut r, &mut m)?,
			10 => parse_code(&mut r, &mut m)?,
			_ => r.pos = end, // skip a section the runtime does not need
		}
		if r.pos != end {
			return Err(ParseError("section size mismatch"));
		}
	}
	Ok(m)
}

fn parse_types(r: &mut Reader, m: &mut Module) -> Result<(), ParseError> {
	let count: u32 = r.u32()?;
	for _ in 0..count {
		if r.byte()? != 0x60 {
			return Err(ParseError("expected a function type"));
		}
		let mut ft: FuncType = FuncType::default();
		let nparams: u32 = r.u32()?;
		for _ in 0..nparams {
			ft.params.push(val_type(r.byte()?)?);
		}
		let nresults: u32 = r.u32()?;
		for _ in 0..nresults {
			ft.results.push(val_type(r.byte()?)?);
		}
		m.types.push(ft);
	}
	Ok(())
}

fn parse_imports(r: &mut Reader, m: &mut Module) -> Result<(), ParseError> {
	let count: u32 = r.u32()?;
	for _ in 0..count {
		let module: String = r.name()?;
		let field: String = r.name()?;
		let kind: u8 = r.byte()?;
		match kind {
			0x00 => {
				// an imported function: its type index follows
				let type_index: u32 = r.u32()?;
				m.imports.push(Import { module, field, type_index });
			}
			_ => return Err(ParseError("only function imports are supported")),
		}
	}
	Ok(())
}

fn parse_functions(r: &mut Reader, m: &mut Module) -> Result<(), ParseError> {
	let count: u32 = r.u32()?;
	for _ in 0..count {
		let type_index: u32 = r.u32()?;
		m.funcs.push(Func { type_index, locals: Vec::new(), body: Vec::new() });
	}
	Ok(())
}

fn parse_memory(r: &mut Reader, m: &mut Module) -> Result<(), ParseError> {
	let count: u32 = r.u32()?;
	if count > 1 {
		return Err(ParseError("at most one memory is supported"));
	}
	if count == 1 {
		let flags: u8 = r.byte()?;
		let min: u32 = r.u32()?;
		if flags & 0x01 != 0 {
			let _max: u32 = r.u32()?; // a maximum is allowed but unused
		}
		m.memory_min_pages = min;
	}
	Ok(())
}

fn parse_exports(r: &mut Reader, m: &mut Module) -> Result<(), ParseError> {
	let count: u32 = r.u32()?;
	for _ in 0..count {
		let name: String = r.name()?;
		let kind_byte: u8 = r.byte()?;
		let index: u32 = r.u32()?;
		let kind: ExportKind = match kind_byte {
			0x00 => ExportKind::Func,
			0x02 => ExportKind::Memory,
			_ => ExportKind::Other,
		};
		m.exports.push(Export { name, kind, index });
	}
	Ok(())
}

fn parse_code(r: &mut Reader, m: &mut Module) -> Result<(), ParseError> {
	let count: usize = r.u32()? as usize;
	for i in 0..count {
		let body_size: usize = r.u32()? as usize;
		let body_end: usize = r.pos + body_size;
		if body_end > r.buf.len() {
			return Err(ParseError("function body runs past end of module"));
		}
		// local declarations: groups of (count, value type)
		let groups: u32 = r.u32()?;
		let mut locals: Vec<ValType> = Vec::new();
		for _ in 0..groups {
			let n: u32 = r.u32()?;
			let t: ValType = val_type(r.byte()?)?;
			for _ in 0..n {
				locals.push(t);
			}
		}
		let body: Vec<u8> = r.bytes(body_end - r.pos)?.to_vec();
		let func: &mut Func = m.funcs.get_mut(i).ok_or(ParseError("code entry without a function"))?;
		func.locals = locals;
		func.body = body;
		if r.pos != body_end {
			return Err(ParseError("function body size mismatch"));
		}
	}
	Ok(())
}

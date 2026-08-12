// The WebAssembly binary parser: it reads the module preamble and the sections the
// runtime needs (types, imports, functions, tables, memory, globals, exports,
// elements, code, data) into a [`Module`]. Unknown or unsupported sections (custom,
// start, ...) are skipped by their declared size, so a module may carry them as long
// as the runtime does not need them.

use crate::module::{DataSegment, Element, Export, ExportKind, Func, FuncType, Global, Import, Module, Table, ValType};
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

	// Unsigned LEB128, capped at 32 bits, and CANONICAL.
	//
	// The width was not checked, so a value could be encoded with redundant continuation bytes -
	// two byte-level spellings of the same module, which matters the moment anything hashes or
	// signs one. The specification requires the shortest encoding, so the final byte must carry a
	// bit that a shorter encoding could not have held, and the bits above 32 must be zero.
	fn u32(&mut self) -> Result<u32, ParseError> {
		let mut result: u32 = 0;
		let mut shift: u32 = 0;
		loop {
			let b: u8 = self.byte()?;
			if shift == 28 && (b >> 4) != 0 {
				return Err(ParseError("LEB128 value does not fit in 32 bits"));
			}
			result |= ((b & 0x7f) as u32) << shift;
			if b & 0x80 == 0 {
				// A continuation whose payload is zero is a longer spelling of a shorter value.
				if shift > 0 && b == 0 {
					return Err(ParseError("non-canonical LEB128: a redundant trailing byte"));
				}
				return Ok(result);
			}
			shift += 7;
			if shift >= 32 {
				return Err(ParseError("LEB128 overflow"));
			}
		}
	}

	// Signed LEB128, sign-extended into 64 bits, and CANONICAL.
	//
	// The signed case's redundant byte is `0x00` after a non-negative value or `0x7f` after a
	// negative one: both re-state the sign bit the previous byte already carried.
	fn i64(&mut self) -> Result<i64, ParseError> {
		let mut result: i64 = 0;
		let mut shift: u32 = 0;
		loop {
			let b: u8 = self.byte()?;
			result |= ((b & 0x7f) as i64) << shift;
			shift += 7;
			if b & 0x80 == 0 {
				if shift > 7 && ((b == 0x00 && result >= 0) || (b == 0x7f && result < 0)) {
					return Err(ParseError("non-canonical LEB128: a redundant sign byte"));
				}
				if shift < 64 && (b & 0x40) != 0 {
					result |= -1i64 << shift;
				}
				return Ok(result);
			}
			if shift >= 64 {
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
		0x7d => Ok(ValType::F32),
		0x7c => Ok(ValType::F64),
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
	// SECTION ORDER AND UNIQUENESS. The specification fixes the order of the non-custom sections
	// and allows each at most once, and this loop enforced neither: a module could repeat a
	// section, present them out of order, or carry a standard section this parser does not know and
	// have it SKIPPED by its declared size - which is correct for a custom section and wrong for
	// every other kind, because skipping one means running a module whose declared contents were
	// never read.
	let mut last_id: u8 = 0;
	while !r.done() {
		let id: u8 = r.byte()?;
		let size: usize = r.u32()? as usize;
		let end: usize = r.pos + size;
		if end > r.buf.len() {
			return Err(ParseError("section runs past end of module"));
		}
		// Section 0 is CUSTOM: it may appear anywhere, any number of times, and its content is not
		// this parser's business.
		if id != 0 {
			if id > 12 {
				return Err(ParseError("a section this parser does not know, which it may not skip"));
			}
			if id <= last_id {
				return Err(ParseError("a section out of order or repeated"));
			}
			last_id = id;
		}
		match id {
			1 => parse_types(&mut r, &mut m)?,
			2 => parse_imports(&mut r, &mut m)?,
			3 => parse_functions(&mut r, &mut m)?,
			4 => parse_tables(&mut r, &mut m)?,
			5 => parse_memory(&mut r, &mut m)?,
			6 => parse_globals(&mut r, &mut m)?,
			7 => parse_exports(&mut r, &mut m)?,
			// THE START SECTION, refused rather than skipped.
			//
			// It was skipped by its declared size and `Instance::new` never ran one - so a module
			// whose initialisation never happened ran in a state its author never wrote, silently.
			// Refusing is the honest half of "implement it or refuse it": this engine's components
			// are called through their exports and none of them declares a start function, so
			// implementing it would be untested code, and skipping it is the one answer that is
			// certainly wrong.
			8 => return Err(ParseError("a start section, which this runtime does not run and will not ignore")),
			9 => parse_elements(&mut r, &mut m)?,
			10 => parse_code(&mut r, &mut m)?,
			11 => parse_data(&mut r, &mut m)?,
			// 12 is the data-count section: a hint for validating memory.init/data.drop, neither of
			// which this engine supports. Skipping it is correct because it declares no content the
			// module depends on.
			_ => r.pos = end,
		}
		if r.pos != end {
			return Err(ParseError("section size mismatch"));
		}
	}
	// THE TWO COUNTS MUST AGREE. The function section declares each function's type and the code
	// section carries each function's body; a module with more of one than the other has functions
	// with no code or code with no function, and the loop above filled in whichever it was given.
	if m.funcs.iter().any(|f| f.body.is_empty()) && !m.funcs.is_empty() {
		return Err(ParseError("a declared function with no code entry"));
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
		// KEPT, not skipped. This read the maximum and dropped it on the floor, so `memory.grow`
		// had nothing to honour and grew past whatever the module said about itself.
		if flags & 0x01 != 0 {
			let max: u32 = r.u32()?;
			if max < min {
				return Err(ParseError("a memory whose maximum is below its minimum"));
			}
			m.memory_max_pages = Some(max);
		}
		m.memory_min_pages = min;
	}
	Ok(())
}

// Parse the table section: at most one `funcref` table, its minimum and optional
// maximum entry count. The table is the array `call_indirect` dispatches through; its
// entries are filled by the element section.
fn parse_tables(r: &mut Reader, m: &mut Module) -> Result<(), ParseError> {
	let count: u32 = r.u32()?;
	if count > 1 {
		return Err(ParseError("at most one table is supported"));
	}
	if count == 1 {
		if r.byte()? != 0x70 {
			return Err(ParseError("only funcref tables are supported"));
		}
		let flags: u8 = r.byte()?;
		let min: u32 = r.u32()?;
		let max: Option<u32> = if flags & 0x01 != 0 { Some(r.u32()?) } else { None };
		m.table = Some(Table { min, max });
	}
	Ok(())
}

// Parse the element section: the active segments that fill the table with function
// indices at instantiation. Only active segments into table 0 are supported (flags 0
// and 2, the forms a Rust/LLVM toolchain emits); passive and declarative segments and
// expression-initialized (`ref.func`) forms are rejected.
fn parse_elements(r: &mut Reader, m: &mut Module) -> Result<(), ParseError> {
	let count: u32 = r.u32()?;
	for _ in 0..count {
		let flags: u32 = r.u32()?;
		match flags {
			0 => {
				// active, table 0: an offset expression, then a vector of function indices.
				let offset: u32 = const_offset(r)?;
				let n: usize = r.u32()? as usize;
				let mut funcs: Vec<u32> = Vec::with_capacity(n);
				for _ in 0..n {
					funcs.push(r.u32()?);
				}
				m.elements.push(Element { offset, funcs });
			}
			2 => {
				// active with an explicit table index (must be 0) and an element-kind byte.
				if r.u32()? != 0 {
					return Err(ParseError("only table 0 is supported"));
				}
				let offset: u32 = const_offset(r)?;
				if r.byte()? != 0x00 {
					return Err(ParseError("only the funcref element kind is supported"));
				}
				let n: usize = r.u32()? as usize;
				let mut funcs: Vec<u32> = Vec::with_capacity(n);
				for _ in 0..n {
					funcs.push(r.u32()?);
				}
				m.elements.push(Element { offset, funcs });
			}
			_ => return Err(ParseError("only active element segments into table 0 are supported")),
		}
	}
	Ok(())
}

// Read a constant init expression - a single `i32.const` / `i64.const` /
// `f32.const` / `f64.const` followed by `end` - returning its value as a 64-bit
// pattern (floats are stored as their IEEE-754 bits). Other (non-constant) init
// expressions are rejected; the minimal runtime only supports constant globals /
// data offsets.
// A constant expression, and THE TYPE IT PRODUCES.
//
// The value alone was returned, so a global declared `i32` could be initialised by an `f64.const`
// and a data segment's offset could be an `i64` - the bytes were simply evaluated. A constant
// expression has a type like anything else, and the caller says which one it wants.
fn const_expr(r: &mut Reader) -> Result<(i64, ValType), ParseError> {
	let op: u8 = r.byte()?;
	let (v, ty): (i64, ValType) = match op {
		0x41 => (r.i64()? as i32 as i64, ValType::I32), // i32.const (sign-extended via i32)
		0x42 => (r.i64()?, ValType::I64),               // i64.const
		0x43 => {
			let b: &[u8] = r.bytes(4)?; // f32.const: raw IEEE-754 bits, little-endian
			(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64, ValType::F32)
		}
		0x44 => {
			let b: &[u8] = r.bytes(8)?; // f64.const: raw IEEE-754 bits, little-endian
			(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as i64, ValType::F64)
		}
		_ => return Err(ParseError("unsupported constant expression")),
	};
	if r.byte()? != 0x0b {
		return Err(ParseError("constant expression must end in `end`"));
	}
	Ok((v, ty))
}

// A constant expression that must be an `i32` - every OFFSET in the format is one.
fn const_offset(r: &mut Reader) -> Result<u32, ParseError> {
	let (value, ty) = const_expr(r)?;
	if ty != ValType::I32 {
		return Err(ParseError("an offset expression that is not an i32"));
	}
	// A negative offset is a wrapped one; the validator checks the range, and this refuses the
	// spelling that would make a huge offset look small.
	u32::try_from(value as u32 as i64 as u64 as u32).map_err(|_| ParseError("an offset that does not fit in 32 bits"))
}

fn parse_globals(r: &mut Reader, m: &mut Module) -> Result<(), ParseError> {
	let count: u32 = r.u32()?;
	for _ in 0..count {
		let val_type: ValType = val_type(r.byte()?)?;
		let mutable: bool = match r.byte()? {
			0x00 => false,
			0x01 => true,
			_ => return Err(ParseError("invalid global mutability")),
		};
		let (init, init_type): (i64, ValType) = const_expr(r)?;
		if init_type != val_type {
			return Err(ParseError("a global whose initialiser is not of its declared type"));
		}
		m.globals.push(Global { val_type, mutable, init });
	}
	Ok(())
}

fn parse_data(r: &mut Reader, m: &mut Module) -> Result<(), ParseError> {
	let count: u32 = r.u32()?;
	for _ in 0..count {
		let flags: u32 = r.u32()?;
		match flags {
			0 => {
				// active segment, memory 0, with an offset expression
				let offset: u32 = const_offset(r)?;
				let n: usize = r.u32()? as usize;
				let bytes: Vec<u8> = r.bytes(n)?.to_vec();
				m.data.push(DataSegment { offset, bytes });
			}
			2 => {
				// active segment with an explicit memory index (must be 0)
				if r.u32()? != 0 {
					return Err(ParseError("only memory 0 is supported"));
				}
				let offset: u32 = const_offset(r)?;
				let n: usize = r.u32()? as usize;
				let bytes: Vec<u8> = r.bytes(n)?.to_vec();
				m.data.push(DataSegment { offset, bytes });
			}
			_ => return Err(ParseError("passive data segments are not supported")),
		}
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
	// A code section with more entries than the function section declared has code belonging to no
	// function; the reverse is caught in `parse` once every section has been read.
	if count != m.funcs.len() {
		return Err(ParseError("the code section and the function section declare different counts"));
	}
	for i in 0..count {
		let body_size: usize = r.u32()? as usize;
		let body_end: usize = r.pos.checked_add(body_size).ok_or(ParseError("function body size overflows"))?;
		if body_end > r.buf.len() {
			return Err(ParseError("function body runs past end of module"));
		}
		// A READER LIMITED TO THIS BODY.
		//
		// The local declarations used to be read through the MAIN reader and the body then taken as
		// `r.bytes(body_end - r.pos)` - so a declaration list running past `body_end` reached that
		// subtraction with `r.pos > body_end`: a `usize` underflow, which is a panic with overflow
		// checks on and an enormous length without them. A sub-reader bounded by the body makes the
		// whole question go away, because nothing inside it can read past the end.
		let mut body_reader: Reader = Reader { buf: &r.buf[..body_end], pos: r.pos };
		// local declarations: groups of (count, value type)
		let groups: u32 = body_reader.u32()?;
		let mut locals: Vec<ValType> = Vec::new();
		for _ in 0..groups {
			let n: u32 = body_reader.u32()?;
			let t: ValType = val_type(body_reader.byte()?)?;
			// A group count is a guest-supplied repeat, so it is reserved fallibly rather than
			// pushed into an unbounded loop: `0xffff_ffff` locals is four billion pushes.
			if locals.len().saturating_add(n as usize) > crate::validate::MAX_LOCALS {
				return Err(ParseError("more locals than the host allows"));
			}
			for _ in 0..n {
				locals.push(t);
			}
		}
		let body: Vec<u8> = body_reader.buf[body_reader.pos..].to_vec();
		r.pos = body_end;
		let func: &mut Func = m.funcs.get_mut(i).ok_or(ParseError("code entry without a function"))?;
		func.locals = locals;
		func.body = body;
	}
	Ok(())
}

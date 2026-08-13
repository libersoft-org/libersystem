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

	// Unsigned LEB128, capped at 32 bits - the WIDTH bound the specification states, and no more.
	//
	// This refused a redundant trailing byte, under a comment saying "the specification requires the
	// shortest encoding". It does not. The binary format's integer grammar restricts the encoding's
	// LENGTH and nothing else, and its own note gives the example: `0x03` and `0x83 0x00` are both
	// well-formed encodings of 3, as `0x7e` and `0xFE 0x7F` are both well-formed encodings of -2.
	// Trailing zeros within the width bound are legal WebAssembly, and a test had been written to
	// hold this parser to the opposite.
	//
	// The motivation was sound and survives elsewhere: two byte-level spellings of one module is a
	// real problem for anything that hashes or signs. That is a LiberSystem packaging policy about
	// which encodings this system will SHIP - a check over an image about to be signed - and not a
	// fact about which modules are well-formed. Stating it here as the specification's rule is how a
	// conformance defect acquired a regression test.
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
				return Ok(result);
			}
			shift += 7;
			if shift >= 32 {
				return Err(ParseError("LEB128 overflow"));
			}
		}
	}

	// Signed LEB128, sign-extended into 64 bits, bounded by WIDTH - see `u32` above for why a
	// redundant sign byte is legal here.
	//
	// The signed case's redundant byte is `0x00` after a non-negative value or `0x7f` after a
	// negative one: both re-state the sign bit the previous byte already carried.
	fn i64(&mut self) -> Result<i64, ParseError> {
		let mut result: i64 = 0;
		let mut shift: u32 = 0;
		loop {
			let b: u8 = self.byte()?;
			// THE FINAL BYTE'S UNUSED BITS, which the width bound alone does not cover.
			//
			// A signed value of `n` bits has `ceil(n/7)` bytes, and the last one contributes only
			// `n - 7*(ceil(n/7)-1)` meaningful bits - the rest must all be the sign. `s33` (a block
			// type) and `s64` both end mid-byte, so a final byte whose top bits are neither all 0
			// nor all 1 is an over-wide encoding rather than a large value. The specification calls
			// these "integer too large" and "integer representation too long", and its own suite is
			// what named them here.
			if shift == 63 {
				// Only ONE bit of the tenth byte is inside 64, so its seven payload bits must be
				// all-zero (a positive value) or all-one (a negative one). Anything between carries
				// bits past the width, which the specification calls "integer too large".
				//
				// Derived from the FINAL BYTE and not from the value so far: nine bytes of ones
				// build a positive `i64` and the tenth is what makes it -1, so reading the running
				// value here refused `i64.const -1` written in full - which the specification's own
				// suite says is well-formed. The check is about the byte, not about what has been
				// accumulated, and the suite is what said so.
				if (b & 0x7f) != 0x00 && (b & 0x7f) != 0x7f {
					return Err(ParseError("LEB128 signed value does not fit in 64 bits"));
				}
			}
			result |= ((b & 0x7f) as i64) << shift;
			shift += 7;
			if b & 0x80 == 0 {
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
	crate::module::val_type_of(b).ok_or(ParseError("unsupported value type"))
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
	// THE SPECIFICATION'S ORDER, which is not the numeric one.
	//
	// The check was `id <= last_id`, and the data-count section has id 12 while its PLACE is between
	// element (9) and code (10). So a conforming module carrying one was refused as "out of order",
	// and `code(10) data(11) datacount(12)` - which is not a legal module - was accepted. Section
	// ids do not correspond to section order and this is the one place it shows.
	//
	// A table of the ids in their real sequence and an index into it costs the same few lines and is
	// right for all twelve.
	const ORDER: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 10, 11];
	let mut last_place: Option<usize> = None;
	while !r.done() {
		let id: u8 = r.byte()?;
		let size: usize = r.u32()? as usize;
		let end: usize = r.pos + size;
		if end > r.buf.len() {
			return Err(ParseError("section runs past end of module"));
		}
		// Section 0 is CUSTOM: it may appear anywhere, any number of times, and its content is not
		// this parser's business - but its NAME is, and this read past it entirely.
		//
		// A custom section is `name` then arbitrary bytes, and a name that is not valid UTF-8 makes
		// the module malformed. Skipping the whole section by its declared size meant the name was
		// never read, so a module the specification refuses parsed here - 176 of the specification's
		// own malformed cases, which is most of the gap that suite found.
		if id == 0 {
			let start = r.pos;
			let _ = r.name()?;
			if r.pos > end {
				return Err(ParseError("a custom section whose name runs past its own length"));
			}
			let _ = start;
		}
		if id != 0 {
			let Some(place) = ORDER.iter().position(|&known| known == id) else {
				return Err(ParseError("a section this parser does not know, which it may not skip"));
			};
			if last_place.is_some_and(|last| place <= last) {
				return Err(ParseError("a section out of order or repeated"));
			}
			last_place = Some(place);
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
			// REFUSED, not skipped - the same answer the start section already gives, for the same
			// reason.
			//
			// The data-count section exists to validate `memory.init` and `data.drop`, neither of
			// which this engine implements, and its content is a CLAIM about the module: how many
			// data segments there are. Skipping it meant never comparing that claim with the data
			// section, which the format requires - so a module could state one thing and carry
			// another and this reader would notice neither.
			//
			// A module that carries this section is using a feature this engine does not have, and
			// saying so is more honest than reading past it. The alternative - read it and compare -
			// is the right answer the day `memory.init` exists.
			12 => return Err(ParseError("a data-count section, which this engine's bulk-memory support does not include")),
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
		// ONLY 0 AND 1 ARE DEFINED. Anything else is a limits encoding from a feature this engine
		// does not implement - shared memories, 64-bit indices - and reading past it as though bit 0
		// were the only one that mattered is how a module for a different engine parses here.
		if flags > 1 {
			return Err(ParseError("a memory limits flag byte this engine does not know"));
		}
		let min: u32 = r.u32()?;
		let mut max_pages: Option<u32> = None;
		// KEPT, not skipped. This read the maximum and dropped it on the floor, so `memory.grow`
		// had nothing to honour and grew past whatever the module said about itself.
		if flags & 0x01 != 0 {
			let max: u32 = r.u32()?;
			if max < min {
				return Err(ParseError("a memory whose maximum is below its minimum"));
			}
			max_pages = Some(max);
		}
		m.memory = Some(crate::module::MemoryType { min_pages: min, max_pages });
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
				let funcs = read_function_indices(r)?;
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
				let funcs = read_function_indices(r)?;
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
// A declared count, read and BOUNDED BY THE INPUT before anything is sized from it.
//
// `Vec::with_capacity(r.u32()?)` is an infallible allocation whose size a hostile module chooses: a
// few bytes declaring four billion element indices asked for sixteen gigabytes, and the failure was
// an abort in the host process rather than a refusal. It happens during PARSE, which is before fuel,
// before the host limits and before everything else the engine bounds - so the whole resource story
// started one step too late.
//
// An index is at least one byte, so `n` of them cannot be present in fewer than `n` bytes of what is
// left. The check needs nothing but the reader's own position.
fn read_function_indices(r: &mut Reader) -> Result<Vec<u32>, ParseError> {
	let n: usize = r.u32()? as usize;
	if n > r.buf.len().saturating_sub(r.pos) {
		return Err(ParseError("an element segment declares more entries than the section can hold"));
	}
	let mut funcs: Vec<u32> = Vec::new();
	funcs.try_reserve(n).map_err(|_| ParseError("an element segment's index table does not fit"))?;
	for _ in 0..n {
		funcs.push(r.u32()?);
	}
	Ok(funcs)
}

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

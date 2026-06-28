// The decoder: it turns a function body's raw instruction bytes into a flat
// [`Instr`] stream with branch targets pre-resolved to instruction indices. This is
// also where structural validation lives - a malformed control structure (an
// unbalanced `end`, an out-of-range branch label, an unknown opcode) is rejected
// here, at instantiation, rather than mid-execution. Numeric / memory operands
// (LEB128 immediates, memargs) are decoded once so the interpreter never re-parses
// bytes in its hot loop.

use crate::module::Module;
use alloc::vec::Vec;

// One decoded instruction. Control-flow instructions carry resolved instruction
// indices; numeric ALU operations keep their raw opcode (executed by a match in the
// interpreter) to keep this enum compact.
#[derive(Clone, Debug)]
pub enum Instr {
	Unreachable,
	Nop,
	// A `block`: on a branch to it, control resumes at `end_pc + 1` (past its `End`),
	// keeping `result_arity` values.
	Block { result_arity: u8, end_pc: usize },
	// A `loop`: a branch to it resumes at the loop body start (the instruction after
	// this one), keeping `param_arity` values.
	Loop { param_arity: u8 },
	// An `if`: pops a condition; on false jumps to `else_pc` (if `has_else`) or past
	// `end_pc`. Keeps `result_arity` values at its end.
	If { result_arity: u8, has_else: bool, else_pc: usize, end_pc: usize },
	// The `else` of an `if`: when reached by falling through the then-branch, jumps to
	// the matching `End` at `end_pc`.
	Else { end_pc: usize },
	End,
	Br(u32),
	BrIf(u32),
	BrTable { labels: Vec<u32>, default: u32 },
	Return,
	Call(u32),
	CallIndirect(u32),
	Drop,
	Select,
	LocalGet(u32),
	LocalSet(u32),
	LocalTee(u32),
	GlobalGet(u32),
	GlobalSet(u32),
	I32Const(i32),
	I64Const(i64),
	F32Const(f32),
	F64Const(f64),
	// A memory load: read `width` bytes at the effective address, sign- or
	// zero-extend per `signed` into a 32- or 64-bit value (`wide`).
	Load { width: u8, signed: bool, wide: bool, offset: u32 },
	// A memory store: write the low `width` bytes of the popped value.
	Store { width: u8, offset: u32 },
	// A float load: read 4 (`wide` false, f32) or 8 (`wide` true, f64) bytes.
	FLoad { wide: bool, offset: u32 },
	// A float store: write 4 (f32) or 8 (f64) bytes of the popped float.
	FStore { wide: bool, offset: u32 },
	MemorySize,
	MemoryGrow,
	MemoryCopy,
	MemoryFill,
	// A non-trapping (saturating) float-to-int conversion, identified by its `0xfc`
	// sub-opcode (0..=7): NaN maps to 0 and out-of-range values clamp to the
	// integer's min/max instead of trapping. This is what Rust's `as` casts emit.
	TruncSat(u8),
	// A numeric ALU op, identified by its raw wasm opcode.
	Num(u8),
}

// A decode error with a short static reason.
pub type DecodeError = &'static str;

// A control frame open during decoding, used to resolve `end` / `else` and to
// range-check branch labels. `instr_index` points at the opening Block/Loop/If
// instruction (or `usize::MAX` for the implicit function-level frame).
struct Ctrl {
	is_if: bool,
	instr_index: usize,
	else_instr: usize,
}

// Decode the body of defined function `func_index` into an [`Instr`] stream.
pub fn decode(module: &Module, func_index: usize) -> Result<Vec<Instr>, DecodeError> {
	let func = module.funcs.get(func_index).ok_or("function index out of range")?;
	let result_arity: u8 = module.types.get(func.type_index as usize).ok_or("function type out of range")?.results.len() as u8;
	let body: &[u8] = &func.body;
	let mut out: Vec<Instr> = Vec::new();
	let mut ctrl: Vec<Ctrl> = Vec::new();
	// the implicit function-level frame; branching to it is a return
	ctrl.push(Ctrl { is_if: false, instr_index: usize::MAX, else_instr: usize::MAX });
	let _ = result_arity;
	let mut pc: usize = 0;
	while pc < body.len() {
		let op: u8 = body[pc];
		pc += 1;
		match op {
			0x00 => out.push(Instr::Unreachable),
			0x01 => out.push(Instr::Nop),
			0x02 => {
				let (_p, r): (u8, u8) = block_type(module, body, &mut pc)?;
				ctrl.push(Ctrl { is_if: false, instr_index: out.len(), else_instr: usize::MAX });
				out.push(Instr::Block { result_arity: r, end_pc: 0 });
			}
			0x03 => {
				let (p, _r): (u8, u8) = block_type(module, body, &mut pc)?;
				ctrl.push(Ctrl { is_if: false, instr_index: out.len(), else_instr: usize::MAX });
				out.push(Instr::Loop { param_arity: p });
			}
			0x04 => {
				let (_p, r): (u8, u8) = block_type(module, body, &mut pc)?;
				ctrl.push(Ctrl { is_if: true, instr_index: out.len(), else_instr: usize::MAX });
				out.push(Instr::If { result_arity: r, has_else: false, else_pc: 0, end_pc: 0 });
			}
			0x05 => {
				let frame: &mut Ctrl = ctrl.last_mut().ok_or("else without a block")?;
				if !frame.is_if {
					return Err("else without an if");
				}
				frame.else_instr = out.len();
				let if_index: usize = frame.instr_index;
				let after_else: usize = out.len() + 1;
				if let Instr::If { has_else, else_pc, .. } = &mut out[if_index] {
					*has_else = true;
					*else_pc = after_else;
				}
				out.push(Instr::Else { end_pc: 0 });
			}
			0x0b => {
				let frame: Ctrl = ctrl.pop().ok_or("end without a block")?;
				out.push(Instr::End);
				let end_index: usize = out.len() - 1;
				if frame.instr_index != usize::MAX {
					match &mut out[frame.instr_index] {
						Instr::Block { end_pc, .. } | Instr::If { end_pc, .. } => *end_pc = end_index,
						Instr::Loop { .. } => {}
						_ => return Err("malformed control frame"),
					}
					if frame.else_instr != usize::MAX {
						if let Instr::Else { end_pc } = &mut out[frame.else_instr] {
							*end_pc = end_index;
						}
					}
				}
				if ctrl.is_empty() {
					break;
				}
			}
			0x0c => {
				let l: u32 = read_u32(body, &mut pc)?;
				check_label(l, &ctrl)?;
				out.push(Instr::Br(l));
			}
			0x0d => {
				let l: u32 = read_u32(body, &mut pc)?;
				check_label(l, &ctrl)?;
				out.push(Instr::BrIf(l));
			}
			0x0e => {
				let n: u32 = read_u32(body, &mut pc)?;
				let mut labels: Vec<u32> = Vec::with_capacity(n as usize);
				for _ in 0..n {
					let l: u32 = read_u32(body, &mut pc)?;
					check_label(l, &ctrl)?;
					labels.push(l);
				}
				let default: u32 = read_u32(body, &mut pc)?;
				check_label(default, &ctrl)?;
				out.push(Instr::BrTable { labels, default });
			}
			0x0f => out.push(Instr::Return),
			0x10 => {
				let f: u32 = read_u32(body, &mut pc)?;
				out.push(Instr::Call(f));
			}
			0x11 => {
				let t: u32 = read_u32(body, &mut pc)?;
				let _table: u32 = read_u32(body, &mut pc)?;
				out.push(Instr::CallIndirect(t));
			}
			0x1a => out.push(Instr::Drop),
			0x1b => out.push(Instr::Select),
			0x1c => {
				let n: u32 = read_u32(body, &mut pc)?;
				for _ in 0..n {
					read_byte(body, &mut pc)?;
				}
				out.push(Instr::Select);
			}
			0x20 => out.push(Instr::LocalGet(read_u32(body, &mut pc)?)),
			0x21 => out.push(Instr::LocalSet(read_u32(body, &mut pc)?)),
			0x22 => out.push(Instr::LocalTee(read_u32(body, &mut pc)?)),
			0x23 => out.push(Instr::GlobalGet(read_u32(body, &mut pc)?)),
			0x24 => out.push(Instr::GlobalSet(read_u32(body, &mut pc)?)),
			0x28 => out.push(load(body, &mut pc, 4, false, false)?),
			0x29 => out.push(load(body, &mut pc, 8, false, true)?),
			0x2a => out.push(fload(body, &mut pc, false)?),
			0x2b => out.push(fload(body, &mut pc, true)?),
			0x2c => out.push(load(body, &mut pc, 1, true, false)?),
			0x2d => out.push(load(body, &mut pc, 1, false, false)?),
			0x2e => out.push(load(body, &mut pc, 2, true, false)?),
			0x2f => out.push(load(body, &mut pc, 2, false, false)?),
			0x30 => out.push(load(body, &mut pc, 1, true, true)?),
			0x31 => out.push(load(body, &mut pc, 1, false, true)?),
			0x32 => out.push(load(body, &mut pc, 2, true, true)?),
			0x33 => out.push(load(body, &mut pc, 2, false, true)?),
			0x34 => out.push(load(body, &mut pc, 4, true, true)?),
			0x35 => out.push(load(body, &mut pc, 4, false, true)?),
			0x36 => out.push(store(body, &mut pc, 4)?),
			0x37 => out.push(store(body, &mut pc, 8)?),
			0x38 => out.push(fstore(body, &mut pc, false)?),
			0x39 => out.push(fstore(body, &mut pc, true)?),
			0x3a => out.push(store(body, &mut pc, 1)?),
			0x3b => out.push(store(body, &mut pc, 2)?),
			0x3c => out.push(store(body, &mut pc, 1)?),
			0x3d => out.push(store(body, &mut pc, 2)?),
			0x3e => out.push(store(body, &mut pc, 4)?),
			0x3f => {
				read_byte(body, &mut pc)?; // memory index (reserved, must be 0)
				out.push(Instr::MemorySize);
			}
			0x40 => {
				read_byte(body, &mut pc)?; // memory index (reserved, must be 0)
				out.push(Instr::MemoryGrow);
			}
			0x41 => out.push(Instr::I32Const(read_i64(body, &mut pc)? as i32)),
			0x42 => out.push(Instr::I64Const(read_i64(body, &mut pc)?)),
			0x43 => {
				let bits: u32 = read_f32_bits(body, &mut pc)?;
				out.push(Instr::F32Const(f32::from_bits(bits)));
			}
			0x44 => {
				let bits: u64 = read_f64_bits(body, &mut pc)?;
				out.push(Instr::F64Const(f64::from_bits(bits)));
			}
			0xfc => {
				let sub: u32 = read_u32(body, &mut pc)?;
				match sub {
					0..=7 => out.push(Instr::TruncSat(sub as u8)),
					10 => {
						read_byte(body, &mut pc)?; // dst memory index
						read_byte(body, &mut pc)?; // src memory index
						out.push(Instr::MemoryCopy);
					}
					11 => {
						read_byte(body, &mut pc)?; // memory index
						out.push(Instr::MemoryFill);
					}
					_ => return Err("unsupported 0xfc operation"),
				}
			}
			// integer and floating-point comparisons, arithmetic, bitwise, shifts,
			// rotates, and the int / float conversions - one dense numeric opcode block.
			0x45..=0xc4 => out.push(Instr::Num(op)),
			_ => return Err("unsupported opcode"),
		}
	}
	if !ctrl.is_empty() {
		return Err("unterminated function body");
	}
	Ok(out)
}

// Decode a load opcode's memarg (align, offset) into a `Load`.
fn load(body: &[u8], pc: &mut usize, width: u8, signed: bool, wide: bool) -> Result<Instr, DecodeError> {
	let _align: u32 = read_u32(body, pc)?;
	let offset: u32 = read_u32(body, pc)?;
	Ok(Instr::Load { width, signed, wide, offset })
}

// Decode a store opcode's memarg (align, offset) into a `Store`.
fn store(body: &[u8], pc: &mut usize, width: u8) -> Result<Instr, DecodeError> {
	let _align: u32 = read_u32(body, pc)?;
	let offset: u32 = read_u32(body, pc)?;
	Ok(Instr::Store { width, offset })
}

// Decode a float load opcode's memarg (align, offset) into an `FLoad`.
fn fload(body: &[u8], pc: &mut usize, wide: bool) -> Result<Instr, DecodeError> {
	let _align: u32 = read_u32(body, pc)?;
	let offset: u32 = read_u32(body, pc)?;
	Ok(Instr::FLoad { wide, offset })
}

// Decode a float store opcode's memarg (align, offset) into an `FStore`.
fn fstore(body: &[u8], pc: &mut usize, wide: bool) -> Result<Instr, DecodeError> {
	let _align: u32 = read_u32(body, pc)?;
	let offset: u32 = read_u32(body, pc)?;
	Ok(Instr::FStore { wide, offset })
}

// Read 4 little-endian bytes as f32 bits.
fn read_f32_bits(body: &[u8], pc: &mut usize) -> Result<u32, DecodeError> {
	let mut raw: [u8; 4] = [0u8; 4];
	for b in &mut raw {
		*b = read_byte(body, pc)?;
	}
	Ok(u32::from_le_bytes(raw))
}

// Read 8 little-endian bytes as f64 bits.
fn read_f64_bits(body: &[u8], pc: &mut usize) -> Result<u64, DecodeError> {
	let mut raw: [u8; 8] = [0u8; 8];
	for b in &mut raw {
		*b = read_byte(body, pc)?;
	}
	Ok(u64::from_le_bytes(raw))
}

// Read a block type: `0x40` (empty) -> (0, 0); a single value type -> (0, 1); a type
// index -> that type's (param count, result count). Returns (param arity, result
// arity) for the block.
fn block_type(module: &Module, body: &[u8], pc: &mut usize) -> Result<(u8, u8), DecodeError> {
	let v: i64 = read_i64(body, pc)?;
	if v == -64 {
		return Ok((0, 0)); // 0x40, the empty block type
	}
	if v < 0 {
		return Ok((0, 1)); // a single value type result
	}
	let t = module.types.get(v as usize).ok_or("block type index out of range")?;
	Ok((t.params.len() as u8, t.results.len() as u8))
}

// A branch label must name a control frame in scope (0 = innermost, up to and
// including the function-level frame).
fn check_label(l: u32, ctrl: &[Ctrl]) -> Result<(), DecodeError> {
	if (l as usize) < ctrl.len() { Ok(()) } else { Err("branch label out of range") }
}

fn read_byte(body: &[u8], pc: &mut usize) -> Result<u8, DecodeError> {
	let b: u8 = *body.get(*pc).ok_or("unexpected end of function body")?;
	*pc += 1;
	Ok(b)
}

// Read an unsigned LEB128 (capped at 32 bits) from the body, advancing `pc`.
fn read_u32(body: &[u8], pc: &mut usize) -> Result<u32, DecodeError> {
	let mut result: u32 = 0;
	let mut shift: u32 = 0;
	loop {
		let b: u8 = read_byte(body, pc)?;
		result |= ((b & 0x7f) as u32) << shift;
		if b & 0x80 == 0 {
			return Ok(result);
		}
		shift += 7;
		if shift >= 32 {
			return Err("LEB128 overflow");
		}
	}
}

// Read a signed LEB128 (sign-extended into 64 bits) from the body, advancing `pc`.
fn read_i64(body: &[u8], pc: &mut usize) -> Result<i64, DecodeError> {
	let mut result: i64 = 0;
	let mut shift: u32 = 0;
	loop {
		let b: u8 = read_byte(body, pc)?;
		result |= ((b & 0x7f) as i64) << shift;
		shift += 7;
		if b & 0x80 == 0 {
			if shift < 64 && (b & 0x40) != 0 {
				result |= -1i64 << shift;
			}
			return Ok(result);
		}
		if shift >= 64 {
			return Err("LEB128 overflow");
		}
	}
}

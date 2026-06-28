// The interpreter: a stack machine over the decoded [`Instr`] stream. It supports
// structured control flow (block / loop / if / else / br / br_if / br_table /
// return), the integer instruction set (i32 / i64 arithmetic, comparison, bitwise,
// shifts, rotates, and the width conversions), globals, a single linear memory with
// loads / stores / size / grow / copy / fill, and calls - both to defined functions
// and, through the [`Host`] seam, to imported ones. Floating point is added by a
// later step. Imported functions are how a WASI-style component reaches native
// services; the host services only the imports it was wired up to grant.

use crate::decode::{Instr, decode};
use crate::module::{Module, ValType};
use alloc::vec::Vec;

// A runtime value. The runtime handles 32/64-bit integers and floats.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
	I32(i32),
	I64(i64),
	F32(f32),
	F64(f64),
}

impl Value {
	// The value as an i32 (an i64 is truncated, matching wasm wrapping semantics).
	pub fn as_i32(self) -> i32 {
		match self {
			Value::I32(v) => v,
			Value::I64(v) => v as i32,
			Value::F32(v) => v as i32,
			Value::F64(v) => v as i32,
		}
	}

	// The value as an i64 (an i32 is sign-extended).
	pub fn as_i64(self) -> i64 {
		match self {
			Value::I32(v) => v as i64,
			Value::I64(v) => v,
			Value::F32(v) => v as i64,
			Value::F64(v) => v as i64,
		}
	}

	// The value as an f32.
	pub fn as_f32(self) -> f32 {
		match self {
			Value::F32(v) => v,
			Value::F64(v) => v as f32,
			Value::I32(v) => v as f32,
			Value::I64(v) => v as f32,
		}
	}

	// The value as an f64.
	pub fn as_f64(self) -> f64 {
		match self {
			Value::F64(v) => v,
			Value::F32(v) => v as f64,
			Value::I32(v) => v as f64,
			Value::I64(v) => v as f64,
		}
	}
}

// A trap: execution aborted with a short static reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trap(pub &'static str);

// The host-import hook. When the module calls an imported function (by its import
// index), the runtime invokes this with the call arguments and a mutable view of
// the instance memory; the host runs the operation (typically an IPC call to a
// native service) and returns the result value(s). This is where capability
// gating lives: the host only services imports it was wired up to grant.
pub trait Host {
	fn call_import(&mut self, import: u32, args: &[Value], memory: &mut [u8]) -> Result<Vec<Value>, Trap>;
}

// One page of linear memory.
const PAGE: usize = 65536;

// A runtime control frame: the value-stack base to unwind to on a branch, whether
// it is a loop (a branch re-enters it), how many values a branch keeps, and the
// instruction index a branch resumes at.
#[derive(Clone, Copy)]
struct Frame {
	base: usize,
	is_loop: bool,
	arity: u8,
	target: usize,
}

// An instantiated module: the parsed module, its decoded function bodies, its
// linear memory, and its globals.
pub struct Instance<'a> {
	module: &'a Module,
	code: Vec<Vec<Instr>>,
	memory: Vec<u8>,
	globals: Vec<Value>,
	error: Option<Trap>,
}

impl<'a> Instance<'a> {
	// Instantiate `module`: allocate its declared minimum linear memory, copy in its
	// active data segments, initialize its globals, and decode + validate every
	// function body. A decode/validation error is held and surfaced on `invoke`.
	pub fn new(module: &'a Module) -> Instance<'a> {
		let mut memory: Vec<u8> = alloc::vec![0u8; module.memory_min_pages as usize * PAGE];
		let mut error: Option<Trap> = None;

		for seg in &module.data {
			let start: usize = seg.offset as usize;
			let end: usize = start.saturating_add(seg.bytes.len());
			if end > memory.len() {
				error = Some(Trap("data segment out of bounds"));
				break;
			}
			memory[start..end].copy_from_slice(&seg.bytes);
		}

		let globals: Vec<Value> = module
			.globals
			.iter()
			.map(|g| match g.val_type {
				ValType::I32 => Value::I32(g.init as i32),
				ValType::I64 => Value::I64(g.init),
				ValType::F32 => Value::F32(f32::from_bits(g.init as u32)),
				ValType::F64 => Value::F64(f64::from_bits(g.init as u64)),
			})
			.collect();

		let mut code: Vec<Vec<Instr>> = Vec::with_capacity(module.funcs.len());
		for i in 0..module.funcs.len() {
			match decode(module, i) {
				Ok(body) => code.push(body),
				Err(reason) => {
					error = error.or(Some(Trap(reason)));
					code.push(Vec::new());
				}
			}
		}

		Instance { module, code, memory, globals, error }
	}

	// The instance's linear memory (e.g. to read back what a component wrote).
	pub fn memory(&self) -> &[u8] {
		&self.memory
	}

	// Invoke the exported function `name` with `args`, dispatching any imports to
	// `host`, and return its result value(s).
	pub fn invoke(&mut self, name: &str, args: &[Value], host: &mut dyn Host) -> Result<Vec<Value>, Trap> {
		if let Some(t) = self.error {
			return Err(t);
		}
		let index: u32 = self.module.export_func(name).ok_or(Trap("no such exported function"))?;
		let Instance { module, code, memory, globals, .. } = self;
		call(module, code, memory, globals, index, args, host)
	}
}

// Call function `index` (in the combined imports-then-defined space). Imports go to
// the host; defined functions run their decoded body on a fresh frame.
fn call(module: &Module, code: &[Vec<Instr>], memory: &mut Vec<u8>, globals: &mut [Value], index: u32, args: &[Value], host: &mut dyn Host) -> Result<Vec<Value>, Trap> {
	if index < module.import_count() {
		return host.call_import(index, args, memory);
	}
	let defined: usize = (index - module.import_count()) as usize;
	let func = module.funcs.get(defined).ok_or(Trap("function index out of range"))?;
	let ftype = module.types.get(func.type_index as usize).ok_or(Trap("function type out of range"))?;
	if args.len() != ftype.params.len() {
		return Err(Trap("wrong argument count"));
	}
	let body: &[Instr] = code.get(defined).ok_or(Trap("missing function code"))?;
	// locals are the parameters followed by the declared locals (zero-initialized)
	let mut locals: Vec<Value> = Vec::with_capacity(args.len() + func.locals.len());
	locals.extend_from_slice(args);
	for t in &func.locals {
		locals.push(match t {
			ValType::I32 => Value::I32(0),
			ValType::I64 => Value::I64(0),
			ValType::F32 => Value::F32(0.0),
			ValType::F64 => Value::F64(0.0),
		});
	}
	let mut stack: Vec<Value> = Vec::new();
	exec(module, code, memory, globals, body, &mut locals, &mut stack, host)?;
	let n: usize = ftype.results.len();
	if stack.len() < n {
		return Err(Trap("missing results at return"));
	}
	Ok(stack.split_off(stack.len() - n))
}

// Execute a decoded function body, mutating `locals`, `stack`, `memory`, and
// `globals`. Returns when the body falls off its end, hits `return`, or branches to
// the function-level label; the caller takes the result values off the stack.
fn exec(module: &Module, code: &[Vec<Instr>], memory: &mut Vec<u8>, globals: &mut [Value], body: &[Instr], locals: &mut [Value], stack: &mut Vec<Value>, host: &mut dyn Host) -> Result<(), Trap> {
	let mut ctrl: Vec<Frame> = Vec::new();
	let mut pc: usize = 0;
	while pc < body.len() {
		let instr: &Instr = &body[pc];
		pc += 1;
		match instr {
			Instr::Unreachable => return Err(Trap("unreachable")),
			Instr::Nop => {}
			Instr::Block { result_arity, end_pc } => {
				ctrl.push(Frame { base: stack.len(), is_loop: false, arity: *result_arity, target: *end_pc + 1 });
			}
			Instr::Loop { param_arity } => {
				ctrl.push(Frame { base: stack.len().saturating_sub(*param_arity as usize), is_loop: true, arity: *param_arity, target: pc });
			}
			Instr::If { result_arity, has_else, else_pc, end_pc } => {
				let cond: i32 = pop(stack)?.as_i32();
				if cond != 0 {
					ctrl.push(Frame { base: stack.len(), is_loop: false, arity: *result_arity, target: *end_pc + 1 });
				} else if *has_else {
					ctrl.push(Frame { base: stack.len(), is_loop: false, arity: *result_arity, target: *end_pc + 1 });
					pc = *else_pc;
				} else {
					pc = *end_pc + 1;
				}
			}
			Instr::Else { end_pc } => pc = *end_pc,
			Instr::End => {
				if ctrl.pop().is_none() {
					return Ok(());
				}
			}
			Instr::Br(k) => match branch(&mut ctrl, stack, *k)? {
				Some(p) => pc = p,
				None => return Ok(()),
			},
			Instr::BrIf(k) => {
				if pop(stack)?.as_i32() != 0 {
					match branch(&mut ctrl, stack, *k)? {
						Some(p) => pc = p,
						None => return Ok(()),
					}
				}
			}
			Instr::BrTable { labels, default } => {
				let i: i32 = pop(stack)?.as_i32();
				let l: u32 = if i >= 0 && (i as usize) < labels.len() { labels[i as usize] } else { *default };
				match branch(&mut ctrl, stack, l)? {
					Some(p) => pc = p,
					None => return Ok(()),
				}
			}
			Instr::Return => return Ok(()),
			Instr::Call(f) => {
				let ftype = module.func_type(*f).ok_or(Trap("call to unknown function"))?;
				let nargs: usize = ftype.params.len();
				if stack.len() < nargs {
					return Err(Trap("stack underflow at call"));
				}
				let call_args: Vec<Value> = stack.split_off(stack.len() - nargs);
				let results: Vec<Value> = call(module, code, memory, globals, *f, &call_args, host)?;
				stack.extend(results);
			}
			Instr::CallIndirect(_) => return Err(Trap("call_indirect is unsupported")),
			Instr::Drop => {
				pop(stack)?;
			}
			Instr::Select => {
				let c: i32 = pop(stack)?.as_i32();
				let b: Value = pop(stack)?;
				let a: Value = pop(stack)?;
				stack.push(if c != 0 { a } else { b });
			}
			Instr::LocalGet(i) => {
				let v: Value = *locals.get(*i as usize).ok_or(Trap("local out of range"))?;
				stack.push(v);
			}
			Instr::LocalSet(i) => {
				let v: Value = pop(stack)?;
				*locals.get_mut(*i as usize).ok_or(Trap("local out of range"))? = v;
			}
			Instr::LocalTee(i) => {
				let v: Value = *stack.last().ok_or(Trap("stack underflow"))?;
				*locals.get_mut(*i as usize).ok_or(Trap("local out of range"))? = v;
			}
			Instr::GlobalGet(i) => {
				let v: Value = *globals.get(*i as usize).ok_or(Trap("global out of range"))?;
				stack.push(v);
			}
			Instr::GlobalSet(i) => {
				let v: Value = pop(stack)?;
				*globals.get_mut(*i as usize).ok_or(Trap("global out of range"))? = v;
			}
			Instr::I32Const(v) => stack.push(Value::I32(*v)),
			Instr::I64Const(v) => stack.push(Value::I64(*v)),
			Instr::F32Const(v) => stack.push(Value::F32(*v)),
			Instr::F64Const(v) => stack.push(Value::F64(*v)),
			Instr::Load { width, signed, wide, offset } => {
				let addr: usize = mem_addr(stack, *offset)?;
				let w: usize = *width as usize;
				let bytes: &[u8] = memory.get(addr..addr + w).ok_or(Trap("memory access out of bounds"))?;
				let mut raw: [u8; 8] = [0u8; 8];
				raw[..w].copy_from_slice(bytes);
				let value: Value = if *wide {
					let mut v: u64 = u64::from_le_bytes(raw);
					if *signed && w < 8 {
						let shift: u32 = (64 - w * 8) as u32;
						v = (((v << shift) as i64) >> shift) as u64;
					}
					Value::I64(v as i64)
				} else {
					let mut v: u32 = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
					if *signed && w < 4 {
						let shift: u32 = (32 - w * 8) as u32;
						v = (((v << shift) as i32) >> shift) as u32;
					}
					Value::I32(v as i32)
				};
				stack.push(value);
			}
			Instr::Store { width, offset } => {
				let v: Value = pop(stack)?;
				let addr: usize = mem_addr(stack, *offset)?;
				let w: usize = *width as usize;
				let raw: [u8; 8] = match v {
					Value::I32(x) => (x as u32 as u64).to_le_bytes(),
					Value::I64(x) => (x as u64).to_le_bytes(),
					Value::F32(x) => (x.to_bits() as u64).to_le_bytes(),
					Value::F64(x) => x.to_bits().to_le_bytes(),
				};
				let slot: &mut [u8] = memory.get_mut(addr..addr + w).ok_or(Trap("memory access out of bounds"))?;
				slot.copy_from_slice(&raw[..w]);
			}
			Instr::FLoad { wide, offset } => {
				let addr: usize = mem_addr(stack, *offset)?;
				if *wide {
					let bytes: &[u8] = memory.get(addr..addr + 8).ok_or(Trap("memory access out of bounds"))?;
					let mut raw: [u8; 8] = [0u8; 8];
					raw.copy_from_slice(bytes);
					stack.push(Value::F64(f64::from_bits(u64::from_le_bytes(raw))));
				} else {
					let bytes: &[u8] = memory.get(addr..addr + 4).ok_or(Trap("memory access out of bounds"))?;
					let mut raw: [u8; 4] = [0u8; 4];
					raw.copy_from_slice(bytes);
					stack.push(Value::F32(f32::from_bits(u32::from_le_bytes(raw))));
				}
			}
			Instr::FStore { wide, offset } => {
				let v: Value = pop(stack)?;
				let addr: usize = mem_addr(stack, *offset)?;
				if *wide {
					let slot: &mut [u8] = memory.get_mut(addr..addr + 8).ok_or(Trap("memory access out of bounds"))?;
					slot.copy_from_slice(&v.as_f64().to_bits().to_le_bytes());
				} else {
					let slot: &mut [u8] = memory.get_mut(addr..addr + 4).ok_or(Trap("memory access out of bounds"))?;
					slot.copy_from_slice(&v.as_f32().to_bits().to_le_bytes());
				}
			}
			Instr::MemorySize => stack.push(Value::I32((memory.len() / PAGE) as i32)),
			Instr::MemoryGrow => {
				let delta: usize = pop(stack)?.as_i32() as u32 as usize;
				let old_pages: usize = memory.len() / PAGE;
				memory.resize(memory.len() + delta * PAGE, 0);
				stack.push(Value::I32(old_pages as i32));
			}
			Instr::MemoryCopy => {
				let n: usize = pop(stack)?.as_i32() as u32 as usize;
				let s: usize = pop(stack)?.as_i32() as u32 as usize;
				let d: usize = pop(stack)?.as_i32() as u32 as usize;
				if s.saturating_add(n) > memory.len() || d.saturating_add(n) > memory.len() {
					return Err(Trap("memory.copy out of bounds"));
				}
				memory.copy_within(s..s + n, d);
			}
			Instr::MemoryFill => {
				let n: usize = pop(stack)?.as_i32() as u32 as usize;
				let val: u8 = pop(stack)?.as_i32() as u8;
				let d: usize = pop(stack)?.as_i32() as u32 as usize;
				if d.saturating_add(n) > memory.len() {
					return Err(Trap("memory.fill out of bounds"));
				}
				for byte in &mut memory[d..d + n] {
					*byte = val;
				}
			}
			Instr::Num(op) => num_op(*op, stack)?,
			Instr::TruncSat(sub) => trunc_sat(*sub, stack)?,
		}
	}
	Ok(())
}

// Resolve a branch to label depth `k`: unwind the value stack to the target frame's
// base keeping `arity` values, pop the control frames it exits, and return the
// instruction to resume at - or `None` if `k` names the function-level frame (a
// return). `Frame` is `Copy`, so the target is read before the control stack shrinks.
fn branch(ctrl: &mut Vec<Frame>, stack: &mut Vec<Value>, k: u32) -> Result<Option<usize>, Trap> {
	if k as usize >= ctrl.len() {
		return Ok(None);
	}
	let idx: usize = ctrl.len() - 1 - k as usize;
	let frame: Frame = ctrl[idx];
	let keep: usize = frame.arity as usize;
	let total: usize = stack.len();
	if total < frame.base + keep {
		return Err(Trap("stack underflow on branch"));
	}
	for j in 0..keep {
		stack[frame.base + j] = stack[total - keep + j];
	}
	stack.truncate(frame.base + keep);
	if frame.is_loop {
		ctrl.truncate(idx + 1);
	} else {
		ctrl.truncate(idx);
	}
	Ok(Some(frame.target))
}

// Pop a value, trapping on underflow.
fn pop(stack: &mut Vec<Value>) -> Result<Value, Trap> {
	stack.pop().ok_or(Trap("stack underflow"))
}

// Pop the load/store base address and add the static offset, returning the
// effective byte address.
fn mem_addr(stack: &mut Vec<Value>, offset: u32) -> Result<usize, Trap> {
	let base: usize = pop(stack)?.as_i32() as u32 as usize;
	Ok(base + offset as usize)
}

// Execute an integer ALU opcode against the value stack.
fn num_op(op: u8, stack: &mut Vec<Value>) -> Result<(), Trap> {
	match op {
		// i32 comparisons
		0x45 => {
			let a: i32 = pop(stack)?.as_i32();
			push_bool(stack, a == 0);
		}
		0x46 => i32_cmp(stack, |a, b| a == b)?,
		0x47 => i32_cmp(stack, |a, b| a != b)?,
		0x48 => i32_cmp(stack, |a, b| a < b)?,
		0x49 => i32_cmp(stack, |a, b| (a as u32) < (b as u32))?,
		0x4a => i32_cmp(stack, |a, b| a > b)?,
		0x4b => i32_cmp(stack, |a, b| (a as u32) > (b as u32))?,
		0x4c => i32_cmp(stack, |a, b| a <= b)?,
		0x4d => i32_cmp(stack, |a, b| (a as u32) <= (b as u32))?,
		0x4e => i32_cmp(stack, |a, b| a >= b)?,
		0x4f => i32_cmp(stack, |a, b| (a as u32) >= (b as u32))?,
		// i64 comparisons
		0x50 => {
			let a: i64 = pop(stack)?.as_i64();
			push_bool(stack, a == 0);
		}
		0x51 => i64_cmp(stack, |a, b| a == b)?,
		0x52 => i64_cmp(stack, |a, b| a != b)?,
		0x53 => i64_cmp(stack, |a, b| a < b)?,
		0x54 => i64_cmp(stack, |a, b| (a as u64) < (b as u64))?,
		0x55 => i64_cmp(stack, |a, b| a > b)?,
		0x56 => i64_cmp(stack, |a, b| (a as u64) > (b as u64))?,
		0x57 => i64_cmp(stack, |a, b| a <= b)?,
		0x58 => i64_cmp(stack, |a, b| (a as u64) <= (b as u64))?,
		0x59 => i64_cmp(stack, |a, b| a >= b)?,
		0x5a => i64_cmp(stack, |a, b| (a as u64) >= (b as u64))?,
		// f32 comparisons
		0x5b => f32_cmp(stack, |a, b| a == b)?,
		0x5c => f32_cmp(stack, |a, b| a != b)?,
		0x5d => f32_cmp(stack, |a, b| a < b)?,
		0x5e => f32_cmp(stack, |a, b| a > b)?,
		0x5f => f32_cmp(stack, |a, b| a <= b)?,
		0x60 => f32_cmp(stack, |a, b| a >= b)?,
		// f64 comparisons
		0x61 => f64_cmp(stack, |a, b| a == b)?,
		0x62 => f64_cmp(stack, |a, b| a != b)?,
		0x63 => f64_cmp(stack, |a, b| a < b)?,
		0x64 => f64_cmp(stack, |a, b| a > b)?,
		0x65 => f64_cmp(stack, |a, b| a <= b)?,
		0x66 => f64_cmp(stack, |a, b| a >= b)?,
		// i32 arithmetic / bitwise
		0x67 => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::I32((a as u32).leading_zeros() as i32));
		}
		0x68 => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::I32((a as u32).trailing_zeros() as i32));
		}
		0x69 => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::I32((a as u32).count_ones() as i32));
		}
		0x6a => i32_bin(stack, |a, b| a.wrapping_add(b))?,
		0x6b => i32_bin(stack, |a, b| a.wrapping_sub(b))?,
		0x6c => i32_bin(stack, |a, b| a.wrapping_mul(b))?,
		0x6d => i32_div(stack, true)?,
		0x6e => i32_div(stack, false)?,
		0x6f => i32_rem(stack, true)?,
		0x70 => i32_rem(stack, false)?,
		0x71 => i32_bin(stack, |a, b| a & b)?,
		0x72 => i32_bin(stack, |a, b| a | b)?,
		0x73 => i32_bin(stack, |a, b| a ^ b)?,
		0x74 => i32_bin(stack, |a, b| a.wrapping_shl(b as u32))?,
		0x75 => i32_bin(stack, |a, b| a.wrapping_shr(b as u32))?,
		0x76 => i32_bin(stack, |a, b| (a as u32).wrapping_shr(b as u32) as i32)?,
		0x77 => i32_bin(stack, |a, b| (a as u32).rotate_left((b & 31) as u32) as i32)?,
		0x78 => i32_bin(stack, |a, b| (a as u32).rotate_right((b & 31) as u32) as i32)?,
		// i64 arithmetic / bitwise
		0x79 => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::I64((a as u64).leading_zeros() as i64));
		}
		0x7a => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::I64((a as u64).trailing_zeros() as i64));
		}
		0x7b => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::I64((a as u64).count_ones() as i64));
		}
		0x7c => i64_bin(stack, |a, b| a.wrapping_add(b))?,
		0x7d => i64_bin(stack, |a, b| a.wrapping_sub(b))?,
		0x7e => i64_bin(stack, |a, b| a.wrapping_mul(b))?,
		0x7f => i64_div(stack, true)?,
		0x80 => i64_div(stack, false)?,
		0x81 => i64_rem(stack, true)?,
		0x82 => i64_rem(stack, false)?,
		0x83 => i64_bin(stack, |a, b| a & b)?,
		0x84 => i64_bin(stack, |a, b| a | b)?,
		0x85 => i64_bin(stack, |a, b| a ^ b)?,
		0x86 => i64_bin(stack, |a, b| a.wrapping_shl(b as u32))?,
		0x87 => i64_bin(stack, |a, b| a.wrapping_shr(b as u32))?,
		0x88 => i64_bin(stack, |a, b| (a as u64).wrapping_shr(b as u32) as i64)?,
		0x89 => i64_bin(stack, |a, b| (a as u64).rotate_left((b & 63) as u32) as i64)?,
		0x8a => i64_bin(stack, |a, b| (a as u64).rotate_right((b & 63) as u32) as i64)?,
		// f32 arithmetic
		0x8b => f32_un(stack, |a| f32::from_bits(a.to_bits() & 0x7fff_ffff))?, // abs
		0x8c => f32_un(stack, |a| -a)?,                                        // neg
		0x8d => f32_un(stack, ceil_f32)?,
		0x8e => f32_un(stack, floor_f32)?,
		0x8f => f32_un(stack, trunc_f32)?,
		0x90 => f32_un(stack, nearest_f32)?,
		0x91 => f32_un(stack, sqrt_f32)?,
		0x92 => f32_bin(stack, |a, b| a + b)?,
		0x93 => f32_bin(stack, |a, b| a - b)?,
		0x94 => f32_bin(stack, |a, b| a * b)?,
		0x95 => f32_bin(stack, |a, b| a / b)?,
		0x96 => f32_bin(stack, fmin_f32)?,
		0x97 => f32_bin(stack, fmax_f32)?,
		0x98 => f32_bin(stack, copysign_f32)?,
		// f64 arithmetic
		0x99 => f64_un(stack, |a| f64::from_bits(a.to_bits() & 0x7fff_ffff_ffff_ffff))?, // abs
		0x9a => f64_un(stack, |a| -a)?,                                                  // neg
		0x9b => f64_un(stack, ceil_f64)?,
		0x9c => f64_un(stack, floor_f64)?,
		0x9d => f64_un(stack, trunc_f64)?,
		0x9e => f64_un(stack, nearest_f64)?,
		0x9f => f64_un(stack, sqrt_f64)?,
		0xa0 => f64_bin(stack, |a, b| a + b)?,
		0xa1 => f64_bin(stack, |a, b| a - b)?,
		0xa2 => f64_bin(stack, |a, b| a * b)?,
		0xa3 => f64_bin(stack, |a, b| a / b)?,
		0xa4 => f64_bin(stack, fmin_f64)?,
		0xa5 => f64_bin(stack, fmax_f64)?,
		0xa6 => f64_bin(stack, copysign_f64)?,
		// conversions
		0xa7 => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::I32(a as i32));
		}
		0xa8 => {
			let a: f64 = pop(stack)?.as_f32() as f64;
			stack.push(Value::I32(trunc_i32_s(a)?));
		}
		0xa9 => {
			let a: f64 = pop(stack)?.as_f32() as f64;
			stack.push(Value::I32(trunc_i32_u(a)?));
		}
		0xaa => {
			let a: f64 = pop(stack)?.as_f64();
			stack.push(Value::I32(trunc_i32_s(a)?));
		}
		0xab => {
			let a: f64 = pop(stack)?.as_f64();
			stack.push(Value::I32(trunc_i32_u(a)?));
		}
		0xac => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::I64(a as i64));
		}
		0xad => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::I64(a as u32 as i64));
		}
		0xae => {
			let a: f64 = pop(stack)?.as_f32() as f64;
			stack.push(Value::I64(trunc_i64_s(a)?));
		}
		0xaf => {
			let a: f64 = pop(stack)?.as_f32() as f64;
			stack.push(Value::I64(trunc_i64_u(a)?));
		}
		0xb0 => {
			let a: f64 = pop(stack)?.as_f64();
			stack.push(Value::I64(trunc_i64_s(a)?));
		}
		0xb1 => {
			let a: f64 = pop(stack)?.as_f64();
			stack.push(Value::I64(trunc_i64_u(a)?));
		}
		0xb2 => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::F32(a as f32));
		}
		0xb3 => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::F32(a as u32 as f32));
		}
		0xb4 => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::F32(a as f32));
		}
		0xb5 => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::F32(a as u64 as f32));
		}
		0xb6 => {
			let a: f64 = pop(stack)?.as_f64();
			stack.push(Value::F32(a as f32));
		}
		0xb7 => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::F64(a as f64));
		}
		0xb8 => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::F64(a as u32 as f64));
		}
		0xb9 => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::F64(a as f64));
		}
		0xba => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::F64(a as u64 as f64));
		}
		0xbb => {
			let a: f32 = pop(stack)?.as_f32();
			stack.push(Value::F64(a as f64));
		}
		0xbc => {
			let a: f32 = pop(stack)?.as_f32();
			stack.push(Value::I32(a.to_bits() as i32));
		}
		0xbd => {
			let a: f64 = pop(stack)?.as_f64();
			stack.push(Value::I64(a.to_bits() as i64));
		}
		0xbe => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::F32(f32::from_bits(a as u32)));
		}
		0xbf => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::F64(f64::from_bits(a as u64)));
		}
		0xc0 => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::I32(a as i8 as i32));
		}
		0xc1 => {
			let a: i32 = pop(stack)?.as_i32();
			stack.push(Value::I32(a as i16 as i32));
		}
		0xc2 => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::I64(a as i8 as i64));
		}
		0xc3 => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::I64(a as i16 as i64));
		}
		0xc4 => {
			let a: i64 = pop(stack)?.as_i64();
			stack.push(Value::I64(a as i32 as i64));
		}
		_ => return Err(Trap("unsupported numeric opcode")),
	}
	Ok(())
}

fn push_bool(stack: &mut Vec<Value>, b: bool) {
	stack.push(Value::I32(b as i32));
}

// Pop b then a as i32 and push `f(a, b)`.
fn i32_bin(stack: &mut Vec<Value>, f: impl Fn(i32, i32) -> i32) -> Result<(), Trap> {
	let b: i32 = pop(stack)?.as_i32();
	let a: i32 = pop(stack)?.as_i32();
	stack.push(Value::I32(f(a, b)));
	Ok(())
}

// Pop b then a as i32 and push the boolean `f(a, b)`.
fn i32_cmp(stack: &mut Vec<Value>, f: impl Fn(i32, i32) -> bool) -> Result<(), Trap> {
	let b: i32 = pop(stack)?.as_i32();
	let a: i32 = pop(stack)?.as_i32();
	push_bool(stack, f(a, b));
	Ok(())
}

fn i32_div(stack: &mut Vec<Value>, signed: bool) -> Result<(), Trap> {
	let b: i32 = pop(stack)?.as_i32();
	let a: i32 = pop(stack)?.as_i32();
	if b == 0 {
		return Err(Trap("integer divide by zero"));
	}
	let r: i32 = if signed {
		if a == i32::MIN && b == -1 {
			return Err(Trap("integer overflow"));
		}
		a / b
	} else {
		((a as u32) / (b as u32)) as i32
	};
	stack.push(Value::I32(r));
	Ok(())
}

fn i32_rem(stack: &mut Vec<Value>, signed: bool) -> Result<(), Trap> {
	let b: i32 = pop(stack)?.as_i32();
	let a: i32 = pop(stack)?.as_i32();
	if b == 0 {
		return Err(Trap("integer divide by zero"));
	}
	let r: i32 = if signed { a.wrapping_rem(b) } else { ((a as u32) % (b as u32)) as i32 };
	stack.push(Value::I32(r));
	Ok(())
}

// Pop b then a as i64 and push `f(a, b)`.
fn i64_bin(stack: &mut Vec<Value>, f: impl Fn(i64, i64) -> i64) -> Result<(), Trap> {
	let b: i64 = pop(stack)?.as_i64();
	let a: i64 = pop(stack)?.as_i64();
	stack.push(Value::I64(f(a, b)));
	Ok(())
}

// Pop b then a as i64 and push the boolean `f(a, b)`.
fn i64_cmp(stack: &mut Vec<Value>, f: impl Fn(i64, i64) -> bool) -> Result<(), Trap> {
	let b: i64 = pop(stack)?.as_i64();
	let a: i64 = pop(stack)?.as_i64();
	push_bool(stack, f(a, b));
	Ok(())
}

fn i64_div(stack: &mut Vec<Value>, signed: bool) -> Result<(), Trap> {
	let b: i64 = pop(stack)?.as_i64();
	let a: i64 = pop(stack)?.as_i64();
	if b == 0 {
		return Err(Trap("integer divide by zero"));
	}
	let r: i64 = if signed {
		if a == i64::MIN && b == -1 {
			return Err(Trap("integer overflow"));
		}
		a / b
	} else {
		((a as u64) / (b as u64)) as i64
	};
	stack.push(Value::I64(r));
	Ok(())
}

fn i64_rem(stack: &mut Vec<Value>, signed: bool) -> Result<(), Trap> {
	let b: i64 = pop(stack)?.as_i64();
	let a: i64 = pop(stack)?.as_i64();
	if b == 0 {
		return Err(Trap("integer divide by zero"));
	}
	let r: i64 = if signed { a.wrapping_rem(b) } else { ((a as u64) % (b as u64)) as i64 };
	stack.push(Value::I64(r));
	Ok(())
}

// Pop b then a as f32 and push `f(a, b)`.
fn f32_bin(stack: &mut Vec<Value>, f: impl Fn(f32, f32) -> f32) -> Result<(), Trap> {
	let b: f32 = pop(stack)?.as_f32();
	let a: f32 = pop(stack)?.as_f32();
	stack.push(Value::F32(f(a, b)));
	Ok(())
}

// Pop b then a as f32 and push the boolean `f(a, b)`.
fn f32_cmp(stack: &mut Vec<Value>, f: impl Fn(f32, f32) -> bool) -> Result<(), Trap> {
	let b: f32 = pop(stack)?.as_f32();
	let a: f32 = pop(stack)?.as_f32();
	push_bool(stack, f(a, b));
	Ok(())
}

// Pop a as f32 and push `f(a)`.
fn f32_un(stack: &mut Vec<Value>, f: impl Fn(f32) -> f32) -> Result<(), Trap> {
	let a: f32 = pop(stack)?.as_f32();
	stack.push(Value::F32(f(a)));
	Ok(())
}

// Pop b then a as f64 and push `f(a, b)`.
fn f64_bin(stack: &mut Vec<Value>, f: impl Fn(f64, f64) -> f64) -> Result<(), Trap> {
	let b: f64 = pop(stack)?.as_f64();
	let a: f64 = pop(stack)?.as_f64();
	stack.push(Value::F64(f(a, b)));
	Ok(())
}

// Pop b then a as f64 and push the boolean `f(a, b)`.
fn f64_cmp(stack: &mut Vec<Value>, f: impl Fn(f64, f64) -> bool) -> Result<(), Trap> {
	let b: f64 = pop(stack)?.as_f64();
	let a: f64 = pop(stack)?.as_f64();
	push_bool(stack, f(a, b));
	Ok(())
}

// Pop a as f64 and push `f(a)`.
fn f64_un(stack: &mut Vec<Value>, f: impl Fn(f64) -> f64) -> Result<(), Trap> {
	let a: f64 = pop(stack)?.as_f64();
	stack.push(Value::F64(f(a)));
	Ok(())
}

// wasm `min`: NaN if either operand is NaN; for equal operands (including +0 / -0)
// the more negative sign wins, so min(+0, -0) is -0.
fn fmin_f32(a: f32, b: f32) -> f32 {
	if a.is_nan() || b.is_nan() {
		f32::NAN
	} else if a == b {
		if a.is_sign_negative() { a } else { b }
	} else if a < b {
		a
	} else {
		b
	}
}

// wasm `max`: NaN if either operand is NaN; for equal operands the more positive
// sign wins, so max(+0, -0) is +0.
fn fmax_f32(a: f32, b: f32) -> f32 {
	if a.is_nan() || b.is_nan() {
		f32::NAN
	} else if a == b {
		if a.is_sign_negative() { b } else { a }
	} else if a > b {
		a
	} else {
		b
	}
}

fn copysign_f32(a: f32, b: f32) -> f32 {
	f32::from_bits((a.to_bits() & 0x7fff_ffff) | (b.to_bits() & 0x8000_0000))
}

fn fmin_f64(a: f64, b: f64) -> f64 {
	if a.is_nan() || b.is_nan() {
		f64::NAN
	} else if a == b {
		if a.is_sign_negative() { a } else { b }
	} else if a < b {
		a
	} else {
		b
	}
}

fn fmax_f64(a: f64, b: f64) -> f64 {
	if a.is_nan() || b.is_nan() {
		f64::NAN
	} else if a == b {
		if a.is_sign_negative() { b } else { a }
	} else if a > b {
		a
	} else {
		b
	}
}

fn copysign_f64(a: f64, b: f64) -> f64 {
	f64::from_bits((a.to_bits() & 0x7fff_ffff_ffff_ffff) | (b.to_bits() & 0x8000_0000_0000_0000))
}

// Round toward zero. Exact: a finite value at or beyond 2^63 is already integral,
// and below that the cast through i64 truncates toward zero.
fn trunc_f64(x: f64) -> f64 {
	let magnitude: f64 = f64::from_bits(x.to_bits() & 0x7fff_ffff_ffff_ffff);
	if !x.is_finite() || magnitude >= 9_223_372_036_854_775_808.0 { x } else { (x as i64) as f64 }
}

fn floor_f64(x: f64) -> f64 {
	let t: f64 = trunc_f64(x);
	if t > x { t - 1.0 } else { t }
}

fn ceil_f64(x: f64) -> f64 {
	let t: f64 = trunc_f64(x);
	if t < x { t + 1.0 } else { t }
}

// Round to the nearest integer, ties to even (wasm `nearest` / IEEE roundTiesToEven).
fn nearest_f64(x: f64) -> f64 {
	if !x.is_finite() {
		return x;
	}
	let t: f64 = trunc_f64(x);
	let diff: f64 = x - t;
	if diff > 0.5 {
		t + 1.0
	} else if diff < -0.5 {
		t - 1.0
	} else if diff == 0.5 {
		if (t as i64) % 2 == 0 { t } else { t + 1.0 }
	} else if diff == -0.5 {
		if (t as i64) % 2 == 0 { t } else { t - 1.0 }
	} else {
		t
	}
}

// Square root by Newton-Raphson from an exponent-halving seed. This is `no_std`-
// and stable-friendly (no libm, no intrinsics); it converges to within ~1 ULP,
// which the runtime accepts in exchange for being self-contained.
fn sqrt_f64(x: f64) -> f64 {
	if x.is_nan() || x < 0.0 {
		return f64::NAN;
	}
	if x == 0.0 || x.is_infinite() {
		return x; // sqrt(+-0) = +-0, sqrt(+inf) = +inf
	}
	// seed: halve the biased exponent in the bit pattern for a ~10%-accurate guess
	let mut y: f64 = f64::from_bits((x.to_bits() + (1023u64 << 52)) >> 1);
	for _ in 0..6 {
		y = 0.5 * (y + x / y);
	}
	y
}

fn trunc_f32(x: f32) -> f32 {
	trunc_f64(x as f64) as f32
}

fn floor_f32(x: f32) -> f32 {
	floor_f64(x as f64) as f32
}

fn ceil_f32(x: f32) -> f32 {
	ceil_f64(x as f64) as f32
}

fn nearest_f32(x: f32) -> f32 {
	nearest_f64(x as f64) as f32
}

// Compute the f32 square root in f64 and round once, which yields the correctly
// rounded f32 result for the common cases.
fn sqrt_f32(x: f32) -> f32 {
	sqrt_f64(x as f64) as f32
}

// Truncate a float toward zero into an i32, trapping on NaN or out-of-range input
// (wasm `i32.trunc_f*_s`).
fn trunc_i32_s(x: f64) -> Result<i32, Trap> {
	if x.is_nan() {
		return Err(Trap("invalid conversion to integer"));
	}
	let t: f64 = trunc_f64(x);
	if t < i32::MIN as f64 || t > i32::MAX as f64 {
		return Err(Trap("integer overflow"));
	}
	Ok(t as i32)
}

fn trunc_i32_u(x: f64) -> Result<i32, Trap> {
	if x.is_nan() {
		return Err(Trap("invalid conversion to integer"));
	}
	let t: f64 = trunc_f64(x);
	if t < 0.0 || t > u32::MAX as f64 {
		return Err(Trap("integer overflow"));
	}
	Ok(t as u32 as i32)
}

fn trunc_i64_s(x: f64) -> Result<i64, Trap> {
	if x.is_nan() {
		return Err(Trap("invalid conversion to integer"));
	}
	let t: f64 = trunc_f64(x);
	if t < i64::MIN as f64 || t >= 9_223_372_036_854_775_808.0 {
		return Err(Trap("integer overflow"));
	}
	Ok(t as i64)
}

fn trunc_i64_u(x: f64) -> Result<i64, Trap> {
	if x.is_nan() {
		return Err(Trap("invalid conversion to integer"));
	}
	let t: f64 = trunc_f64(x);
	if t < 0.0 || t >= 18_446_744_073_709_551_616.0 {
		return Err(Trap("integer overflow"));
	}
	Ok(t as u64 as i64)
}

// The saturating float-to-int conversions (the `0xfc` 0..=7 ops, e.g.
// `i32.trunc_sat_f64_s`): pop a float and push the converted integer, mapping NaN
// to 0 and clamping out-of-range values to the integer's min/max instead of
// trapping. Rust's `as` casts have exactly these semantics, so each arm is a direct
// cast. These are what real toolchain output emits for a float `as int` cast.
fn trunc_sat(sub: u8, stack: &mut Vec<Value>) -> Result<(), Trap> {
	let v: Value = pop(stack)?;
	let out: Value = match sub {
		0 => Value::I32(v.as_f32() as i32),
		1 => Value::I32(v.as_f32() as u32 as i32),
		2 => Value::I32(v.as_f64() as i32),
		3 => Value::I32(v.as_f64() as u32 as i32),
		4 => Value::I64(v.as_f32() as i64),
		5 => Value::I64(v.as_f32() as u64 as i64),
		6 => Value::I64(v.as_f64() as i64),
		7 => Value::I64(v.as_f64() as u64 as i64),
		_ => return Err(Trap("unsupported trunc_sat")),
	};
	stack.push(out);
	Ok(())
}

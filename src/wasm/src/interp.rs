// The interpreter: a small stack machine over the integer subset of WebAssembly,
// enough to run a capability-gated component that calls host imports. There is no
// control flow (blocks / loops / branches) and no floating point; a body is a flat
// instruction sequence ending in `end`. Imported functions are dispatched to a
// [`Host`], the seam where a component reaches native services.

use crate::module::{Module, ValType};
use alloc::vec::Vec;

// A runtime value. The runtime handles 32/64-bit integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
	I32(i32),
	I64(i64),
}

impl Value {
	// The value as an i32 (an i64 is truncated, matching wasm wrapping semantics).
	pub fn as_i32(self) -> i32 {
		match self {
			Value::I32(v) => v,
			Value::I64(v) => v as i32,
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

// An instantiated module: the parsed module plus its linear memory.
pub struct Instance<'a> {
	module: &'a Module,
	memory: Vec<u8>,
}

impl<'a> Instance<'a> {
	// Instantiate `module`, allocating its declared minimum linear memory.
	pub fn new(module: &'a Module) -> Instance<'a> {
		let bytes: usize = module.memory_min_pages as usize * PAGE;
		Instance { module, memory: alloc::vec![0u8; bytes] }
	}

	// The instance's linear memory (e.g. to read back what a component wrote).
	pub fn memory(&self) -> &[u8] {
		&self.memory
	}

	// Invoke the exported function `name` with `args`, dispatching any imports to
	// `host`, and return its result value(s).
	pub fn invoke(&mut self, name: &str, args: &[Value], host: &mut dyn Host) -> Result<Vec<Value>, Trap> {
		let index: u32 = self.module.export_func(name).ok_or(Trap("no such exported function"))?;
		call(self.module, &mut self.memory, index, args, host)
	}
}

// Call function `index` (in the combined imports-then-defined space). Imports go to
// the host; defined functions run their body on a fresh frame.
fn call(module: &Module, memory: &mut Vec<u8>, index: u32, args: &[Value], host: &mut dyn Host) -> Result<Vec<Value>, Trap> {
	if index < module.import_count() {
		return host.call_import(index, args, memory);
	}
	let func = module.funcs.get((index - module.import_count()) as usize).ok_or(Trap("function index out of range"))?;
	let ftype = module.types.get(func.type_index as usize).ok_or(Trap("function type out of range"))?;
	if args.len() != ftype.params.len() {
		return Err(Trap("wrong argument count"));
	}
	// locals are the parameters followed by the declared locals (zero-initialized)
	let mut locals: Vec<Value> = Vec::with_capacity(args.len() + func.locals.len());
	locals.extend_from_slice(args);
	for t in &func.locals {
		locals.push(match t {
			ValType::I32 => Value::I32(0),
			ValType::I64 => Value::I64(0),
		});
	}
	let mut stack: Vec<Value> = Vec::new();
	exec(module, memory, &func.body, &mut locals, &mut stack, host)?;
	let n: usize = ftype.results.len();
	if stack.len() < n {
		return Err(Trap("missing results at return"));
	}
	Ok(stack.split_off(stack.len() - n))
}

// Execute a flat instruction body, mutating `locals`, `stack`, and `memory`.
fn exec(module: &Module, memory: &mut Vec<u8>, body: &[u8], locals: &mut [Value], stack: &mut Vec<Value>, host: &mut dyn Host) -> Result<(), Trap> {
	let mut pc: usize = 0;
	while pc < body.len() {
		let op: u8 = body[pc];
		pc += 1;
		match op {
			0x0b => return Ok(()), // end of the function body
			0x0f => return Ok(()), // return (results are left on the stack)
			0x1a => {
				pop(stack)?; // drop
			}
			0x20 => {
				// local.get
				let i: u32 = read_u32(body, &mut pc)?;
				let v: Value = *locals.get(i as usize).ok_or(Trap("local out of range"))?;
				stack.push(v);
			}
			0x21 => {
				// local.set
				let i: u32 = read_u32(body, &mut pc)?;
				let v: Value = pop(stack)?;
				*locals.get_mut(i as usize).ok_or(Trap("local out of range"))? = v;
			}
			0x22 => {
				// local.tee
				let i: u32 = read_u32(body, &mut pc)?;
				let v: Value = *stack.last().ok_or(Trap("stack underflow"))?;
				*locals.get_mut(i as usize).ok_or(Trap("local out of range"))? = v;
			}
			0x41 => {
				// i32.const
				let v: i32 = read_i32(body, &mut pc)?;
				stack.push(Value::I32(v));
			}
			0x6a => bin(stack, |a: i32, b: i32| a.wrapping_add(b))?, // i32.add
			0x6b => bin(stack, |a: i32, b: i32| a.wrapping_sub(b))?, // i32.sub
			0x6c => bin(stack, |a: i32, b: i32| a.wrapping_mul(b))?, // i32.mul
			0x71 => bin(stack, |a: i32, b: i32| a & b)?,             // i32.and
			0x72 => bin(stack, |a: i32, b: i32| a | b)?,             // i32.or
			0x45 => {
				// i32.eqz
				let a: i32 = pop(stack)?.as_i32();
				stack.push(Value::I32((a == 0) as i32));
			}
			0x28 => {
				// i32.load
				let addr: usize = mem_addr(body, &mut pc, stack)?;
				let b: &[u8] = memory.get(addr..addr + 4).ok_or(Trap("memory access out of bounds"))?;
				stack.push(Value::I32(i32::from_le_bytes([b[0], b[1], b[2], b[3]])));
			}
			0x2d => {
				// i32.load8_u
				let addr: usize = mem_addr(body, &mut pc, stack)?;
				let b: u8 = *memory.get(addr).ok_or(Trap("memory access out of bounds"))?;
				stack.push(Value::I32(b as i32));
			}
			0x36 => {
				// i32.store
				let v: i32 = pop(stack)?.as_i32();
				let addr: usize = mem_addr(body, &mut pc, stack)?;
				let slot: &mut [u8] = memory.get_mut(addr..addr + 4).ok_or(Trap("memory access out of bounds"))?;
				slot.copy_from_slice(&v.to_le_bytes());
			}
			0x3a => {
				// i32.store8
				let v: i32 = pop(stack)?.as_i32();
				let addr: usize = mem_addr(body, &mut pc, stack)?;
				*memory.get_mut(addr).ok_or(Trap("memory access out of bounds"))? = v as u8;
			}
			0x10 => {
				// call
				let target: u32 = read_u32(body, &mut pc)?;
				let ftype = module.func_type(target).ok_or(Trap("call to unknown function"))?;
				let nargs: usize = ftype.params.len();
				if stack.len() < nargs {
					return Err(Trap("stack underflow at call"));
				}
				let call_args: Vec<Value> = stack.split_off(stack.len() - nargs);
				let results: Vec<Value> = call(module, memory, target, &call_args, host)?;
				for v in results {
					stack.push(v);
				}
			}
			_ => return Err(Trap("unsupported opcode")),
		}
	}
	Ok(())
}

// Pop a value, trapping on underflow.
fn pop(stack: &mut Vec<Value>) -> Result<Value, Trap> {
	stack.pop().ok_or(Trap("stack underflow"))
}

// Apply a binary i32 operator: pop b, pop a, push f(a, b).
fn bin(stack: &mut Vec<Value>, f: fn(i32, i32) -> i32) -> Result<(), Trap> {
	let b: i32 = pop(stack)?.as_i32();
	let a: i32 = pop(stack)?.as_i32();
	stack.push(Value::I32(f(a, b)));
	Ok(())
}

// Read a load/store memarg (align, offset) and pop the base address, returning the
// effective byte address (base + offset).
fn mem_addr(body: &[u8], pc: &mut usize, stack: &mut Vec<Value>) -> Result<usize, Trap> {
	let _align: u32 = read_u32(body, pc)?;
	let offset: u32 = read_u32(body, pc)?;
	let base: i32 = pop(stack)?.as_i32();
	Ok(base as usize + offset as usize)
}

// Read an unsigned LEB128 from the body, advancing `pc`.
fn read_u32(body: &[u8], pc: &mut usize) -> Result<u32, Trap> {
	let mut result: u32 = 0;
	let mut shift: u32 = 0;
	loop {
		let b: u8 = *body.get(*pc).ok_or(Trap("unexpected end of body"))?;
		*pc += 1;
		result |= ((b & 0x7f) as u32) << shift;
		if b & 0x80 == 0 {
			return Ok(result);
		}
		shift += 7;
		if shift >= 32 {
			return Err(Trap("LEB128 overflow"));
		}
	}
}

// Read a signed LEB128 from the body, advancing `pc`.
fn read_i32(body: &[u8], pc: &mut usize) -> Result<i32, Trap> {
	let mut result: i64 = 0;
	let mut shift: u32 = 0;
	loop {
		let b: u8 = *body.get(*pc).ok_or(Trap("unexpected end of body"))?;
		*pc += 1;
		result |= ((b & 0x7f) as i64) << shift;
		shift += 7;
		if b & 0x80 == 0 {
			if shift < 64 && (b & 0x40) != 0 {
				result |= -1i64 << shift;
			}
			return Ok(result as i32);
		}
		if shift >= 64 {
			return Err(Trap("LEB128 overflow"));
		}
	}
}

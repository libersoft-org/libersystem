// The interpreter: a stack machine over the decoded [`Instr`] stream. It supports
// structured control flow (block / loop / if / else / br / br_if / br_table /
// return), the integer AND floating-point instruction sets (i32 / i64 / f32 / f64
// arithmetic, comparison, bitwise, shifts, rotates, and the width and float
// conversions), globals, a single linear memory with loads / stores / size / grow /
// copy / fill, and calls - both to defined functions and, through the [`Host`] seam,
// to imported ones. Imported functions are how a WASI-style component reaches native
// services; the host services only the imports it was wired up to grant.
//
// "Floating point is added by a later step" is what this said, and it is not: `Value`
// three lines below carries `F32` and `F64`, the spec corpus runs the float suites,
// and `lib.rs` has described the supported subset as including them for some time.
// This engine has been reopened by a stale comment before, and a comment describing a
// subset the code no longer has is the one a new contributor builds on.

use crate::decode::Instr;
use crate::module::{Module, ValType};
use crate::validate::ValidatedModule;
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
	// Which value type this is, for the two boundary checks validation cannot make: the arguments a
	// host passes to `invoke`, and the results a `Host` returns from an import.
	pub fn val_type(self) -> crate::module::ValType {
		match self {
			Value::I32(_) => crate::module::ValType::I32,
			Value::I64(_) => crate::module::ValType::I64,
			Value::F32(_) => crate::module::ValType::F32,
			Value::F64(_) => crate::module::ValType::F64,
		}
	}

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

	// Bytes this host touched serving the CALL THAT JUST RETURNED, so the interpreter can charge for
	// them.
	//
	// Without it a host import was free: one instruction's worth of fuel bought a call that could
	// read or write the whole 64 MB linear memory, so fuel bounded INTERPRETATION rather than WORK,
	// which is the opposite of what it exists to do - the cheapest way to spend a host's time was to
	// stop executing wasm and start calling out.
	//
	// The default is zero for a host that only exchanges scalars, which costs no more than the call
	// itself. A host that copies reports what it copied and is charged at the same per-kilobyte rate
	// as `memory.copy` and `memory.grow`, so the price of moving a byte does not depend on which
	// side of the boundary moved it.
	fn work_bytes(&self) -> usize {
		0
	}
}

// What one host import costs before anything it moves is priced. A host call crosses a boundary,
// validates its arguments and usually performs a syscall, so it is worth more than an add - and a
// guest that does nothing but call out must still run out of fuel.
const HOST_CALL_UNITS: u64 = 16;

// One page of linear memory.
pub const PAGE: usize = 65536;

// The most linear memory ONE INSTANCE may hold, whatever it declares about itself.
//
// A module's own maximum is a statement by the module; this is the host's, and a component host in
// a capability system needs both. wasm32 can address 65536 pages (4 GB) and nothing here comes near
// it: the packaging gate refuses a component whose declared minimum is above four pages, and the
// SDK's own example uses one. 1024 pages is 64 MB - room for a component that genuinely needs to
// grow, and far below what would trouble the service running it.
pub const MAX_MEMORY_PAGES: usize = 1024;

// A runtime control frame: the value-stack base to unwind to on a branch, whether
// it is a loop (a branch re-enters it), how many values a branch keeps, and the
// instruction index a branch resumes at.
#[derive(Clone, Copy)]
struct Frame {
	base: usize,
	is_loop: bool,
	arity: usize,
	target: usize,
}

// An instantiated module: the parsed module, its decoded function bodies, its
// linear memory, its globals, and its function table (the array call_indirect
// dispatches through).
// THE EXECUTION BUDGET, and the call depth.
//
// `loop br 0 end` was an unbounded loop inside the interpreter with nothing that ended it, and
// guest recursion was Rust recursion - so a deeply recursive module overflowed the HOST's stack
// rather than trapping. A component host in a capability system may not have "the guest can decide
// never to return" among its outcomes, and it certainly may not have "the guest can decide to
// overflow the host".
//
// One instruction is one unit. The number is a wall rather than a schedule: an ordinary component
// call is thousands of instructions, and anything reaching ten million is not computing, it is
// spinning. A caller that wants a different wall calls `invoke_with_fuel`.
pub const DEFAULT_FUEL: u64 = 10_000_000;

// How deep guest calls may nest. Each level is a Rust frame in `exec`, so this is a statement about
// the HOST's stack, which is why it is small and fixed rather than derived from the module.
//
// MEASURED, NOT CHOSEN. 256 still overflowed a 2 MB host thread - `exec`'s frame is large, because
// it holds the control stack and the operand stack of one activation - so the depth was reduced
// until the deliberately-infinite recursion in `unbounded_guest_recursion_traps_rather_than_
// overflowing_the_host` trapped instead of aborting. Anything that raises `exec`'s frame size has
// to re-run that test, which is why it exists rather than being replaced by an assertion here.
pub const MAX_CALL_DEPTH: usize = 32;

pub struct Budget {
	fuel: u64,
	depth: usize,
}

impl Budget {
	pub fn new() -> Budget {
		Budget { fuel: DEFAULT_FUEL, depth: 0 }
	}

	pub fn with_fuel(fuel: u64) -> Budget {
		Budget { fuel, depth: 0 }
	}

	// Spend one instruction, or say the guest has run out.
	fn step(&mut self) -> Result<(), Trap> {
		self.spend(1)
	}

	// Spend what a BULK operation costs, which is not one.
	//
	// `memory.copy` and `memory.fill` each cost one unit while touching up to sixty-four megabytes,
	// so a loop of bulk copies spent fuel at a millionth of the rate the budget assumes. The wall
	// stopped `loop br 0 end` - which was the item - and it was not a work budget, which is what the
	// DONE note said in its own words ("a wall rather than a schedule").
	//
	// A byte is far cheaper than an instruction, so the rate is per kilobyte rather than per byte:
	// a whole 64 MiB memory costs about 65,000 units against a ten-million budget, which bounds a
	// runaway without pricing an ordinary component's one large copy out of existence.
	fn charge_bulk(&mut self, bytes: usize) -> Result<(), Trap> {
		self.spend(1 + (bytes as u64 / 1024))
	}

	fn spend(&mut self, units: u64) -> Result<(), Trap> {
		match self.fuel.checked_sub(units) {
			Some(left) => {
				self.fuel = left;
				Ok(())
			}
			None => Err(Trap("out of fuel: the guest ran for longer than the host allows")),
		}
	}

	fn enter(&mut self) -> Result<(), Trap> {
		if self.depth >= MAX_CALL_DEPTH {
			return Err(Trap("call depth limit reached"));
		}
		self.depth += 1;
		Ok(())
	}

	fn leave(&mut self) {
		self.depth = self.depth.saturating_sub(1);
	}
}

impl Default for Budget {
	fn default() -> Self {
		Self::new()
	}
}

pub struct Instance<'a> {
	module: &'a Module,
	code: &'a [Vec<Instr>],
	memory: Vec<u8>,
	globals: Vec<Value>,
	table: Vec<Option<u32>>,
}

impl<'a> Instance<'a> {
	// Instantiate a VALIDATED module: allocate its declared minimum linear memory, copy in its
	// active data segments, initialise its globals and build its function table.
	//
	// IT TAKES ONLY A `ValidatedModule`, which is the point of the type. This used to take a
	// `Module`, decode every body itself, hold any decode error in a field and surface it on the
	// first `invoke` - so a module could be instantiated, and its data segments and globals
	// installed, before anything had established that its code was well formed.
	//
	// AND IT RETURNS `Result`. It could not report a module it was unable to instantiate, which is
	// the other half of a validator: something has to be able to say no. The remaining failures
	// after validation are the ones validation cannot see - a host that cannot allocate the
	// declared memory - and those are now refusals rather than a panic or a deferred trap.
	pub fn new(validated: &'a ValidatedModule) -> Result<Instance<'a>, Trap> {
		let module: &Module = validated.module();
		let mut memory: Vec<u8> = Vec::new();
		let want: usize = module.memory.map_or(0, |m| m.min_pages as usize) * PAGE;
		if memory.try_reserve_exact(want).is_err() {
			return Err(Trap("the host cannot allocate the module's declared memory"));
		}
		memory.resize(want, 0);

		// Validation has already established that every data segment is inside the declared
		// memory, which is why this cannot fail any more.
		for seg in &module.data {
			let start: usize = seg.offset as usize;
			let end: usize = start + seg.bytes.len();
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

		// The function table, at exactly the size the module DECLARED. It used to be sized to "the
		// larger of the declared minimum and the highest write", so an element segment past the end
		// silently extended the table beyond the module's own limits; validation now refuses such a
		// segment, so the declared minimum is the whole answer.
		let table_len: usize = module.table.as_ref().map(|t| t.min as usize).unwrap_or(0);
		let mut table: Vec<Option<u32>> = Vec::new();
		if table.try_reserve_exact(table_len).is_err() {
			return Err(Trap("the host cannot allocate the module's declared table"));
		}
		table.resize(table_len, None);
		for e in &module.elements {
			for (j, &f) in e.funcs.iter().enumerate() {
				table[e.offset as usize + j] = Some(f);
			}
		}

		Ok(Instance { module, code: validated.code(), memory, globals, table })
	}

	// The instance's linear memory (e.g. to read back what a component wrote).
	pub fn memory(&self) -> &[u8] {
		&self.memory
	}

	// Invoke the exported function `name` with `args`, dispatching any imports to
	// `host`, and return its result value(s).
	pub fn invoke(&mut self, name: &str, args: &[Value], host: &mut dyn Host) -> Result<Vec<Value>, Trap> {
		self.invoke_with_fuel(name, args, host, DEFAULT_FUEL)
	}

	// The same call against a wall the CALLER chooses.
	//
	// `DEFAULT_FUEL`'s own comment says "a caller that wants a different wall calls
	// `invoke_with_fuel`", and there was no such function: `Budget::with_fuel` existed and was
	// reachable from nothing, so the sentence described an API that had never been written. An
	// embedder metering a component it does not trust is the reason the budget exists.
	pub fn invoke_with_fuel(&mut self, name: &str, args: &[Value], host: &mut dyn Host, fuel: u64) -> Result<Vec<Value>, Trap> {
		let index: u32 = self.module.export_func(name).ok_or(Trap("no such exported function"))?;
		let Instance { module, code, memory, globals, table } = self;
		// A fresh budget per call, and a fresh depth. Both are per-INVOCATION rather than
		// per-instance: a component that is called many times gets its budget each time, and one
		// that never returns is stopped inside the call it never returned from.
		let mut budget = Budget::with_fuel(fuel);
		call(module, code, memory, globals, table, index, args, host, &mut budget)
	}
}

// Call function `index` (in the combined imports-then-defined space). Imports go to
// the host; defined functions run their decoded body on a fresh frame.
fn call(module: &Module, code: &[Vec<Instr>], memory: &mut Vec<u8>, globals: &mut [Value], table: &[Option<u32>], index: u32, args: &[Value], host: &mut dyn Host, budget: &mut Budget) -> Result<Vec<Value>, Trap> {
	budget.enter()?;
	let result = call_inner(module, code, memory, globals, table, index, args, host, budget);
	budget.leave();
	result
}

fn call_inner(module: &Module, code: &[Vec<Instr>], memory: &mut Vec<u8>, globals: &mut [Value], table: &[Option<u32>], index: u32, args: &[Value], host: &mut dyn Host, budget: &mut Budget) -> Result<Vec<Value>, Trap> {
	// THE ARGUMENTS FIRST, FOR EITHER KIND OF CALLEE.
	//
	// The count-and-type check below used to sit in the DEFINED-function branch, past the return
	// that dispatches an import - so a module importing `(func (param i32 i32))` and re-exporting it
	// handed the embedder's arguments to `Host::call_import` with neither checked. That is the exact
	// invariant this check exists for, reached by the one call shape it did not cover: the boundary
	// is where the values enter, not where the body is.
	//
	// Both hosts in this tree destructure their arguments exactly and are not at risk; the generic
	// `Host` API is, and it is what a third implementation reaches for.
	if let Some(ftype) = module.func_type(index) {
		if args.len() != ftype.params.len() {
			return Err(Trap("wrong argument count"));
		}
		// Validation proved the BODY consistent with its declared signature; a caller invoking
		// `("takes_i32", &[Value::F64(1.0)])` breaks that signature from outside, which validation
		// cannot see. The value then sat in a local the body reads as an `i32`, and `Value::as_i32`
		// converts every variant, so it did not even surface as a wrong answer.
		for (arg, declared) in args.iter().zip(ftype.params.iter()) {
			if arg.val_type() != *declared {
				return Err(Trap("an argument's type does not match the function's signature"));
			}
		}
	}
	if index < module.import_count() {
		// A HOST CALL IS NOT FREE. The base unit is the call; `work_bytes` prices what it moved.
		// A HOST CALL IS NOT FREE. The base unit is the call; `work_bytes` prices what it moved.
		budget.spend(HOST_CALL_UNITS)?;
		let results = host.call_import(index, args, memory)?;
		budget.charge_bulk(host.work_bytes())?;
		// WHAT THE HOST RETURNED, against what the import declared.
		//
		// The vector went straight onto the operand stack with neither its length nor its types
		// checked, so a mistake in a host implementation became a mistyped value inside a body
		// validation had proved consistent - the one place the guarantee could be broken from
		// outside, and the only one not checked.
		let declared = module.imports.get(index as usize).and_then(|import| module.types.get(import.type_index as usize));
		if let Some(ftype) = declared {
			if results.len() != ftype.results.len() {
				return Err(Trap("a host import returned the wrong number of results"));
			}
			for (value, expected) in results.iter().zip(ftype.results.iter()) {
				if value.val_type() != *expected {
					return Err(Trap("a host import returned a result of the wrong type"));
				}
			}
		}
		return Ok(results);
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
	exec(module, code, memory, globals, table, body, &mut locals, &mut stack, host, budget)?;
	let n: usize = ftype.results.len();
	if stack.len() < n {
		return Err(Trap("missing results at return"));
	}
	Ok(stack.split_off(stack.len() - n))
}

// Execute a decoded function body, mutating `locals`, `stack`, `memory`, and
// `globals`. Returns when the body falls off its end, hits `return`, or branches to
// the function-level label; the caller takes the result values off the stack.
fn exec(module: &Module, code: &[Vec<Instr>], memory: &mut Vec<u8>, globals: &mut [Value], table: &[Option<u32>], body: &[Instr], locals: &mut [Value], stack: &mut Vec<Value>, host: &mut dyn Host, budget: &mut Budget) -> Result<(), Trap> {
	let mut ctrl: Vec<Frame> = Vec::new();
	let mut pc: usize = 0;
	while pc < body.len() {
		// ONE UNIT PER INSTRUCTION, charged before the instruction runs, so a loop that branches
		// to itself forever runs out rather than never returning.
		budget.step()?;
		let instr: &Instr = &body[pc];
		pc += 1;
		match instr {
			Instr::Unreachable => return Err(Trap("unreachable")),
			Instr::Nop => {}
			// The interpreter needs only HOW MANY values a frame keeps, which the block type answers -
			// so the arities it used to be handed are derived here rather than carried, and the
			// validator gets the types it needs from the same field.
			// THE LABEL'S HEIGHT IS THE STACK AFTER THE PARAMETERS ARE TAKEN, for all three.
			//
			// Entering a block of type `[t1*] -> [t2*]` pops the `t1*` and pushes a label whose
			// height is what is left; the parameters then become the block's own operands, and a
			// branch to that label truncates the stack to the label's height. `Block` and `If`
			// recorded `stack.len()` with the parameters still counted, so the truncation left them
			// behind: `(param i32) (result i32)` entered with `[10, 20]` and branched out carrying
			// `30` produced `[10, 20, 30]` instead of `[10, 30]`, and the function answered 50 where
			// the specification says 40.
			//
			// A module that VALIDATES and then computes the wrong answer, which is the one failure
			// an interpreter exists to make impossible. It arrived through the repair that gave
			// `BlockType` the real type: the validator was taught what block parameters mean
			// (`validate.rs` pops them and takes the height AFTER the pop) and the frame setup here
			// was left as it was, so the two halves of the engine disagreed about the same block and
			// the half that decides the answer was the wrong one.
			Instr::Block { ty, end_pc } => {
				let base = stack.len().saturating_sub(block_params(module, ty));
				ctrl.push(Frame { base, is_loop: false, arity: block_results(module, ty), target: *end_pc + 1 });
			}
			// A loop's label targets the loop itself, so a branch to it re-supplies the PARAMETERS
			// rather than delivering the results - which is why its arity is `block_params` where
			// the other two use `block_results`.
			Instr::Loop { ty } => {
				let params = block_params(module, ty);
				ctrl.push(Frame { base: stack.len().saturating_sub(params), is_loop: true, arity: params, target: pc });
			}
			Instr::If { ty, has_else, else_pc, end_pc } => {
				let cond: i32 = pop(stack)?.as_i32();
				let arity = block_results(module, ty);
				// After the condition is popped, and after the parameters: the condition is not one
				// of them - `if` is `[t1* i32] -> [t2*]` and only the `t1*` reach the arms.
				let base = stack.len().saturating_sub(block_params(module, ty));
				if cond != 0 {
					ctrl.push(Frame { base, is_loop: false, arity, target: *end_pc + 1 });
				} else if *has_else {
					ctrl.push(Frame { base, is_loop: false, arity, target: *end_pc + 1 });
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
				let results: Vec<Value> = call(module, code, memory, globals, table, *f, &call_args, host, budget)?;
				stack.extend(results);
			}
			Instr::CallIndirect(type_idx) => {
				// the operand is the table index; the immediate is the signature the call
				// site expects. Trap on an out-of-bounds index, a null entry, or a callee
				// whose actual signature does not match the expected one (the spec semantics).
				let entry: i32 = pop(stack)?.as_i32();
				if entry < 0 || entry as usize >= table.len() {
					return Err(Trap("call_indirect: table index out of bounds"));
				}
				let target: u32 = table[entry as usize].ok_or(Trap("call_indirect: null table entry"))?;
				let expected = module.types.get(*type_idx as usize).ok_or(Trap("call_indirect: unknown type"))?;
				let actual = module.func_type(target).ok_or(Trap("call_indirect: unknown target function"))?;
				if actual != expected {
					return Err(Trap("call_indirect: signature mismatch"));
				}
				let nargs: usize = expected.params.len();
				if stack.len() < nargs {
					return Err(Trap("stack underflow at call_indirect"));
				}
				let call_args: Vec<Value> = stack.split_off(stack.len() - nargs);
				let results: Vec<Value> = call(module, code, memory, globals, table, target, &call_args, host, budget)?;
				stack.extend(results);
			}
			Instr::Drop => {
				pop(stack)?;
			}
			Instr::Select => {
				let c: i32 = pop(stack)?.as_i32();
				let b: Value = pop(stack)?;
				let a: Value = pop(stack)?;
				stack.push(if c != 0 { a } else { b });
			}
			// The typed form runs exactly as the untyped one does: validation has already proved the
			// operands are the declared type, so there is nothing left for execution to check.
			Instr::SelectTyped(_) => {
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
			Instr::Store { width, offset, .. } => {
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
				// BOUNDED, AND IT ANSWERS -1.
				//
				// This was `memory.resize(memory.len() + delta * PAGE, 0)` with `delta` off the
				// guest's stack: the declared maximum was not consulted, the host imposed none, the
				// multiplication was unchecked, and `resize` is infallible. So a module asking to
				// grow by a large number of pages did not get the failure code WebAssembly defines
				// for exactly this - it took the host process down with it. That is a denial of
				// service reachable from any component, in a service that runs other people's code.
				//
				// The specification's answer to a refused grow is -1 and an unchanged memory, which
				// is what a guest is written to handle. All four reasons to refuse give it:
				// arithmetic that would wrap, the module's own maximum, the host's ceiling, and an
				// allocation the host cannot make.
				let delta: usize = pop(stack)?.as_i32() as u32 as usize;
				let old_pages: usize = memory.len() / PAGE;
				let refused = Value::I32(-1);
				let Some(new_pages) = old_pages.checked_add(delta) else {
					stack.push(refused);
					continue;
				};
				let declared = module.memory.and_then(|m| m.max_pages).unwrap_or(u32::MAX) as usize;
				if new_pages > declared.min(MAX_MEMORY_PAGES) {
					stack.push(refused);
					continue;
				}
				let Some(new_bytes) = new_pages.checked_mul(PAGE) else {
					stack.push(refused);
					continue;
				};
				// CHARGED BEFORE THE RESERVATION, which is the ordering the comment below already
				// argues for and the code did not have.
				//
				// The reservation came first and the charge after it, so a guest with too little
				// fuel still got the host to reserve the memory and only then trapped - and the
				// reservation STAYS. "A refused grow does no work" is the rule; a refusal that has
				// already made the host commit sixty-four megabytes is not one.
				budget.charge_bulk(new_bytes - memory.len())?;
				// `try_reserve` before `resize`, so a host that cannot find the memory answers the
				// guest instead of aborting under it.
				if memory.try_reserve(new_bytes.saturating_sub(memory.len())).is_err() {
					stack.push(refused);
					continue;
				}
				// AND CHARGED FOR THE PAGES, like the bulk operations below.
				//
				// `resize` zeroes every byte it adds, so this instruction's cost is bounded by an
				// operand - which is the rule that put `memory.copy` and `memory.fill` in the bulk
				// class. It was costing one unit for up to sixty-four megabytes of zeroing while a
				// `memory.fill` over the same bytes cost 65,000, so the budget priced the same work
				// two ways depending on which instruction asked for it.
				//
				// Charged only on the path that grows: a refused grow does no work, and charging it
				// would let a rejected request drain the budget. The charge itself has moved above
				// the reservation - see there.
				memory.resize(new_bytes, 0);
				stack.push(Value::I32(old_pages as i32));
			}
			// CHARGED FOR WHAT THEY TOUCH. `Budget::charge` takes one unit per instruction, and a
			// `memory.copy` or `memory.fill` over sixty-four megabytes cost the same one unit as a
			// `nop` - so a loop of bulk copies spent its fuel at a millionth of the rate the budget
			// assumes. The wall still stopped `loop br 0 end`, which was the item; it was not a
			// work budget, and anything whose cost is bounded by an OPERAND belongs in this class.
			Instr::MemoryCopy => {
				let n: usize = pop(stack)?.as_i32() as u32 as usize;
				let s: usize = pop(stack)?.as_i32() as u32 as usize;
				let d: usize = pop(stack)?.as_i32() as u32 as usize;
				if s.saturating_add(n) > memory.len() || d.saturating_add(n) > memory.len() {
					return Err(Trap("memory.copy out of bounds"));
				}
				budget.charge_bulk(n)?;
				memory.copy_within(s..s + n, d);
			}
			Instr::MemoryFill => {
				let n: usize = pop(stack)?.as_i32() as u32 as usize;
				let val: u8 = pop(stack)?.as_i32() as u8;
				let d: usize = pop(stack)?.as_i32() as u32 as usize;
				if d.saturating_add(n) > memory.len() {
					return Err(Trap("memory.fill out of bounds"));
				}
				budget.charge_bulk(n)?;
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
	let keep: usize = frame.arity;
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

// Round toward zero. Exact: a finite value at or beyond 2^63 is already integral, and below that
// the cast through i64 truncates toward zero.
//
// THE SIGN OF A ZERO RESULT IS THE INPUT'S. `(-0.5) as i64` is 0 and `0 as f64` is +0.0, so this
// used to answer `trunc(-0.5) = +0.0` where the specification says `-0.0` - and the same for every
// subnormal, whose truncation is a zero of its own sign. It is a one-bit difference that
// `copysign`, `1.0/x` and any component comparing bit patterns can see.
fn trunc_f64(x: f64) -> f64 {
	let magnitude: f64 = f64::from_bits(x.to_bits() & 0x7fff_ffff_ffff_ffff);
	if !x.is_finite() || magnitude >= 9_223_372_036_854_775_808.0 {
		return x;
	}
	copysign_f64((x as i64) as f64, x)
}

fn floor_f64(x: f64) -> f64 {
	let t: f64 = trunc_f64(x);
	if t > x { t - 1.0 } else { t }
}

// `ceil(-0.5)` is `-0.0`, not `+0.0`: a negative input whose ceiling is zero keeps its sign. The
// same `copysign` for the same reason as `trunc`, applied where `t - 1.0` / `t + 1.0` cannot
// produce a zero.
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
	let rounded: f64 = if diff > 0.5 {
		t + 1.0
	} else if diff < -0.5 {
		t - 1.0
	} else if diff == 0.5 {
		if (t as i64) % 2 == 0 { t } else { t + 1.0 }
	} else if diff == -0.5 {
		if (t as i64) % 2 == 0 { t } else { t - 1.0 }
	} else {
		t
	};
	// `nearest(-0.3)` is `-0.0`. The arithmetic above produces `+0.0` for every negative input that
	// rounds to zero, which is the same signed-zero defect as `trunc`'s.
	if rounded == 0.0 { copysign_f64(0.0, x) } else { rounded }
}

// CORRECTLY ROUNDED square root, with no libm and no intrinsics.
//
// This was Newton-Raphson from an exponent-halving seed, six iterations, "within ~1 ULP, which the
// runtime accepts in exchange for being self-contained". A component that computes a different last
// bit here than everywhere else is a portability defect in an ABI that is supposed to be a
// contract - and the accuracy was accepted for a reason that turns out not to hold: being
// self-contained does not require being approximate.
//
// Newton still produces the estimate. What is new is the finish: the exact residual of a candidate
// is computable in f64 arithmetic alone (Dekker's splitting gives an exact product as a pair of
// doubles), so the estimate can be corrected to the true floor and the choice between it and its
// successor decided by comparing the exact midpoint's square against the input. That is the
// definition of correct rounding, evaluated rather than approached.
//
// `sqrt_is_correctly_rounded_against_the_host` checks every answer against the platform's own
// `f64::sqrt` over a corpus that includes subnormals, powers of two and the values a Newton
// iteration is worst at.
fn sqrt_f64(x: f64) -> f64 {
	if x.is_nan() || x < 0.0 {
		return f64::NAN;
	}
	if x == 0.0 || x.is_infinite() {
		return x; // sqrt(+-0) = +-0, sqrt(+inf) = +inf
	}
	// SCALE A SUBNORMAL UP FIRST. The seed below reads the biased exponent, which a subnormal does
	// not have, and every product in the correction would underflow. Scaling by 2^106 (an even
	// power, so the square root scales by exactly 2^53) moves the whole computation into the normal
	// range and back.
	let (x, unscale): (f64, f64) = if x < f64::MIN_POSITIVE { (x * 81_129_638_414_606_681_695_789_005_144_064.0, 1.0 / 9_007_199_254_740_992.0) } else { (x, 1.0) };

	// Seed: halve the biased exponent in the bit pattern for a ~10%-accurate guess.
	let mut y: f64 = f64::from_bits((x.to_bits() + (1023u64 << 52)) >> 1);
	for _ in 0..6 {
		y = 0.5 * (y + x / y);
	}

	// STEP TO THE FLOOR. After Newton the estimate is within an ulp either way; at most a couple of
	// steps put it on the largest float whose square does not exceed `x`.
	while residual_sign(y, x) > 0 {
		y = next_down(y);
	}
	while residual_sign(next_up(y), x) <= 0 {
		y = next_up(y);
	}
	if residual_sign(y, x) == 0 {
		return y * unscale; // exactly representable
	}

	// CHOOSE BETWEEN `y` AND ITS SUCCESSOR by the exact midpoint. `m = y + ulp/2` is not a double,
	// but it is exactly the sum of two doubles - and `two_sum`/`two_prod` square such a pair
	// exactly, so `m^2` can be compared with `x` with no rounding anywhere in the comparison.
	let half_ulp: f64 = (next_up(y) - y) * 0.5;
	match midpoint_square_cmp(y, half_ulp, x) {
		// The midpoint's square is below `x`, so the true root is above the midpoint.
		core::cmp::Ordering::Less => next_up(y) * unscale,
		core::cmp::Ordering::Greater => y * unscale,
		// A tie: the root is exactly halfway, so round to even.
		core::cmp::Ordering::Equal => {
			if y.to_bits() & 1 == 0 {
				y * unscale
			} else {
				next_up(y) * unscale
			}
		}
	}
}

// The float helpers, for the tests that compare them with the hardware.
#[cfg(test)]
pub fn sqrt_f64_for_test(x: f64) -> f64 {
	sqrt_f64(x)
}

#[cfg(test)]
pub fn trunc_f64_for_test(x: f64) -> f64 {
	trunc_f64(x)
}

#[cfg(test)]
pub fn ceil_f64_for_test(x: f64) -> f64 {
	ceil_f64(x)
}

#[cfg(test)]
pub fn nearest_f64_for_test(x: f64) -> f64 {
	nearest_f64(x)
}

// Dekker's splitting: `v = hi + lo` exactly, with `hi` holding the top 26 bits.
fn split(v: f64) -> (f64, f64) {
	let c: f64 = 134_217_729.0 * v; // 2^27 + 1
	let hi: f64 = c - (c - v);
	(hi, v - hi)
}

// `a * b` as an exact pair: the rounded product and the error it dropped.
fn two_prod(a: f64, b: f64) -> (f64, f64) {
	let p: f64 = a * b;
	let (ah, al) = split(a);
	let (bh, bl) = split(b);
	let e: f64 = ((ah * bh - p) + ah * bl + al * bh) + al * bl;
	(p, e)
}

// `a + b` as an exact pair (Knuth's two-sum), for any two doubles.
fn two_sum(a: f64, b: f64) -> (f64, f64) {
	let s: f64 = a + b;
	let bb: f64 = s - a;
	let e: f64 = (a - (s - bb)) + (b - bb);
	(s, e)
}

// The sign of `y*y - x`, computed exactly: negative, zero or positive.
fn residual_sign(y: f64, x: f64) -> i32 {
	let (p, e) = two_prod(y, y);
	let (s, se) = two_sum(p - x, e);
	if s > 0.0 || (s == 0.0 && se > 0.0) {
		1
	} else if s < 0.0 || (s == 0.0 && se < 0.0) {
		-1
	} else {
		0
	}
}

// Compare `(y + half_ulp)^2` with `x`, exactly.
//
// `m = y + half_ulp` is exact as a pair, so `m^2 = y^2 + 2*y*half_ulp + half_ulp^2` and each of the
// three terms is an exact pair. Summing the six pieces against `-x` with `two_sum` keeps the whole
// comparison free of rounding.
fn midpoint_square_cmp(y: f64, half_ulp: f64, x: f64) -> core::cmp::Ordering {
	let (p0, e0) = two_prod(y, y);
	let (p1, e1) = two_prod(2.0 * y, half_ulp);
	let (p2, e2) = two_prod(half_ulp, half_ulp);
	// Accumulate as a running (sum, error) pair, largest term first so the errors stay small.
	let mut sum: f64 = p0 - x;
	let mut err: f64 = 0.0;
	for term in [e0, p1, e1, p2, e2] {
		let (s, e) = two_sum(sum, term);
		sum = s;
		err += e;
	}
	let (s, e) = two_sum(sum, err);
	if s > 0.0 || (s == 0.0 && e > 0.0) {
		core::cmp::Ordering::Greater
	} else if s < 0.0 || (s == 0.0 && e < 0.0) {
		core::cmp::Ordering::Less
	} else {
		core::cmp::Ordering::Equal
	}
}

// The next representable double above / below a POSITIVE finite value. Only that case is needed:
// every caller here has already handled zero, negatives, infinities and NaN.
fn next_up(x: f64) -> f64 {
	f64::from_bits(x.to_bits() + 1)
}

fn next_down(x: f64) -> f64 {
	f64::from_bits(x.to_bits() - 1)
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

// How many values a block's type leaves on the stack, and how many it takes.
//
// The decoder used to answer these two numbers and nothing else; it now carries the TYPE and these
// derive the counts, so the interpreter is unchanged in what it needs while the validator gains what
// it could not reconstruct.
// Both counts from ONE resolution - `BlockType::arity`, which the validator's `block_signature` also
// goes through. These were two separate matches over `BlockType` beside a third in the validator, and
// the three disagreeing is what put a wrong answer behind a valid module.
//
// `usize` rather than `u8`, here and in `Frame::arity`: a block type with 256 results wrapped to zero
// through `as u8`, and the engine's own limits already assume more than 255 values can be live - the
// validator caps the operand stack at 8192.
fn block_results(module: &Module, ty: &crate::decode::BlockType) -> usize {
	ty.arity(module).1
}

fn block_params(module: &Module, ty: &crate::decode::BlockType) -> usize {
	ty.arity(module).0
}

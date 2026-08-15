// The validation pass, and the type that proves it ran.
//
// WebAssembly's own design puts a VALIDATION stage between decoding and execution, and this engine
// had a decode stage and an execution stage with nothing between them: `Instance::new` took a
// `Module` that had been parsed and structurally decoded, and ran it. Every index was checked when
// execution reached it, which means a module could be instantiated and partially run before it was
// known to be malformed - and no operand-stack type was checked at all, so a function declared
// `(param i32 i32)` could receive whatever its caller left on the stack.
//
//     bytes -> parse -> Module -> VALIDATE -> ValidatedModule -> Instance::new -> interpret
//
// `Instance::new` takes only a `ValidatedModule`, so the type system carries the guarantee rather
// than a convention: there is no way to reach the interpreter with a module nothing checked.
//
// The alternative - adding each check to whichever of the two passes was convenient - is how a
// validator gets written badly: the invariants end up stated nowhere, and the next instruction
// added is validated by whoever remembers.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::decode::{Instr, decode};
use crate::module::{ExportKind, Module, ValType};

// A validation failure: which function it was in, which instruction, and which rule was broken.
//
// `ParseError` and `Trap` carry a `&'static str` and nothing else, which is a refusal nobody can
// act on once there are a great many of them. This one says where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
	// The defined-function index the error was found in, when it was found in a body.
	pub func: Option<u32>,
	// The instruction index within that body, when applicable.
	pub instr: Option<usize>,
	pub reason: String,
}

impl ValidationError {
	fn module(reason: impl Into<String>) -> Self {
		Self { func: None, instr: None, reason: reason.into() }
	}
}

impl core::fmt::Display for ValidationError {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		match (self.func, self.instr) {
			(Some(func), Some(instr)) => write!(f, "function {func}, instruction {instr}: {}", self.reason),
			(Some(func), None) => write!(f, "function {func}: {}", self.reason),
			_ => write!(f, "{}", self.reason),
		}
	}
}

// THE HOST'S limits, which are not the module's.
//
// A maximum a module declares is a statement about itself; these are what this engine will run
// whatever the module says. They live here rather than in the interpreter because a limit is only
// meaningful once something refuses a module for exceeding it.
// The deepest the operand stack may go inside one body.
//
// The per-instance limits item NAMED this - "maximum operand stack depth" - and its DONE note listed
// four constants, none of which is one. Call depth is bounded, which covers the recursive case; what
// was not bounded is one body that pushes without popping, and the two vectors that grow with it are
// the validator's `Vec<Type>` and the interpreter's `Vec<Value>`.
//
// Checked HERE, where the maximum depth of a body is a static property, so execution needs no check
// of its own. It is deliberately generous: a body reaching this has thousands of live operands,
// which no compiler emits and a hand-written module has to mean.
pub const MAX_STACK_DEPTH: usize = 8192;
// How deep blocks may nest inside one body.
//
// The operand stack bounds itself and does NOT bound this, which is what made it a hole. Entering a
// block pops its parameters and pushes them straight back, so a body nesting blocks of a type with
// 8192 parameters and 8192 results holds the stack at exactly 8192 forever while every frame clones
// three type vectors - about 24 KiB of validator allocation for the two bytes that encode the block.
// A few hundred kilobytes of hostile module then asks the host for gigabytes, which is the same
// allocation amplification `br_table` was fixed for, in the structure that walks it.
//
// A bound rather than a restructure: making the frame borrow its signatures instead of owning them
// divides the constant by three and leaves the growth linear in a number the module chooses, which
// is the part that matters.
pub const MAX_CONTROL_DEPTH: usize = 1024;
pub const MAX_LOCALS: usize = 4096;
pub const MAX_TABLE_ENTRIES: usize = 65536;
pub const MAX_FUNCTIONS: usize = 65536;
pub const MAX_GLOBALS: usize = 4096;

// A module that has passed validation, and the decoded body of each of its functions.
//
// The decode happens here rather than in `Instance::new` because validation has to walk the
// instruction stream anyway, and decoding twice would let the two walks disagree about what they
// were looking at.
#[derive(Clone, Debug)]
pub struct ValidatedModule {
	module: Module,
	code: Vec<Vec<Instr>>,
}

impl ValidatedModule {
	pub fn module(&self) -> &Module {
		&self.module
	}

	pub fn code(&self) -> &[Vec<Instr>] {
		&self.code
	}
}

// What the operand-stack simulation tracks. `Unknown` is the specification's polymorphic value: it
// arises after `unreachable` or an unconditional branch, where the stack shape is whatever the
// unreachable code would have produced, and it unifies with everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Type {
	Val(ValType),
	Unknown,
}

impl Type {
	fn matches(self, want: ValType) -> bool {
		match self {
			Type::Unknown => true,
			Type::Val(have) => have == want,
		}
	}
}

// One control frame during validation: what a branch to it carries, the stack height when it
// opened, and whether the code after it is reachable.
struct Frame {
	// The types a branch targeting this frame must carry. For a `loop` these are its PARAMETERS - a
	// branch to a loop restarts it - and for everything else its results.
	label_types: Vec<ValType>,
	// The types the frame was entered with - its block type's PARAMETERS, which are pushed inside
	// the frame and become its own operands.
	//
	// The specification's validation algorithm carries them for two reasons and this carried
	// neither. `else` starts a second arm from the same stack the first one started from, so the
	// parameters have to be pushed again; without them the `then` arm saw them and the `else` arm
	// did not, and a valid parameterised `if` was refused. And an `if` with no `else` is legal
	// exactly when its empty branch typechecks - `start_types == end_types`, which for
	// `[i32] -> [i32]` it does, the parameter BEING the result.
	start_types: Vec<ValType>,
	// The types on the stack when the frame ends normally.
	end_types: Vec<ValType>,
	height: usize,
	// Set by `unreachable` or an unconditional branch: the rest of this frame is polymorphic.
	unreachable: bool,
	is_if: bool,
	// An `if` with no `else` must produce exactly what it consumes.
	had_else: bool,
}

// Validate `module`, or say which rule it broke.
pub fn validate(module: Module) -> Result<ValidatedModule, ValidationError> {
	// The host's own ceilings first, because everything after them walks these lists.
	if module.funcs.len() > MAX_FUNCTIONS {
		return Err(ValidationError::module(format!("a module with {} defined functions exceeds the host limit of {MAX_FUNCTIONS}", module.funcs.len())));
	}
	// THE HOST'S MEMORY CEILING, ON THE DECLARED MINIMUM.
	//
	// `MAX_MEMORY_PAGES` appeared exactly twice - its definition and `memory.grow` - so a module
	// declaring `(memory 100000)` was a six-byte declaration asking the host for six gigabytes
	// before a single instruction ran. `try_reserve_exact` turned a failure into a refusal rather
	// than an abort, which is why this is a resource-policy hole and not a crash; the policy this
	// milestone states is "a limit the module declares is not a limit the host imposed", and there
	// the module's declaration WAS the limit.
	//
	// Checked here rather than at instantiation so the refusal arrives with the module and carries a
	// `ValidationError` naming the rule.
	if let Some(memory) = module.memory {
		if memory.min_pages as usize > crate::interp::MAX_MEMORY_PAGES {
			return Err(ValidationError::module(format!("a module declaring {} initial memory pages exceeds the host limit of {}", memory.min_pages, crate::interp::MAX_MEMORY_PAGES)));
		}
		if let Some(max) = memory.max_pages
			&& max as usize > crate::interp::MAX_MEMORY_PAGES
		{
			return Err(ValidationError::module(format!("a module declaring a maximum of {max} memory pages exceeds the host limit of {}", crate::interp::MAX_MEMORY_PAGES)));
		}
	}
	if module.globals.len() > MAX_GLOBALS {
		return Err(ValidationError::module(format!("a module with {} globals exceeds the host limit of {MAX_GLOBALS}", module.globals.len())));
	}

	// EVERY INDEX, HERE, rather than as a trap when execution reaches it.
	for (index, import) in module.imports.iter().enumerate() {
		if import.type_index as usize >= module.types.len() {
			return Err(ValidationError::module(format!("import {index} names type {} of {}", import.type_index, module.types.len())));
		}
	}
	for (index, func) in module.funcs.iter().enumerate() {
		if func.type_index as usize >= module.types.len() {
			return Err(ValidationError::module(format!("function {index} names type {} of {}", func.type_index, module.types.len())));
		}
	}
	let total_funcs = module.imports.len() + module.funcs.len();
	let has_memory = module.memory.is_some();
	for export in &module.exports {
		match export.kind {
			ExportKind::Func if export.index as usize >= total_funcs => {
				return Err(ValidationError::module(format!("export \"{}\" names function {} of {total_funcs}", export.name, export.index)));
			}
			// THE INDEX AS WELL AS THE EXISTENCE. This checked only that the module declared A
			// memory, so `(memory 1) (export "mem" (memory 1))` validated - and there is no memory 1,
			// because one memory is memory 0 and this engine supports exactly one. Every other kind
			// of export had its index checked; this one had the question it asks written as the
			// easier half.
			ExportKind::Memory if !has_memory || export.index != 0 => {
				return Err(ValidationError::module(format!("export \"{}\" names memory {}, and the module declares {}", export.name, export.index, usize::from(has_memory))));
			}
			// THE INDEX IS CHECKED, which is the rule; whether an embedder can reach the item is a
			// separate question and not this one's.
			//
			// These two used to arrive as a catch-all `Other` that the validator matched with
			// `_ => {}`, so they were the one place where "every static index is validated before an
			// instance exists" was not true. REFUSING them was tried and is wrong: every Rust wasm
			// artifact exports `__data_end` and `__heap_base` as globals, so a rule against global
			// exports is a rule against the toolchain this host is built to run. A dangling one is
			// still a malformed module, and that is what is refused.
			ExportKind::Table if export.index as usize >= module.table.iter().count() => {
				return Err(ValidationError::module(format!("export \"{}\" names table {}, and the module declares {}", export.name, export.index, module.table.iter().count())));
			}
			// No global imports are supported, so the defined globals are the whole index space.
			ExportKind::Global if export.index as usize >= module.globals.len() => {
				return Err(ValidationError::module(format!("export \"{}\" names global {} of {}", export.name, export.index, module.globals.len())));
			}
			ExportKind::Func | ExportKind::Memory | ExportKind::Table | ExportKind::Global => {}
		}
	}
	// EVERY EXPORT NAME IN A MODULE IS DISTINCT. A module-level rule, and the only export rule that
	// cannot be decided from one entry: two exports named `run` make `Module::export_func` answer
	// whichever came first, which is a host resolving a name to an implementation the module never
	// uniquely named.
	//
	// A SET rather than a pairwise scan - a module may declare as many exports as its section holds,
	// and a quadratic comparison over them is work an untrusted input chooses the size of. A
	// `BTreeSet` rather than sorting a vector, because `slice::sort_unstable` instantiates the whole
	// pattern-defeating sort in this crate, and this crate is a shared library whose every imported
	// symbol must have a declared provider.
	let mut seen: alloc::collections::BTreeSet<&str> = alloc::collections::BTreeSet::new();
	for export in &module.exports {
		if !seen.insert(export.name.as_str()) {
			return Err(ValidationError::module(format!("two exports share the name \"{}\"", export.name)));
		}
	}

	// THE TABLE, and the element segments bounded BY it rather than able to extend it.
	//
	// `Instance::new` sized the table to "the larger of the declared minimum and the highest
	// write", so a segment past the end silently grew the table beyond what the module declared and
	// past what its maximum permitted. A declared limit a segment can raise is not a limit.
	let table_len = module.table.as_ref().map(|t| t.min as usize).unwrap_or(0);
	if table_len > MAX_TABLE_ENTRIES {
		return Err(ValidationError::module(format!("a table of {table_len} entries exceeds the host limit of {MAX_TABLE_ENTRIES}")));
	}
	if let Some(table) = &module.table {
		if table.max.is_some_and(|max| max < table.min) {
			return Err(ValidationError::module("a table whose maximum is below its minimum"));
		}
	}
	for (index, element) in module.elements.iter().enumerate() {
		let end = (element.offset as usize).saturating_add(element.funcs.len());
		if end > table_len {
			return Err(ValidationError::module(format!("element segment {index} writes entries {}..{end} of a table declared with {table_len}", element.offset)));
		}
		for &func in &element.funcs {
			if func as usize >= total_funcs {
				return Err(ValidationError::module(format!("element segment {index} names function {func} of {total_funcs}")));
			}
		}
	}

	// The data segments, against the memory the module DECLARES rather than the memory that happens
	// to exist at instantiation.
	// A DATA SEGMENT NAMES A MEMORY, and a module with none has no memory 0 for it to name. The
	// bound below is a byte comparison, so a module with no memory and a zero-length segment at
	// offset 0 passed it - which the specification calls "unknown memory 0".
	if !module.data.is_empty() && module.memory.is_none() {
		return Err(ValidationError::module(String::from("a data segment in a module that declares no memory")));
	}
	let memory_bytes = module.memory.map_or(0, |m| m.min_pages as usize).saturating_mul(crate::interp::PAGE);
	for (index, seg) in module.data.iter().enumerate() {
		let end = (seg.offset as usize).saturating_add(seg.bytes.len());
		if end > memory_bytes {
			return Err(ValidationError::module(format!("data segment {index} writes bytes {}..{end} of a memory declared with {memory_bytes}", seg.offset)));
		}
	}

	// Decode and type-check every body.
	let mut code: Vec<Vec<Instr>> = Vec::with_capacity(module.funcs.len());
	for index in 0..module.funcs.len() {
		let body = decode(&module, index).map_err(|reason| ValidationError { func: Some(index as u32), instr: None, reason: String::from(reason) })?;
		check_body(&module, index, &body)?;
		code.push(body);
	}

	Ok(ValidatedModule { module, code })
}

// Type-check one function body: operand-stack types through every instruction, block and loop
// parameter and result types, branch label arity, and the stack shape at every `end`.
fn check_body(module: &Module, func_index: usize, body: &[Instr]) -> Result<(), ValidationError> {
	let func = &module.funcs[func_index];
	let ftype = &module.types[func.type_index as usize];
	if func.locals.len() + ftype.params.len() > MAX_LOCALS {
		return Err(ValidationError { func: Some(func_index as u32), instr: None, reason: format!("a frame of {} locals exceeds the host limit of {MAX_LOCALS}", func.locals.len() + ftype.params.len()) });
	}
	// Parameters first, then the declared locals - the interpreter's own layout.
	let mut locals: Vec<ValType> = Vec::with_capacity(ftype.params.len() + func.locals.len());
	locals.extend_from_slice(&ftype.params);
	locals.extend_from_slice(&func.locals);

	let mut stack: Vec<Type> = Vec::new();
	let mut frames: Vec<Frame> = Vec::new();
	// The implicit function-level frame: a branch to it returns, so it carries the results. Its
	// parameters are the function's LOCALS rather than stack operands, so it starts empty - and
	// `start_types` is only read for an `if`, which this frame is not.
	frames.push(Frame { label_types: ftype.results.clone(), start_types: Vec::new(), end_types: ftype.results.clone(), height: 0, unreachable: false, is_if: false, had_else: false });

	let total_funcs = module.imports.len() + module.funcs.len();
	let has_memory = module.memory.is_some();

	for (pc, instr) in body.iter().enumerate() {
		let at = |reason: String| ValidationError { func: Some(func_index as u32), instr: Some(pc), reason };
		// ONE CHECK, AFTER each instruction rather than at each of the twenty places that push: an
		// instruction adds a bounded number of operands, so testing the depth once per instruction
		// bounds it within one instruction's worth - which is what a ceiling needs to do and is a
		// rule a new opcode cannot forget.
		if stack.len() > MAX_STACK_DEPTH {
			return Err(at(format!("the operand stack reaches {}, past the host limit of {MAX_STACK_DEPTH}", stack.len())));
		}
		match instr {
			Instr::Unreachable => mark_unreachable(&mut stack, &mut frames),
			Instr::Nop => {}
			Instr::Block { .. } | Instr::Loop { .. } | Instr::If { .. } => {
				// `if` pops its condition before its parameters, because the condition is pushed
				// last.
				if matches!(instr, Instr::If { .. }) {
					pop_expect(&mut stack, &frames, &[ValType::I32]).map_err(&at)?;
				}
				// `frames` starts at one - the function's own frame, which is not a block - so the
				// blocks open are `frames.len() - 1` and this one would make `frames.len()`.
				if frames.len() > MAX_CONTROL_DEPTH {
					return Err(at(format!("blocks nest {} deep, past the host limit of {MAX_CONTROL_DEPTH}", frames.len())));
				}
				let (params, results) = block_signature(module, instr).map_err(&at)?;
				pop_expect(&mut stack, &frames, &params).map_err(&at)?;
				let label_types = if matches!(instr, Instr::Loop { .. }) { params.clone() } else { results.clone() };
				let height = stack.len();
				for param in &params {
					stack.push(Type::Val(*param));
				}
				frames.push(Frame { label_types, start_types: params, end_types: results, height, unreachable: false, is_if: matches!(instr, Instr::If { .. }), had_else: false });
			}
			Instr::Else { .. } => {
				let Some(frame) = frames.last_mut() else {
					return Err(at(String::from("else outside a block")));
				};
				if !frame.is_if {
					return Err(at(String::from("else without an if")));
				}
				if frame.had_else {
					return Err(at(String::from("a second else for one if")));
				}
				frame.had_else = true;
				let end_types = frame.end_types.clone();
				let start_types = frame.start_types.clone();
				let height = frame.height;
				let unreachable = frame.unreachable;
				// The then arm must have produced the block's results, unless it ended in
				// unreachable code - and the else arm then starts from the stack the then arm did.
				if !unreachable {
					expect_at(&stack, height, &end_types).map_err(&at)?;
				}
				stack.truncate(height);
				// AND THE PARAMETERS GO BACK. Both arms of an `if` are entered with the block's
				// parameters on the stack; truncating to `height` and stopping there gave them to
				// the `then` arm only, so a valid `(param i32) (result i32) if ... else ... end`
				// was refused for a stack underflow the module does not have.
				for param in &start_types {
					stack.push(Type::Val(*param));
				}
				frames.last_mut().expect("checked above").unreachable = false;
			}
			Instr::End => {
				let Some(frame) = frames.pop() else {
					return Err(at(String::from("end without a block")));
				};
				// An `if` with no `else` is legal exactly when its ABSENT arm typechecks, which is
				// `start_types == end_types`: the missing arm does nothing, so what it produces is
				// what it was entered with.
				//
				// This tested `!end_types.is_empty()`, which is the same rule only for blocks with
				// no parameters. `[i32] -> [i32]` has a legal empty branch - the parameter IS the
				// result - and the binary format permits omitting `else` for exactly that case.
				// Requiring no results is stricter than requiring the empty branch to typecheck.
				if frame.is_if && !frame.had_else && frame.start_types != frame.end_types {
					return Err(at(format!("an if whose missing else arm would leave {:?} where the block produces {:?}", frame.start_types, frame.end_types)));
				}
				if !frame.unreachable {
					expect_at(&stack, frame.height, &frame.end_types).map_err(&at)?;
				}
				stack.truncate(frame.height);
				for result in &frame.end_types {
					stack.push(Type::Val(*result));
				}
			}
			Instr::Br(label) => {
				let types = label_types(&frames, *label).ok_or_else(|| at(format!("branch to label {label} with {} frames in scope", frames.len())))?;
				pop_expect(&mut stack, &frames, &types).map_err(&at)?;
				mark_unreachable(&mut stack, &mut frames);
			}
			Instr::BrIf(label) => {
				pop_expect(&mut stack, &frames, &[ValType::I32]).map_err(&at)?;
				let types = label_types(&frames, *label).ok_or_else(|| at(format!("branch to label {label} with {} frames in scope", frames.len())))?;
				pop_expect(&mut stack, &frames, &types).map_err(&at)?;
				for value in &types {
					stack.push(Type::Val(*value));
				}
			}
			Instr::BrTable { labels, default } => {
				pop_expect(&mut stack, &frames, &[ValType::I32]).map_err(&at)?;
				let want = label_types(&frames, *default).ok_or_else(|| at(format!("br_table default label {default} out of range")))?;
				for label in labels {
					let types = label_types(&frames, *label).ok_or_else(|| at(format!("br_table label {label} out of range")))?;
					// EVERY ARM MUST CARRY THE SAME THING, or what the branch delivers depends on
					// which index the guest chose at run time.
					if types != want {
						return Err(at(format!("br_table label {label} takes {} values where the default takes {}", types.len(), want.len())));
					}
				}
				pop_expect(&mut stack, &frames, &want).map_err(&at)?;
				mark_unreachable(&mut stack, &mut frames);
			}
			Instr::Return => {
				let results = ftype.results.clone();
				pop_expect(&mut stack, &frames, &results).map_err(&at)?;
				mark_unreachable(&mut stack, &mut frames);
			}
			Instr::Call(index) => {
				if *index as usize >= total_funcs {
					return Err(at(format!("call to function {index} of {total_funcs}")));
				}
				let callee = module.func_type(*index).ok_or_else(|| at(format!("call to function {index} whose type is out of range")))?.clone();
				pop_expect(&mut stack, &frames, &callee.params).map_err(&at)?;
				for result in &callee.results {
					stack.push(Type::Val(*result));
				}
			}
			Instr::CallIndirect(type_index) => {
				if module.table.is_none() {
					return Err(at(String::from("call_indirect in a module with no table")));
				}
				let callee = module.types.get(*type_index as usize).ok_or_else(|| at(format!("call_indirect names type {type_index} of {}", module.types.len())))?.clone();
				// The table index is on top, above the arguments.
				pop_expect(&mut stack, &frames, &[ValType::I32]).map_err(&at)?;
				pop_expect(&mut stack, &frames, &callee.params).map_err(&at)?;
				for result in &callee.results {
					stack.push(Type::Val(*result));
				}
			}
			Instr::Drop => {
				pop_any(&mut stack, &frames).map_err(&at)?;
			}
			Instr::Select => {
				pop_expect(&mut stack, &frames, &[ValType::I32]).map_err(&at)?;
				let first = pop_any(&mut stack, &frames).map_err(&at)?;
				let second = pop_any(&mut stack, &frames).map_err(&at)?;
				// The two arms must agree; `Unknown` takes the other's type.
				let result = match (first, second) {
					(Type::Unknown, other) | (other, Type::Unknown) => other,
					(Type::Val(a), Type::Val(b)) if a == b => Type::Val(a),
					(Type::Val(a), Type::Val(b)) => return Err(at(format!("select on a {a:?} and a {b:?}"))),
				};
				stack.push(result);
			}
			// TYPED SELECT: the operands must match the type the instruction DECLARES, not merely
			// each other. The annotation used to be read and discarded, so an annotation asking for
			// an `i64` over two `i32`s was accepted - the declaration checked against nothing.
			Instr::SelectTyped(declared) => {
				pop_expect(&mut stack, &frames, &[ValType::I32]).map_err(&at)?;
				pop_expect(&mut stack, &frames, &[*declared, *declared]).map_err(&at)?;
				stack.push(Type::Val(*declared));
			}
			Instr::LocalGet(index) => {
				let ty = *locals.get(*index as usize).ok_or_else(|| at(format!("local.get {index} of {} locals", locals.len())))?;
				stack.push(Type::Val(ty));
			}
			Instr::LocalSet(index) => {
				let ty = *locals.get(*index as usize).ok_or_else(|| at(format!("local.set {index} of {} locals", locals.len())))?;
				pop_expect(&mut stack, &frames, &[ty]).map_err(&at)?;
			}
			Instr::LocalTee(index) => {
				let ty = *locals.get(*index as usize).ok_or_else(|| at(format!("local.tee {index} of {} locals", locals.len())))?;
				pop_expect(&mut stack, &frames, &[ty]).map_err(&at)?;
				stack.push(Type::Val(ty));
			}
			Instr::GlobalGet(index) => {
				let global = module.globals.get(*index as usize).ok_or_else(|| at(format!("global.get {index} of {} globals", module.globals.len())))?;
				stack.push(Type::Val(global.val_type));
			}
			Instr::GlobalSet(index) => {
				let global = module.globals.get(*index as usize).ok_or_else(|| at(format!("global.set {index} of {} globals", module.globals.len())))?;
				// MUTABILITY, which the parser recorded and nothing read. An immutable global is a
				// constant a module may rely on; without this any module could change any of them.
				if !global.mutable {
					return Err(at(format!("global.set on immutable global {index}")));
				}
				let ty = global.val_type;
				pop_expect(&mut stack, &frames, &[ty]).map_err(&at)?;
			}
			Instr::I32Const(_) => stack.push(Type::Val(ValType::I32)),
			Instr::I64Const(_) => stack.push(Type::Val(ValType::I64)),
			Instr::F32Const(_) => stack.push(Type::Val(ValType::F32)),
			Instr::F64Const(_) => stack.push(Type::Val(ValType::F64)),
			Instr::Load { wide, .. } => {
				require_memory(has_memory).map_err(&at)?;
				pop_expect(&mut stack, &frames, &[ValType::I32]).map_err(&at)?;
				stack.push(Type::Val(if *wide { ValType::I64 } else { ValType::I32 }));
			}
			Instr::Store { wide, .. } => {
				require_memory(has_memory).map_err(&at)?;
				// THE OPCODE'S VALUE TYPE, not its storage width.
				//
				// This was `if width == 8 { I64 } else { I32 }`, under a comment observing that the
				// decoder folds `i64.store32` and `i32.store` into one `Store` - and then typing
				// them the same anyway. `i64.store8`, `i64.store16` and `i64.store32` all take an
				// `i64` and narrow it on the way to memory, so all three were refused when written
				// correctly and accepted when given an `i32`.
				//
				// Not a memory-safety defect: the interpreter takes the low `w` bytes of whatever is
				// on the stack and bounds-checks the slice. A validator that refuses valid modules
				// and accepts invalid ones is its own problem.
				let value = if *wide { ValType::I64 } else { ValType::I32 };
				pop_expect(&mut stack, &frames, &[ValType::I32, value]).map_err(&at)?;
			}
			Instr::FLoad { wide, .. } => {
				require_memory(has_memory).map_err(&at)?;
				pop_expect(&mut stack, &frames, &[ValType::I32]).map_err(&at)?;
				stack.push(Type::Val(if *wide { ValType::F64 } else { ValType::F32 }));
			}
			Instr::FStore { wide, .. } => {
				require_memory(has_memory).map_err(&at)?;
				let value = if *wide { ValType::F64 } else { ValType::F32 };
				pop_expect(&mut stack, &frames, &[ValType::I32, value]).map_err(&at)?;
			}
			Instr::MemorySize => {
				require_memory(has_memory).map_err(&at)?;
				stack.push(Type::Val(ValType::I32));
			}
			Instr::MemoryGrow => {
				require_memory(has_memory).map_err(&at)?;
				pop_expect(&mut stack, &frames, &[ValType::I32]).map_err(&at)?;
				stack.push(Type::Val(ValType::I32));
			}
			Instr::MemoryCopy | Instr::MemoryFill => {
				require_memory(has_memory).map_err(&at)?;
				pop_expect(&mut stack, &frames, &[ValType::I32, ValType::I32, ValType::I32]).map_err(&at)?;
			}
			Instr::TruncSat(sub) => {
				let (from, to) = trunc_sat_types(*sub).ok_or_else(|| at(format!("trunc_sat sub-opcode {sub}")))?;
				pop_expect(&mut stack, &frames, &[from]).map_err(&at)?;
				stack.push(Type::Val(to));
			}
			Instr::Num(op) => {
				let (params, results) = num_signature(*op).ok_or_else(|| at(format!("numeric opcode {op:#04x} has no signature")))?;
				pop_expect(&mut stack, &frames, &params).map_err(&at)?;
				for result in results {
					stack.push(Type::Val(result));
				}
			}
		}
	}

	if !frames.is_empty() {
		return Err(ValidationError { func: Some(func_index as u32), instr: None, reason: String::from("a function body that ends inside a block") });
	}
	Ok(())
}

// A block / loop / if's (parameters, results), READ FROM ITS TYPE.
//
// This used to reconstruct a signature it did not have, because the decoder recorded arities. A
// one-result block got `i32` - so `(block (result i64) i64.const 7 end)` was refused and
// `(block (result i64) i32.const 7 end)` was accepted, the validator wrong in both directions at
// once. And an arity above one searched `module.types` for the FIRST candidate with matching counts,
// so a block naming type 3 got type 0 if type 0 had the same shape: the index was present in the
// binary, discarded by the decoder, and re-derived here by a rule that cannot be right.
//
// The decoder carries `BlockType` now, so there is nothing to reconstruct.
fn block_signature(module: &Module, instr: &Instr) -> Result<(Vec<ValType>, Vec<ValType>), String> {
	let ty = match instr {
		Instr::Block { ty, .. } | Instr::If { ty, .. } | Instr::Loop { ty } => ty,
		_ => return Err(String::from("not a block")),
	};
	// THE SAME RESOLUTION THE INTERPRETER USES, cloned into owned vectors because validation needs
	// them to outlive the borrow of `module`. The counts the interpreter derives and the types
	// checked here now come from one place, which is what makes the two halves of the engine unable
	// to disagree about where a block's label sits.
	let (params, results) = ty.signature(module).ok_or_else(|| match ty {
		crate::decode::BlockType::TypeIndex(index) => format!("a block naming type {index} of {}", module.types.len()),
		_ => String::from("a block with no resolvable type"),
	})?;
	Ok((params.to_vec(), results.to_vec()))
}

// The types a branch to `label` must carry (0 = innermost).
fn label_types(frames: &[Frame], label: u32) -> Option<Vec<ValType>> {
	let index = frames.len().checked_sub(1)?.checked_sub(label as usize)?;
	Some(frames[index].label_types.clone())
}

// Everything after an unconditional branch or `unreachable` is polymorphic: the stack is cut back
// to the frame's height and the frame is marked, so pops there succeed with `Unknown`.
fn mark_unreachable(stack: &mut Vec<Type>, frames: &mut [Frame]) {
	if let Some(frame) = frames.last_mut() {
		stack.truncate(frame.height);
		frame.unreachable = true;
	}
}

// Pop one value of any type, or `Unknown` inside unreachable code.
fn pop_any(stack: &mut Vec<Type>, frames: &[Frame]) -> Result<Type, String> {
	let height = frames.last().map(|f| f.height).unwrap_or(0);
	if stack.len() > height {
		return Ok(stack.pop().expect("checked above"));
	}
	if frames.last().is_some_and(|f| f.unreachable) {
		return Ok(Type::Unknown);
	}
	Err(String::from("an instruction that pops from an empty operand stack"))
}

// Pop `want`, given in push order so its last element is on top.
fn pop_expect(stack: &mut Vec<Type>, frames: &[Frame], want: &[ValType]) -> Result<(), String> {
	for expected in want.iter().rev() {
		let got = pop_any(stack, frames)?;
		if !got.matches(*expected) {
			return Err(format!("an instruction wanting a {expected:?} found a {got:?}"));
		}
	}
	Ok(())
}

// Check that exactly `want` sits above `height`, without popping.
fn expect_at(stack: &[Type], height: usize, want: &[ValType]) -> Result<(), String> {
	if stack.len() != height + want.len() {
		return Err(format!("a block that should leave {} value(s) leaves {}", want.len(), stack.len().saturating_sub(height)));
	}
	for (offset, expected) in want.iter().enumerate() {
		let got = stack[height + offset];
		if !got.matches(*expected) {
			return Err(format!("a block declaring a {expected:?} result leaves a {got:?}"));
		}
	}
	Ok(())
}

fn require_memory(has_memory: bool) -> Result<(), String> {
	if has_memory { Ok(()) } else { Err(String::from("a memory instruction in a module with no memory")) }
}

// The (from, to) types of a saturating truncation, by its `0xfc` sub-opcode.
fn trunc_sat_types(sub: u8) -> Option<(ValType, ValType)> {
	Some(match sub {
		0 | 1 => (ValType::F32, ValType::I32),
		2 | 3 => (ValType::F64, ValType::I32),
		4 | 5 => (ValType::F32, ValType::I64),
		6 | 7 => (ValType::F64, ValType::I64),
		_ => return None,
	})
}

// The signature of a numeric opcode: what it pops (in push order) and what it pushes.
//
// One table, so an opcode the interpreter can execute and the validator cannot type shows up as a
// refusal in the `None` arm rather than as an unchecked instruction.
fn num_signature(op: u8) -> Option<(Vec<ValType>, Vec<ValType>)> {
	use ValType::{F32, F64, I32, I64};
	let one = |a: ValType, r: ValType| Some((alloc::vec![a], alloc::vec![r]));
	let two = |a: ValType, b: ValType, r: ValType| Some((alloc::vec![a, b], alloc::vec![r]));
	match op {
		0x45 => one(I32, I32),             // i32.eqz
		0x46..=0x4f => two(I32, I32, I32), // i32 comparisons
		0x50 => one(I64, I32),             // i64.eqz
		0x51..=0x5a => two(I64, I64, I32), // i64 comparisons
		0x5b..=0x60 => two(F32, F32, I32), // f32 comparisons
		0x61..=0x66 => two(F64, F64, I32), // f64 comparisons
		0x67..=0x69 => one(I32, I32),      // i32 clz / ctz / popcnt
		0x6a..=0x78 => two(I32, I32, I32), // i32 arithmetic, bitwise, shifts, rotates
		0x79..=0x7b => one(I64, I64),      // i64 clz / ctz / popcnt
		0x7c..=0x8a => two(I64, I64, I64), // i64 arithmetic, bitwise, shifts, rotates
		0x8b..=0x91 => one(F32, F32),      // f32 unary
		0x92..=0x98 => two(F32, F32, F32), // f32 binary
		0x99..=0x9f => one(F64, F64),      // f64 unary
		0xa0..=0xa6 => two(F64, F64, F64), // f64 binary
		0xa7 => one(I64, I32),             // i32.wrap_i64
		0xa8 | 0xa9 => one(F32, I32),      // i32.trunc_f32_s/u
		0xaa | 0xab => one(F64, I32),      // i32.trunc_f64_s/u
		0xac | 0xad => one(I32, I64),      // i64.extend_i32_s/u
		0xae | 0xaf => one(F32, I64),      // i64.trunc_f32_s/u
		0xb0 | 0xb1 => one(F64, I64),      // i64.trunc_f64_s/u
		0xb2 | 0xb3 => one(I32, F32),      // f32.convert_i32_s/u
		0xb4 | 0xb5 => one(I64, F32),      // f32.convert_i64_s/u
		0xb6 => one(F64, F32),             // f32.demote_f64
		0xb7 | 0xb8 => one(I32, F64),      // f64.convert_i32_s/u
		0xb9 | 0xba => one(I64, F64),      // f64.convert_i64_s/u
		0xbb => one(F32, F64),             // f64.promote_f32
		0xbc => one(F32, I32),             // i32.reinterpret_f32
		0xbd => one(F64, I64),             // i64.reinterpret_f64
		0xbe => one(I32, F32),             // f32.reinterpret_i32
		0xbf => one(I64, F64),             // f64.reinterpret_i64
		0xc0 | 0xc1 => one(I32, I32),      // i32.extend8_s / extend16_s
		0xc2..=0xc4 => one(I64, I64),      // i64.extend8_s / 16_s / 32_s
		_ => None,
	}
}

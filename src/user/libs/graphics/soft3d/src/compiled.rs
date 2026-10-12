//! THE COMPILED SHADER: the same stage lowered ONCE into a flat list of instructions over a flat
//! register file, so that a fragment stops paying for a tree walk it has already been through.
//!
//! WHY IT EXISTS. The interpreter walks the module's statements for every covered pixel: a `match` on
//! the statement, another on the operation, a stamp, a type and a length checked per operand, a type
//! cloned per result, and a component loop that re-decides the operation and the scalar type for every
//! component. The scene this milestone measures spends about three microseconds a fragment there. None
//! of those decisions depends on the pixel - only on the module and on the types its loads answer - so
//! they are made here once, and what runs per pixel is the arithmetic.
//!
//! THE INTERPRETER STAYS THE DEFINITION, AND THIS FILE HAS NO RULES OF ITS OWN TO GET WRONG:
//! - A RESULT'S TYPE AND LENGTH ARE THE INTERPRETER'S. Every operation is typed by running
//!   `interpreter::operation` itself once, at compile time, on zeroed operands of the types they will
//!   have - so a type rule changed there is a type rule changed here.
//! - AN OPERATION WITHOUT A SPECIALISED INSTRUCTION IS THE INTERPRETER'S CODE. It runs as `Generic`,
//!   which calls that same function over this register file. Only the common floating-point shapes a
//!   lit surface is made of have instructions of their own, and each computes the same `f32`
//!   operations in the same order as the interpreter's loop, which the differential tests compare bit
//!   for bit.
//! - A LOAD WHOSE ANSWER IS NOT THE TYPE THE PROGRAM WAS COMPILED FOR IS NOT GUESSED AT. An attribute or
//!   a texture read answering another type ends the compiled run with `Ran::Deopt`, and the caller
//!   runs the interpreter for that invocation instead; a uniform answering another type makes the whole
//!   draw take the interpreter. A shader has no side effects before it returns, so running it again is
//!   the same run.
//! - A VALUE READ BEFORE ITS ASSIGNMENT RAN IS THE SAME FAULT. A read that the structured control flow
//!   cannot prove assigned turns on the per-run stamps the interpreter uses, and the instruction that
//!   reads it becomes `Generic`, whose reads fault in the interpreter's operand order.
//!
//! UNIFORMS ARE READ ONCE PER DRAW. A `Source` is the frame's inputs and nothing writes it while the
//! frame runs (see `frame::Source`), so a uniform answers the same value for every vertex and every
//! fragment of a draw; reading it per pixel was a trait call and a ninety-byte value per load per
//! pixel for one answer.

use alloc::vec::Vec;

use render_shader::ir::{BuiltIn, Interpolation, Output};
use render_shader::{BinaryOp, Binding, Module, Op, ScalarType, Stage, Stmt, Transcendental, Type, UnaryOp, Value};

use crate::interpreter::{self, Fault, Machine, Operands};
use crate::value::{Reg, Val};

const STRIDE: usize = crate::value::INLINE_WORDS;

/// Where one value lives in the register file, and what it is.
#[derive(Clone, PartialEq, Debug)]
struct Slot {
	offset: u32,
	len: u32,
	kind: Type,
}

/// The floating-point binary operations with an instruction of their own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FloatBinary {
	Add,
	Subtract,
	Multiply,
	Divide,
	Modulo,
	Min,
	Max,
}

/// The floating-point unary operations with an instruction of their own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FloatUnary {
	Negate,
	Abs,
	Floor,
	Ceil,
	Fract,
}

/// No second operand.
const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Ins {
	Const {
		dst: u32,
		word: u32,
	},
	/// Words copied in place: an extracted component, a single-part compose.
	Copy {
		dst: u32,
		src: u32,
		len: u32,
	},
	/// Several parts, in order, from `parts[first..first + count]`.
	Compose {
		dst: u32,
		first: u32,
		count: u32,
	},
	/// A uniform from the draw's cache entry.
	Uniform {
		dst: u32,
		entry: u32,
	},
	/// A varying from one of the three interpolated arrays, by the map's offset and length.
	Varying {
		dst: u32,
		array: u8,
		offset: u32,
		len: u32,
	},
	BuiltIn {
		dst: u32,
		which: BuiltIn,
	},
	Attribute {
		dst: u32,
		location: u32,
		value: u32,
	},
	Sample {
		dst: u32,
		texture: u32,
		sampler: u32,
		coordinate: u32,
		value: u32,
	},
	FloatBinary {
		op: FloatBinary,
		dst: u32,
		a: u32,
		b: u32,
		n: u32,
		a_step: u32,
		b_step: u32,
	},
	FloatUnary {
		op: FloatUnary,
		dst: u32,
		src: u32,
		n: u32,
	},
	Dot {
		dst: u32,
		a: u32,
		b: u32,
		n: u32,
	},
	Normalize {
		dst: u32,
		src: u32,
		n: u32,
		len: u32,
	},
	Length {
		dst: u32,
		src: u32,
		len: u32,
	},
	MatrixVector {
		dst: u32,
		matrix: u32,
		vector: u32,
		size: u32,
	},
	Transcendental {
		which: Transcendental,
		dst: u32,
		a: u32,
		b: u32,
		n: u32,
		b_step: u32,
	},
	Clamp {
		dst: u32,
		value: u32,
		low: u32,
		high: u32,
		n: u32,
		value_step: u32,
		low_step: u32,
		high_step: u32,
	},
	Mix {
		dst: u32,
		from: u32,
		to: u32,
		at: u32,
		n: u32,
		at_step: u32,
	},
	Convert {
		dst: u32,
		src: u32,
		n: u32,
		from: ScalarType,
		to: ScalarType,
	},
	/// Any other operation, through the interpreter's own code: `ops[op]` assigned to `value`.
	Generic {
		value: u32,
		op: u32,
	},
	/// Fault with `Unassigned` unless `value` was assigned in this run.
	Check {
		value: u32,
	},
	/// Record that `value` was assigned in this run.
	Mark {
		value: u32,
	},
	Store {
		output: Output,
		value: u32,
	},
	Jump {
		target: u32,
	},
	/// Jump to `target` unless the value's first word is non-zero.
	Unless {
		offset: u32,
		len: u32,
		target: u32,
	},
	/// Jump to `target` when the value's first word, as an `i32`, is `case`.
	Case {
		offset: u32,
		len: u32,
		case: i32,
		target: u32,
	},
	LoopStart {
		counter: u32,
		trips: u32,
	},
	/// Leave the loop for `exit` once its trips are used up; otherwise take one.
	LoopNext {
		counter: u32,
		exit: u32,
	},
	Discard,
	Exit,
	Raise {
		fault: u32,
	},
}

/// One uniform the program reads, and what it must answer to be used.
#[derive(Clone, PartialEq, Debug)]
struct UniformEntry {
	block: u32,
	member: u32,
	kind: Type,
	len: u32,
	offset: u32,
}

/// A stage, compiled.
#[derive(Clone, PartialEq, Debug)]
pub struct Program {
	stage: Stage,
	code: Vec<Ins>,
	/// One per value of the module, by its id.
	slots: Vec<Slot>,
	/// The `(offset, len)` parts of every `Compose`.
	parts: Vec<(u32, u32)>,
	/// The operations `Generic` runs, with the value each assigns.
	ops: Vec<Op>,
	faults: Vec<Fault>,
	uniforms: Vec<UniformEntry>,
	uniform_words: usize,
	words: usize,
	counters: usize,
	/// Whether any read needs the per-run stamps.
	checked: bool,
	/// Straight-line and unstamped: `run_batch` can run it.
	batchable: bool,
}

/// What one compiled run did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ran {
	/// The stage ran; its outputs are in the machine.
	Done,
	/// A load answered a type the program was not compiled for. The interpreter must run this
	/// invocation instead, and its outputs are what the stage produced.
	Deopt,
}

/// The uniforms one draw reads, read once.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Uniforms {
	words: Vec<u32>,
	/// Per entry: whether the source supplied it. An absent one faults where a load of it runs.
	present: Vec<bool>,
	/// False when a supplied uniform is not the type the program was compiled for: the draw then
	/// runs the interpreter, which takes whatever the source answers.
	usable: bool,
}

impl Uniforms {
	/// Storage for one program's uniforms. ALLOCATES - at preparation, never in a frame.
	pub fn for_program(program: &Program) -> Self {
		Self { words: alloc::vec![0; program.uniform_words], present: alloc::vec![false; program.uniforms.len()], usable: false }
	}

	/// Read every uniform the program loads from `source`, once.
	pub fn fill(&mut self, program: &Program, read: impl Fn(u32, u32) -> Option<Val>) {
		self.usable = self.words.len() == program.uniform_words && self.present.len() == program.uniforms.len();
		if !self.usable {
			return;
		}
		for (index, entry) in program.uniforms.iter().enumerate() {
			match read(entry.block, entry.member) {
				Some(value) => {
					if value.kind != entry.kind || value.words().len() != entry.len as usize {
						self.usable = false;
						return;
					}
					let start = entry.offset as usize;
					self.words[start..start + entry.len as usize].copy_from_slice(value.words());
					self.present[index] = true;
				}
				None => self.present[index] = false,
			}
		}
	}

	pub fn usable(&self) -> bool {
		self.usable
	}

	pub(crate) fn reserved_bytes(&self) -> usize {
		self.words.capacity() * core::mem::size_of::<u32>() + self.present.capacity()
	}
}

/// What a compiled stage reads that is not a uniform.
///
/// THE STAGE'S OWN INPUTS, NOT A SECOND RESOURCE MODEL. Each method answers exactly what the stage's
/// `interpreter::Resources` implementation answers for the same question; the varying map and the
/// built-in types were resolved at compile time from the same tables.
pub trait Inputs {
	/// The interpolated values of one qualifier: 0 smooth, 1 no-perspective, 2 flat.
	fn varyings(&self, array: u8) -> &[f32];
	/// A built-in's words, or `None` where the stage does not supply it.
	fn built_in(&self, which: BuiltIn, out: &mut [u32; 4]) -> Option<usize>;
	fn attribute(&self, location: u32) -> Option<Val>;
	fn sample(&self, texture: u32, sampler: u32, coordinate: &Val) -> Option<Val>;
}

/// The type a stage's built-in answers, which must be what its `Resources` builds.
fn built_in_kind(stage: Stage, which: BuiltIn) -> Option<(Type, u32)> {
	match (stage, which) {
		(Stage::Fragment, BuiltIn::FragmentCoordinate) => Some((Type::Vector(ScalarType::F32, 4), 4)),
		(Stage::Fragment, BuiltIn::FrontFacing) => Some((Type::Scalar(ScalarType::Bool), 1)),
		(Stage::Fragment, BuiltIn::SampleIndex | BuiltIn::SampleMaskIn) => Some((Type::Scalar(ScalarType::U32), 1)),
		(Stage::Vertex, BuiltIn::VertexIndex | BuiltIn::InstanceIndex) => Some((Type::Scalar(ScalarType::U32), 1)),
		_ => None,
	}
}

/// The register file at run time, for `Generic`.
struct View<'a> {
	slots: &'a [Slot],
	regs: &'a [u32],
	stamps: Option<(&'a [u32], u32)>,
}

impl Operands for View<'_> {
	fn read(&self, value: Value) -> Result<Reg<'_>, Fault> {
		let index = value.0 as usize;
		let Some(slot) = self.slots.get(index) else { return Err(Fault::Unassigned { value: value.0 }) };
		if let Some((stamps, run)) = self.stamps {
			if stamps.get(index).copied() != Some(run) {
				return Err(Fault::Unassigned { value: value.0 });
			}
		}
		let start = slot.offset as usize;
		Ok(Reg::new(&slot.kind, &self.regs[start..start + slot.len as usize]))
	}
}

/// The varying map `frame` builds per pipeline: location, qualifier, offset, count and type.
pub type VaryingMap = [(u32, Interpolation, usize, usize, Type)];

/// Where a `break` and a `continue` go inside the innermost loop being emitted.
struct LoopContext {
	/// The loop's test, which a `continue` jumps back to.
	top: u32,
	/// The jumps a `break` emitted, patched to the loop's exit once it is known.
	breaks: Vec<u32>,
}

struct Compiler<'a> {
	module: &'a Module,
	map: Option<&'a VaryingMap>,
	code: Vec<Ins>,
	/// Where each value lives once some path assigns it: its offset, length and type. A value no
	/// instruction can complete the assignment of has none.
	slots: Vec<Option<Slot>>,
	next: u32,
	parts: Vec<(u32, u32)>,
	ops: Vec<Op>,
	faults: Vec<Fault>,
	uniforms: Vec<UniformEntry>,
	uniform_words: usize,
	counters: usize,
	checked: bool,
	zeros: Vec<u32>,
	/// Set when the module cannot be compiled - a result type that depends on a runtime value.
	refused: bool,
}

/// Compile a validated stage. `map` is the pipeline's varying map for a fragment stage.
///
/// `None` WHEN THE PROGRAM WOULD NOT BE THE MODULE: the caller then runs the interpreter, which is
/// always correct. The one shape refused is a `Select` between two values of different types, whose
/// result type depends on the condition's value.
pub fn compile(module: &Module, map: Option<&VaryingMap>) -> Option<Program> {
	let count = module.types.len();
	let widest = module.types.iter().map(Val::words_in).chain(map.unwrap_or(&[]).iter().map(|entry| entry.3)).max().unwrap_or(0).max(STRIDE);
	// FIRST PASS: does any read need the stamps? Decided before code is emitted, because once any
	// read checks a stamp, EVERY assignment must write one.
	let mut assigned = alloc::vec![false; count];
	let checked = !definitely_assigned(&module.body, &mut assigned).ok()?;
	let mut compiler = Compiler { module, map, code: Vec::new(), slots: alloc::vec![None; count], next: 0, parts: Vec::new(), ops: Vec::new(), faults: Vec::new(), uniforms: Vec::new(), uniform_words: 0, counters: 0, checked, zeros: alloc::vec![0; widest], refused: false };
	let mut assigned = alloc::vec![false; count];
	compiler.block(&module.body, &mut assigned, None).ok()?;
	compiler.code.push(Ins::Exit);
	if compiler.refused {
		return None;
	}
	// A value nothing assigns keeps an empty slot: any read of it faults before it is reached.
	let slots = compiler.slots.into_iter().enumerate().map(|(index, slot)| slot.unwrap_or(Slot { offset: 0, len: 0, kind: module.types[index].clone() })).collect();
	// A BATCH RUNS EVERY LANE THROUGH THE SAME INSTRUCTIONS, so only code with no branch, loop, stamp
	// or fault of its own can be one: everything else is the scalar driver's.
	let batchable = !compiler.checked && compiler.code.iter().all(|ins| !matches!(ins, Ins::Check { .. } | Ins::Mark { .. } | Ins::Jump { .. } | Ins::Unless { .. } | Ins::Case { .. } | Ins::LoopStart { .. } | Ins::LoopNext { .. } | Ins::Raise { .. }));
	Some(Program { stage: module.stage, code: compiler.code, slots, parts: compiler.parts, ops: compiler.ops, faults: compiler.faults, uniforms: compiler.uniforms, uniform_words: compiler.uniform_words, words: compiler.next as usize, counters: compiler.counters, checked: compiler.checked, batchable })
}

/// Whether every read in `body` reads a value the structured control flow assigns on every path to
/// it. `Err` for a module naming an id it does not declare, which is left to the interpreter.
///
/// CONSERVATIVE: a branch's assignments survive the join only when every side makes them, and a
/// loop's never survive it. Proving less than is true costs a stamp; proving more would skip a fault.
fn definitely_assigned(body: &[Stmt], assigned: &mut Vec<bool>) -> Result<bool, ()> {
	let count = assigned.len();
	let mut all = true;
	let read = |value: &Value, assigned: &Vec<bool>| -> Result<bool, ()> { assigned.get(value.0 as usize).copied().ok_or(()) };
	for statement in body {
		match statement {
			Stmt::Assign(value, op) => {
				for operand in operands(op) {
					all &= read(&operand, assigned)?;
				}
				*assigned.get_mut(value.0 as usize).ok_or(())? = true;
			}
			Stmt::Store(_, value) => all &= read(value, assigned)?,
			Stmt::If { condition, then_body, else_body } => {
				all &= read(condition, assigned)?;
				let mut then_set = assigned.clone();
				let mut else_set = assigned.clone();
				all &= definitely_assigned(then_body, &mut then_set)?;
				all &= definitely_assigned(else_body, &mut else_set)?;
				for index in 0..count {
					assigned[index] = then_set[index] && else_set[index];
				}
			}
			Stmt::Switch { selector, cases, default } => {
				all &= read(selector, assigned)?;
				let mut joined = assigned.clone();
				all &= definitely_assigned(default, &mut joined)?;
				for (_, case) in cases {
					let mut set = assigned.clone();
					all &= definitely_assigned(case, &mut set)?;
					for index in 0..count {
						joined[index] &= set[index];
					}
				}
				*assigned = joined;
			}
			Stmt::Loop { body, .. } => {
				let mut inside = assigned.clone();
				all &= definitely_assigned(body, &mut inside)?;
			}
			Stmt::Break | Stmt::Continue | Stmt::Return | Stmt::Discard => {}
		}
	}
	Ok(all)
}

/// An operation's operands, in the order the interpreter reads them.
fn operands(op: &Op) -> Vec<Value> {
	match op {
		Op::Const(_) | Op::Load(_) => Vec::new(),
		Op::Compose(_, parts) => parts.clone(),
		Op::Extract(value, _) | Op::Unary(_, value) => alloc::vec![*value],
		Op::Binary(_, left, right) | Op::Compare(_, left, right) => alloc::vec![*left, *right],
		Op::Select { condition, on_true, on_false } => alloc::vec![*condition, *on_true, *on_false],
		Op::Transcendental(_, value, second) => {
			let mut out = alloc::vec![*value];
			out.extend(second.iter().copied());
			out
		}
		Op::Clamp { value, low, high } => alloc::vec![*value, *low, *high],
		Op::Mix { from, to, at } => alloc::vec![*from, *to, *at],
		Op::Index { array, index } => alloc::vec![*array, *index],
		// A SAMPLE THROUGH A BINDING THAT IS NOT A TEXTURE faults before it reads its coordinate.
		Op::Sample { binding, coordinate } => {
			if matches!(binding, Binding::Texture { .. }) {
				alloc::vec![*coordinate]
			} else {
				Vec::new()
			}
		}
	}
}

/// Zeroed operands of known types, for typing an operation at compile time.
struct Probe<'a> {
	slots: &'a [Option<Slot>],
	zeros: &'a [u32],
}

impl Operands for Probe<'_> {
	fn read(&self, value: Value) -> Result<Reg<'_>, Fault> {
		match self.slots.get(value.0 as usize) {
			Some(Some(slot)) => Ok(Reg::new(&slot.kind, &self.zeros[..slot.len as usize])),
			_ => Err(Fault::Unassigned { value: value.0 }),
		}
	}
}

impl Compiler<'_> {
	fn label(&self) -> u32 {
		self.code.len() as u32
	}

	fn jump(&mut self) -> u32 {
		let at = self.label();
		self.code.push(Ins::Jump { target: 0 });
		at
	}

	fn patch(&mut self, at: u32, to: u32) {
		match &mut self.code[at as usize] {
			Ins::Jump { target } | Ins::Unless { target, .. } | Ins::Case { target, .. } => *target = to,
			Ins::LoopNext { exit, .. } => *exit = to,
			_ => {}
		}
	}

	fn raise(&mut self, fault: Fault) {
		let index = self.faults.len() as u32;
		self.faults.push(fault);
		self.code.push(Ins::Raise { fault: index });
	}

	fn mark(&mut self, value: Value) {
		if self.checked {
			self.code.push(Ins::Mark { value: value.0 });
		}
	}

	/// A stamp check for a read the flow analysis could not prove assigned.
	fn check(&mut self, value: Value, assigned: &[bool]) {
		if self.checked && !assigned.get(value.0 as usize).copied().unwrap_or(false) {
			self.code.push(Ins::Check { value: value.0 });
		}
	}

	fn slot(&self, value: Value) -> Option<&Slot> {
		self.slots.get(value.0 as usize).and_then(Option::as_ref)
	}

	/// Where a value's words start and how many there are; an empty place for one never assigned,
	/// which is only ever read after a fault or a failed check.
	fn place(&self, value: Value) -> (u32, u32) {
		self.slot(value).map_or((0, 0), |slot| (slot.offset, slot.len))
	}

	/// Give a value its words. A value assigned by two textual statements is refused by validation, so
	/// this runs once per value.
	fn allocate(&mut self, value: Value, kind: Type, len: u32) -> u32 {
		if let Some(slot) = self.slot(value) {
			return slot.offset;
		}
		let offset = self.next;
		self.next += len;
		self.slots[value.0 as usize] = Some(Slot { offset, len, kind });
		offset
	}

	/// One structured body. Inside a loop, `context` is where `break` and `continue` go; outside one
	/// both end the stage, as the interpreter's top-level body does with the flow they return.
	fn block(&mut self, body: &[Stmt], assigned: &mut Vec<bool>, mut context: Option<&mut LoopContext>) -> Result<(), ()> {
		let count = assigned.len();
		for statement in body {
			match statement {
				Stmt::Assign(value, op) => {
					if value.0 as usize >= count {
						return Err(());
					}
					if self.assign(*value, op, assigned) {
						assigned[value.0 as usize] = true;
					}
				}
				Stmt::Store(output, value) => {
					self.check(*value, assigned);
					self.code.push(Ins::Store { output: *output, value: value.0 });
				}
				Stmt::If { condition, then_body, else_body } => {
					self.check(*condition, assigned);
					let (offset, len) = self.place(*condition);
					let branch = self.label();
					self.code.push(Ins::Unless { offset, len, target: 0 });
					let mut then_set = assigned.clone();
					self.block(then_body, &mut then_set, context.as_deref_mut())?;
					let skip = self.jump();
					let otherwise = self.label();
					self.patch(branch, otherwise);
					let mut else_set = assigned.clone();
					self.block(else_body, &mut else_set, context.as_deref_mut())?;
					let end = self.label();
					self.patch(skip, end);
					for index in 0..count {
						assigned[index] = then_set[index] && else_set[index];
					}
				}
				Stmt::Switch { selector, cases, default } => {
					self.check(*selector, assigned);
					let (offset, len) = self.place(*selector);
					// THE CASES ARE TRIED IN ORDER AND THE FIRST MATCH RUNS; the default otherwise.
					let tests = self.label();
					for (case, _) in cases {
						self.code.push(Ins::Case { offset, len, case: *case, target: 0 });
					}
					let mut joined = assigned.clone();
					self.block(default, &mut joined, context.as_deref_mut())?;
					let mut ends = alloc::vec![self.jump()];
					for (position, (_, case)) in cases.iter().enumerate() {
						let start = self.label();
						self.patch(tests + position as u32, start);
						let mut set = assigned.clone();
						self.block(case, &mut set, context.as_deref_mut())?;
						for index in 0..count {
							joined[index] &= set[index];
						}
						ends.push(self.jump());
					}
					let end = self.label();
					for at in ends {
						self.patch(at, end);
					}
					*assigned = joined;
				}
				Stmt::Loop { trips, body } => {
					let counter = self.counters as u32;
					self.counters += 1;
					self.code.push(Ins::LoopStart { counter, trips: *trips });
					let top = self.label();
					self.code.push(Ins::LoopNext { counter, exit: 0 });
					let mut inner = LoopContext { top, breaks: Vec::new() };
					let mut inside = assigned.clone();
					self.block(body, &mut inside, Some(&mut inner))?;
					self.code.push(Ins::Jump { target: top });
					let exit = self.label();
					self.patch(top, exit);
					for at in inner.breaks {
						self.patch(at, exit);
					}
				}
				Stmt::Break => match context.as_deref_mut() {
					Some(inner) => {
						let at = self.jump();
						inner.breaks.push(at);
					}
					None => self.code.push(Ins::Exit),
				},
				Stmt::Continue => match context.as_deref() {
					Some(inner) => self.code.push(Ins::Jump { target: inner.top }),
					None => self.code.push(Ins::Exit),
				},
				Stmt::Return => self.code.push(Ins::Exit),
				Stmt::Discard => self.code.push(Ins::Discard),
			}
		}
		Ok(())
	}

	/// Emit one assignment. Answers whether its instruction can complete, which a load the stage
	/// cannot answer and an operation its types make fault cannot - its value then stays unassigned
	/// for the flow analysis.
	fn assign(&mut self, value: Value, op: &Op, assigned: &[bool]) -> bool {
		let declared = self.module.types[value.0 as usize].clone();
		match op {
			Op::Load(binding) => {
				let (kind, len) = match binding {
					Binding::Uniform { .. } | Binding::Attribute { .. } => {
						let len = Val::words_in(&declared) as u32;
						(declared.clone(), len)
					}
					Binding::Varying { location } => {
						// THE FIRST MAP ENTRY FOR THE LOCATION, which is what the bindings' `find` takes.
						let found = match (self.module.stage, self.map) {
							(Stage::Fragment, Some(map)) => map.iter().find(|(name, ..)| name == location).cloned(),
							_ => None,
						};
						let Some((_, _, _, count, kind)) = found else {
							self.raise(Fault::MissingBinding);
							return false;
						};
						(kind, count as u32)
					}
					Binding::BuiltIn(which) => {
						let Some(typed) = built_in_kind(self.module.stage, *which) else {
							self.raise(Fault::MissingBinding);
							return false;
						};
						typed
					}
					Binding::Texture { .. } => {
						self.raise(Fault::MissingBinding);
						return false;
					}
				};
				if matches!(binding, Binding::Attribute { .. }) && self.module.stage != Stage::Vertex {
					self.raise(Fault::MissingBinding);
					return false;
				}
				let dst = self.allocate(value, kind.clone(), len);
				match binding {
					Binding::Uniform { block, member } => {
						let entry = match self.uniforms.iter().position(|entry| entry.block == *block && entry.member == *member && entry.kind == kind) {
							Some(index) => index as u32,
							None => {
								let offset = self.uniform_words as u32;
								self.uniform_words += len as usize;
								self.uniforms.push(UniformEntry { block: *block, member: *member, kind, len, offset });
								(self.uniforms.len() - 1) as u32
							}
						};
						self.code.push(Ins::Uniform { dst, entry });
					}
					Binding::Attribute { location } => self.code.push(Ins::Attribute { dst, location: *location, value: value.0 }),
					Binding::Varying { location } => {
						let Some((_, interpolation, offset, count, _)) = self.map.and_then(|map| map.iter().find(|(name, ..)| name == location)) else { return false };
						let array = match interpolation {
							Interpolation::Smooth => 0,
							Interpolation::NoPerspective => 1,
							Interpolation::Flat => 2,
						};
						self.code.push(Ins::Varying { dst, array, offset: *offset as u32, len: *count as u32 });
					}
					Binding::BuiltIn(which) => self.code.push(Ins::BuiltIn { dst, which: *which }),
					Binding::Texture { .. } => {}
				}
				self.mark(value);
				true
			}
			Op::Sample { binding, coordinate } => {
				let Binding::Texture { texture, sampler } = binding else {
					self.raise(Fault::TypeMismatch);
					return false;
				};
				self.check(*coordinate, assigned);
				let len = Val::words_in(&declared) as u32;
				let dst = self.allocate(value, declared, len);
				self.code.push(Ins::Sample { dst, texture: *texture, sampler: *sampler, coordinate: coordinate.0, value: value.0 });
				self.mark(value);
				true
			}
			_ => {
				// TYPED BY THE INTERPRETER'S OWN CODE, on zeroed operands of the types they will have.
				let typed = {
					let probe = Probe { slots: &self.slots, zeros: &self.zeros };
					let mut out = [0_u32; STRIDE];
					interpreter::operation(&probe, op, &mut out).map(|(count, kind)| (count as u32, kind))
				};
				// A SELECT BETWEEN TWO TYPES has a result type that depends on its condition's value.
				if let Op::Select { condition, on_true, on_false } = op {
					let scalar_condition = self.slot(*condition).is_some_and(|slot| slot.len <= 1);
					let shape = |slot: Option<&Slot>| slot.map(|slot| (slot.kind.clone(), slot.len));
					if scalar_condition && shape(self.slot(*on_true)) != shape(self.slot(*on_false)) {
						self.refused = true;
					}
				}
				let Ok((count, kind)) = typed else {
					// A FAULT THE TYPES DECIDE, or a read of a value no path assigns: the interpreter's
					// own code raises it, in its own operand order.
					self.generic(value, op);
					return false;
				};
				self.allocate(value, kind, count);
				let proven = !self.checked || operands(op).iter().all(|operand| assigned.get(operand.0 as usize).copied().unwrap_or(false));
				if !proven || !self.specialise(value, op, count) {
					self.generic(value, op);
				}
				self.mark(value);
				true
			}
		}
	}

	fn generic(&mut self, value: Value, op: &Op) {
		let index = self.ops.len() as u32;
		self.ops.push(op.clone());
		self.code.push(Ins::Generic { value: value.0, op: index });
	}

	/// Emit an instruction of its own for `op`, if it has one for these operand types. The result's
	/// type and length are the probe's; each instruction computes its words exactly as
	/// `interpreter::operation` does, operation for operation and in the same order.
	fn specialise(&mut self, value: Value, op: &Op, count: u32) -> bool {
		let (dst, _) = self.place(value);
		let shape = |compiler: &Self, value: Value| compiler.slot(value).map(|slot| (slot.kind.clone(), slot.offset, slot.len));
		let float = |kind: &Type| crate::value::scalar_type_of(kind) == ScalarType::F32;
		// A BROADCAST OPERAND reads word zero for every component and a full one its own. One shorter
		// than the result and longer than one reads zero past its end in the interpreter, which is left
		// to `Generic`.
		let step = |len: u32| -> Option<u32> {
			if len == 1 {
				Some(0)
			} else if len >= count {
				Some(1)
			} else {
				None
			}
		};
		match op {
			Op::Const(constant) => {
				let word = match constant {
					render_shader::Constant::Bool(value) => u32::from(*value),
					render_shader::Constant::I32(value) => *value as u32,
					render_shader::Constant::U32(value) => *value,
					render_shader::Constant::F32(value) => value.to_bits(),
				};
				self.code.push(Ins::Const { dst, word });
				true
			}
			Op::Compose(_, parts) => {
				if parts.len() == 1 {
					let (src, _) = self.place(parts[0]);
					self.code.push(Ins::Copy { dst, src, len: count });
				} else {
					let first = self.parts.len() as u32;
					for part in parts {
						let place = self.place(*part);
						self.parts.push(place);
					}
					self.code.push(Ins::Compose { dst, first, count: parts.len() as u32 });
				}
				true
			}
			Op::Extract(source, component) => {
				let (src, _) = self.place(*source);
				self.code.push(Ins::Copy { dst, src: src + *component as u32, len: 1 });
				true
			}
			Op::Binary(which, left, right) => {
				let (Some((left_kind, a, left_len)), Some((right_kind, b, right_len))) = (shape(self, *left), shape(self, *right)) else { return false };
				match which {
					BinaryOp::Dot => {
						self.code.push(Ins::Dot { dst, a, b, n: left_len.min(right_len) });
						true
					}
					BinaryOp::MatrixProduct => {
						let Type::Matrix(size) = left_kind else { return false };
						let size = size as u32;
						let Type::Vector(_, components) = right_kind else { return false };
						if components as u32 != size || left_len < size * size || right_len < size || count != size || size > 4 {
							return false;
						}
						self.code.push(Ins::MatrixVector { dst, matrix: a, vector: b, size });
						true
					}
					_ => {
						let scalar_kind = if left_len >= right_len { left_kind } else { right_kind };
						if !float(&scalar_kind) {
							return false;
						}
						let op = match which {
							BinaryOp::Add => FloatBinary::Add,
							BinaryOp::Subtract => FloatBinary::Subtract,
							BinaryOp::Multiply => FloatBinary::Multiply,
							BinaryOp::Divide => FloatBinary::Divide,
							BinaryOp::Modulo => FloatBinary::Modulo,
							BinaryOp::Min => FloatBinary::Min,
							BinaryOp::Max => FloatBinary::Max,
							_ => return false,
						};
						let (Some(a_step), Some(b_step)) = (step(left_len), step(right_len)) else { return false };
						self.code.push(Ins::FloatBinary { op, dst, a, b, n: count, a_step, b_step });
						true
					}
				}
			}
			Op::Unary(which, source) => {
				let Some((kind, src, len)) = shape(self, *source) else { return false };
				match which {
					UnaryOp::Normalize => {
						self.code.push(Ins::Normalize { dst, src, n: count, len });
						true
					}
					UnaryOp::Length => {
						self.code.push(Ins::Length { dst, src, len });
						true
					}
					UnaryOp::Convert(target) => {
						if len < count {
							return false;
						}
						self.code.push(Ins::Convert { dst, src, n: count, from: crate::value::scalar_type_of(&kind), to: *target });
						true
					}
					UnaryOp::Negate | UnaryOp::Abs | UnaryOp::Floor | UnaryOp::Ceil | UnaryOp::Fract => {
						// Floor, ceil and fract read `f32` whatever the type; negate and abs differ by it.
						if matches!(which, UnaryOp::Negate | UnaryOp::Abs) && !float(&kind) {
							return false;
						}
						if len < count {
							return false;
						}
						let op = match which {
							UnaryOp::Negate => FloatUnary::Negate,
							UnaryOp::Abs => FloatUnary::Abs,
							UnaryOp::Floor => FloatUnary::Floor,
							UnaryOp::Ceil => FloatUnary::Ceil,
							_ => FloatUnary::Fract,
						};
						self.code.push(Ins::FloatUnary { op, dst, src, n: count });
						true
					}
					_ => false,
				}
			}
			Op::Transcendental(which, first, second) => {
				let Some((_, a, first_len)) = shape(self, *first) else { return false };
				if first_len < count {
					return false;
				}
				let (b, b_step) = match second {
					None => (NONE, 0),
					Some(second) => {
						let Some((_, b, second_len)) = shape(self, *second) else { return false };
						let Some(b_step) = step(second_len) else { return false };
						(b, b_step)
					}
				};
				self.code.push(Ins::Transcendental { which: *which, dst, a, b, n: count, b_step });
				true
			}
			Op::Clamp { value: source, low, high } => {
				let (Some((_, value, value_len)), Some((_, low, low_len)), Some((_, high, high_len))) = (shape(self, *source), shape(self, *low), shape(self, *high)) else { return false };
				let (Some(value_step), Some(low_step), Some(high_step)) = (step(value_len), step(low_len), step(high_len)) else { return false };
				self.code.push(Ins::Clamp { dst, value, low, high, n: count, value_step, low_step, high_step });
				true
			}
			Op::Mix { from, to, at } => {
				let (Some((_, from, from_len)), Some((_, to, to_len)), Some((_, at, at_len))) = (shape(self, *from), shape(self, *to), shape(self, *at)) else { return false };
				let Some(at_step) = step(at_len) else { return false };
				if from_len < count || to_len < count {
					return false;
				}
				self.code.push(Ins::Mix { dst, from, to, at, n: count, at_step });
				true
			}
			Op::Compare(..) | Op::Select { .. } | Op::Index { .. } | Op::Load(_) | Op::Sample { .. } => false,
		}
	}
}

/// The machine a compiled run uses, inside the interpreter's `Machine` so one lane holds both.
#[derive(Default)]
pub struct Registers {
	words: Vec<u32>,
	stamps: Vec<u32>,
	run: u32,
	counters: Vec<u32>,
}

impl Registers {
	/// Reserve what `program` needs, so a frame's first fragment does not allocate.
	pub(crate) fn reserve(&mut self, program: &Program) -> Result<(), render3d::Error> {
		fn grow<T: Clone + Default>(values: &mut Vec<T>, count: usize) -> Result<(), render3d::Error> {
			if values.len() < count {
				let bytes = count.checked_mul(core::mem::size_of::<T>()).ok_or(render3d::Error::OutOfMemory { bytes: u64::MAX })?;
				values.try_reserve_exact(count - values.len()).map_err(|_| render3d::Error::OutOfMemory { bytes: bytes as u64 })?;
				values.resize(count, T::default());
			}
			Ok(())
		}
		grow(&mut self.words, program.words)?;
		grow(&mut self.counters, program.counters)?;
		if program.checked {
			grow(&mut self.stamps, program.slots.len())?;
		}
		Ok(())
	}

	pub(crate) fn reserved_bytes(&self) -> usize {
		(self.words.capacity() + self.stamps.capacity() + self.counters.capacity()) * core::mem::size_of::<u32>()
	}
}

impl Program {
	pub fn stage(&self) -> Stage {
		self.stage
	}

	/// The bytes this program owns.
	pub fn reserved_bytes(&self) -> usize {
		fn of<T>(values: &Vec<T>) -> usize {
			values.capacity() * core::mem::size_of::<T>()
		}
		of(&self.code) + of(&self.slots) + of(&self.parts) + of(&self.ops) + of(&self.faults) + of(&self.uniforms)
	}

	/// How many instructions run through the interpreter's own code, for a test that wants to know the
	/// hot path is specialised.
	pub fn generic_count(&self) -> usize {
		self.code.iter().filter(|ins| matches!(ins, Ins::Generic { .. })).count()
	}
}

#[inline(always)]
fn f(word: u32) -> f32 {
	f32::from_bits(word)
}

// ---------------------------------------------------------------------------------------------
// The instructions' bodies, on one invocation's registers. Both drivers below call these and
// nothing else computes a word, so the scalar run and the batch cannot disagree.
// ---------------------------------------------------------------------------------------------

/// Copy a value's words. SMALL COPIES ARE WORD MOVES: a call to the runtime's `memmove` for four
/// words costs more than the four words.
#[inline(always)]
fn copy_words(regs: &mut [u32], dst: usize, src: usize, len: usize) {
	match len {
		1 => regs[dst] = regs[src],
		4 => {
			let words: [u32; 4] = regs[src..src + 4].try_into().unwrap_or([0; 4]);
			regs[dst..dst + 4].copy_from_slice(&words);
		}
		_ => {
			for index in 0..len {
				regs[dst + index] = regs[src + index];
			}
		}
	}
}

#[inline(always)]
fn compose(regs: &mut [u32], parts: &[(u32, u32)], dst: usize) {
	let mut at = dst;
	for &(offset, len) in parts {
		copy_words(regs, at, offset as usize, len as usize);
		at += len as usize;
	}
}

#[inline(always)]
fn uniform(regs: &mut [u32], program: &Program, uniforms: &Uniforms, dst: usize, entry: usize) -> Result<(), Fault> {
	if !uniforms.present[entry] {
		return Err(Fault::MissingBinding);
	}
	let entry = &program.uniforms[entry];
	let (start, len) = (entry.offset as usize, entry.len as usize);
	for index in 0..len {
		regs[dst + index] = uniforms.words[start + index];
	}
	Ok(())
}

#[inline(always)]
fn varying<I: Inputs + ?Sized>(regs: &mut [u32], inputs: &I, dst: usize, array: u8, offset: usize, len: usize) -> Result<(), Fault> {
	let Some(values) = inputs.varyings(array).get(offset..offset + len) else { return Err(Fault::MissingBinding) };
	for (slot, value) in regs[dst..dst + len].iter_mut().zip(values) {
		*slot = value.to_bits();
	}
	Ok(())
}

#[inline(always)]
fn built_in<I: Inputs + ?Sized>(regs: &mut [u32], inputs: &I, dst: usize, which: BuiltIn) -> Result<(), Fault> {
	let mut words = [0_u32; 4];
	let Some(len) = inputs.built_in(which, &mut words) else { return Err(Fault::MissingBinding) };
	for (index, word) in words[..len].iter().enumerate() {
		regs[dst + index] = *word;
	}
	Ok(())
}

/// `Ok(false)` when the attribute is not the type the program was compiled for.
#[inline(always)]
fn attribute<I: Inputs + ?Sized>(regs: &mut [u32], program: &Program, inputs: &I, dst: usize, location: u32, value: usize) -> Result<bool, Fault> {
	let Some(loaded) = inputs.attribute(location) else { return Err(Fault::MissingBinding) };
	let slot = &program.slots[value];
	if loaded.kind != slot.kind || loaded.words().len() != slot.len as usize {
		return Ok(false);
	}
	regs[dst..dst + slot.len as usize].copy_from_slice(loaded.words());
	Ok(true)
}

/// `Ok(false)` when the texel is not the type the program was compiled for.
#[inline(always)]
fn sample<I: Inputs + ?Sized>(regs: &mut [u32], program: &Program, inputs: &I, dst: usize, texture: u32, sampler: u32, coordinate: usize, value: usize) -> Result<bool, Fault> {
	let from = &program.slots[coordinate];
	let start = from.offset as usize;
	let coordinate = Val::new(from.kind.clone(), &regs[start..start + from.len as usize]);
	let Some(texel) = inputs.sample(texture, sampler, &coordinate) else { return Err(Fault::SampleFailed) };
	let slot = &program.slots[value];
	if texel.kind != slot.kind || texel.words().len() != slot.len as usize {
		return Ok(false);
	}
	for (index, word) in texel.words().iter().enumerate() {
		regs[dst + index] = *word;
	}
	Ok(true)
}

/// THE OPERATION IS DECIDED ONCE, OUTSIDE THE COMPONENT LOOP, and a four-component operation on two
/// full operands - most of a lit surface - reads and writes its four words at once. Each component is
/// the interpreter's expression on the same two `f32`s. A result never shares words with an operand:
/// an operand is assigned before the value that reads it, and every value has words of its own.
#[inline(always)]
fn float_binary(regs: &mut [u32], op: FloatBinary, dst: usize, a: usize, b: usize, n: usize, a_step: usize, b_step: usize) {
	macro_rules! each {
		(|$x:ident, $y:ident| $e:expr) => {{
			if n == 4 && a_step == 1 && b_step == 1 {
				let left: [u32; 4] = regs[a..a + 4].try_into().unwrap_or([0; 4]);
				let right: [u32; 4] = regs[b..b + 4].try_into().unwrap_or([0; 4]);
				let mut out = [0_u32; 4];
				for index in 0..4 {
					let ($x, $y) = (f(left[index]), f(right[index]));
					out[index] = ($e).to_bits();
				}
				regs[dst..dst + 4].copy_from_slice(&out);
			} else {
				for index in 0..n {
					let ($x, $y) = (f(regs[a + index * a_step]), f(regs[b + index * b_step]));
					regs[dst + index] = ($e).to_bits();
				}
			}
		}};
	}
	match op {
		FloatBinary::Add => each!(|x, y| x + y),
		FloatBinary::Subtract => each!(|x, y| x - y),
		FloatBinary::Multiply => each!(|x, y| x * y),
		FloatBinary::Divide => each!(|x, y| x / y),
		FloatBinary::Modulo => each!(|x, y| x - y * libm::floorf(x / y)),
		FloatBinary::Min => each!(|x, y| x.min(y)),
		FloatBinary::Max => each!(|x, y| x.max(y)),
	}
}

#[inline(always)]
fn float_unary(regs: &mut [u32], op: FloatUnary, dst: usize, src: usize, n: usize) {
	for index in 0..n {
		let x = f(regs[src + index]);
		regs[dst + index] = match op {
			FloatUnary::Negate => -x,
			FloatUnary::Abs => libm::fabsf(x),
			FloatUnary::Floor => libm::floorf(x),
			FloatUnary::Ceil => libm::ceilf(x),
			FloatUnary::Fract => x - libm::floorf(x),
		}
		.to_bits();
	}
}

#[inline(always)]
fn dot(regs: &mut [u32], dst: usize, a: usize, b: usize, n: usize) {
	let mut sum = 0.0_f32;
	for index in 0..n {
		sum += f(regs[a + index]) * f(regs[b + index]);
	}
	regs[dst] = sum.to_bits();
}

#[inline(always)]
fn normalize(regs: &mut [u32], dst: usize, src: usize, n: usize, len: usize) {
	let mut sum = 0.0_f32;
	for index in 0..len {
		let x = f(regs[src + index]);
		sum += x * x;
	}
	let length = libm::sqrtf(sum);
	// A ZERO-LENGTH VECTOR NORMALISES TO ITSELF, as the interpreter's does.
	if length == 0.0 || !length.is_finite() {
		copy_words(regs, dst, src, n);
	} else {
		for index in 0..n {
			regs[dst + index] = (f(regs[src + index]) / length).to_bits();
		}
	}
}

#[inline(always)]
fn length(regs: &mut [u32], dst: usize, src: usize, len: usize) {
	let mut sum = 0.0_f32;
	for index in 0..len {
		let x = f(regs[src + index]);
		sum += x * x;
	}
	regs[dst] = libm::sqrtf(sum).to_bits();
}

#[inline(always)]
fn matrix_vector(regs: &mut [u32], dst: usize, matrix: usize, vector: usize, size: usize) {
	let mut out = [0_u32; 4];
	for (row, slot) in out[..size].iter_mut().enumerate() {
		let mut sum = 0.0_f32;
		for column in 0..size {
			sum += f(regs[matrix + column * size + row]) * f(regs[vector + column]);
		}
		*slot = sum.to_bits();
	}
	regs[dst..dst + size].copy_from_slice(&out[..size]);
}

#[inline(always)]
fn transcendentals(regs: &mut [u32], which: Transcendental, dst: usize, a: usize, b: u32, n: usize, b_step: usize) {
	for index in 0..n {
		let x = f(regs[a + index]);
		let y = if b == NONE { 0.0 } else { f(regs[b as usize + index * b_step]) };
		regs[dst + index] = transcendental(which, x, y).to_bits();
	}
}

#[inline(always)]
fn clamp(regs: &mut [u32], dst: usize, value: usize, low: usize, high: usize, n: usize, steps: [usize; 3]) {
	for index in 0..n {
		let x = f(regs[value + index * steps[0]]);
		let lo = f(regs[low + index * steps[1]]);
		let hi = f(regs[high + index * steps[2]]);
		// `min(max(v, low), high)`, the interpreter's order, which a NaN bound makes matter.
		regs[dst + index] = x.max(lo).min(hi).to_bits();
	}
}

#[inline(always)]
fn mix(regs: &mut [u32], dst: usize, from: usize, to: usize, at: usize, n: usize, at_step: usize) {
	for index in 0..n {
		let t = f(regs[at + index * at_step]);
		regs[dst + index] = ((1.0 - t) * f(regs[from + index]) + t * f(regs[to + index])).to_bits();
	}
}

#[inline(always)]
fn convert(regs: &mut [u32], dst: usize, src: usize, n: usize, from: ScalarType, to: ScalarType) {
	for index in 0..n {
		regs[dst + index] = convert_word(from, to, regs[src + index]);
	}
}

/// One component of `UnaryOp::Convert`, the interpreter's table row for row.
#[inline(always)]
fn convert_word(from: ScalarType, to: ScalarType, word: u32) -> u32 {
	let float = f32::from_bits(word);
	match (from, to) {
		(ScalarType::F32, ScalarType::I32) => float as i32 as u32,
		(ScalarType::F32, ScalarType::U32) => float as u32,
		(ScalarType::F32, ScalarType::Bool) => u32::from(float != 0.0),
		(ScalarType::I32, ScalarType::F32) => ((word as i32) as f32).to_bits(),
		(ScalarType::U32, ScalarType::F32) => (word as f32).to_bits(),
		(ScalarType::Bool, ScalarType::F32) => f32::from(word != 0).to_bits(),
		(ScalarType::Bool, _) => u32::from(word != 0),
		(_, ScalarType::Bool) => u32::from(word != 0),
		_ => word,
	}
}

#[inline(always)]
fn generic(regs: &mut [u32], program: &Program, op: usize, value: usize, stamps: Option<(&[u32], u32)>) -> Result<(), Fault> {
	let mut out = [0_u32; STRIDE];
	let (count, _) = {
		let view = View { slots: &program.slots, regs: &*regs, stamps };
		interpreter::operation(&view, &program.ops[op], &mut out)?
	};
	let slot = &program.slots[value];
	debug_assert_eq!(count, slot.len as usize, "the probe typed this operation");
	let start = slot.offset as usize;
	let len = (slot.len as usize).min(count);
	regs[start..start + len].copy_from_slice(&out[..len]);
	Ok(())
}

#[inline(always)]
fn store(regs: &[u32], program: &Program, output: Output, value: usize, outputs: &mut interpreter::Outputs) {
	let slot = &program.slots[value];
	let start = slot.offset as usize;
	let words = &regs[start..start + slot.len as usize];
	let word = |index: usize| words.get(index).copied().unwrap_or(0);
	match output {
		Output::Position => outputs.position = Some(Val::new(slot.kind.clone(), words)),
		Output::PointSize => outputs.point_size = Some(f(word(0))),
		Output::Varying(location) => outputs.varyings.push((location, Val::new(slot.kind.clone(), words))),
		Output::Colour(index) => outputs.colour.push((index, Val::new(slot.kind.clone(), words))),
		Output::Integer(index) => outputs.integer.push((index, word(0))),
		Output::Depth => outputs.depth = Some(f(word(0))),
		Output::SampleMask => outputs.sample_mask = Some(word(0)),
	}
}

/// Run a compiled stage into `machine`'s outputs.
///
/// `Err` is the fault the interpreter raises for the same invocation; `Ran::Deopt` asks the caller to
/// run the interpreter instead. The registers must have been reserved for `program`.
pub fn run<I: Inputs + ?Sized>(program: &Program, uniforms: &Uniforms, inputs: &I, machine: &mut Machine) -> Result<Ran, Fault> {
	machine.outputs.clear();
	let registers = &mut machine.registers;
	if registers.words.len() < program.words || registers.counters.len() < program.counters || (program.checked && registers.stamps.len() < program.slots.len()) {
		// Not reserved for this program: the interpreter, which grows its own storage, runs instead.
		return Ok(Ran::Deopt);
	}
	if program.checked {
		// A WRAP MUST NOT MAKE A STALE STAMP LOOK FRESH, exactly as the interpreter's table.
		registers.run = registers.run.wrapping_add(1);
		if registers.run == 0 {
			for stamp in registers.stamps.iter_mut() {
				*stamp = 0;
			}
			registers.run = 1;
		}
	}
	let run = registers.run;
	let regs = &mut registers.words[..];
	let counters = &mut registers.counters[..];
	let stamps = &mut registers.stamps[..];
	let outputs = &mut machine.outputs;
	let code = &program.code[..];
	let mut pc = 0_usize;
	loop {
		let ins = &code[pc];
		pc += 1;
		match *ins {
			Ins::Const { dst, word } => regs[dst as usize] = word,
			Ins::Copy { dst, src, len } => copy_words(regs, dst as usize, src as usize, len as usize),
			Ins::Compose { dst, first, count } => compose(regs, &program.parts[first as usize..(first + count) as usize], dst as usize),
			Ins::Uniform { dst, entry } => uniform(regs, program, uniforms, dst as usize, entry as usize)?,
			Ins::Varying { dst, array, offset, len } => varying(regs, inputs, dst as usize, array, offset as usize, len as usize)?,
			Ins::BuiltIn { dst, which } => built_in(regs, inputs, dst as usize, which)?,
			Ins::Attribute { dst, location, value } => {
				if !attribute(regs, program, inputs, dst as usize, location, value as usize)? {
					return Ok(Ran::Deopt);
				}
			}
			Ins::Sample { dst, texture, sampler, coordinate, value } => {
				if !sample(regs, program, inputs, dst as usize, texture, sampler, coordinate as usize, value as usize)? {
					return Ok(Ran::Deopt);
				}
			}
			Ins::FloatBinary { op, dst, a, b, n, a_step, b_step } => float_binary(regs, op, dst as usize, a as usize, b as usize, n as usize, a_step as usize, b_step as usize),
			Ins::FloatUnary { op, dst, src, n } => float_unary(regs, op, dst as usize, src as usize, n as usize),
			Ins::Dot { dst, a, b, n } => dot(regs, dst as usize, a as usize, b as usize, n as usize),
			Ins::Normalize { dst, src, n, len } => normalize(regs, dst as usize, src as usize, n as usize, len as usize),
			Ins::Length { dst, src, len } => length(regs, dst as usize, src as usize, len as usize),
			Ins::MatrixVector { dst, matrix, vector, size } => matrix_vector(regs, dst as usize, matrix as usize, vector as usize, size as usize),
			Ins::Transcendental { which, dst, a, b, n, b_step } => transcendentals(regs, which, dst as usize, a as usize, b, n as usize, b_step as usize),
			Ins::Clamp { dst, value, low, high, n, value_step, low_step, high_step } => clamp(regs, dst as usize, value as usize, low as usize, high as usize, n as usize, [value_step as usize, low_step as usize, high_step as usize]),
			Ins::Mix { dst, from, to, at, n, at_step } => mix(regs, dst as usize, from as usize, to as usize, at as usize, n as usize, at_step as usize),
			Ins::Convert { dst, src, n, from, to } => convert(regs, dst as usize, src as usize, n as usize, from, to),
			Ins::Generic { value, op } => generic(regs, program, op as usize, value as usize, if program.checked { Some((&*stamps, run)) } else { None })?,
			Ins::Check { value } => {
				if stamps.get(value as usize).copied() != Some(run) {
					return Err(Fault::Unassigned { value });
				}
			}
			Ins::Mark { value } => stamps[value as usize] = run,
			Ins::Store { output, value } => store(regs, program, output, value as usize, outputs),
			Ins::Jump { target } => pc = target as usize,
			Ins::Unless { offset, len, target } => {
				if len == 0 || regs[offset as usize] == 0 {
					pc = target as usize;
				}
			}
			Ins::Case { offset, len, case, target } => {
				let selector = if len == 0 { 0 } else { regs[offset as usize] as i32 };
				if selector == case {
					pc = target as usize;
				}
			}
			Ins::LoopStart { counter, trips } => counters[counter as usize] = trips,
			Ins::LoopNext { counter, exit } => {
				let left = &mut counters[counter as usize];
				if *left == 0 {
					pc = exit as usize;
				} else {
					*left -= 1;
				}
			}
			Ins::Discard => {
				outputs.discarded = true;
				return Ok(Ran::Done);
			}
			Ins::Exit => return Ok(Ran::Done),
			Ins::Raise { fault } => return Err(program.faults[fault as usize].clone()),
		}
	}
}

/// How many invocations one batched run carries.
pub const LANES: usize = 8;

/// The registers and outputs of up to `LANES` invocations of one program.
#[derive(Default)]
pub struct Batch {
	/// Lane `l`'s registers at `l * words`, where `words` is the program's.
	words: Vec<u32>,
	outputs: Vec<interpreter::Outputs>,
}

impl Batch {
	/// Reserve what `program` needs for `LANES` invocations. ALLOCATES - before a frame's first draw.
	pub(crate) fn reserve(&mut self, program: &Program, module: &Module) -> Result<(), render3d::Error> {
		let words = program.words.checked_mul(LANES).ok_or(render3d::Error::OutOfMemory { bytes: u64::MAX })?;
		if self.words.len() < words {
			self.words.try_reserve_exact(words - self.words.len()).map_err(|_| render3d::Error::OutOfMemory { bytes: (words * 4) as u64 })?;
			self.words.resize(words, 0);
		}
		if self.outputs.len() < LANES {
			self.outputs.try_reserve_exact(LANES - self.outputs.len()).map_err(|_| render3d::Error::OutOfMemory { bytes: (LANES * core::mem::size_of::<interpreter::Outputs>()) as u64 })?;
			self.outputs.resize_with(LANES, Default::default);
		}
		for outputs in &mut self.outputs {
			interpreter::reserve_outputs(outputs, module)?;
		}
		Ok(())
	}

	/// What lane `lane` of the last batched run wrote.
	pub fn outputs(&self, lane: usize) -> &interpreter::Outputs {
		&self.outputs[lane]
	}

	pub(crate) fn reserved_bytes(&self) -> usize {
		self.words.capacity() * core::mem::size_of::<u32>() + self.outputs.capacity() * core::mem::size_of::<interpreter::Outputs>() + self.outputs.iter().map(interpreter::outputs_reserved_bytes).sum::<usize>()
	}
}

impl Program {
	/// Whether this program runs in batches: straight-line code with no stamps, so every lane runs the
	/// same instructions in the same order and nothing but a fault or a type can tell them apart.
	pub fn batched(&self) -> bool {
		self.batchable
	}
}

/// Run `inputs.len()` invocations of a batchable program together, ONE INSTRUCTION FOR EVERY LANE
/// BEFORE THE NEXT - which is what spreads an instruction's dispatch over the lanes.
///
/// `false` WHEN ANY LANE FAULTED OR WAS HANDED BACK: the caller then runs every lane on its own, in
/// order, which gives the serial path's result and its first fault exactly. Nothing a lane did is
/// visible outside the batch, so nothing has to be undone.
pub fn run_batch<I: Inputs>(program: &Program, uniforms: &Uniforms, inputs: &[I], batch: &mut Batch) -> bool {
	let lanes = inputs.len();
	let words = program.words;
	if !program.batchable || lanes > LANES || batch.outputs.len() < lanes || batch.words.len() < words * lanes {
		return false;
	}
	for outputs in &mut batch.outputs[..lanes] {
		outputs.clear();
	}
	macro_rules! each {
		(|$regs:ident, $input:ident| $body:expr) => {
			for (lane, $input) in inputs.iter().enumerate() {
				let $regs = &mut batch.words[lane * words..(lane + 1) * words];
				let _ = &$input;
				$body;
			}
		};
	}
	macro_rules! fallible {
		(|$regs:ident, $input:ident| $body:expr) => {
			for (lane, $input) in inputs.iter().enumerate() {
				let $regs = &mut batch.words[lane * words..(lane + 1) * words];
				let _ = &$input;
				match $body {
					Ok(true) => {}
					_ => return false,
				}
			}
		};
	}
	for ins in &program.code {
		match *ins {
			Ins::Const { dst, word } => each!(|regs, input| regs[dst as usize] = word),
			Ins::Copy { dst, src, len } => each!(|regs, input| copy_words(regs, dst as usize, src as usize, len as usize)),
			Ins::Compose { dst, first, count } => {
				let parts = &program.parts[first as usize..(first + count) as usize];
				each!(|regs, input| compose(regs, parts, dst as usize))
			}
			Ins::Uniform { dst, entry } => fallible!(|regs, input| uniform(regs, program, uniforms, dst as usize, entry as usize).map(|()| true)),
			Ins::Varying { dst, array, offset, len } => fallible!(|regs, input| varying(regs, input, dst as usize, array, offset as usize, len as usize).map(|()| true)),
			Ins::BuiltIn { dst, which } => fallible!(|regs, input| built_in(regs, input, dst as usize, which).map(|()| true)),
			Ins::Attribute { dst, location, value } => fallible!(|regs, input| attribute(regs, program, input, dst as usize, location, value as usize)),
			Ins::Sample { dst, texture, sampler, coordinate, value } => fallible!(|regs, input| sample(regs, program, input, dst as usize, texture, sampler, coordinate as usize, value as usize)),
			Ins::FloatBinary { op, dst, a, b, n, a_step, b_step } => each!(|regs, input| float_binary(regs, op, dst as usize, a as usize, b as usize, n as usize, a_step as usize, b_step as usize)),
			Ins::FloatUnary { op, dst, src, n } => each!(|regs, input| float_unary(regs, op, dst as usize, src as usize, n as usize)),
			Ins::Dot { dst, a, b, n } => each!(|regs, input| dot(regs, dst as usize, a as usize, b as usize, n as usize)),
			Ins::Normalize { dst, src, n, len } => each!(|regs, input| normalize(regs, dst as usize, src as usize, n as usize, len as usize)),
			Ins::Length { dst, src, len } => each!(|regs, input| length(regs, dst as usize, src as usize, len as usize)),
			Ins::MatrixVector { dst, matrix, vector, size } => each!(|regs, input| matrix_vector(regs, dst as usize, matrix as usize, vector as usize, size as usize)),
			Ins::Transcendental { which, dst, a, b, n, b_step } => each!(|regs, input| transcendentals(regs, which, dst as usize, a as usize, b, n as usize, b_step as usize)),
			Ins::Clamp { dst, value, low, high, n, value_step, low_step, high_step } => each!(|regs, input| clamp(regs, dst as usize, value as usize, low as usize, high as usize, n as usize, [value_step as usize, low_step as usize, high_step as usize])),
			Ins::Mix { dst, from, to, at, n, at_step } => each!(|regs, input| mix(regs, dst as usize, from as usize, to as usize, at as usize, n as usize, at_step as usize)),
			Ins::Convert { dst, src, n, from, to } => each!(|regs, input| convert(regs, dst as usize, src as usize, n as usize, from, to)),
			Ins::Generic { value, op } => fallible!(|regs, input| generic(regs, program, op as usize, value as usize, None).map(|()| true)),
			Ins::Store { output, value } => {
				for lane in 0..lanes {
					store(&batch.words[lane * words..(lane + 1) * words], program, output, value as usize, &mut batch.outputs[lane]);
				}
			}
			Ins::Discard => {
				for outputs in &mut batch.outputs[..lanes] {
					outputs.discarded = true;
				}
				return true;
			}
			Ins::Exit => return true,
			// A batchable program has none of these; meeting one is not a run this can finish.
			Ins::Check { .. } | Ins::Mark { .. } | Ins::Jump { .. } | Ins::Unless { .. } | Ins::Case { .. } | Ins::LoopStart { .. } | Ins::LoopNext { .. } | Ins::Raise { .. } => return false,
		}
	}
	false
}

/// One transcendental on one component, as the interpreter's table computes it.
#[inline(always)]
fn transcendental(which: Transcendental, a: f32, b: f32) -> f32 {
	match which {
		Transcendental::Sqrt => libm::sqrtf(a),
		Transcendental::InverseSqrt => 1.0 / libm::sqrtf(a),
		Transcendental::Sin => libm::sinf(a),
		Transcendental::Cos => libm::cosf(a),
		Transcendental::Tan => libm::tanf(a),
		Transcendental::Asin => libm::asinf(a),
		Transcendental::Acos => libm::acosf(a),
		Transcendental::Atan => libm::atanf(a),
		Transcendental::Atan2 => libm::atan2f(a, b),
		Transcendental::Exp => libm::expf(a),
		Transcendental::Exp2 => libm::exp2f(a),
		Transcendental::Log => libm::logf(a),
		Transcendental::Log2 => libm::log2f(a),
		Transcendental::Pow => libm::powf(a, b),
	}
}

#[cfg(test)]
#[path = "compiled/tests.rs"]
mod tests;

//! VALIDATION AT MODULE LOAD, AND THE POSITION DEPENDENCY SLICE.
//!
//! A SHADER THAT LOADS AND THEN REFUSES TO DRAW IS A FAILURE NOBODY CAN ATTRIBUTE. Everything that
//! can be decided from the module is decided here, once, and the refusal names the operation and the
//! path that reached it rather than the draw that tripped over it.
//!
//! THE INTERESTING HALF IS THE SLICE. StrictF32 applies to every DATA AND CONTROL dependency of a
//! vertex position: the arithmetic that produces it, THE CONDITIONS OF THE BRANCHES THAT SELECT IT,
//! the indices of the memory it is read from, and any sampling permitted to contribute to it. The
//! control half is what a naive implementation misses - a position selected by `if (sin(t) > 0)` is
//! as backend-dependent as one computed from `sin(t)`, because the two backends take different
//! branches - so a branch's condition is on the slice of everything the branch stores.
//!
//! AND IT IS THE COMPLETE SLICE AND NOT THE FINAL MULTIPLY. Walking one step back from the position
//! store would admit a matrix built from a transcendental.

use alloc::vec::Vec;

use crate::ir::{Constant, Module, Op, Output, ScalarType, Stage, Stmt, Transcendental, Type, Value};

/// Why a module was refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
	/// A value used before it was assigned, or assigned twice. SSA is a property the validator
	/// establishes rather than assumes, because a builder with a bug produces a module that reads
	/// an undefined value and a backend would read whatever register was there.
	NotSingleAssignment {
		value: u32,
	},
	UndefinedValue {
		value: u32,
	},
	/// An operand whose type the operation cannot take.
	TypeMismatch {
		value: u32,
		expected: &'static str,
	},
	/// A loop whose trip count is zero or past the device's bound, or nesting past it.
	LoopBound {
		trips: u32,
		ceiling: u32,
	},
	LoopDepth {
		depth: u32,
		ceiling: u32,
	},
	/// More instructions than the device admits.
	TooManyInstructions {
		count: u32,
		ceiling: u32,
	},
	/// A constant index outside its array.
	IndexOutOfRange {
		index: i64,
		length: u32,
	},
	/// `Break` or `Continue` outside a loop, or `Discard` outside a fragment stage.
	MisplacedControlFlow {
		what: &'static str,
	},
	/// A stage that never writes what it exists to produce.
	MissingOutput {
		what: &'static str,
	},
	/// An output the stage may not write.
	WrongStageOutput {
		what: &'static str,
	},
	/// An evaluation location where it has no meaning: `centroid` or `sample` on a `flat` varying,
	/// which is constant and therefore the same wherever it is evaluated, or on a VERTEX stage,
	/// which has no coverage to evaluate against.
	MeaninglessSampling {
		what: &'static str,
	},
	/// THE ONE THIS MODULE EXISTS FOR: an operation on the position's dependency path whose result
	/// is not deterministic across backends. It names the operation AND how many steps from the
	/// position it was found, which is the path a reader needs.
	NonDeterministicPosition {
		operation: &'static str,
		max_ulp: u32,
		depth: u32,
	},
}

/// What a device will admit of a shader.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ShaderLimits {
	pub max_instructions: u32,
	pub max_loop_depth: u32,
	pub max_loop_trips: u32,
}

impl ShaderLimits {
	/// The profile's own floor, from the frozen minimum-limit table rather than from a copy.
	pub fn profile_minimum() -> Self {
		let find = |name: &str| graphics_profile::render3d_spec::RENDER3D_PROFILE_1_MIN_LIMITS.iter().find(|entry| entry.name == name).map(|entry| entry.minimum).unwrap_or(0);
		Self { max_instructions: find("max_shader_instructions"), max_loop_depth: 8, max_loop_trips: 65_536 }
	}
}

/// Validate a module. Everything decidable is decided here.
pub fn validate(module: &Module, limits: &ShaderLimits) -> Result<(), Error> {
	// THE EVALUATION LOCATION IS CHECKED BEFORE THE BODY, because it is a property of the interface
	// and a body that reads a meaningless varying would be reported as a body problem.
	for varying in &module.varyings {
		if varying.sampling != crate::ir::Sampling::Pixel {
			if varying.interpolation == crate::ir::Interpolation::Flat {
				return Err(Error::MeaninglessSampling { what: "`centroid` or `sample` on a `flat` varying, which is constant and therefore the same wherever it is evaluated" });
			}
			if matches!(module.stage, Stage::Vertex) {
				return Err(Error::MeaninglessSampling { what: "`centroid` or `sample` on a vertex stage, which has no coverage to evaluate against" });
			}
		}
	}
	let mut state = Walk { module, limits, assigned: Vec::new(), instructions: 0, wrote_position: false, wrote_colour: false };
	state.assigned.resize(module.types.len(), false);
	state.body(&module.body, 0, false)?;
	if state.instructions > limits.max_instructions {
		return Err(Error::TooManyInstructions { count: state.instructions, ceiling: limits.max_instructions });
	}
	match module.stage {
		Stage::Vertex if !state.wrote_position => return Err(Error::MissingOutput { what: "a vertex stage that never writes a position" }),
		Stage::Fragment if !state.wrote_colour => return Err(Error::MissingOutput { what: "a fragment stage that writes no colour, integer or depth output" }),
		_ => {}
	}
	if matches!(module.stage, Stage::Vertex) {
		strict_position_slice(module)?;
	}
	Ok(())
}

struct Walk<'a> {
	module: &'a Module,
	limits: &'a ShaderLimits,
	assigned: Vec<bool>,
	instructions: u32,
	wrote_position: bool,
	wrote_colour: bool,
}

impl Walk<'_> {
	fn body(&mut self, body: &[Stmt], depth: u32, in_loop: bool) -> Result<(), Error> {
		for statement in body {
			self.statement(statement, depth, in_loop)?;
		}
		Ok(())
	}

	fn statement(&mut self, statement: &Stmt, depth: u32, in_loop: bool) -> Result<(), Error> {
		self.instructions += 1;
		match statement {
			Stmt::Assign(value, op) => {
				self.operation(op)?;
				let index = value.0 as usize;
				if index >= self.assigned.len() {
					return Err(Error::UndefinedValue { value: value.0 });
				}
				// SSA IS ESTABLISHED AND NOT ASSUMED: a builder with a bug produces a module that
				// assigns twice, and a backend would then read whichever register was there.
				if self.assigned[index] {
					return Err(Error::NotSingleAssignment { value: value.0 });
				}
				self.assigned[index] = true;
				Ok(())
			}
			Stmt::Store(output, value) => {
				self.defined(*value)?;
				match (self.module.stage, output) {
					(Stage::Vertex, Output::Position) => self.wrote_position = true,
					(Stage::Vertex, Output::PointSize | Output::Varying(_)) => {}
					(Stage::Fragment, Output::Colour(_) | Output::Integer(_) | Output::Depth) => self.wrote_colour = true,
					(Stage::Fragment, Output::SampleMask) => {}
					(Stage::Vertex, _) => return Err(Error::WrongStageOutput { what: "a vertex stage writing a fragment output" }),
					(Stage::Fragment, _) => return Err(Error::WrongStageOutput { what: "a fragment stage writing a vertex output" }),
				}
				Ok(())
			}
			Stmt::If { condition, then_body, else_body } => {
				self.defined(*condition)?;
				self.expect_scalar(*condition, ScalarType::Bool)?;
				self.body(then_body, depth, in_loop)?;
				self.body(else_body, depth, in_loop)
			}
			Stmt::Switch { selector, cases, default } => {
				self.defined(*selector)?;
				for (_, body) in cases {
					self.body(body, depth, in_loop)?;
				}
				self.body(default, depth, in_loop)
			}
			Stmt::Loop { trips, body } => {
				if *trips == 0 || *trips > self.limits.max_loop_trips {
					return Err(Error::LoopBound { trips: *trips, ceiling: self.limits.max_loop_trips });
				}
				if depth + 1 > self.limits.max_loop_depth {
					return Err(Error::LoopDepth { depth: depth + 1, ceiling: self.limits.max_loop_depth });
				}
				self.body(body, depth + 1, true)
			}
			Stmt::Break | Stmt::Continue => {
				if !in_loop {
					return Err(Error::MisplacedControlFlow { what: "a break or a continue outside a loop" });
				}
				Ok(())
			}
			Stmt::Return => Ok(()),
			Stmt::Discard => {
				if !matches!(self.module.stage, Stage::Fragment) {
					return Err(Error::MisplacedControlFlow { what: "a discard outside a fragment stage" });
				}
				Ok(())
			}
		}
	}

	fn operation(&mut self, op: &Op) -> Result<(), Error> {
		match op {
			Op::Const(_) | Op::Load(_) => Ok(()),
			Op::Compose(_, parts) => {
				for part in parts {
					self.defined(*part)?;
				}
				Ok(())
			}
			Op::Extract(value, _) | Op::Unary(_, value) => self.defined(*value),
			Op::Binary(_, left, right) | Op::Compare(_, left, right) => {
				self.defined(*left)?;
				self.defined(*right)
			}
			Op::Select { condition, on_true, on_false } => {
				self.defined(*condition)?;
				self.expect_scalar(*condition, ScalarType::Bool)?;
				self.defined(*on_true)?;
				self.defined(*on_false)
			}
			Op::Transcendental(_, first, second) => {
				self.defined(*first)?;
				if let Some(second) = second {
					self.defined(*second)?;
				}
				Ok(())
			}
			Op::Clamp { value, low, high } => {
				self.defined(*value)?;
				self.defined(*low)?;
				self.defined(*high)
			}
			Op::Mix { from, to, at } => {
				self.defined(*from)?;
				self.defined(*to)?;
				self.defined(*at)
			}
			Op::Index { array, index } => {
				self.defined(*array)?;
				self.defined(*index)?;
				// A CONSTANT INDEX IS CHECKED HERE. A dynamic one cannot be, and the contract is that
				// a backend clamps it - which is what "checked memory access" means for an index
				// nothing at load time can know.
				let Type::Array(_, length) = &self.module.types[array.0 as usize] else {
					return Err(Error::TypeMismatch { value: array.0, expected: "an array" });
				};
				if let Some(Constant::I32(literal)) = self.constant(*index) {
					if literal < 0 || literal as i64 >= *length as i64 {
						return Err(Error::IndexOutOfRange { index: literal as i64, length: *length });
					}
				}
				if let Some(Constant::U32(literal)) = self.constant(*index) {
					if literal >= *length {
						return Err(Error::IndexOutOfRange { index: literal as i64, length: *length });
					}
				}
				Ok(())
			}
			Op::Sample { coordinate, .. } => self.defined(*coordinate),
		}
	}

	fn defined(&self, value: Value) -> Result<(), Error> {
		match self.assigned.get(value.0 as usize) {
			Some(true) => Ok(()),
			Some(false) => Err(Error::UndefinedValue { value: value.0 }),
			None => Err(Error::UndefinedValue { value: value.0 }),
		}
	}

	fn expect_scalar(&self, value: Value, scalar: ScalarType) -> Result<(), Error> {
		match self.module.types.get(value.0 as usize) {
			Some(Type::Scalar(found)) if *found == scalar => Ok(()),
			_ => Err(Error::TypeMismatch { value: value.0, expected: "a boolean scalar" }),
		}
	}

	/// The constant a value is, if it is one. Used for the index check alone.
	fn constant(&self, value: Value) -> Option<Constant> {
		find_assignment(&self.module.body, value).and_then(|op| match op {
			Op::Const(constant) => Some(*constant),
			_ => None,
		})
	}
}

/// The assignment of one value, wherever in the nesting it is.
fn find_assignment<'a>(body: &'a [Stmt], value: Value) -> Option<&'a Op> {
	for statement in body {
		match statement {
			Stmt::Assign(assigned, op) if *assigned == value => return Some(op),
			Stmt::If { then_body, else_body, .. } => {
				if let Some(found) = find_assignment(then_body, value).or_else(|| find_assignment(else_body, value)) {
					return Some(found);
				}
			}
			Stmt::Switch { cases, default, .. } => {
				for (_, case) in cases {
					if let Some(found) = find_assignment(case, value) {
						return Some(found);
					}
				}
				if let Some(found) = find_assignment(default, value) {
					return Some(found);
				}
			}
			Stmt::Loop { body, .. } => {
				if let Some(found) = find_assignment(body, value) {
					return Some(found);
				}
			}
			_ => {}
		}
	}
	None
}

/// THE POSITION DEPENDENCY SLICE, walked backwards from every position store.
///
/// DATA AND CONTROL, which is the half a naive implementation misses: a position selected by
/// `if (sin(t) > 0)` is as backend-dependent as one computed from `sin(t)`, because two backends take
/// different branches at the boundary. So every enclosing branch's condition is on the slice of
/// everything inside it.
///
/// THE REFUSAL NAMES THE OPERATION AND THE DEPTH, because "this shader is not deterministic" is not
/// something anybody can act on and "`sin`, four ULP, three steps from the position" is.
fn strict_position_slice(module: &Module) -> Result<(), Error> {
	let mut roots: Vec<(Value, u32)> = Vec::new();
	collect_position_roots(&module.body, &[], &mut roots);
	// A breadth-first walk backwards, carrying how many steps from the position each value is.
	let mut seen: Vec<bool> = Vec::new();
	seen.resize(module.types.len(), false);
	let mut frontier = roots;
	while let Some((value, depth)) = frontier.pop() {
		let index = value.0 as usize;
		if index >= seen.len() || seen[index] {
			continue;
		}
		seen[index] = true;
		let Some(op) = find_assignment(&module.body, value) else { continue };
		if let Op::Transcendental(which, ..) = op {
			// PERMITTED ONLY WITH A STRICT DEFINITION, which is what a zero-ULP bound is.
			if !which.strict() {
				return Err(Error::NonDeterministicPosition { operation: which.name(), max_ulp: which.max_ulp().unwrap_or(u32::MAX), depth });
			}
		}
		for operand in operands(op) {
			frontier.push((operand, depth + 1));
		}
	}
	Ok(())
}

/// Every value a position store depends on, with the CONDITIONS of the branches that enclose it.
fn collect_position_roots(body: &[Stmt], enclosing: &[Value], out: &mut Vec<(Value, u32)>) {
	for statement in body {
		match statement {
			Stmt::Store(Output::Position, value) => {
				out.push((*value, 0));
				// THE CONTROL HALF: every branch condition that selects this store is on its slice.
				for condition in enclosing {
					out.push((*condition, 1));
				}
			}
			Stmt::If { condition, then_body, else_body } => {
				let mut deeper = enclosing.to_vec();
				deeper.push(*condition);
				collect_position_roots(then_body, &deeper, out);
				collect_position_roots(else_body, &deeper, out);
			}
			Stmt::Switch { selector, cases, default } => {
				let mut deeper = enclosing.to_vec();
				deeper.push(*selector);
				for (_, case) in cases {
					collect_position_roots(case, &deeper, out);
				}
				collect_position_roots(default, &deeper, out);
			}
			Stmt::Loop { body, .. } => collect_position_roots(body, enclosing, out),
			_ => {}
		}
	}
}

/// The values an operation reads.
fn operands(op: &Op) -> Vec<Value> {
	match op {
		Op::Const(_) | Op::Load(_) => Vec::new(),
		Op::Compose(_, parts) => parts.clone(),
		Op::Extract(value, _) | Op::Unary(_, value) => alloc::vec![*value],
		Op::Binary(_, left, right) | Op::Compare(_, left, right) => alloc::vec![*left, *right],
		Op::Select { condition, on_true, on_false } => alloc::vec![*condition, *on_true, *on_false],
		Op::Transcendental(_, first, second) => match second {
			Some(second) => alloc::vec![*first, *second],
			None => alloc::vec![*first],
		},
		Op::Clamp { value, low, high } => alloc::vec![*value, *low, *high],
		Op::Mix { from, to, at } => alloc::vec![*from, *to, *at],
		// THE INDEX IS ON THE PATH TOO: an index that differed by one between backends would read a
		// different vertex, which is a different position.
		Op::Index { array, index } => alloc::vec![*array, *index],
		Op::Sample { coordinate, .. } => alloc::vec![*coordinate],
	}
}

/// The operations that are never allowed on a position path, for a report that wants the list rather
/// than to discover it one refusal at a time.
pub fn non_strict_operations() -> Vec<(&'static str, u32)> {
	let mut out = Vec::new();
	for which in [
		Transcendental::Sqrt,
		Transcendental::InverseSqrt,
		Transcendental::Sin,
		Transcendental::Cos,
		Transcendental::Tan,
		Transcendental::Asin,
		Transcendental::Acos,
		Transcendental::Atan,
		Transcendental::Atan2,
		Transcendental::Exp,
		Transcendental::Exp2,
		Transcendental::Log,
		Transcendental::Log2,
		Transcendental::Pow,
	] {
		if !which.strict() {
			out.push((which.name(), which.max_ulp().unwrap_or(u32::MAX)));
		}
	}
	out
}

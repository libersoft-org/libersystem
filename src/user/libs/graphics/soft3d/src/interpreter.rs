//! THE SHADER INTERPRETER: both stages, every operation, and a DETERMINISTIC SCALAR PATH.
//!
//! SCALAR AND NOT VECTORISED, and that is the contract rather than a limitation. This is the
//! reference every faster path is differential-tested against; a reference that was itself
//! vectorised would have nothing to be tested against, and the first time a SIMD path disagreed with
//! it there would be no way to say which was right.
//!
//! EVERY LOOP IS BOUNDED BEFORE IT RUNS. `render-shader` makes an unbounded loop unrepresentable -
//! the trip count is a field of the statement - and validation has already checked it against the
//! device. So the interpreter counts down a known number and can state that a shader cannot hang,
//! which is what makes a frame's worst case computable rather than hoped for.
//!
//! EVERY ACCESS IS CHECKED. A constant index was checked at validation; a computed one is checked
//! HERE, because its value is not known until it is computed, and the answer is a typed refusal
//! rather than a read of whatever followed the array.
//!
//! THE TRANSCENDENTALS ARE ON THE TOLERANCE SIDE OF THE SPLIT. A position may depend only on the
//! operations the profile bounds at zero ULP, and `render-shader::validate` is what refuses one that
//! depends on the others - so this file does not need to be careful about which is which, and must
//! not try to be: re-deciding it here would be a second rule that can disagree with the first.

use alloc::vec::Vec;

use render_shader::ir::{Sampling, Varying};
use render_shader::{BinaryOp, Binding, CompareKind, Constant, Module, Op, ScalarType, Stage, Stmt, Transcendental, Type, UnaryOp, Value};

use crate::value::Val;

/// Why a shader could not be executed.
///
/// SEPARATE FROM VALIDATION'S ERRORS. Validation refuses what is decidable without running; these
/// are the things only a running shader finds - a computed index outside its array, a resource the
/// environment does not have.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fault {
	/// A value read before the statement that assigns it ran. Validation establishes single
	/// assignment over the whole body; a branch not taken means the value was never produced.
	Unassigned { value: u32 },
	/// A COMPUTED index outside its array. The constant case is validation's.
	IndexOutOfRange { index: i64, length: u32 },
	/// A binding the environment does not supply.
	MissingBinding,
	/// An operand shape the operation cannot take, which validation should have caught - so this is
	/// the interpreter refusing rather than guessing, not a case a correct module reaches.
	TypeMismatch,
	/// A texture read the sampler refused.
	SampleFailed,
}

/// What a stage wrote.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Outputs {
	pub position: Option<Val>,
	pub point_size: Option<f32>,
	pub varyings: Vec<(u32, Val)>,
	pub colour: Vec<(u32, Val)>,
	pub integer: Vec<(u32, u32)>,
	pub depth: Option<f32>,
	pub sample_mask: Option<u32>,
	/// A DISCARDED FRAGMENT WRITES NOTHING - not depth, not stencil, not colour, not an identity.
	pub discarded: bool,
}

/// Where a stage's inputs come from.
///
/// A TRAIT AND NOT A STRUCT OF BUFFERS, because the vertex stage's attributes, the fragment stage's
/// interpolated varyings and a texture read come from three different places in a real frame and
/// pretending they are one table would make the interpreter own the pipeline.
pub trait Resources {
	fn uniform(&self, block: u32, member: u32) -> Option<Val>;
	fn attribute(&self, location: u32) -> Option<Val>;
	fn varying(&self, location: u32) -> Option<Val>;
	fn built_in(&self, which: render_shader::ir::BuiltIn) -> Option<Val>;
	fn sample(&self, texture: u32, sampler: u32, coordinate: &Val) -> Option<Val>;
}

impl Outputs {
	/// Empty it, KEEPING every allocation. A stage runs once per covered pixel, so freeing these
	/// vectors and building them again is an allocation per pixel.
	pub fn clear(&mut self) {
		self.position = None;
		self.point_size = None;
		self.varyings.clear();
		self.colour.clear();
		self.integer.clear();
		self.depth = None;
		self.sample_mask = None;
		self.discarded = false;
	}
}

/// The interpreter's reusable state: one slot per value the module assigns, and the outputs.
///
/// KEPT BY THE CALLER BETWEEN RUNS. A fragment stage runs once per covered pixel; allocating its
/// value table and its output vectors each time is millions of allocations in a frame, and a frame
/// whose cost depends on the allocator rather than on the geometry.
#[derive(Default)]
pub struct Machine {
	values: Vec<Option<Val>>,
	outputs: Outputs,
}

impl Machine {
	pub fn outputs(&self) -> &Outputs {
		&self.outputs
	}
}

/// Run one stage into a machine the caller keeps.
pub fn execute_into(module: &Module, resources: &dyn Resources, machine: &mut Machine) -> Result<(), Fault> {
	machine.values.clear();
	machine.values.resize(module.types.len(), None);
	machine.outputs.clear();
	let mut state = Run { resources, values: core::mem::take(&mut machine.values), outputs: core::mem::take(&mut machine.outputs) };
	let outcome = state.body(&module.body);
	machine.values = state.values;
	machine.outputs = state.outputs;
	outcome.map(|_| ())
}

/// Run one stage. ALLOCATES A MACHINE - a fixture, or a one-off. A renderer uses `execute_into`.
pub fn execute(module: &Module, resources: &dyn Resources) -> Result<Outputs, Fault> {
	let mut machine = Machine::default();
	execute_into(module, resources, &mut machine)?;
	Ok(machine.outputs)
}

/// Which varying declaration a location names, for a caller that interpolates before calling.
pub fn varying_of(module: &Module, location: u32) -> Option<&Varying> {
	module.varyings.iter().find(|varying| varying.location == location)
}

/// Whether a fragment stage must run once per sample rather than once per pixel.
pub fn per_sample(module: &Module) -> bool {
	module.varyings.iter().any(|varying| varying.sampling == Sampling::Sample)
}

/// A collector that fills a fixed array, so a component-wise loop reads as an iterator without
/// allocating one value per instruction per pixel.
struct Inline {
	words: [u32; crate::value::INLINE_WORDS],
	len: usize,
}

impl core::ops::Deref for Inline {
	type Target = [u32];

	fn deref(&self) -> &[u32] {
		&self.words[..self.len]
	}
}

impl FromIterator<u32> for Inline {
	fn from_iter<I: IntoIterator<Item = u32>>(source: I) -> Self {
		let mut out = Self { words: [0; crate::value::INLINE_WORDS], len: 0 };
		for word in source {
			if out.len >= crate::value::INLINE_WORDS {
				break;
			}
			out.words[out.len] = word;
			out.len += 1;
		}
		out
	}
}

/// What a statement did, which is how a `break` or a `return` leaves a nested body.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Flow {
	Next,
	Break,
	Continue,
	Return,
}

struct Run<'a> {
	resources: &'a dyn Resources,
	values: Vec<Option<Val>>,
	outputs: Outputs,
}

impl Run<'_> {
	fn body(&mut self, body: &[Stmt]) -> Result<Flow, Fault> {
		for statement in body {
			match self.statement(statement)? {
				Flow::Next => {}
				other => return Ok(other),
			}
		}
		Ok(Flow::Next)
	}

	fn statement(&mut self, statement: &Stmt) -> Result<Flow, Fault> {
		match statement {
			Stmt::Assign(value, op) => {
				let computed = self.operation(op)?;
				let slot = value.0 as usize;
				if slot >= self.values.len() {
					return Err(Fault::TypeMismatch);
				}
				self.values[slot] = Some(computed);
				Ok(Flow::Next)
			}
			Stmt::Store(output, value) => {
				let value = self.read(*value)?.clone();
				self.store(*output, value);
				Ok(Flow::Next)
			}
			Stmt::If { condition, then_body, else_body } => {
				let condition = self.read(*condition)?;
				if condition.bool_at(0) { self.body(then_body) } else { self.body(else_body) }
			}
			Stmt::Switch { selector, cases, default } => {
				let selector = self.read(*selector)?.i32_at(0);
				// NO FALL-THROUGH. A case runs its own body and stops; fall-through is a control
				// flow a structured IR has no way to express and a reader has no way to see.
				for (value, body) in cases {
					if *value == selector {
						return self.body(body);
					}
				}
				self.body(default)
			}
			Stmt::Loop { trips, body } => {
				// THE TRIP COUNT IS THE STATEMENT'S OWN and was checked against the device before
				// this ran, so the loop cannot hang.
				for _ in 0..*trips {
					match self.body(body)? {
						Flow::Break => break,
						Flow::Return => return Ok(Flow::Return),
						Flow::Next | Flow::Continue => {}
					}
				}
				Ok(Flow::Next)
			}
			Stmt::Break => Ok(Flow::Break),
			Stmt::Continue => Ok(Flow::Continue),
			Stmt::Return => Ok(Flow::Return),
			Stmt::Discard => {
				self.outputs.discarded = true;
				// A DISCARD ENDS THE STAGE. Continuing would run stores whose results are thrown
				// away, and a texture read among them would be work a discarded fragment paid for.
				Ok(Flow::Return)
			}
		}
	}

	fn store(&mut self, output: render_shader::ir::Output, value: Val) {
		use render_shader::ir::Output;
		match output {
			Output::Position => self.outputs.position = Some(value),
			Output::PointSize => self.outputs.point_size = Some(value.f32_at(0)),
			Output::Varying(location) => self.outputs.varyings.push((location, value)),
			Output::Colour(index) => self.outputs.colour.push((index, value)),
			Output::Integer(index) => self.outputs.integer.push((index, value.u32_at(0))),
			Output::Depth => self.outputs.depth = Some(value.f32_at(0)),
			Output::SampleMask => self.outputs.sample_mask = Some(value.u32_at(0)),
		}
	}

	/// BORROWED AND NOT CLONED, which is the difference between an interpreter that copies a value
	/// per operand and one that reads it in place.
	///
	/// A `Val` is a type plus sixteen inline words - about eighty bytes - and a lit fragment stage
	/// reads two of them per instruction over forty-odd instructions. Returning owned values copied
	/// seven kilobytes per FRAGMENT and ran a type's clone ninety times, for values every arm below
	/// either reads component-wise or copies into a fixed buffer anyway. The two arms that really do
	/// need an owned value - a select that yields one of its operands, and a store - clone at the one
	/// place they need it, and they run once each rather than per operand.
	fn read(&self, value: Value) -> Result<&Val, Fault> {
		self.values.get(value.0 as usize).and_then(|slot| slot.as_ref()).ok_or(Fault::Unassigned { value: value.0 })
	}

	fn operation(&self, op: &Op) -> Result<Val, Fault> {
		match op {
			Op::Const(constant) => Ok(match constant {
				Constant::Bool(value) => Val::scalar_bool(*value),
				Constant::I32(value) => Val::scalar_i32(*value),
				Constant::U32(value) => Val::scalar_u32(*value),
				Constant::F32(value) => Val::scalar_f32(*value),
			}),
			Op::Load(binding) => self.load(binding),
			Op::Compose(kind, parts) => {
				// A FIXED BUFFER: a composed value is at most a `mat4`, and the type check below
				// refuses anything that is not the shape the operation declared.
				let mut words = [0_u32; crate::value::INLINE_WORDS];
				let mut count = 0;
				for part in parts {
					for word in self.read(*part)?.words() {
						if count >= crate::value::INLINE_WORDS {
							return Err(Fault::TypeMismatch);
						}
						words[count] = *word;
						count += 1;
					}
				}
				if count != Val::words_in(kind) {
					return Err(Fault::TypeMismatch);
				}
				Ok(Val::new(kind.clone(), &words[..count]))
			}
			Op::Extract(value, component) => {
				let value = self.read(*value)?;
				let index = *component as usize;
				// A COMPONENT PAST THE VALUE IS REFUSED and not wrapped: wrapping would read the
				// next component of a vector, which is a colour that is almost right.
				let word = *value.words().get(index).ok_or(Fault::IndexOutOfRange { index: index as i64, length: value.words().len() as u32 })?;
				Ok(Val::new(Type::Scalar(value.scalar_type()), &[word]))
			}
			Op::Unary(unary, value) => self.unary(*unary, self.read(*value)?),
			Op::Binary(binary, left, right) => self.binary(*binary, self.read(*left)?, self.read(*right)?),
			Op::Compare(kind, left, right) => Ok(Val::scalar_bool(compare(*kind, self.read(*left)?, self.read(*right)?))),
			Op::Select { condition, on_true, on_false } => {
				let condition = self.read(*condition)?;
				let on_true = self.read(*on_true)?;
				let on_false = self.read(*on_false)?;
				// COMPONENT-WISE FOR A VECTOR CONDITION, which is what makes a per-component mask
				// expressible without a branch per component.
				if condition.len() > 1 {
					let mut words = [0_u32; crate::value::INLINE_WORDS];
					let count = on_true.len().min(crate::value::INLINE_WORDS);
					for (index, slot) in words[..count].iter_mut().enumerate() {
						*slot = if condition.bool_at(index) { on_true.words()[index] } else { on_false.words()[index] };
					}
					return Ok(Val::new(on_true.kind.clone(), &words[..count]));
				}
				Ok(if condition.bool_at(0) { on_true.clone() } else { on_false.clone() })
			}
			Op::Transcendental(which, value, second) => {
				let value = self.read(*value)?;
				let second = match second {
					Some(other) => Some(self.read(*other)?),
					None => None,
				};
				Ok(transcendental(*which, value, second))
			}
			Op::Clamp { value, low, high } => {
				let value = self.read(*value)?;
				let low = self.read(*low)?;
				let high = self.read(*high)?;
				let mut words = [0_u32; crate::value::INLINE_WORDS];
				let count = value.len().min(crate::value::INLINE_WORDS);
				for (index, slot) in words[..count].iter_mut().enumerate() {
					let at = |source: &Val, index: usize| if source.len() == 1 { source.f32_at(0) } else { source.f32_at(index) };
					// `min(max(v, low), high)` AND NOT `clamp`: the order is stated because with a
					// NaN bound the two differ, and two backends that chose differently would
					// produce different colours from the same shader.
					*slot = f32::to_bits(at(value, index).max(at(low, index)).min(at(high, index)));
				}
				Ok(Val::new(value.kind.clone(), &words[..count]))
			}
			Op::Mix { from, to, at } => {
				let from = self.read(*from)?;
				let to = self.read(*to)?;
				let at_value = self.read(*at)?;
				let mut words = [0_u32; crate::value::INLINE_WORDS];
				let count = from.len().min(crate::value::INLINE_WORDS);
				for (index, slot) in words[..count].iter_mut().enumerate() {
					let t = if at_value.len() == 1 { at_value.f32_at(0) } else { at_value.f32_at(index) };
					// `(1-t)*a + t*b`, exact at both ends whatever the rounding.
					*slot = f32::to_bits((1.0 - t) * from.f32_at(index) + t * to.f32_at(index));
				}
				Ok(Val::new(from.kind.clone(), &words[..count]))
			}
			Op::Index { array, index } => {
				let array = self.read(*array)?;
				let index = self.read(*index)?.i32_at(0) as i64;
				let Type::Array(element, length) = array.kind.clone() else { return Err(Fault::TypeMismatch) };
				if index < 0 || index >= length as i64 {
					// THE COMPUTED CASE, which validation cannot decide.
					return Err(Fault::IndexOutOfRange { index, length });
				}
				let stride = Val::words_in(&element);
				let start = index as usize * stride;
				let slice = array.words().get(start..start + stride).ok_or(Fault::IndexOutOfRange { index, length })?;
				Ok(Val::new((*element).clone(), slice))
			}
			Op::Sample { binding, coordinate } => {
				let Binding::Texture { texture, sampler } = binding else { return Err(Fault::TypeMismatch) };
				let coordinate = self.read(*coordinate)?;
				self.resources.sample(*texture, *sampler, coordinate).ok_or(Fault::SampleFailed)
			}
		}
	}

	fn load(&self, binding: &Binding) -> Result<Val, Fault> {
		match binding {
			Binding::Uniform { block, member } => self.resources.uniform(*block, *member),
			Binding::Attribute { location } => self.resources.attribute(*location),
			Binding::Varying { location } => self.resources.varying(*location),
			Binding::BuiltIn(which) => self.resources.built_in(*which),
			Binding::Texture { .. } => None,
		}
		.ok_or(Fault::MissingBinding)
	}

	fn unary(&self, unary: UnaryOp, value: &Val) -> Result<Val, Fault> {
		let scalar = value.scalar_type();
		let words: Inline = match unary {
			UnaryOp::Negate => (0..value.len())
				.map(|index| match scalar {
					ScalarType::F32 => (-value.f32_at(index)).to_bits(),
					ScalarType::I32 => value.i32_at(index).wrapping_neg() as u32,
					_ => value.u32_at(index).wrapping_neg(),
				})
				.collect(),
			UnaryOp::Not => (0..value.len()).map(|index| u32::from(!value.bool_at(index))).collect(),
			UnaryOp::Complement => (0..value.len()).map(|index| !value.u32_at(index)).collect(),
			UnaryOp::Abs => (0..value.len())
				.map(|index| match scalar {
					ScalarType::F32 => libm::fabsf(value.f32_at(index)).to_bits(),
					ScalarType::I32 => value.i32_at(index).wrapping_abs() as u32,
					_ => value.u32_at(index),
				})
				.collect(),
			UnaryOp::Floor => (0..value.len()).map(|index| libm::floorf(value.f32_at(index)).to_bits()).collect(),
			UnaryOp::Ceil => (0..value.len()).map(|index| libm::ceilf(value.f32_at(index)).to_bits()).collect(),
			// `x - floor(x)` AND NOT `x % 1`. The two differ for a negative `x`, and `fract(-0.25)`
			// is `0.75` here - which is what a repeating texture coordinate needs and what the
			// language's `%` does not give.
			UnaryOp::Fract => (0..value.len()).map(|index| (value.f32_at(index) - libm::floorf(value.f32_at(index))).to_bits()).collect(),
			UnaryOp::Length => {
				let sum: f32 = (0..value.len()).map(|index| value.f32_at(index) * value.f32_at(index)).sum();
				return Ok(Val::scalar_f32(libm::sqrtf(sum)));
			}
			UnaryOp::Normalize => {
				let sum: f32 = (0..value.len()).map(|index| value.f32_at(index) * value.f32_at(index)).sum();
				let length = libm::sqrtf(sum);
				// A ZERO-LENGTH VECTOR NORMALISES TO ITSELF rather than to a NaN. The alternative
				// puts a NaN into a lighting term, and one NaN makes a whole surface black.
				if length == 0.0 || !length.is_finite() {
					return Ok(value.clone());
				}
				(0..value.len()).map(|index| (value.f32_at(index) / length).to_bits()).collect()
			}
			UnaryOp::Convert(target) => {
				let words: Inline = (0..value.len())
					.map(|index| match (scalar, target) {
						(ScalarType::F32, ScalarType::I32) => value.f32_at(index) as i32 as u32,
						(ScalarType::F32, ScalarType::U32) => value.f32_at(index) as u32,
						(ScalarType::F32, ScalarType::Bool) => u32::from(value.f32_at(index) != 0.0),
						(ScalarType::I32, ScalarType::F32) => (value.i32_at(index) as f32).to_bits(),
						(ScalarType::U32, ScalarType::F32) => (value.u32_at(index) as f32).to_bits(),
						(ScalarType::Bool, ScalarType::F32) => f32::from(value.bool_at(index)).to_bits(),
						(ScalarType::Bool, _) => u32::from(value.bool_at(index)),
						(_, ScalarType::Bool) => u32::from(value.u32_at(index) != 0),
						_ => value.u32_at(index),
					})
					.collect();
				let kind = match value.kind {
					Type::Vector(_, components) => Type::Vector(target, components),
					_ => Type::Scalar(target),
				};
				return Ok(Val::new(kind, &words));
			}
		};
		let kind = match (unary, &value.kind) {
			(UnaryOp::Not, Type::Vector(_, components)) => Type::Vector(ScalarType::Bool, *components),
			(UnaryOp::Not, _) => Type::Scalar(ScalarType::Bool),
			(_, kind) => kind.clone(),
		};
		Ok(Val::new(kind, &words))
	}

	fn binary(&self, binary: BinaryOp, left: &Val, right: &Val) -> Result<Val, Fault> {
		match binary {
			BinaryOp::Dot => {
				let sum: f32 = (0..left.len().min(right.len())).map(|index| left.f32_at(index) * right.f32_at(index)).sum();
				return Ok(Val::scalar_f32(sum));
			}
			BinaryOp::Cross => {
				if left.len() != 3 || right.len() != 3 {
					return Err(Fault::TypeMismatch);
				}
				let (a, b) = (left.to_f32(), right.to_f32());
				return Ok(Val::vector_f32(&[a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]));
			}
			BinaryOp::MatrixProduct => return matrix_product(&left, &right),
			_ => {}
		}
		let scalar = if left.len() >= right.len() { left.scalar_type() } else { right.scalar_type() };
		let count = left.len().max(right.len());
		let words: Inline = (0..count)
			.map(|index| {
				let at = |source: &Val, index: usize| if source.len() == 1 { 0 } else { index };
				let (li, ri) = (at(&left, index), at(&right, index));
				match scalar {
					ScalarType::F32 => {
						let (a, b) = (left.f32_at(li), right.f32_at(ri));
						match binary {
							BinaryOp::Add => a + b,
							BinaryOp::Subtract => a - b,
							BinaryOp::Multiply => a * b,
							BinaryOp::Divide => a / b,
							// EUCLIDEAN REMAINDER and not the language's `%`, so `-0.25 mod 1` is
							// `0.75` - which is what a repeating coordinate needs and what makes the
							// answer independent of the sign convention of the host language.
							BinaryOp::Modulo => a - b * libm::floorf(a / b),
							BinaryOp::Min => a.min(b),
							BinaryOp::Max => a.max(b),
							_ => a,
						}
						.to_bits()
					}
					ScalarType::I32 => {
						let (a, b) = (left.i32_at(li), right.i32_at(ri));
						(match binary {
							BinaryOp::Add => a.wrapping_add(b),
							BinaryOp::Subtract => a.wrapping_sub(b),
							BinaryOp::Multiply => a.wrapping_mul(b),
							// A DIVISION BY ZERO ANSWERS ZERO rather than trapping. A shader has no
							// way to report one and a trap would take the frame down; the profile's
							// non-finite rules cover the float case and this is the integer one.
							BinaryOp::Divide => {
								if b == 0 {
									0
								} else {
									a.wrapping_div(b)
								}
							}
							BinaryOp::Modulo => {
								if b == 0 {
									0
								} else {
									a.rem_euclid(b)
								}
							}
							BinaryOp::And => a & b,
							BinaryOp::Or => a | b,
							BinaryOp::Xor => a ^ b,
							BinaryOp::ShiftLeft => a.wrapping_shl(b as u32),
							BinaryOp::ShiftRight => a.wrapping_shr(b as u32),
							BinaryOp::Min => a.min(b),
							BinaryOp::Max => a.max(b),
							_ => a,
						}) as u32
					}
					_ => {
						let (a, b) = (left.u32_at(li), right.u32_at(ri));
						match binary {
							BinaryOp::Add => a.wrapping_add(b),
							BinaryOp::Subtract => a.wrapping_sub(b),
							BinaryOp::Multiply => a.wrapping_mul(b),
							BinaryOp::Divide => {
								if b == 0 {
									0
								} else {
									a / b
								}
							}
							BinaryOp::Modulo => {
								if b == 0 {
									0
								} else {
									a % b
								}
							}
							BinaryOp::And => a & b,
							BinaryOp::Or => a | b,
							BinaryOp::Xor => a ^ b,
							BinaryOp::ShiftLeft => a.wrapping_shl(b),
							BinaryOp::ShiftRight => a.wrapping_shr(b),
							BinaryOp::Min => a.min(b),
							BinaryOp::Max => a.max(b),
							_ => a,
						}
					}
				}
			})
			.collect();
		let kind = if left.len() >= right.len() { &left.kind } else { &right.kind };
		Ok(Val::new(kind.clone(), &words))
	}
}

/// A matrix times a vector, or a matrix times a matrix. COLUMN-MAJOR AND `M * v`, the same
/// convention `render-math` fixes - a second convention here would make a shader and the scene layer
/// disagree about what a transform does.
fn matrix_product(left: &Val, right: &Val) -> Result<Val, Fault> {
	let Type::Matrix(size) = left.kind else { return Err(Fault::TypeMismatch) };
	let size = size as usize;
	let element = |value: &Val, row: usize, column: usize| value.f32_at(column * size + row);
	match right.kind {
		Type::Vector(_, components) if components as usize == size => {
			let out: Vec<f32> = (0..size).map(|row| (0..size).map(|column| element(left, row, column) * right.f32_at(column)).sum()).collect();
			Ok(Val::vector_f32(&out))
		}
		Type::Matrix(other) if other as usize == size => {
			let mut words = [0_u32; crate::value::INLINE_WORDS];
			let mut count = 0;
			for column in 0..size {
				for row in 0..size {
					if count >= crate::value::INLINE_WORDS {
						return Err(Fault::TypeMismatch);
					}
					let value: f32 = (0..size).map(|step| element(left, row, step) * element(right, step, column)).sum();
					words[count] = value.to_bits();
					count += 1;
				}
			}
			Ok(Val::new(Type::Matrix(size as u8), &words[..count]))
		}
		_ => Err(Fault::TypeMismatch),
	}
}

fn compare(kind: CompareKind, left: &Val, right: &Val) -> bool {
	match left.scalar_type() {
		ScalarType::F32 => {
			let (a, b) = (left.f32_at(0), right.f32_at(0));
			match kind {
				// A NaN COMPARES FALSE TO EVERYTHING INCLUDING ITSELF, except that `NotEqual` is
				// true - which is IEEE's rule and the one the frozen non-finite handling states.
				CompareKind::Equal => a == b,
				CompareKind::NotEqual => a != b,
				CompareKind::Less => a < b,
				CompareKind::LessOrEqual => a <= b,
				CompareKind::Greater => a > b,
				CompareKind::GreaterOrEqual => a >= b,
			}
		}
		ScalarType::I32 => {
			let (a, b) = (left.i32_at(0), right.i32_at(0));
			ordered(kind, a.cmp(&b))
		}
		_ => {
			let (a, b) = (left.u32_at(0), right.u32_at(0));
			ordered(kind, a.cmp(&b))
		}
	}
}

fn ordered(kind: CompareKind, ordering: core::cmp::Ordering) -> bool {
	use core::cmp::Ordering;
	match kind {
		CompareKind::Equal => ordering == Ordering::Equal,
		CompareKind::NotEqual => ordering != Ordering::Equal,
		CompareKind::Less => ordering == Ordering::Less,
		CompareKind::LessOrEqual => ordering != Ordering::Greater,
		CompareKind::Greater => ordering == Ordering::Greater,
		CompareKind::GreaterOrEqual => ordering != Ordering::Less,
	}
}

/// The transcendentals, component-wise.
fn transcendental(which: Transcendental, value: &Val, second: Option<&Val>) -> Val {
	let words: Inline = (0..value.len())
		.map(|index| {
			let a = value.f32_at(index);
			let b = second.map(|other| if other.len() == 1 { other.f32_at(0) } else { other.f32_at(index) }).unwrap_or(0.0);
			match which {
				Transcendental::Sqrt => libm::sqrtf(a),
				// `1/sqrt(x)` AND NOT A FAST APPROXIMATION. A reciprocal square root that was
				// approximated would put this operation on a different accuracy footing from the
				// square root beside it, and the profile bounds them separately for that reason.
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
			.to_bits()
		})
		.collect();
	Val::new(value.kind.clone(), &words)
}

/// The stage a module is, so a caller can refuse to run one where the other belongs.
pub fn stage(module: &Module) -> Stage {
	module.stage
}

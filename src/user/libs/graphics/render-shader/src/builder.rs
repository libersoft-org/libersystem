//! THE PROGRAMMATIC BUILDER, which is what is shipped instead of a language.
//!
//! A TEXT SHADING LANGUAGE IS A SEPARATE, LATER, OPTIONAL PIECE. Writing one now would mean writing a
//! parser, a type checker and a diagnostic story before the first shader runs - and the builder
//! produces the same IR the language eventually would, so nothing written against it is rewritten
//! when one arrives.
//!
//! THE BUILDER ASSIGNS THE IDS, which is what makes SSA a property of construction rather than a
//! convention: there is no way to hand out the same `Value` twice, because a caller never chooses one.
//!
//! IT DOES NOT VALIDATE. Construction and validation are separate on purpose: a builder that refused
//! as it went could not express a forward reference, and a module is validated ONCE at load with the
//! whole program in front of it - which is where the position dependency slice can be walked at all.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::ir::{Binding, Constant, Interpolation, Module, Op, Output, Sampling, Stage, Stmt, Type, Value, Varying};

/// Builds one stage.
pub struct Builder {
	stage: Stage,
	name: String,
	types: Vec<Type>,
	varyings: Vec<Varying>,
	/// The statement list being appended to, innermost last - which is how a nested body is opened
	/// and closed without the caller holding a cursor.
	stack: Vec<Vec<Stmt>>,
}

impl Builder {
	pub fn new(stage: Stage, name: &str) -> Self {
		Self { stage, name: name.to_string(), types: Vec::new(), varyings: Vec::new(), stack: alloc::vec![Vec::new()] }
	}

	/// Assign an operation, answering the value it produced.
	pub fn assign(&mut self, kind: Type, op: Op) -> Value {
		let value = Value(self.types.len() as u32);
		self.types.push(kind);
		self.emit(Stmt::Assign(value, op));
		value
	}

	pub fn constant(&mut self, constant: Constant) -> Value {
		self.assign(Type::Scalar(constant.scalar_type()), Op::Const(constant))
	}

	pub fn load(&mut self, kind: Type, binding: Binding) -> Value {
		self.assign(kind, Op::Load(binding))
	}

	pub fn store(&mut self, output: Output, value: Value) {
		self.emit(Stmt::Store(output, value));
	}

	pub fn varying(&mut self, location: u32, kind: Type, interpolation: Interpolation) {
		self.varyings.push(Varying { location, kind, interpolation, sampling: Sampling::Pixel });
	}

	/// A varying with an explicit evaluation location: `centroid` or `sample`.
	pub fn varying_at(&mut self, location: u32, kind: Type, interpolation: Interpolation, sampling: Sampling) {
		self.varyings.push(Varying { location, kind, interpolation, sampling });
	}

	/// Open a conditional. The next statements go into its `then` body until `else_branch` or `end`.
	pub fn if_then(&mut self, condition: Value) -> Conditional {
		self.stack.push(Vec::new());
		Conditional { condition, then_body: None }
	}

	pub fn else_branch(&mut self, mut conditional: Conditional) -> Conditional {
		conditional.then_body = Some(self.stack.pop().unwrap_or_default());
		self.stack.push(Vec::new());
		conditional
	}

	pub fn end_if(&mut self, conditional: Conditional) {
		let closed = self.stack.pop().unwrap_or_default();
		let (then_body, else_body) = match conditional.then_body {
			Some(then_body) => (then_body, closed),
			None => (closed, Vec::new()),
		};
		self.emit(Stmt::If { condition: conditional.condition, then_body, else_body });
	}

	/// Open a loop. THE TRIP COUNT IS REQUIRED HERE, which is where "an unbounded loop is
	/// unrepresentable" is enforced: there is no `loop()` without a number.
	pub fn loop_bounded(&mut self, trips: u32) {
		self.stack.push(Vec::new());
		let _ = trips;
	}

	pub fn end_loop(&mut self, trips: u32) {
		let body = self.stack.pop().unwrap_or_default();
		self.emit(Stmt::Loop { trips, body });
	}

	pub fn break_loop(&mut self) {
		self.emit(Stmt::Break);
	}

	pub fn continue_loop(&mut self) {
		self.emit(Stmt::Continue);
	}

	pub fn discard(&mut self) {
		self.emit(Stmt::Discard);
	}

	/// Finish. The module is NOT validated here - see the note at the top of this file.
	pub fn finish(mut self) -> Module {
		let body = self.stack.pop().unwrap_or_default();
		Module { stage: self.stage, name: self.name, types: self.types, varyings: self.varyings, body }
	}

	fn emit(&mut self, statement: Stmt) {
		if let Some(body) = self.stack.last_mut() {
			body.push(statement);
		}
	}
}

/// A conditional being built. Carried by the caller so the builder does not need a second stack for
/// which branch is open.
pub struct Conditional {
	condition: Value,
	then_body: Option<Vec<Stmt>>,
}

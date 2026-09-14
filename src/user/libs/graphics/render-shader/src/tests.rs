//! The fixtures. The ones that matter are the REFUSALS: an IR that validates a correct module and
//! nothing else has not been shown to validate anything.

use super::*;
use crate::ir::{BuiltIn, Output, Varying};
use crate::validate::{ShaderLimits, validate};
use alloc::vec;

fn limits() -> ShaderLimits {
	ShaderLimits::profile_minimum()
}

/// A vertex stage that writes a position from a uniform matrix times an attribute. The smallest
/// module that is a real one.
fn simple_vertex() -> Module {
	let mut builder = Builder::new(Stage::Vertex, "simple");
	let matrix = builder.load(Type::Matrix(4), Binding::Uniform { block: 0, member: 0 });
	let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let clip = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, matrix, position));
	builder.store(Output::Position, clip);
	builder.finish()
}

#[test]
// THE MODULE THAT SHOULD VALIDATE, so every refusal below is a refusal of something and not of
// everything.
fn a_vertex_stage_that_writes_a_position_validates() {
	assert_eq!(validate(&simple_vertex(), &limits()), Ok(()));
}

#[test]
// A STAGE THAT NEVER WRITES WHAT IT EXISTS TO PRODUCE is refused at LOAD. A vertex stage with no
// position produces no geometry, and finding that out at the draw would name the draw.
fn a_stage_that_writes_no_output_is_refused_at_load() {
	let mut builder = Builder::new(Stage::Vertex, "silent");
	let _ = builder.constant(Constant::F32(1.0));
	assert!(matches!(validate(&builder.finish(), &limits()), Err(Error::MissingOutput { .. })));

	let mut fragment = Builder::new(Stage::Fragment, "silent");
	let _ = fragment.constant(Constant::F32(1.0));
	assert!(matches!(validate(&fragment.finish(), &limits()), Err(Error::MissingOutput { .. })));
}

#[test]
// A STAGE MAY NOT WRITE THE OTHER STAGE'S OUTPUTS. A fragment stage writing a position is not a
// shader with a harmless extra store; it is a shader whose author believes something false.
fn a_stage_writing_the_other_stages_output_is_refused() {
	let mut builder = Builder::new(Stage::Fragment, "confused");
	let value = builder.constant(Constant::F32(1.0));
	let position = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![value, value, value, value]));
	builder.store(Output::Position, position);
	assert!(matches!(validate(&builder.finish(), &limits()), Err(Error::WrongStageOutput { .. })));
}

#[test]
// SSA IS ESTABLISHED AND NOT ASSUMED. A module that reads an unassigned value would have a backend
// reading whichever register was there, and one that assigns twice would have two.
fn a_value_used_before_assignment_or_assigned_twice_is_refused() {
	// Reading a value that is never assigned.
	let module = Module { stage: Stage::Vertex, name: alloc::string::String::from("undefined"), types: vec![Type::vec(4), Type::vec(4)], varyings: vec![], body: vec![Stmt::Store(Output::Position, Value(1))] };
	assert_eq!(validate(&module, &limits()), Err(Error::UndefinedValue { value: 1 }));

	// Assigning one twice.
	let twice = Module {
		stage: Stage::Vertex,
		name: alloc::string::String::from("twice"),
		types: vec![Type::vec(4)],
		varyings: vec![],
		body: vec![
			Stmt::Assign(Value(0), Op::Load(Binding::Attribute { location: 0 })),
			Stmt::Assign(Value(0), Op::Load(Binding::Attribute { location: 1 })),
			Stmt::Store(Output::Position, Value(0)),
		],
	};
	assert_eq!(validate(&twice, &limits()), Err(Error::NotSingleAssignment { value: 0 }));
}

#[test]
// AN UNBOUNDED LOOP IS UNREPRESENTABLE - `Loop` carries its trip count as a field - and a bound of
// zero or one past the device's is refused. That is what makes a frame's worst case computable.
fn a_loop_carries_its_bound_and_the_bound_is_checked() {
	let build = |trips: u32| {
		let mut builder = Builder::new(Stage::Vertex, "looping");
		let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
		builder.loop_bounded(trips);
		builder.break_loop();
		builder.end_loop(trips);
		builder.store(Output::Position, position);
		builder.finish()
	};
	assert_eq!(validate(&build(16), &limits()), Ok(()));
	assert!(matches!(validate(&build(0), &limits()), Err(Error::LoopBound { trips: 0, .. })), "a loop of no iterations is a mistake rather than a no-op");
	assert!(matches!(validate(&build(100_000), &limits()), Err(Error::LoopBound { .. })), "and one past the device's bound is refused");
}

#[test]
// NESTING IS BOUNDED TOO, because a trip count per level multiplies: eight nested loops of the
// maximum trip count is a number no frame finishes.
fn loop_nesting_is_bounded() {
	let mut builder = Builder::new(Stage::Vertex, "nested");
	let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let depth = limits().max_loop_depth + 1;
	for _ in 0..depth {
		builder.loop_bounded(2);
	}
	for _ in 0..depth {
		builder.end_loop(2);
	}
	builder.store(Output::Position, position);
	assert!(matches!(validate(&builder.finish(), &limits()), Err(Error::LoopDepth { .. })));
}

#[test]
// `break` AND `continue` OUTSIDE A LOOP, and `discard` outside a fragment stage. Each is a statement
// with no meaning where it is, and structured control flow is what makes that decidable.
fn misplaced_control_flow_is_refused_by_name() {
	let mut builder = Builder::new(Stage::Vertex, "loose");
	let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	builder.break_loop();
	builder.store(Output::Position, position);
	assert!(matches!(validate(&builder.finish(), &limits()), Err(Error::MisplacedControlFlow { .. })));

	let mut vertex = Builder::new(Stage::Vertex, "discarding");
	let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
	vertex.discard();
	vertex.store(Output::Position, position);
	assert!(matches!(validate(&vertex.finish(), &limits()), Err(Error::MisplacedControlFlow { .. })));
}

#[test]
// A CONSTANT INDEX OUTSIDE ITS ARRAY IS REFUSED AT LOAD. A dynamic one cannot be, and the contract
// is that a backend clamps it - which is what "checked memory access" means for an index nothing at
// load time can know.
fn a_constant_index_outside_its_array_is_refused() {
	let build = |literal: i32| {
		let mut builder = Builder::new(Stage::Vertex, "indexing");
		let array = builder.load(Type::Array(alloc::boxed::Box::new(Type::vec(4)), 4), Binding::Uniform { block: 0, member: 0 });
		let index = builder.constant(Constant::I32(literal));
		let element = builder.assign(Type::vec(4), Op::Index { array, index });
		builder.store(Output::Position, element);
		builder.finish()
	};
	assert_eq!(validate(&build(3), &limits()), Ok(()));
	assert!(matches!(validate(&build(4), &limits()), Err(Error::IndexOutOfRange { index: 4, length: 4 })));
	assert!(matches!(validate(&build(-1), &limits()), Err(Error::IndexOutOfRange { index: -1, length: 4 })));
}

#[test]
// THE ONE THIS MODULE EXISTS FOR: a POSITION may depend only on operations with a strict definition.
// `sqrt` is correctly rounded and is allowed; `sin` has a four-ULP bound and is NOT deterministic
// across backends, so a position depending on it is refused AT MODULE LOAD.
fn a_position_may_depend_on_sqrt_and_not_on_sin() {
	let build = |which: Transcendental| {
		let mut builder = Builder::new(Stage::Vertex, "transcendental");
		let attribute = builder.load(Type::f32(), Binding::Attribute { location: 0 });
		let transformed = builder.assign(Type::f32(), Op::Transcendental(which, attribute, None));
		let position = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![transformed, transformed, transformed, transformed]));
		builder.store(Output::Position, position);
		builder.finish()
	};
	assert_eq!(validate(&build(Transcendental::Sqrt), &limits()), Ok(()), "sqrt is correctly rounded, which is what a strict definition is");
	assert_eq!(validate(&build(Transcendental::Sin), &limits()), Err(Error::NonDeterministicPosition { operation: "sin, cos", max_ulp: 4, depth: 1 }), "and the refusal names the operation, its bound and how far from the position it was found");
	assert!(matches!(validate(&build(Transcendental::Pow), &limits()), Err(Error::NonDeterministicPosition { max_ulp: 16, .. })));
	assert!(matches!(validate(&build(Transcendental::InverseSqrt), &limits()), Err(Error::NonDeterministicPosition { max_ulp: 2, .. })), "even two ULP is not zero");
}

#[test]
// IT IS THE COMPLETE DEPENDENCY SLICE AND NOT THE FINAL MULTIPLY. Walking one step back from the
// position store would admit a matrix built from a transcendental - which is the shape a real shader
// has, and the reason this check is a slice rather than a look at the last operation.
fn the_slice_reaches_a_transcendental_several_steps_from_the_position() {
	let mut builder = Builder::new(Stage::Vertex, "deep");
	let time = builder.load(Type::f32(), Binding::Uniform { block: 0, member: 0 });
	let wave = builder.assign(Type::f32(), Op::Transcendental(Transcendental::Sin, time, None));
	let scaled = builder.assign(Type::f32(), Op::Binary(BinaryOp::Multiply, wave, time));
	let offset = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![scaled, scaled, scaled, scaled]));
	let attribute = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let position = builder.assign(Type::vec(4), Op::Binary(BinaryOp::Add, attribute, offset));
	builder.store(Output::Position, position);
	let error = validate(&builder.finish(), &limits());
	assert!(matches!(error, Err(Error::NonDeterministicPosition { operation: "sin, cos", .. })), "four steps back, and still on the path: {error:?}");
}

#[test]
// THE CONTROL HALF, which is what a naive implementation misses: a position SELECTED by a branch on
// a transcendental is as backend-dependent as one computed from it, because the two backends take
// different branches at the boundary.
fn a_position_selected_by_a_branch_on_a_transcendental_is_refused() {
	let mut builder = Builder::new(Stage::Vertex, "branching");
	let time = builder.load(Type::f32(), Binding::Uniform { block: 0, member: 0 });
	let wave = builder.assign(Type::f32(), Op::Transcendental(Transcendental::Cos, time, None));
	let zero = builder.constant(Constant::F32(0.0));
	let condition = builder.assign(Type::Scalar(ScalarType::Bool), Op::Compare(CompareKind::Greater, wave, zero));
	// TWO POSITIONS, NEITHER OF WHICH TOUCHES THE TRANSCENDENTAL. Only the branch does.
	let first = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let second = builder.load(Type::vec(4), Binding::Attribute { location: 1 });
	let conditional = builder.if_then(condition);
	builder.store(Output::Position, first);
	let conditional = builder.else_branch(conditional);
	builder.store(Output::Position, second);
	builder.end_if(conditional);
	let error = validate(&builder.finish(), &limits());
	assert!(matches!(error, Err(Error::NonDeterministicPosition { operation: "sin, cos", .. })), "the arithmetic of the position is strict and the BRANCH is not, which is the half a data-only slice misses: {error:?}");
}

#[test]
// AND A FRAGMENT STAGE DOES NOT ACQUIRE STRICT REQUIREMENTS BY SHARING THE IR WITH A VERTEX ONE. A
// colour that differs by one ULP is not a wrong picture; a position that does is a wrong triangle.
fn a_fragment_stage_may_use_every_transcendental() {
	let mut builder = Builder::new(Stage::Fragment, "relaxed");
	let coordinate = builder.load(Type::f32(), Binding::Varying { location: 0 });
	let wave = builder.assign(Type::f32(), Op::Transcendental(Transcendental::Sin, coordinate, None));
	let colour = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![wave, wave, wave, wave]));
	builder.store(Output::Colour(0), colour);
	assert_eq!(validate(&builder.finish(), &limits()), Ok(()));
}

#[test]
// THE INDEX OF A MEMORY READ IS ON THE PATH TOO, because an index that differed by one between
// backends would read a different vertex - which is a different position.
fn an_index_computed_from_a_transcendental_is_on_the_position_path() {
	let mut builder = Builder::new(Stage::Vertex, "indexed");
	let time = builder.load(Type::f32(), Binding::Uniform { block: 0, member: 0 });
	let wave = builder.assign(Type::f32(), Op::Transcendental(Transcendental::Sin, time, None));
	let index = builder.assign(Type::Scalar(ScalarType::I32), Op::Unary(UnaryOp::Convert(ScalarType::I32), wave));
	let array = builder.load(Type::Array(alloc::boxed::Box::new(Type::vec(4)), 8), Binding::Uniform { block: 0, member: 1 });
	let element = builder.assign(Type::vec(4), Op::Index { array, index });
	builder.store(Output::Position, element);
	assert!(matches!(validate(&builder.finish(), &limits()), Err(Error::NonDeterministicPosition { operation: "sin, cos", .. })));
}

#[test]
// THE LIST OF WHAT IS NEVER ALLOWED ON A POSITION PATH, for a report that wants it rather than
// discovering it one refusal at a time - and it is DERIVED from the frozen accuracy table, so an
// accuracy corrected in the profile corrects this without a second edit.
fn the_non_strict_set_is_derived_from_the_frozen_accuracy_table() {
	let refused = validate::non_strict_operations();
	assert!(refused.iter().any(|(name, ulp)| *name == "sin, cos" && *ulp == 4));
	assert!(refused.iter().any(|(name, ulp)| *name == "pow" && *ulp == 16));
	assert!(!refused.iter().any(|(name, _)| *name == "sqrt"), "sqrt is correctly rounded and is the one that is allowed");
	// Every entry names an operation the profile's own table has.
	for (name, _) in &refused {
		assert!(graphics_profile::shader_ir::TRANSCENDENTAL_ACCURACY.iter().any(|entry| entry.operation == *name), "{name} is not in the frozen table");
	}
}

#[test]
// AN INSTRUCTION COUNT PAST THE DEVICE'S BOUND, which is a limit on the shader rather than on the
// frame - the same distinction the device limits make.
fn a_module_past_the_instruction_bound_is_refused() {
	let mut builder = Builder::new(Stage::Vertex, "long");
	let mut value = builder.load(Type::f32(), Binding::Attribute { location: 0 });
	let tight = ShaderLimits { max_instructions: 16, ..limits() };
	for _ in 0..32 {
		value = builder.assign(Type::f32(), Op::Binary(BinaryOp::Add, value, value));
	}
	let position = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![value, value, value, value]));
	builder.store(Output::Position, position);
	assert!(matches!(validate(&builder.finish(), &tight), Err(Error::TooManyInstructions { ceiling: 16, .. })));
}

#[test]
// THE BUILT-INS AND THE VARYINGS EXIST AND ARE NAMED, which a fixture holds so that a stage reading
// one is a module that validates rather than a plan that says it should.
fn a_stage_reads_its_built_ins_and_declares_its_varyings() {
	let mut builder = Builder::new(Stage::Vertex, "instanced");
	let instance = builder.load(Type::Scalar(ScalarType::U32), Binding::BuiltIn(BuiltIn::InstanceIndex));
	let as_float = builder.assign(Type::f32(), Op::Unary(UnaryOp::Convert(ScalarType::F32), instance));
	let position = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![as_float, as_float, as_float, as_float]));
	builder.varying(0, Type::vec(2), Interpolation::Smooth);
	builder.varying(1, Type::Scalar(ScalarType::U32), Interpolation::Flat);
	builder.store(Output::Varying(0), position);
	builder.store(Output::Position, position);
	let module = builder.finish();
	assert_eq!(validate(&module, &limits()), Ok(()));
	assert_eq!(
		module.varyings,
		vec![
			Varying { location: 0, kind: Type::vec(2), interpolation: Interpolation::Smooth, sampling: crate::ir::Sampling::Pixel },
			Varying { location: 1, kind: Type::Scalar(ScalarType::U32), interpolation: Interpolation::Flat, sampling: crate::ir::Sampling::Pixel },
		]
	);
}

// ---------------------------------------------------------------------------------------------
// Hostile IR: what a builder with a bug, or a caller with bad intentions, hands the validator.
// ---------------------------------------------------------------------------------------------

/// A deterministic generator. NOT A RANDOM ONE: a fuzz fixture whose failing case cannot be
/// reproduced reports a defect nobody can find, so the sequence is fixed and a failure names the
/// iteration that produced it.
struct Noise(u64);

impl Noise {
	fn next(&mut self) -> u64 {
		let mut state = self.0;
		state ^= state >> 12;
		state ^= state << 25;
		state ^= state >> 27;
		self.0 = state;
		state.wrapping_mul(0x2545_F491_4F6C_DD1D)
	}

	fn below(&mut self, ceiling: u32) -> u32 {
		if ceiling == 0 { 0 } else { (self.next() % ceiling as u64) as u32 }
	}
}

/// A module that is a VALID one with zero or more hostile mutations applied.
///
/// NOT A BODY OF PURE NOISE. A generator that emitted arbitrary statements over arbitrary value ids
/// would be refused every single time - by SSA, by types, by the missing output - and a fixture that
/// never reaches the accepting path has not shown that anything is accepted. Starting from a module
/// that validates and breaking it in one place at a time is what puts the validator's YES and its NO
/// under the same generator.
fn mutated_module(noise: &mut Noise) -> Module {
	let mut module = if noise.next() & 1 == 0 { simple_vertex() } else { simple_fragment() };
	for _ in 0..noise.below(4) {
		mutate(noise, &mut module);
	}
	module
}

fn simple_fragment() -> Module {
	let mut builder = Builder::new(Stage::Fragment, "simple");
	let one = builder.constant(Constant::F32(1.0));
	let colour = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![one, one, one, one]));
	builder.store(Output::Colour(0), colour);
	builder.finish()
}

/// One hostile change. Each is a real mistake a builder can make.
fn mutate(noise: &mut Noise, module: &mut Module) {
	let ids = module.types.len() as u32;
	let stray = Value(noise.below(ids + 3));
	match noise.below(10) {
		// Read a value that was never assigned, or assign one twice.
		0 => module.body.push(Stmt::Assign(stray, Op::Binary(BinaryOp::Add, stray, stray))),
		1 => module.body.push(Stmt::Assign(Value(ids + 5), Op::Const(Constant::F32(2.0)))),
		// Store to the other stage's output.
		2 => module.body.push(Stmt::Store(if module.stage == Stage::Vertex { Output::Colour(0) } else { Output::Position }, stray)),
		// Remove the store the stage exists to produce.
		3 => module.body.retain(|statement| !matches!(statement, Stmt::Store(..))),
		// A loop whose trip count is zero, or one far past the ceiling.
		4 => module.body.push(Stmt::Loop { trips: 0, body: vec![] }),
		5 => module.body.push(Stmt::Loop { trips: 70_000, body: vec![] }),
		// Control flow where it does not belong.
		6 => module.body.push(if noise.next() & 1 == 0 { Stmt::Break } else { Stmt::Continue }),
		7 => module.body.push(Stmt::Discard),
		// A transcendental on the position's path, which is the refusal this crate exists for.
		8 => {
			let source = Value(ids);
			module.types.push(Type::vec(4));
			module.body.insert(0, Stmt::Assign(source, Op::Transcendental(Transcendental::Sin, Value(0), None)));
			module.body.push(Stmt::Store(Output::Position, source));
		}
		// An index that cannot be in range.
		_ => {
			let index = Value(ids);
			module.types.push(Type::Scalar(crate::ir::ScalarType::I32));
			module.body.push(Stmt::Assign(index, Op::Const(Constant::I32(1_000))));
			module.body.push(Stmt::Assign(Value(ids + 1), Op::Index { array: stray, index }));
			module.types.push(Type::f32());
		}
	}
}

#[test]
// HOSTILE IR IS ALWAYS ANSWERED. Two thousand generated modules reach the validator; every one comes
// back `Ok` or with a TYPED error, and nothing panics, hangs or overflows a stack. A validator that
// is only ever handed modules its own builder produced has not been shown to validate anything.
fn two_thousand_hostile_modules_are_each_answered_rather_than_crashing() {
	let limits = limits();
	let mut noise = Noise(0xF0F0_1234_5678_0001);
	let mut accepted = 0;
	let mut refusals = 0;
	for iteration in 0..2000 {
		let module = mutated_module(&mut noise);
		match validate(&module, &limits) {
			Ok(()) => {
				accepted += 1;
				// WHAT IS ACCEPTED MUST SATISFY THE RULES, which is the half a refusal-only fixture
				// never checks: a validator that accepted everything would pass a "nothing panics"
				// property with no trouble at all.
				assert!(writes_its_output(&module), "iteration {iteration}: accepted a stage that writes nothing");
				assert!(!has_misplaced_control_flow(&module.body, false), "iteration {iteration}: accepted misplaced control flow");
				assert!(every_loop_is_bounded(&module.body, &limits), "iteration {iteration}: accepted an unbounded loop");
			}
			Err(_) => refusals += 1,
		}
	}
	assert!(accepted > 50, "only {accepted} modules were accepted, so the generator is not reaching the accepting path");
	assert!(refusals > 500, "only {refusals} refusals, which is too few to be testing them");
}

fn writes_its_output(module: &Module) -> bool {
	fn stores(body: &[Stmt], wanted: &dyn Fn(&Output) -> bool) -> bool {
		body.iter().any(|statement| match statement {
			Stmt::Store(output, _) => wanted(output),
			Stmt::If { then_body, else_body, .. } => stores(then_body, wanted) || stores(else_body, wanted),
			Stmt::Loop { body, .. } => stores(body, wanted),
			Stmt::Switch { cases, default, .. } => cases.iter().any(|(_, body)| stores(body, wanted)) || stores(default, wanted),
			_ => false,
		})
	}
	match module.stage {
		Stage::Vertex => stores(&module.body, &|output| matches!(output, Output::Position)),
		Stage::Fragment => stores(&module.body, &|output| matches!(output, Output::Colour(_) | Output::Integer(_) | Output::Depth)) || stores(&module.body, &|output| matches!(output, Output::SampleMask)),
	}
}

fn has_misplaced_control_flow(body: &[Stmt], in_loop: bool) -> bool {
	body.iter().any(|statement| match statement {
		Stmt::Break | Stmt::Continue => !in_loop,
		Stmt::If { then_body, else_body, .. } => has_misplaced_control_flow(then_body, in_loop) || has_misplaced_control_flow(else_body, in_loop),
		Stmt::Loop { body, .. } => has_misplaced_control_flow(body, true),
		Stmt::Switch { cases, default, .. } => cases.iter().any(|(_, body)| has_misplaced_control_flow(body, in_loop)) || has_misplaced_control_flow(default, in_loop),
		_ => false,
	})
}

fn every_loop_is_bounded(body: &[Stmt], limits: &ShaderLimits) -> bool {
	body.iter().all(|statement| match statement {
		Stmt::Loop { trips, body } => *trips > 0 && *trips <= limits.max_loop_trips && every_loop_is_bounded(body, limits),
		Stmt::If { then_body, else_body, .. } => every_loop_is_bounded(then_body, limits) && every_loop_is_bounded(else_body, limits),
		Stmt::Switch { cases, default, .. } => cases.iter().all(|(_, body)| every_loop_is_bounded(body, limits)) && every_loop_is_bounded(default, limits),
		_ => true,
	})
}

#[test]
// A DEEPLY NESTED BODY IS REFUSED BY THE LOOP-DEPTH BOUND AND NOT BY RUNNING OUT OF STACK. The
// nesting here is far past the ceiling, which is the input that turns a recursive validator with no
// bound into a crash rather than an error.
fn nesting_far_past_the_ceiling_is_a_typed_refusal_and_not_a_crash() {
	let mut body = vec![Stmt::Store(Output::Position, Value(0))];
	for _ in 0..200 {
		body = vec![Stmt::Loop { trips: 2, body }];
	}
	let module = Module { stage: Stage::Vertex, name: String::from("deep"), types: vec![Type::vec(4)], varyings: vec![], body };
	assert!(matches!(validate(&module, &limits()), Err(Error::LoopDepth { .. })));

	// The same shape in conditionals, which have no depth bound of their own and must still answer.
	let mut body = vec![Stmt::Store(Output::Position, Value(0))];
	for _ in 0..200 {
		body = vec![Stmt::If { condition: Value(0), then_body: body, else_body: vec![] }];
	}
	let module = Module { stage: Stage::Vertex, name: String::from("deep"), types: vec![Type::vec(4)], varyings: vec![], body };
	let outcome = validate(&module, &limits());
	assert!(outcome.is_err(), "a position selected by a branch on itself is not a valid module: {outcome:?}");
}

#[test]
// `centroid` AND `sample` ARE EVALUATION LOCATIONS AND NOT INTERPOLATION RULES, which the frozen
// profile says in those words. Keeping them separate is what makes `centroid smooth` and
// `centroid noperspective` one modifier on two rules rather than two unrelated variants - and what
// makes `centroid flat` unwritable, which is the combination that means nothing.
fn an_evaluation_location_is_separate_from_the_interpolation_rule() {
	use crate::ir::Sampling;

	let mut builder = Builder::new(Stage::Fragment, "sampled");
	builder.varying_at(0, Type::vec(2), Interpolation::Smooth, Sampling::Centroid);
	builder.varying_at(1, Type::f32(), Interpolation::NoPerspective, Sampling::Sample);
	let value = builder.load(Type::vec(4), Binding::Varying { location: 0 });
	builder.store(Output::Colour(0), value);
	let module = builder.finish();
	assert_eq!(validate(&module, &limits()), Ok(()));
	// THE SHADING RATE IS A CONSEQUENCE OF THE SHADER, not a state a caller sets: a stage with a
	// `sample`-qualified input cannot be run once per pixel, because the input differs per sample.
	assert!(module.per_sample_shading());

	let mut centroid_only = Builder::new(Stage::Fragment, "centroid");
	centroid_only.varying_at(0, Type::vec(2), Interpolation::Smooth, Sampling::Centroid);
	let value = centroid_only.load(Type::vec(4), Binding::Varying { location: 0 });
	centroid_only.store(Output::Colour(0), value);
	let module = centroid_only.finish();
	assert_eq!(validate(&module, &limits()), Ok(()));
	assert!(!module.per_sample_shading(), "a centroid input is still once per pixel");

	// A LOCATION ON A CONSTANT IS REFUSED: a flat varying is the same value wherever it is evaluated,
	// so asking for it somewhere else is a shader whose author believes something false.
	let mut flat = Builder::new(Stage::Fragment, "flat");
	flat.varying_at(0, Type::f32(), Interpolation::Flat, Sampling::Centroid);
	let value = flat.load(Type::vec(4), Binding::Varying { location: 0 });
	flat.store(Output::Colour(0), value);
	assert!(matches!(validate(&flat.finish(), &limits()), Err(Error::MeaninglessSampling { .. })));

	// And a vertex stage has no coverage to evaluate against.
	let mut vertex = Builder::new(Stage::Vertex, "vertex");
	vertex.varying_at(0, Type::f32(), Interpolation::Smooth, Sampling::Sample);
	let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
	vertex.store(Output::Position, position);
	assert!(matches!(validate(&vertex.finish(), &limits()), Err(Error::MeaninglessSampling { .. })));
}

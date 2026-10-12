//! The compiled program against the interpreter, on shapes chosen to reach every instruction family.
//!
//! THE SAME OUTPUTS BIT FOR BIT, OR THE SAME FAULT. The frame's own differential check already holds
//! every compiled stage this crate's tests and the conformance suite run to that rule; what this file
//! adds is what a scene may never draw - every arithmetic operation on the values that break naive
//! arithmetic (NaN, both zeros, both infinities, a denormal, a value whose square overflows), control
//! flow that reads a value only some paths assign, and the batched path lane by lane. AND THAT THE
//! COMPILED PATH RAN: a program that handed every run back to the interpreter would agree with it
//! trivially, so a case that deoptimises fails here rather than passing for the wrong reason.

use super::*;
use crate::interpreter::{Machine, Resources};
use alloc::string::String;
use alloc::vec;
use render_shader::ir::{CompareKind, Constant};

/// The values a case's uniforms are drawn from: ordinary ones and the ones IEEE arithmetic treats
/// specially.
const SPECIAL: [f32; 10] = [0.0, -0.0, 1.5, -2.25, f32::INFINITY, f32::NEG_INFINITY, f32::NAN, 1.0e-40, 3.0e38, 0.3];

/// Everything a case reads: uniforms, and the fragment coordinate that tells one lane from another.
struct Bindings {
	uniforms: Vec<((u32, u32), Val)>,
	coordinate: [f32; 4],
}

impl Bindings {
	fn new(uniforms: &[Val]) -> Self {
		Self { uniforms: uniforms.iter().enumerate().map(|(member, value)| ((0, member as u32), value.clone())).collect(), coordinate: [0.5, 0.5, 0.25, 1.0] }
	}

	fn at(mut self, x: f32, y: f32) -> Self {
		self.coordinate = [x, y, 0.25, 1.0];
		self
	}
}

impl Resources for Bindings {
	fn uniform(&self, block: u32, member: u32) -> Option<Val> {
		self.uniforms.iter().find(|(key, _)| *key == (block, member)).map(|(_, value)| value.clone())
	}

	fn attribute(&self, _location: u32) -> Option<Val> {
		None
	}

	fn varying(&self, _location: u32) -> Option<Val> {
		None
	}

	fn built_in(&self, which: BuiltIn) -> Option<Val> {
		match which {
			BuiltIn::FragmentCoordinate => Some(Val::vector_f32(&self.coordinate)),
			BuiltIn::FrontFacing => Some(Val::scalar_bool(true)),
			_ => None,
		}
	}

	fn sample(&self, _texture: u32, _sampler: u32, _coordinate: &Val) -> Option<Val> {
		None
	}
}

impl Inputs for Bindings {
	fn varyings(&self, _array: u8) -> &[f32] {
		&[]
	}

	fn built_in(&self, which: BuiltIn, out: &mut [u32; 4]) -> Option<usize> {
		let value = Resources::built_in(self, which)?;
		let words = value.words();
		out[..words.len()].copy_from_slice(words);
		Some(words.len())
	}

	fn attribute(&self, location: u32) -> Option<Val> {
		Resources::attribute(self, location)
	}

	fn sample(&self, texture: u32, sampler: u32, coordinate: &Val) -> Option<Val> {
		Resources::sample(self, texture, sampler, coordinate)
	}
}

/// A fragment stage over `types` and `body`.
fn fragment(name: &str, types: Vec<Type>, body: Vec<Stmt>) -> Module {
	Module { stage: Stage::Fragment, name: String::from(name), types, varyings: vec![], body }
}

fn compiled(module: &Module) -> Program {
	compile(module, None).unwrap_or_else(|| panic!("`{}` did not compile", module.name))
}

/// Run `program` compiled and `module` interpreted on `bindings`: the same outputs bit for bit, or
/// the same fault - and never a run handed back.
fn agree(module: &Module, program: &Program, bindings: &Bindings) {
	let mut uniforms = Uniforms::for_program(program);
	uniforms.fill(program, |block, member| bindings.uniform(block, member));
	assert!(uniforms.usable(), "`{}`: the uniforms did not fill", module.name);
	let mut machine = Machine::default();
	machine.registers.reserve(program).expect("registers for the program");
	let outcome = run(program, &uniforms, bindings, &mut machine);
	let mut shadow = Machine::default();
	let reference = interpreter::execute_into(module, bindings, &mut shadow);
	match outcome {
		Ok(Ran::Done) => {
			assert_eq!(reference, Ok(()), "`{}`: the interpreter faulted where the compiled run finished", module.name);
			assert!(machine.outputs().same_bits(shadow.outputs()), "`{}` disagrees with the interpreter on {:?}: {:?} against {:?}", module.name, bindings.uniforms, machine.outputs(), shadow.outputs());
		}
		Ok(Ran::Deopt) => panic!("`{}` handed its run back to the interpreter", module.name),
		Err(fault) => assert_eq!(reference, Err(fault), "`{}` faulted differently", module.name),
	}
}

/// `out = op(a, b)` on two uniforms of `kind`, written with both operands beside it so a wrong
/// operand order shows too.
fn binary_module(op: BinaryOp, kind: Type) -> Module {
	let scalar = kind == Type::f32();
	let mut types = vec![kind.clone(), kind.clone(), if op == BinaryOp::Dot { Type::f32() } else { kind.clone() }, Type::vec(4)];
	let mut body = vec![
		Stmt::Assign(Value(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
		Stmt::Assign(Value(1), Op::Load(Binding::Uniform { block: 0, member: 1 })),
		Stmt::Assign(Value(2), Op::Binary(op, Value(0), Value(1))),
	];
	if scalar || op == BinaryOp::Dot {
		body.push(Stmt::Assign(Value(3), Op::Compose(Type::vec(4), vec![Value(2), Value(2), Value(2), Value(2)])));
	} else {
		types[3] = kind;
		body.push(Stmt::Assign(Value(3), Op::Binary(BinaryOp::Add, Value(2), Value(0))));
	}
	body.push(Stmt::Store(Output::Colour(0), if scalar || op == BinaryOp::Dot { Value(3) } else { Value(2) }));
	body.push(Stmt::Store(Output::Colour(1), Value(3)));
	fragment("binary", types, body)
}

#[test]
fn every_float_binary_operation_agrees_on_every_pair_of_special_values() {
	for op in [BinaryOp::Add, BinaryOp::Subtract, BinaryOp::Multiply, BinaryOp::Divide, BinaryOp::Modulo, BinaryOp::Min, BinaryOp::Max] {
		let module = binary_module(op, Type::f32());
		let program = compiled(&module);
		for a in SPECIAL {
			for b in SPECIAL {
				agree(&module, &program, &Bindings::new(&[Val::scalar_f32(a), Val::scalar_f32(b)]));
			}
		}
	}
	// AND FOUR WIDE, where the compiled path has its own fixed-size form and the order of the
	// components is one more thing to get wrong.
	for op in [BinaryOp::Add, BinaryOp::Subtract, BinaryOp::Multiply, BinaryOp::Divide, BinaryOp::Min, BinaryOp::Max, BinaryOp::Dot] {
		let module = binary_module(op, Type::vec(4));
		let program = compiled(&module);
		for (index, a) in SPECIAL.iter().enumerate() {
			let left = Val::vector_f32(&[*a, SPECIAL[(index + 1) % 10], SPECIAL[(index + 3) % 10], SPECIAL[(index + 7) % 10]]);
			for b in SPECIAL {
				let right = Val::vector_f32(&[b, -b, b * 0.5, 2.0]);
				agree(&module, &program, &Bindings::new(&[left.clone(), right]));
			}
		}
	}
}

#[test]
fn every_float_unary_operation_and_conversion_agrees_on_every_special_value() {
	let scalar = [UnaryOp::Negate, UnaryOp::Abs, UnaryOp::Floor, UnaryOp::Ceil, UnaryOp::Fract];
	for op in scalar {
		let module = fragment(
			"unary",
			vec![Type::f32(), Type::f32(), Type::vec(4)],
			vec![
				Stmt::Assign(Value(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
				Stmt::Assign(Value(1), Op::Unary(op, Value(0))),
				Stmt::Assign(Value(2), Op::Compose(Type::vec(4), vec![Value(1), Value(0), Value(1), Value(0)])),
				Stmt::Store(Output::Colour(0), Value(2)),
			],
		);
		let program = compiled(&module);
		for a in SPECIAL {
			agree(&module, &program, &Bindings::new(&[Val::scalar_f32(a)]));
		}
	}
	for op in [UnaryOp::Normalize, UnaryOp::Length] {
		let result = if op == UnaryOp::Length { Type::f32() } else { Type::vec(4) };
		let mut body = vec![Stmt::Assign(Value(0), Op::Load(Binding::Uniform { block: 0, member: 0 })), Stmt::Assign(Value(1), Op::Unary(op, Value(0)))];
		if op == UnaryOp::Length {
			body.push(Stmt::Assign(Value(2), Op::Compose(Type::vec(4), vec![Value(1), Value(1), Value(1), Value(1)])));
		} else {
			body.push(Stmt::Assign(Value(2), Op::Binary(BinaryOp::Multiply, Value(1), Value(0))));
		}
		body.push(Stmt::Store(Output::Colour(0), Value(2)));
		let module = fragment("geometric", vec![Type::vec(4), result, Type::vec(4)], body);
		let program = compiled(&module);
		for (index, a) in SPECIAL.iter().enumerate() {
			agree(&module, &program, &Bindings::new(&[Val::vector_f32(&[*a, SPECIAL[(index + 2) % 10], 1.0, -0.5])]));
		}
	}
	// THE CONVERSIONS, which are where a saturating rule and a wrapping one part: out of range, NaN and
	// the infinities into each integer type and back.
	for target in [ScalarType::I32, ScalarType::U32] {
		let module = fragment(
			"convert",
			vec![Type::f32(), Type::Scalar(target), Type::f32(), Type::vec(4)],
			vec![
				Stmt::Assign(Value(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
				Stmt::Assign(Value(1), Op::Unary(UnaryOp::Convert(target), Value(0))),
				Stmt::Assign(Value(2), Op::Unary(UnaryOp::Convert(ScalarType::F32), Value(1))),
				Stmt::Assign(Value(3), Op::Compose(Type::vec(4), vec![Value(2), Value(0), Value(2), Value(0)])),
				Stmt::Store(Output::Colour(0), Value(3)),
			],
		);
		let program = compiled(&module);
		for a in SPECIAL.iter().chain([-1.0e10, 4.0e9, -1.0, 2_147_483_648.0].iter()) {
			agree(&module, &program, &Bindings::new(&[Val::scalar_f32(*a)]));
		}
	}
}

#[test]
fn every_transcendental_agrees_on_every_special_value() {
	let one = [
		Transcendental::Sqrt,
		Transcendental::InverseSqrt,
		Transcendental::Sin,
		Transcendental::Cos,
		Transcendental::Tan,
		Transcendental::Asin,
		Transcendental::Acos,
		Transcendental::Atan,
		Transcendental::Exp,
		Transcendental::Exp2,
		Transcendental::Log,
		Transcendental::Log2,
	];
	for which in one.into_iter().chain([Transcendental::Atan2, Transcendental::Pow]) {
		let two = matches!(which, Transcendental::Atan2 | Transcendental::Pow);
		let module = fragment(
			"transcendental",
			vec![Type::f32(), Type::f32(), Type::f32(), Type::vec(4)],
			vec![
				Stmt::Assign(Value(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
				Stmt::Assign(Value(1), Op::Load(Binding::Uniform { block: 0, member: 1 })),
				Stmt::Assign(Value(2), Op::Transcendental(which, Value(0), two.then_some(Value(1)))),
				Stmt::Assign(Value(3), Op::Compose(Type::vec(4), vec![Value(2), Value(2), Value(0), Value(1)])),
				Stmt::Store(Output::Colour(0), Value(3)),
			],
		);
		let program = compiled(&module);
		for a in SPECIAL {
			for b in if two { &SPECIAL[..] } else { &SPECIAL[..1] } {
				agree(&module, &program, &Bindings::new(&[Val::scalar_f32(a), Val::scalar_f32(*b)]));
			}
		}
	}
}

#[test]
fn clamp_mix_select_and_every_comparison_agree() {
	let module = fragment(
		"blend",
		vec![Type::vec(4), Type::vec(4), Type::f32(), Type::vec(4), Type::vec(4), Type::vec(4)],
		vec![
			Stmt::Assign(Value(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
			Stmt::Assign(Value(1), Op::Load(Binding::Uniform { block: 0, member: 1 })),
			Stmt::Assign(Value(2), Op::Load(Binding::Uniform { block: 0, member: 2 })),
			Stmt::Assign(Value(3), Op::Clamp { value: Value(0), low: Value(1), high: Value(0) }),
			Stmt::Assign(Value(4), Op::Mix { from: Value(0), to: Value(1), at: Value(2) }),
			Stmt::Assign(Value(5), Op::Binary(BinaryOp::Add, Value(3), Value(4))),
			Stmt::Store(Output::Colour(0), Value(5)),
		],
	);
	let program = compiled(&module);
	for (index, a) in SPECIAL.iter().enumerate() {
		for at in [0.0, 0.25, 1.0, -1.0, f32::NAN] {
			let low = Val::vector_f32(&[SPECIAL[(index + 1) % 10], 0.0, -1.0, 1.0]);
			agree(&module, &program, &Bindings::new(&[Val::vector_f32(&[*a, 1.0, *a, -2.0]), low, Val::scalar_f32(at)]));
		}
	}
	for kind in [CompareKind::Equal, CompareKind::NotEqual, CompareKind::Less, CompareKind::LessOrEqual, CompareKind::Greater, CompareKind::GreaterOrEqual] {
		let module = fragment(
			"compare",
			vec![Type::f32(), Type::f32(), Type::Scalar(ScalarType::Bool), Type::f32(), Type::vec(4)],
			vec![
				Stmt::Assign(Value(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
				Stmt::Assign(Value(1), Op::Load(Binding::Uniform { block: 0, member: 1 })),
				Stmt::Assign(Value(2), Op::Compare(kind, Value(0), Value(1))),
				Stmt::Assign(Value(3), Op::Select { condition: Value(2), on_true: Value(0), on_false: Value(1) }),
				Stmt::Assign(Value(4), Op::Compose(Type::vec(4), vec![Value(3), Value(0), Value(1), Value(3)])),
				Stmt::If { condition: Value(2), then_body: vec![Stmt::Store(Output::Depth, Value(0))], else_body: vec![Stmt::Store(Output::Depth, Value(1))] },
				Stmt::Store(Output::Colour(0), Value(4)),
			],
		);
		let program = compiled(&module);
		for a in SPECIAL {
			for b in SPECIAL {
				agree(&module, &program, &Bindings::new(&[Val::scalar_f32(a), Val::scalar_f32(b)]));
			}
		}
	}
}

#[test]
fn a_matrix_product_and_a_cross_product_agree() {
	let module = fragment(
		"matrix",
		vec![Type::Matrix(4), Type::vec(4), Type::vec(4), Type::vec(3), Type::vec(3), Type::vec(3), Type::f32(), Type::vec(4), Type::vec(4)],
		vec![
			Stmt::Assign(Value(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
			Stmt::Assign(Value(1), Op::Load(Binding::Uniform { block: 0, member: 1 })),
			Stmt::Assign(Value(2), Op::Binary(BinaryOp::MatrixProduct, Value(0), Value(1))),
			Stmt::Assign(Value(3), Op::Load(Binding::Uniform { block: 0, member: 2 })),
			Stmt::Assign(Value(4), Op::Load(Binding::Uniform { block: 0, member: 3 })),
			Stmt::Assign(Value(5), Op::Binary(BinaryOp::Cross, Value(3), Value(4))),
			Stmt::Assign(Value(6), Op::Extract(Value(5), 1)),
			Stmt::Assign(Value(7), Op::Compose(Type::vec(4), vec![Value(6), Value(6), Value(6), Value(6)])),
			Stmt::Assign(Value(8), Op::Binary(BinaryOp::Add, Value(2), Value(7))),
			Stmt::Store(Output::Colour(0), Value(8)),
		],
	);
	let program = compiled(&module);
	for (index, a) in SPECIAL.iter().enumerate() {
		let b = SPECIAL[(index + 5) % 10];
		let matrix = Val::matrix(&[[1.0, *a, 0.0, 0.0], [0.0, 1.0, b, 0.0], [0.5, 0.0, 1.0, 0.0], [10.0, 20.0, 30.0, 1.0]], 4);
		agree(&module, &program, &Bindings::new(&[matrix, Val::vector_f32(&[1.0, 2.0, *a, 1.0]), Val::vector_f32(&[*a, 1.0, 0.0]), Val::vector_f32(&[0.0, b, 2.0])]));
	}
}

#[test]
// A VALUE ONLY ONE PATH ASSIGNS is read through a stamp on the compiled side, and the interpreter's
// answer for a read on the other path - a fault - is what the compiled run must give too. Loops with
// a break and a continue, a switch and a discard ride along: each is a jump the compiled code takes
// by itself.
fn control_flow_and_a_value_only_one_path_assigns_agree() {
	let module = fragment(
		"flow",
		vec![
			Type::f32(),
			Type::f32(),
			Type::Scalar(ScalarType::Bool),
			Type::f32(),
			Type::vec(4),
			Type::Scalar(ScalarType::I32),
			Type::f32(),
			Type::Scalar(ScalarType::Bool),
			Type::Scalar(ScalarType::Bool),
		],
		vec![
			Stmt::Assign(Value(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
			Stmt::Assign(Value(1), Op::Load(Binding::Uniform { block: 0, member: 1 })),
			Stmt::Assign(Value(2), Op::Compare(CompareKind::Less, Value(0), Value(1))),
			Stmt::If { condition: Value(2), then_body: vec![Stmt::Assign(Value(3), Op::Binary(BinaryOp::Add, Value(0), Value(1)))], else_body: vec![] },
			Stmt::Assign(Value(5), Op::Unary(UnaryOp::Convert(ScalarType::I32), Value(1))),
			Stmt::Switch { selector: Value(5), cases: vec![(0, vec![Stmt::Store(Output::Depth, Value(0))]), (2, vec![Stmt::Discard])], default: vec![] },
			Stmt::Loop {
				trips: 4,
				body: vec![
					Stmt::Assign(Value(6), Op::Binary(BinaryOp::Multiply, Value(0), Value(0))),
					Stmt::Assign(Value(7), Op::Compare(CompareKind::Greater, Value(6), Value(1))),
					Stmt::If { condition: Value(7), then_body: vec![Stmt::Break], else_body: vec![Stmt::Continue] },
				],
			},
			Stmt::Assign(Value(8), Op::Compare(CompareKind::Equal, Value(1), Value(1))),
			Stmt::If { condition: Value(8), then_body: vec![], else_body: vec![Stmt::Return] },
			// READ ON EVERY PATH, ASSIGNED ON ONE: the interpreter refuses the read where `Value(3)` was
			// never assigned.
			Stmt::Assign(Value(4), Op::Compose(Type::vec(4), vec![Value(3), Value(0), Value(1), Value(0)])),
			Stmt::Store(Output::Colour(0), Value(4)),
		],
	);
	let program = compiled(&module);
	for a in SPECIAL {
		for b in [0.0, 1.0, 2.0, -3.0, f32::NAN, 5.5] {
			agree(&module, &program, &Bindings::new(&[Val::scalar_f32(a), Val::scalar_f32(b)]));
		}
	}
}

#[test]
// THE BATCHED PATH RUNS `LANES` INVOCATIONS AT ONCE, and each lane must be exactly the interpreter's run
// of that lane alone - which only a case whose lanes differ can show. Here they differ by the fragment
// coordinate.
fn the_batched_path_agrees_with_the_interpreter_lane_by_lane() {
	let module = fragment(
		"batched",
		vec![Type::vec(4), Type::vec(4), Type::vec(4), Type::f32(), Type::f32(), Type::vec(4), Type::vec(4)],
		vec![
			Stmt::Assign(Value(0), Op::Load(Binding::BuiltIn(BuiltIn::FragmentCoordinate))),
			Stmt::Assign(Value(1), Op::Load(Binding::Uniform { block: 0, member: 0 })),
			Stmt::Assign(Value(2), Op::Binary(BinaryOp::Multiply, Value(0), Value(1))),
			Stmt::Assign(Value(3), Op::Binary(BinaryOp::Dot, Value(2), Value(1))),
			Stmt::Assign(Value(4), Op::Transcendental(Transcendental::Sin, Value(3), None)),
			Stmt::Assign(Value(5), Op::Compose(Type::vec(4), vec![Value(4), Value(3), Value(4), Value(3)])),
			Stmt::Assign(Value(6), Op::Mix { from: Value(2), to: Value(5), at: Value(4) }),
			Stmt::Store(Output::Colour(0), Value(6)),
		],
	);
	let program = compiled(&module);
	assert!(program.batchable, "a straight-line stage with no texture read is batchable");
	let mut batch = Batch::default();
	batch.reserve(&program, &module).expect("a batch for the program");
	for (index, scale) in SPECIAL.iter().enumerate() {
		let uniform = Val::vector_f32(&[*scale, 0.5, SPECIAL[(index + 4) % 10], 1.0]);
		let lanes: Vec<Bindings> = (0..LANES).map(|lane| Bindings::new(&[uniform.clone()]).at(lane as f32 + 0.5, index as f32 + 0.5)).collect();
		let mut uniforms = Uniforms::for_program(&program);
		uniforms.fill(&program, |block, member| lanes[0].uniform(block, member));
		assert!(run_batch(&program, &uniforms, &lanes, &mut batch), "the batch was refused");
		for (lane, bindings) in lanes.iter().enumerate() {
			let mut shadow = Machine::default();
			assert_eq!(interpreter::execute_into(&module, bindings, &mut shadow), Ok(()));
			assert!(batch.outputs(lane).same_bits(shadow.outputs()), "lane {lane} disagrees with the interpreter on {uniform:?}: {:?} against {:?}", batch.outputs(lane), shadow.outputs());
		}
	}
}

#[test]
// A CONSTANT IS FOLDED INTO THE CODE, and the folded value must be the one the interpreter loads.
fn constants_of_every_scalar_type_agree() {
	let module = fragment(
		"constants",
		vec![Type::f32(), Type::Scalar(ScalarType::I32), Type::Scalar(ScalarType::U32), Type::Scalar(ScalarType::Bool), Type::f32(), Type::f32(), Type::vec(4)],
		vec![
			Stmt::Assign(Value(0), Op::Const(Constant::F32(-0.0))),
			Stmt::Assign(Value(1), Op::Const(Constant::I32(-7))),
			Stmt::Assign(Value(2), Op::Const(Constant::U32(u32::MAX))),
			Stmt::Assign(Value(3), Op::Const(Constant::Bool(true))),
			Stmt::Assign(Value(4), Op::Unary(UnaryOp::Convert(ScalarType::F32), Value(1))),
			Stmt::Assign(Value(5), Op::Unary(UnaryOp::Convert(ScalarType::F32), Value(2))),
			Stmt::Assign(Value(6), Op::Compose(Type::vec(4), vec![Value(0), Value(4), Value(5), Value(0)])),
			Stmt::If { condition: Value(3), then_body: vec![Stmt::Store(Output::Colour(0), Value(6))], else_body: vec![] },
		],
	);
	let program = compiled(&module);
	agree(&module, &program, &Bindings::new(&[]));
}

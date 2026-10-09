//! The frozen bloom and resolve operators expressed in portable shader IR.
//! Texture zero is the previous level (or scene); texture one is the level being added.

use alloc::vec;
use render_shader::builder::Builder;
use render_shader::ir::{BinaryOp, Binding, CompareKind, Constant, Module, Op, Output, Stage, Type, Value};

pub fn vertex() -> Module {
	let mut b = Builder::new(Stage::Vertex, "postprocess-vertex");
	b.varying(0, Type::vec(2), render_shader::Interpolation::NoPerspective);
	let position = b.load(Type::vec(4), Binding::Attribute { location: 0 });
	let uv = b.load(Type::vec(2), Binding::Attribute { location: 1 });
	b.store(Output::Position, position);
	b.store(Output::Varying(0), uv);
	b.finish()
}

fn scalar(b: &mut Builder, value: f32) -> Value {
	b.constant(Constant::F32(value))
}
fn bin(b: &mut Builder, op: BinaryOp, a: Value, c: Value) -> Value {
	b.assign(Type::f32(), Op::Binary(op, a, c))
}
fn vector(b: &mut Builder, values: &[f32]) -> Value {
	let values = values.iter().map(|v| scalar(b, *v)).collect();
	b.assign(Type::vec(2), Op::Compose(Type::vec(2), values))
}
fn scale(b: &mut Builder, colour: Value, factor: Value) -> Value {
	let factor = b.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![factor; 4]));
	b.assign(Type::vec(4), Op::Binary(BinaryOp::Multiply, colour, factor))
}
fn add(b: &mut Builder, a: Value, c: Value) -> Value {
	b.assign(Type::vec(4), Op::Binary(BinaryOp::Add, a, c))
}
fn sample(b: &mut Builder, uv: Value, texture: u32) -> Value {
	b.assign(Type::vec(4), Op::Sample { binding: Binding::Texture { texture, sampler: 0 }, coordinate: uv })
}
fn luminance(b: &mut Builder, colour: Value) -> Value {
	let mut result = scalar(b, 0.0);
	for (channel, weight) in [super::LUMINANCE_REC709.x, super::LUMINANCE_REC709.y, super::LUMINANCE_REC709.z].iter().enumerate() {
		let channel = b.assign(Type::f32(), Op::Extract(colour, channel as u8));
		let weight = scalar(b, *weight);
		let term = bin(b, BinaryOp::Multiply, channel, weight);
		result = bin(b, BinaryOp::Add, result, term);
	}
	result
}
fn prefilter(b: &mut Builder, colour: Value) -> Value {
	let light = luminance(b, colour);
	let zero = scalar(b, 0.0);
	let threshold = scalar(b, super::BLOOM_THRESHOLD);
	let knee = scalar(b, super::BLOOM_KNEE);
	let excess = bin(b, BinaryOp::Subtract, light, threshold);
	let over = bin(b, BinaryOp::Add, excess, knee);
	let over = bin(b, BinaryOp::Max, over, zero);
	let square = bin(b, BinaryOp::Multiply, over, over);
	let divisor = scalar(b, 4.0 * super::BLOOM_KNEE);
	let soft = bin(b, BinaryOp::Divide, square, divisor);
	let upper = scalar(b, super::BLOOM_THRESHOLD + super::BLOOM_KNEE);
	let hard = b.assign(Type::Scalar(render_shader::ir::ScalarType::Bool), Op::Compare(CompareKind::GreaterOrEqual, light, upper));
	let excess = b.assign(Type::f32(), Op::Select { condition: hard, on_true: excess, on_false: soft });
	let lower = scalar(b, super::BLOOM_THRESHOLD - super::BLOOM_KNEE);
	let active = b.assign(Type::Scalar(render_shader::ir::ScalarType::Bool), Op::Compare(CompareKind::Greater, light, lower));
	let ratio = bin(b, BinaryOp::Divide, excess, light);
	let ratio = b.assign(Type::f32(), Op::Select { condition: active, on_true: ratio, on_false: zero });
	scale(b, colour, ratio)
}
fn begin(name: &str) -> (Builder, Value) {
	let mut b = Builder::new(Stage::Fragment, name);
	b.varying(0, Type::vec(2), render_shader::Interpolation::NoPerspective);
	let uv = b.load(Type::vec(2), Binding::Varying { location: 0 });
	(b, uv)
}
fn tap(b: &mut Builder, uv: Value, texel: (f32, f32), offset: (f32, f32)) -> Value {
	let offset = vector(b, &[texel.0 * offset.0, texel.1 * offset.1]);
	let at = b.assign(Type::vec(2), Op::Binary(BinaryOp::Add, uv, offset));
	sample(b, at, 0)
}
fn group(b: &mut Builder, uv: Value, texel: (f32, f32), offsets: &[(f32, f32)]) -> Value {
	let mut sum = tap(b, uv, texel, offsets[0]);
	for offset in &offsets[1..] {
		let value = tap(b, uv, texel, *offset);
		sum = add(b, sum, value);
	}
	sum
}
fn weighted(b: &mut Builder, value: Value, weight: f32) -> Value {
	let weight = scalar(b, weight);
	scale(b, value, weight)
}
fn finish(mut b: Builder, colour: Value) -> Module {
	let rgb: alloc::vec::Vec<_> = (0..3).map(|i| b.assign(Type::f32(), Op::Extract(colour, i))).collect();
	let one = scalar(&mut b, 1.0);
	let colour = b.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![rgb[0], rgb[1], rgb[2], one]));
	b.store(Output::Colour(0), colour);
	b.finish()
}

/// Thirteen taps, with the soft knee applied once to the first downsample's result.
pub fn downsample(width: u32, height: u32, threshold: bool) -> Module {
	let (mut b, uv) = begin("bloom-downsample-13");
	let texel = (1.0 / width as f32, 1.0 / height as f32);
	let outer = group(&mut b, uv, texel, &[(-2.0, -2.0), (2.0, -2.0), (-2.0, 2.0), (2.0, 2.0)]);
	let edges = group(&mut b, uv, texel, &[(0.0, -2.0), (-2.0, 0.0), (2.0, 0.0), (0.0, 2.0)]);
	let inner = group(&mut b, uv, texel, &[(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]);
	let centre = tap(&mut b, uv, texel, (0.0, 0.0));
	let mut sum = weighted(&mut b, centre, 0.125);
	for (value, weight) in [(outer, 0.03125), (edges, 0.0625), (inner, 0.125)] {
		let term = weighted(&mut b, value, weight);
		sum = add(&mut b, sum, term);
	}
	if threshold {
		sum = prefilter(&mut b, sum);
	}
	finish(b, sum)
}

/// Nine-tap tent from texture zero plus this level of the descending pyramid in texture one.
pub fn upsample(width: u32, height: u32) -> Module {
	let (mut b, uv) = begin("bloom-upsample-tent-9");
	let texel = (1.0 / width as f32, 1.0 / height as f32);
	let corners = group(&mut b, uv, texel, &[(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]);
	let edges = group(&mut b, uv, texel, &[(0.0, -1.0), (-1.0, 0.0), (1.0, 0.0), (0.0, 1.0)]);
	let centre = tap(&mut b, uv, texel, (0.0, 0.0));
	let edges = weighted(&mut b, edges, 2.0);
	let centre = weighted(&mut b, centre, 4.0);
	let sum = add(&mut b, corners, edges);
	let sum = add(&mut b, sum, centre);
	let sum = weighted(&mut b, sum, 1.0 / 16.0);
	let own = sample(&mut b, uv, 1);
	let sum = add(&mut b, own, sum);
	finish(b, sum)
}

/// Bloom addition followed by extended Reinhard on luminance, in linear Rec. 709.
pub fn resolve() -> Module {
	let (mut b, uv) = begin("hdr-resolve");
	let scene = sample(&mut b, uv, 0);
	let bloom = sample(&mut b, uv, 1);
	let bloom = weighted(&mut b, bloom, super::BLOOM_WEIGHT);
	let colour = add(&mut b, scene, bloom);
	let light = luminance(&mut b, colour);
	let one = scalar(&mut b, 1.0);
	let white = scalar(&mut b, super::tone_map_white() * super::tone_map_white());
	let numerator = bin(&mut b, BinaryOp::Divide, light, white);
	let numerator = bin(&mut b, BinaryOp::Add, one, numerator);
	let numerator = bin(&mut b, BinaryOp::Multiply, light, numerator);
	let denominator = bin(&mut b, BinaryOp::Add, one, light);
	let mapped = bin(&mut b, BinaryOp::Divide, numerator, denominator);
	let ratio = bin(&mut b, BinaryOp::Divide, mapped, light);
	let zero = scalar(&mut b, 0.0);
	let positive = b.assign(Type::Scalar(render_shader::ir::ScalarType::Bool), Op::Compare(CompareKind::Greater, light, zero));
	let ratio = b.assign(Type::f32(), Op::Select { condition: positive, on_true: ratio, on_false: one });
	let colour = scale(&mut b, colour, ratio);
	finish(b, colour)
}

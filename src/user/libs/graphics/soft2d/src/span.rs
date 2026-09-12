//! THE SPAN LOOP, with a SCALAR REFERENCE and a WIDE path that must agree with it BIT FOR BIT.
//!
//! THE RULE THAT KEEPS A FAST PATH HONEST. There is a deterministic scalar implementation, and every
//! wider path is differential-tested against it - not within a tolerance, bit for bit, because the
//! two are ONE algorithm rather than two backends. A tolerance between them would let the fast path
//! drift until the difference showed as a seam between the pixels one covered and the pixels the
//! other did.
//!
//! THE WIDE PATH IS FOUR LANES OF PLAIN ARITHMETIC OVER FIXED-SIZE ARRAYS, which is what this tree
//! can actually use: `core::simd` is a nightly feature and the host-test gate runs on stable, and a
//! per-architecture intrinsic in the middle of the compositor would be three implementations to keep
//! in step instead of one. A fixed-length array of four with no branch and no division inside the
//! loop is the shape LLVM vectorises on x86_64, aarch64 and riscv64 alike, and it stays ONE source.
//!
//! AND IT IS THE COMMON CASE AND NOTHING ELSE: source-over with a normal blend, which is what almost
//! every drawing is made of. Every other operator and mode goes through the scalar path, where the
//! equations are read from the frozen registry.

use graphics_core::composite::{BlendMode, Operator, composite};
use graphics_core::pixel::Rgba;

/// How many pixels the wide path does at once.
///
/// FOUR, because four `f32` lanes is the width every architecture this system targets has - SSE2 on
/// x86_64, NEON on aarch64 and the smallest useful vector length on riscv64.
pub const LANES: usize = 4;

/// Composite a run of source pixels onto a run of destination pixels.
pub fn composite_span(destination: &mut [Rgba], source: &[Rgba], operator: Operator, blend: BlendMode) {
	if matches!(operator, Operator::SrcOver) && matches!(blend, BlendMode::Normal) {
		composite_span_over(destination, source);
		return;
	}
	composite_span_scalar(destination, source, operator, blend);
}

/// THE REFERENCE. Every path above agrees with this one or is a defect.
pub fn composite_span_scalar(destination: &mut [Rgba], source: &[Rgba], operator: Operator, blend: BlendMode) {
	for (slot, value) in destination.iter_mut().zip(source.iter()) {
		*slot = composite(operator, blend, *value, *slot);
	}
}

/// Source-over, four pixels at a time.
///
/// `co = Cs + Cb * (1 - as)` ON PREMULTIPLIED VALUES is the whole equation, and it is exactly the
/// form that vectorises: no division, no branch and no per-pixel decision in it.
pub fn composite_span_over(destination: &mut [Rgba], source: &[Rgba]) {
	let count = destination.len().min(source.len());
	let mut index = 0usize;
	while index + LANES <= count {
		let mut red = [0.0f32; LANES];
		let mut green = [0.0f32; LANES];
		let mut blue = [0.0f32; LANES];
		let mut alpha = [0.0f32; LANES];
		for lane in 0..LANES {
			let backdrop = destination[index + lane];
			let over = source[index + lane];
			// SEPARATE MULTIPLY AND ADD, never a fused one. A fused multiply-add keeps more precision
			// than the scalar reference's two operations, and "more precision" is still a DIFFERENT
			// number - which would break the bit-for-bit agreement the two paths are held to.
			let inverse = 1.0 - over.alpha;
			red[lane] = over.red + backdrop.red * inverse;
			green[lane] = over.green + backdrop.green * inverse;
			blue[lane] = over.blue + backdrop.blue * inverse;
			alpha[lane] = over.alpha + backdrop.alpha * inverse;
		}
		for lane in 0..LANES {
			destination[index + lane] = Rgba::new(red[lane], green[lane], blue[lane], alpha[lane]);
		}
		index += LANES;
	}
	// THE TAIL IS THE SCALAR PATH. A tail handled by a masked wide store that was wrong would be
	// invisible on every width that happens to be a multiple of four.
	for offset in index..count {
		destination[offset] = composite(Operator::SrcOver, BlendMode::Normal, source[offset], destination[offset]);
	}
}

/// Scale a run of premultiplied colours by a run of coverages, four at a time.
///
/// COVERAGE MULTIPLIES A PREMULTIPLIED COLOUR AND THAT IS ALL IT DOES, which is why an antialiased
/// edge composites correctly without a second equation: a pixel that is a quarter covered is a
/// quarter of the colour and a quarter of the alpha.
pub fn scale_span(values: &mut [Rgba], coverage: &[f32]) {
	let count = values.len().min(coverage.len());
	let mut index = 0usize;
	while index + LANES <= count {
		let mut scaled = [Rgba::TRANSPARENT; LANES];
		for lane in 0..LANES {
			let value = values[index + lane];
			let weight = coverage[index + lane];
			scaled[lane] = Rgba::new(value.red * weight, value.green * weight, value.blue * weight, value.alpha * weight);
		}
		values[index..index + LANES].copy_from_slice(&scaled);
		index += LANES;
	}
	for offset in index..count {
		values[offset] = values[offset].scaled(coverage[offset]);
	}
}

/// The scalar reference for `scale_span`.
pub fn scale_span_scalar(values: &mut [Rgba], coverage: &[f32]) {
	for (value, weight) in values.iter_mut().zip(coverage.iter()) {
		*value = value.scaled(*weight);
	}
}

//! THE MSAA GEOMETRY, not the sample count alone.
//!
//! "4x MSAA" WITHOUT SAMPLE POSITIONS IS TWO DIFFERENT COVERAGE RESULTS THAT BOTH SATISFY THE WORDS.
//! An edge falls on a different side of a sample on two backends that chose differently, and the
//! conformance suite compares coverage - so the positions are frozen in the profile and read from it
//! here rather than restated.
//!
//! WHAT THIS MODULE IMPLEMENTS, each of which the profile states as a rule and none of which a
//! sample count implies: where the samples are, whether shading runs per pixel or per sample, where
//! a `centroid` input is evaluated, the ORDER in which the sample mask and alpha-to-coverage are
//! applied, the alpha-to-coverage mapping for each count, and what a resolve is.

use crate::error::Error;
use graphics_profile::render3d_spec::{MSAA_2X, MSAA_4X, SamplePosition};
use render_math::{Vec2, Vec4};

/// The sample counts `Render3D Core Profile 1` admits.
pub const SAMPLE_COUNTS: &[u32] = &[1, 2, 4];

/// Where the samples of one pixel are, in the pixel's own `[0, 1]` space with the origin at its
/// top-left corner - the same origin a surface row has.
///
/// ONE SAMPLE IS THE PIXEL CENTRE, which is the only position that makes a one-sample target the
/// same picture an unmultisampled one would be.
pub fn sample_positions(count: u32) -> Result<&'static [SamplePosition], Error> {
	const CENTRE: &[SamplePosition] = &[SamplePosition { index: 0, x: 0.5, y: 0.5 }];
	match count {
		1 => Ok(CENTRE),
		2 => Ok(MSAA_2X),
		4 => Ok(MSAA_4X),
		other => Err(Error::LimitExceeded { limit: "sample count", ceiling: 4, asked: other as u64 }),
	}
}

/// How often the fragment stage runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShadingRate {
	/// ONCE PER COVERED PIXEL, with the result written to every covered sample. The default, and
	/// what makes multisampling cheaper than supersampling.
	PerPixel,
	/// Once per covered SAMPLE. Reached only by declaring an input `sample`, which is why it is a
	/// consequence of the shader rather than a state a caller sets.
	PerSample,
}

/// The shading rate a pipeline runs at, from whether any input is declared `sample`.
///
/// IT IS DERIVED AND NOT SET, because a pipeline whose shader reads a per-sample input and whose
/// state said per-pixel would be a contradiction with no correct resolution.
pub const fn shading_rate(any_sample_qualified_input: bool) -> ShadingRate {
	if any_sample_qualified_input { ShadingRate::PerSample } else { ShadingRate::PerPixel }
}

/// Where a `centroid` input is evaluated: the CENTRE OF MASS of the covered sample positions.
///
/// NOT "A COVERED SAMPLE'S POSITION". An average moves smoothly as coverage changes along an edge,
/// where "the first covered sample" jumps from one position to another between adjacent pixels - and
/// an interpolated value that jumps is a visible seam in exactly the place antialiasing exists for.
///
/// A fragment with NO covered sample cannot exist, and the pixel centre is the answer if one ever
/// reaches here rather than a division by zero.
pub fn centroid(count: u32, coverage: u32) -> Result<Vec2, Error> {
	let positions = sample_positions(count)?;
	let mut sum = Vec2::ZERO;
	let mut covered = 0u32;
	for position in positions {
		if coverage & (1 << position.index) != 0 {
			sum = sum.add(Vec2::new(position.x, position.y));
			covered += 1;
		}
	}
	if covered == 0 {
		return Ok(Vec2::new(0.5, 0.5));
	}
	Ok(sum.scale(1.0 / covered as f32))
}

/// The coverage mask alpha-to-coverage produces.
///
/// THE FIRST `round(alpha * count)` SAMPLE BITS IN INDEX ORDER. In index order rather than by a
/// dither pattern, because a pattern makes the result depend on the pixel's POSITION - two adjacent
/// pixels with the same alpha get different coverage, which is noise along every soft edge.
///
/// `alpha` is attachment 0's alpha AFTER the fragment stage and BEFORE blending, and is clamped: a
/// shader may produce anything, and a mask is a count of bits.
pub fn alpha_to_coverage(alpha: f32, count: u32) -> Result<u32, Error> {
	let positions = sample_positions(count)?;
	let clamped = if alpha.is_nan() { 0.0 } else { alpha.clamp(0.0, 1.0) };
	let bits = round_half_away(clamped * count as f32).min(count);
	let mut mask = 0u32;
	let mut index = 0u32;
	while index < bits {
		mask |= 1 << positions[index as usize].index;
		index += 1;
	}
	Ok(mask)
}

/// The coverage a fragment ends up with, with the two maskings applied IN THE FROZEN ORDER.
///
/// THE ORDER IS THE CONTRACT. The sample mask is ANDed with coverage BEFORE the depth test, so a
/// masked-out sample is not tested and NOT WRITTEN - which is what makes a sample mask a way to
/// render into a subset of samples rather than a way to discard colour after the fact.
/// Alpha-to-coverage is ANDed in the same place, from the alpha the fragment stage produced.
pub fn coverage_after_masks(raster_coverage: u32, sample_mask: u32, alpha_to_coverage_mask: Option<u32>) -> u32 {
	let mut coverage = raster_coverage & sample_mask;
	if let Some(mask) = alpha_to_coverage_mask {
		coverage &= mask;
	}
	coverage
}

/// Resolve a multisampled colour pixel: THE ARITHMETIC MEAN of the samples.
///
/// COMPUTED IN THE ATTACHMENT'S OWN NUMERIC SPACE AND ROUNDED ONCE AT THE END, which is what the
/// caller passing linear premultiplied values is doing. Averaging in an encoded space - sRGB values,
/// say - is the classic resolve that darkens every edge, and it is a mistake this signature cannot
/// make because it takes numbers rather than bytes.
///
/// EVERY SAMPLE COUNTS, covered or not: a resolve averages the attachment's samples, and a sample no
/// fragment covered holds whatever the pass cleared or loaded, which is part of the picture.
pub fn resolve_colour(samples: &[Vec4]) -> Result<Vec4, Error> {
	if samples.is_empty() {
		return Err(Error::TargetMismatch { reason: crate::error::AttachmentFault::ResolveMismatch { reason: "a resolve of no samples has no value" } });
	}
	let mut sum = Vec4::ZERO;
	for sample in samples {
		sum = sum.add(*sample);
	}
	Ok(sum.scale(1.0 / samples.len() as f32))
}

/// Resolve a multisampled INTEGER, DEPTH or STENCIL pixel: sample 0, and the rest discarded.
///
/// THERE IS NO AVERAGE OF TWO OBJECT IDS THAT IDENTIFIES ANYTHING, and a resolve that produced one
/// would invent an id. Depth is the same decision for a different reason the profile states: a
/// resolved depth is used for reconstruction, and the MINIMUM of the samples belongs to whichever
/// surface is nearest rather than to the surface the colour resolve is mostly made of.
pub fn resolve_first<T: Copy>(samples: &[T]) -> Result<T, Error> {
	samples.first().copied().ok_or(Error::TargetMismatch { reason: crate::error::AttachmentFault::ResolveMismatch { reason: "a resolve of no samples has no value" } })
}

/// Round half away from zero, without `std`. Only ever called on a clamped, finite product.
fn round_half_away(value: f32) -> u32 {
	if !(value > 0.0) {
		return 0;
	}
	(value + 0.5) as u32
}

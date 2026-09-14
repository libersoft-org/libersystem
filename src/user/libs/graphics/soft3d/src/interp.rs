//! PERSPECTIVE-CORRECT INTERPOLATION, AND THE TWO OTHER RULES BESIDE IT.
//!
//! THE SCREEN-SPACE BARYCENTRIC WEIGHTS ARE NOT WHAT A `smooth` VARYING USES. Interpolating an
//! attribute linearly across a projected triangle is the affine texture warp that made early
//! software renderers famous: the texture swims as the camera turns, worst on a floor seen at a
//! grazing angle. The correction is to interpolate `a/w` against `1/w` and divide back at the
//! sample, because both of those ARE linear in screen space.
//!
//! `noperspective` IS THE UNCORRECTED FORM AND IS NOT A MISTAKE. A value declared linear on the
//! screen - a screen-space gradient, a stipple parameter, a line's distance along itself - must stay
//! linear on the screen, and applying the correction to it would bend it.
//!
//! `flat` DOES NOT INTERPOLATE AT ALL. It is the provoking vertex's value, carried from before the
//! clip.

/// Interpolate one `smooth` attribute, perspective-correctly.
///
/// `weights` are the screen-space barycentric coordinates and `inverse_w` is `1/w` per vertex, both
/// of which the rasteriser already has.
pub fn smooth(weights: [f32; 3], inverse_w: [f32; 3], values: [f32; 3]) -> f32 {
	let denominator = weights[0] * inverse_w[0] + weights[1] * inverse_w[1] + weights[2] * inverse_w[2];
	if denominator == 0.0 || !denominator.is_finite() {
		// A SAMPLE WHERE EVERY `1/w` CONTRIBUTION CANCELS has no perspective-correct value. It cannot
		// arise from a clipped primitive - every `w` is positive after the `w` plane - so this is the
		// arithmetic's own guard rather than a case with a meaning.
		return values[0];
	}
	let numerator = weights[0] * inverse_w[0] * values[0] + weights[1] * inverse_w[1] * values[1] + weights[2] * inverse_w[2] * values[2];
	numerator / denominator
}

/// Interpolate one `noperspective` attribute: linear in screen space, no `1/w` weighting.
pub fn noperspective(weights: [f32; 3], values: [f32; 3]) -> f32 {
	weights[0] * values[0] + weights[1] * values[1] + weights[2] * values[2]
}

/// The depth a fragment carries.
///
/// DEPTH IS ALREADY LINEAR IN SCREEN SPACE and is interpolated WITHOUT the correction. `z/w` is what
/// the projection produced and what the viewport mapped; applying the perspective correction to it a
/// second time is the defect that makes a depth buffer almost right - near geometry correct, far
/// geometry subtly wrong, and z-fighting in places nothing overlaps.
pub fn depth(weights: [f32; 3], values: [f32; 3]) -> f32 {
	noperspective(weights, values)
}

/// `1/w` at a sample, which a shader's own perspective-dependent work needs.
pub fn inverse_w_at(weights: [f32; 3], inverse_w: [f32; 3]) -> f32 {
	noperspective(weights, inverse_w)
}

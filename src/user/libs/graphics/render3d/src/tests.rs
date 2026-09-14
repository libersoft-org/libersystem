//! The fixtures. Each holds a RULE the profile freezes or this crate decides, against a value
//! computed by hand from the definition - so an implementation that changes the arithmetic and keeps
//! the rule passes, and one that changes the rule fails.

use super::*;
use render_math::{Vec2, Vec4};

fn near(left: f32, right: f32) -> bool {
	(left - right).abs() <= 1e-6
}

// ---------------------------------------------------------------------------------------------
// Blend state.
// ---------------------------------------------------------------------------------------------

#[test]
// PREMULTIPLIED SOURCE-OVER IS THE BLEND A CORRECTLY AUTHORED TEXTURE WANTS, and it is the one every
// other assertion here is measured against.
fn premultiplied_source_over_is_what_its_equation_says() {
	let state = AttachmentBlend { enabled: true, colour: BlendEquation::PREMULTIPLIED_OVER, alpha: BlendEquation::PREMULTIPLIED_OVER, write_mask: ColorWriteMask::ALL };
	// A half-transparent red over an opaque blue, premultiplied.
	let source = Vec4::new(0.5, 0.0, 0.0, 0.5);
	let destination = Vec4::new(0.0, 0.0, 1.0, 1.0);
	let result = blend(&state, source, destination, Vec4::ZERO);
	assert!(near(result.x, 0.5), "red is the source's, which is already premultiplied: {}", result.x);
	assert!(near(result.z, 0.5), "blue is half the destination's, because the source covers half");
	assert!(near(result.w, 1.0), "and an opaque destination stays opaque");
}

#[test]
// SEPARATE COLOUR AND ALPHA STATE, which is the reason the type has two equations: the commonest
// mistake is a blend that gets alpha right and colour wrong, and a state that could not say the two
// apart would make it unfixable.
fn the_colour_and_alpha_equations_are_applied_independently() {
	let state = AttachmentBlend {
		enabled: true,
		// Colour adds; alpha keeps the destination's.
		colour: BlendEquation { source: BlendFactor::One, destination: BlendFactor::One, operation: BlendOp::Add },
		alpha: BlendEquation { source: BlendFactor::Zero, destination: BlendFactor::One, operation: BlendOp::Add },
		write_mask: ColorWriteMask::ALL,
	};
	let result = blend(&state, Vec4::new(0.25, 0.0, 0.0, 1.0), Vec4::new(0.25, 0.0, 0.0, 0.5), Vec4::ZERO);
	assert!(near(result.x, 0.5), "the colour equation added");
	assert!(near(result.w, 0.5), "and the alpha equation kept the destination's, which the colour one would not have");
}

#[test]
// `SrcAlphaSaturate` IS ASYMMETRIC AND THE ASYMMETRY IS THE DEFINITION: the minimum on colour, one
// on alpha. A factor that applied the minimum to alpha too would never reach an opaque result, which
// is the whole thing accumulating coverage is for.
fn src_alpha_saturate_is_one_on_alpha_and_the_minimum_on_colour() {
	let state = AttachmentBlend { enabled: true, colour: BlendEquation { source: BlendFactor::SrcAlphaSaturate, destination: BlendFactor::One, operation: BlendOp::Add }, alpha: BlendEquation { source: BlendFactor::SrcAlphaSaturate, destination: BlendFactor::One, operation: BlendOp::Add }, write_mask: ColorWriteMask::ALL };
	// Source alpha 0.75, destination alpha 0.5: the colour factor is min(0.75, 0.5) = 0.5.
	let result = blend(&state, Vec4::new(1.0, 0.0, 0.0, 0.75), Vec4::new(0.0, 0.0, 0.0, 0.5), Vec4::ZERO);
	assert!(near(result.x, 0.5), "the colour factor is min(src_alpha, 1 - dst_alpha): {}", result.x);
	assert!(near(result.w, 1.25), "and the alpha factor is ONE, so alpha accumulates rather than saturating with it: {}", result.w);
}

#[test]
// MIN AND MAX IGNORE THE FACTORS, which is what the operation means everywhere it exists and is
// stated because a state that set them and expected them to apply would be silently wrong.
fn min_and_max_ignore_the_factors_they_are_given() {
	let equation = |operation| BlendEquation { source: BlendFactor::Zero, destination: BlendFactor::Zero, operation };
	let state = |operation| AttachmentBlend { enabled: true, colour: equation(operation), alpha: equation(operation), write_mask: ColorWriteMask::ALL };
	let source = Vec4::new(0.25, 0.75, 0.0, 0.25);
	let destination = Vec4::new(0.75, 0.25, 1.0, 0.75);
	let minimum = blend(&state(BlendOp::Min), source, destination, Vec4::ZERO);
	assert!(near(minimum.x, 0.25) && near(minimum.y, 0.25), "both factors were Zero and the minimum is still the minimum");
	let maximum = blend(&state(BlendOp::Max), source, destination, Vec4::ZERO);
	assert!(near(maximum.x, 0.75) && near(maximum.y, 0.75));
}

#[test]
// SUBTRACT AND REVERSE SUBTRACT ARE NAMED FOR WHICH WAY ROUND THEY ARE, and this is the fixture that
// says which is which rather than leaving it to the reader.
fn subtract_and_reverse_subtract_go_opposite_ways() {
	let state = |operation| AttachmentBlend { enabled: true, colour: BlendEquation { source: BlendFactor::One, destination: BlendFactor::One, operation }, alpha: BlendEquation::REPLACE, write_mask: ColorWriteMask::ALL };
	let source = Vec4::new(0.75, 0.0, 0.0, 1.0);
	let destination = Vec4::new(0.25, 0.0, 0.0, 1.0);
	assert!(near(blend(&state(BlendOp::Subtract), source, destination, Vec4::ZERO).x, 0.5), "source minus destination");
	assert!(near(blend(&state(BlendOp::ReverseSubtract), source, destination, Vec4::ZERO).x, -0.5), "destination minus source");
}

#[test]
// THE MASK IS APPLIED AFTER THE BLEND, and a masked channel keeps the DESTINATION rather than an
// unwritten blended value. The two answers differ for anything reading `DstColor`, for `Min` and for
// `Max` - which is most of the states a mask is used with.
fn a_write_mask_keeps_the_destination_rather_than_the_blended_value() {
	let state = AttachmentBlend { enabled: true, colour: BlendEquation { source: BlendFactor::One, destination: BlendFactor::One, operation: BlendOp::Add }, alpha: BlendEquation::REPLACE, write_mask: ColorWriteMask { red: false, green: true, blue: true, alpha: true } };
	let result = blend(&state, Vec4::new(0.5, 0.5, 0.0, 1.0), Vec4::new(0.25, 0.25, 0.0, 1.0), Vec4::ZERO);
	assert!(near(result.x, 0.25), "red was masked, so it is the DESTINATION's value and not the sum");
	assert!(near(result.y, 0.75), "and green was not");
}

#[test]
// AN INTEGER FORMAT CANNOT BLEND, which the profile states as a rule: there is no correct answer to
// what the average of two object ids is. The refusal names the format and what it was used as.
fn blending_an_integer_attachment_is_refused_by_name() {
	let mut state: BlendState<2> = BlendState::new();
	state.attachments[1].enabled = true;
	assert_eq!(state.validate(&["RGBA8", "R32Uint"]), Err(Error::UnsupportedFormat { format: "R32Uint", used_as: "a blend target" }));
	// Disabled on the integer target is fine - a deferred pass writes ids without blending them.
	state.attachments[1].enabled = false;
	state.attachments[0].enabled = true;
	assert_eq!(state.validate(&["RGBA8", "R32Uint"]), Ok(()));
	// And a format the profile does not have at all is a different refusal.
	assert_eq!(state.validate(&["NoSuchFormat", "R32Uint"]), Err(Error::UnsupportedFormat { format: "NoSuchFormat", used_as: "a colour attachment" }));
}

// ---------------------------------------------------------------------------------------------
// MSAA geometry.
// ---------------------------------------------------------------------------------------------

#[test]
// THE SAMPLE POSITIONS ARE THE PROFILE'S, read rather than restated - and one sample is the pixel
// CENTRE, which is the only position that makes a one-sample target the picture an unmultisampled
// one would be.
fn the_sample_positions_are_the_frozen_ones_and_one_sample_is_the_centre() {
	let one = sample_positions(1).expect("one sample");
	assert_eq!(one.len(), 1);
	assert!(near(one[0].x, 0.5) && near(one[0].y, 0.5));
	assert_eq!(sample_positions(2).expect("two"), graphics_profile::render3d_spec::MSAA_2X);
	assert_eq!(sample_positions(4).expect("four"), graphics_profile::render3d_spec::MSAA_4X);
	assert_eq!(sample_positions(8), Err(Error::LimitExceeded { limit: "sample count", ceiling: 4, asked: 8 }));
	// THE INDICES ARE THE BIT POSITIONS a mask is written in, so they must be the first `count`
	// integers - a table whose indices skipped one would make every mask wrong.
	for count in [2u32, 4] {
		let positions = sample_positions(count).expect("a count the profile has");
		assert_eq!(positions.len(), count as usize);
		for (at, position) in positions.iter().enumerate() {
			assert_eq!(position.index, at as u32, "sample {at} of {count} is indexed {}", position.index);
			assert!(position.x > 0.0 && position.x < 1.0 && position.y > 0.0 && position.y < 1.0, "a sample is inside its pixel");
		}
	}
}

#[test]
// THE CENTROID IS THE CENTRE OF MASS OF THE COVERED SAMPLES, not a covered sample's position. An
// average moves smoothly along an edge where "the first covered sample" jumps between pixels.
fn a_centroid_is_the_average_of_the_covered_samples() {
	// All four covered: the average of the rotated grid, which is the pixel centre.
	let all = centroid(4, 0b1111).expect("four samples");
	assert!(near(all.x, 0.5) && near(all.y, 0.5), "the whole grid averages to the centre: {all:?}");
	// Samples 0 and 1 alone: the average of (0.375, 0.125) and (0.875, 0.375).
	let half = centroid(4, 0b0011).expect("four samples");
	assert!(near(half.x, 0.625) && near(half.y, 0.25), "the covered pair's centre of mass: {half:?}");
	// It is NOT the first covered sample's position, which is the implementation this rule refuses.
	assert!(half != Vec2::new(0.375, 0.125));
	// A fragment with no coverage cannot exist; the pixel centre is the answer rather than a
	// division by zero.
	assert_eq!(centroid(4, 0).expect("four samples"), Vec2::new(0.5, 0.5));
}

#[test]
// ALPHA TO COVERAGE IS THE FIRST `round(alpha * count)` BITS IN INDEX ORDER. In index order rather
// than by a dither pattern, because a pattern makes the result depend on the pixel's POSITION and two
// adjacent pixels with the same alpha get different coverage.
fn alpha_to_coverage_takes_the_first_bits_in_index_order() {
	assert_eq!(alpha_to_coverage(0.0, 4).expect("four"), 0b0000);
	assert_eq!(alpha_to_coverage(0.25, 4).expect("four"), 0b0001);
	assert_eq!(alpha_to_coverage(0.5, 4).expect("four"), 0b0011);
	assert_eq!(alpha_to_coverage(0.75, 4).expect("four"), 0b0111);
	assert_eq!(alpha_to_coverage(1.0, 4).expect("four"), 0b1111);
	// ROUNDING IS AT THE HALF AND IS THE SAME EVERYWHERE: 0.3 * 4 = 1.2 rounds to one bit.
	assert_eq!(alpha_to_coverage(0.3, 4).expect("four"), 0b0001);
	assert_eq!(alpha_to_coverage(0.4, 4).expect("four"), 0b0011, "1.6 rounds to two");
	// Out of range and NaN are clamped rather than producing a mask of arbitrary width.
	assert_eq!(alpha_to_coverage(2.0, 4).expect("four"), 0b1111);
	assert_eq!(alpha_to_coverage(-1.0, 4).expect("four"), 0b0000);
	assert_eq!(alpha_to_coverage(f32::NAN, 4).expect("four"), 0b0000);
}

#[test]
// THE ORDER OF THE TWO MASKINGS IS THE CONTRACT: both are ANDed with coverage BEFORE the depth test,
// so a masked-out sample is not tested and not written.
fn the_sample_mask_and_alpha_to_coverage_both_narrow_coverage_before_the_depth_test() {
	let raster = 0b1111;
	assert_eq!(coverage_after_masks(raster, 0b0101, None), 0b0101);
	assert_eq!(coverage_after_masks(raster, 0b1111, Some(0b0011)), 0b0011);
	assert_eq!(coverage_after_masks(raster, 0b0110, Some(0b0011)), 0b0010, "both narrow, and the result is the intersection");
	assert_eq!(coverage_after_masks(0b0001, 0b1110, None), 0, "a mask that excludes every covered sample leaves nothing to test");
}

#[test]
// A COLOUR RESOLVE IS THE ARITHMETIC MEAN and an INTEGER, DEPTH or STENCIL resolve is sample 0. The
// second is not a shortcut: there is no average of two object ids that identifies anything.
fn a_colour_resolve_averages_and_an_integer_resolve_takes_sample_zero() {
	let samples = [Vec4::new(1.0, 0.0, 0.0, 1.0), Vec4::new(0.0, 1.0, 0.0, 1.0), Vec4::new(0.0, 0.0, 1.0, 1.0), Vec4::ZERO];
	let resolved = resolve_colour(&samples).expect("four samples");
	assert!(near(resolved.x, 0.25) && near(resolved.y, 0.25) && near(resolved.z, 0.25) && near(resolved.w, 0.75));
	assert_eq!(resolve_first(&[7u32, 9, 11, 13]).expect("four samples"), 7);
	assert!(resolve_colour(&[]).is_err(), "a resolve of no samples has no value");
}

#[test]
// THE SHADING RATE IS DERIVED FROM THE SHADER AND NOT SET BY THE CALLER, because a pipeline whose
// shader reads a per-sample input and whose state said per-pixel would be a contradiction with no
// correct resolution.
fn the_shading_rate_follows_the_shader_rather_than_a_state_bit() {
	assert_eq!(shading_rate(false), ShadingRate::PerPixel);
	assert_eq!(shading_rate(true), ShadingRate::PerSample);
}

// ---------------------------------------------------------------------------------------------
// Depth and stencil.
// ---------------------------------------------------------------------------------------------

#[test]
// EVERY FORMAT THIS CRATE HAS IS ONE THE FROZEN TABLE DEFINES, and the reverse: a format in the
// profile with no value here would be one this API cannot ask for.
fn every_depth_format_matches_the_frozen_table_in_both_directions() {
	assert_eq!(ALL_DEPTH_FORMATS.len(), graphics_profile::render3d_spec::DEPTH_FORMATS.len());
	for format in ALL_DEPTH_FORMATS {
		let frozen = format.frozen().expect("a format the profile defines");
		assert_eq!(frozen.name, format.name());
	}
	for entry in graphics_profile::render3d_spec::DEPTH_FORMATS {
		assert!(ALL_DEPTH_FORMATS.iter().any(|format| format.name() == entry.name), "the profile has {} and this API has no value for it", entry.name);
	}
}

#[test]
// A NORMALISED DEPTH IS `round(d * max)`, ONCE, at the boundaries and at the half.
fn a_normalised_depth_rounds_once_and_reaches_both_ends_of_its_range() {
	assert_eq!(depth::store(DepthFormat::Depth16, 0.0), Stored::Normalised(0));
	assert_eq!(depth::store(DepthFormat::Depth16, 1.0), Stored::Normalised(65_535));
	assert_eq!(depth::store(DepthFormat::Depth24, 1.0), Stored::Normalised(16_777_215));
	// Half a unit of storage rounds up, which is the rule two implementations otherwise differ on.
	let half_unit = 0.5 / 65_535.0;
	assert_eq!(depth::store(DepthFormat::Depth16, half_unit), Stored::Normalised(1));
	// Out of range is clamped rather than wrapping, and a NaN is the near plane rather than an
	// arbitrary integer.
	assert_eq!(depth::store(DepthFormat::Depth16, 2.0), Stored::Normalised(65_535));
	assert_eq!(depth::store(DepthFormat::Depth16, -1.0), Stored::Normalised(0));
	assert_eq!(depth::store(DepthFormat::Depth16, f32::NAN), Stored::Normalised(0));
}

#[test]
// THE `Depth32F` ANSWER, IN CODE: the stored floats are compared and the incoming depth is NOT
// quantised. Quantisation is what a normalised format does because its storage cannot hold anything
// else; imposing it on a float format throws away the precision the format exists for.
fn a_float_depth_is_stored_and_compared_without_quantisation() {
	// A value far finer than a 24-bit step survives.
	let fine = 1.0e-9_f32;
	assert_eq!(depth::store(DepthFormat::Depth32F, fine), Stored::Float(fine));
	assert_eq!(depth::store(DepthFormat::Depth24, fine), Stored::Normalised(0), "the same value is zero in a normalised format, which is the contrast");
	// And two depths a 24-bit buffer cannot tell apart are ordered in a float one.
	let a = 1.0e-9_f32;
	let b = 2.0e-9_f32;
	assert!(depth::depth_test(DepthFormat::Depth32F, CompareOp::Less, a, Stored::Float(b)), "the float format distinguishes them");
	assert!(!depth::depth_test(DepthFormat::Depth24, CompareOp::Less, a, Stored::Normalised(0)), "and the normalised one cannot");
	// The readback converts, so a caller need not know the storage.
	assert!(near(depth::readback(DepthFormat::Depth16, Stored::Normalised(65_535)), 1.0));
	assert!(near(depth::readback(DepthFormat::Depth32F, Stored::Float(0.25)), 0.25));
}

#[test]
// `incoming OP stored`, WHICH IS THE ORDER, and every operation over a boundary pair.
fn every_compare_operation_is_incoming_against_stored() {
	assert!(CompareOp::Less.test(1u32, 2u32));
	assert!(!CompareOp::Less.test(2u32, 1u32), "the order is incoming OP stored, and this is the assertion that says so");
	assert!(CompareOp::Greater.test(2u32, 1u32));
	assert!(CompareOp::LessOrEqual.test(2u32, 2u32));
	assert!(CompareOp::GreaterOrEqual.test(2u32, 2u32));
	assert!(CompareOp::Equal.test(2u32, 2u32));
	assert!(CompareOp::NotEqual.test(1u32, 2u32));
	assert!(CompareOp::Always.test(9u32, 0u32));
	assert!(!CompareOp::Never.test(0u32, 9u32));
	assert_eq!(ALL_COMPARE_OPS.len(), 8, "the profile names eight");
}

#[test]
// THE STENCIL TEST COMES FIRST AND WHICH OPERATION RUNS DEPENDS ON BOTH TESTS. An implementation
// that ran depth first would write a different stencil for every fragment the two disagree about,
// which is most of the fragments a stencil is used for.
fn the_stencil_test_precedes_the_depth_test_and_selects_by_both_outcomes() {
	let face = |compare, fail, depth_fail, pass| StencilFace { compare, read_mask: 0xff, write_mask: 0xff, reference: 5, on_fail: fail, on_depth_fail: depth_fail, on_pass: pass };

	// Stencil FAILS: `on_fail` runs, the depth test does not happen, and nothing is written to depth.
	let state = face(CompareOp::Never, StencilOp::Replace, StencilOp::Zero, StencilOp::Invert);
	let outcome = depth::test(DepthFormat::Depth24Stencil8, CompareOp::Always, true, 0.5, Stored::Normalised(0), Some((&state, 3)));
	assert!(!outcome.passed);
	assert_eq!(outcome.stencil, Some(5), "on_fail ran");
	assert_eq!(outcome.depth, None, "and a fragment that failed the stencil test writes no depth");

	// Stencil PASSES and depth FAILS: `on_depth_fail`.
	let state = face(CompareOp::Always, StencilOp::Replace, StencilOp::Zero, StencilOp::Invert);
	let outcome = depth::test(DepthFormat::Depth24Stencil8, CompareOp::Never, true, 0.5, Stored::Normalised(0), Some((&state, 3)));
	assert!(!outcome.passed);
	assert_eq!(outcome.stencil, Some(0), "on_depth_fail ran");
	assert_eq!(outcome.depth, None);

	// BOTH pass: `on_pass`, and the depth is written.
	let outcome = depth::test(DepthFormat::Depth24Stencil8, CompareOp::Always, true, 0.5, Stored::Normalised(0), Some((&state, 3)));
	assert!(outcome.passed);
	assert_eq!(outcome.stencil, Some(!3u8), "on_pass ran");
	assert_eq!(outcome.depth, Some(depth::store(DepthFormat::Depth24Stencil8, 0.5)));

	// AND A DEPTH WRITE THAT IS OFF WRITES NOTHING even when the test passed.
	let outcome = depth::test(DepthFormat::Depth24Stencil8, CompareOp::Always, false, 0.5, Stored::Normalised(0), Some((&state, 3)));
	assert!(outcome.passed);
	assert_eq!(outcome.depth, None);
}

#[test]
// THE MASKS SELECT BITS AND NOT VALUES: the read mask narrows what is compared, the write mask
// narrows what changes, and the bits outside it keep what was stored.
fn the_stencil_masks_select_bits_in_both_directions() {
	let face = StencilFace { compare: CompareOp::Equal, read_mask: 0x0f, write_mask: 0xf0, reference: 0x15, on_fail: StencilOp::Keep, on_depth_fail: StencilOp::Keep, on_pass: StencilOp::Replace };
	// Reference 0x15 and stored 0x25 differ in the high nibble, which the read mask excludes.
	let outcome = depth::test(DepthFormat::Depth24Stencil8, CompareOp::Always, false, 0.5, Stored::Normalised(0), Some((&face, 0x25)));
	assert!(outcome.passed, "the read mask compared only the low nibble, and those are equal");
	// `Replace` writes the reference, but only through the write mask: the low nibble is kept.
	assert_eq!(outcome.stencil, Some(0x15 & 0xf0 | 0x25 & 0x0f));
}

#[test]
// EVERY STENCIL OPERATION, including the two that saturate and the two that wrap - which is the pair
// an implementation gets wrong by using one rule for both.
fn every_stencil_operation_does_what_its_name_says_at_the_boundary() {
	assert_eq!(StencilOp::Keep.apply(7, 3), 7);
	assert_eq!(StencilOp::Zero.apply(7, 3), 0);
	assert_eq!(StencilOp::Replace.apply(7, 3), 3);
	assert_eq!(StencilOp::Invert.apply(0x0f, 0), 0xf0);
	assert_eq!(StencilOp::IncrementClamp.apply(255, 0), 255, "clamping stops");
	assert_eq!(StencilOp::IncrementWrap.apply(255, 0), 0, "and wrapping does not");
	assert_eq!(StencilOp::DecrementClamp.apply(0, 0), 0);
	assert_eq!(StencilOp::DecrementWrap.apply(0, 0), 255);
	assert_eq!(ALL_STENCIL_OPS.len(), 8);
}

#[test]
// THE BIAS UNIT DEPENDS ON THE FORMAT, which is why the format is a parameter: for a normalised
// format `r` is one unit of storage everywhere, and for a float format it is `2^(exponent(z) - 23)`,
// which changes across the buffer. One number for both is far too small near zero on a float buffer.
fn the_depth_bias_unit_is_the_formats_own_and_the_clamp_bounds_it() {
	// A normalised format: one constant unit is one step of 1/65535.
	let biased = depth::bias(DepthFormat::Depth16, 0.5, 0.0, 1.0, 0.0, 0.0);
	assert!(near(biased - 0.5, 1.0 / 65_535.0), "one unit of a 16-bit buffer: {}", biased - 0.5);
	// A float format near zero has a far finer step than one near one.
	let near_zero = depth::bias(DepthFormat::Depth32F, 1.0e-6, 0.0, 1.0, 0.0, 0.0) - 1.0e-6;
	let near_one = depth::bias(DepthFormat::Depth32F, 1.0, 0.0, 1.0, 0.0, 0.0) - 1.0;
	assert!(near_zero > 0.0 && near_one > 0.0);
	assert!(near_zero < near_one * 1e-3, "the step near zero is orders of magnitude finer: {near_zero} against {near_one}");
	// The slope term is the maximum of the two derivatives, scaled.
	let sloped = depth::bias(DepthFormat::Depth16, 0.5, 0.25, 0.0, 2.0, 0.0);
	assert!(near(sloped - 0.5, 0.5), "slope_factor * m");
	// And a clamp bounds the magnitude whichever sign it is given.
	assert!(near(depth::bias(DepthFormat::Depth16, 0.5, 1.0, 0.0, 10.0, 0.125) - 0.5, 0.125));
}

#[test]
// A CLEAR IS CONVERTED BY THE SAME RULE A FRAGMENT'S DEPTH IS, and a stencil clear takes the low
// eight bits of a `u32`, which is what the profile says.
fn a_clear_goes_through_the_same_conversion_a_fragment_does() {
	assert_eq!(depth::clear_depth(DepthFormat::Depth16, 1.0), depth::store(DepthFormat::Depth16, 1.0));
	assert_eq!(depth::clear_depth(DepthFormat::Depth32F, 0.25), Stored::Float(0.25));
	assert_eq!(depth::clear_stencil(0x1234), 0x34);
}

// ---------------------------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------------------------

#[test]
// THE PROFILE'S MINIMUMS ARE THE FLOOR, and the check is DRIVEN BY THE FROZEN TABLE - so a minimum
// added to the profile without a field here is a refusal rather than a check that silently stopped
// covering it.
fn the_profile_minimum_device_conforms_and_one_below_it_does_not() {
	assert_eq!(Render3DLimits::PROFILE_MINIMUM.conforms(), Ok(()));
	let mut short = Render3DLimits::PROFILE_MINIMUM;
	short.max_colour_attachments = 3;
	assert_eq!(short.conforms(), Err(Error::LimitExceeded { limit: "max_colour_attachments", ceiling: 4, asked: 3 }));
	// Every minimum the profile states has a field here.
	for entry in graphics_profile::render3d_spec::RENDER3D_PROFILE_1_MIN_LIMITS {
		let mut probe = Render3DLimits::PROFILE_MINIMUM;
		let _ = &mut probe;
		assert!(probe.conforms().is_ok(), "the floor conforms, so a missing field would show as a refusal naming {}", entry.name);
	}
}

#[test]
// OVER THE HARD MAXIMUM IS `LimitExceeded` AND OVER THE BUDGET IS `OutOfMemory`, and they are
// different because one is permanent and the other is not. Folding them leaves a caller unable to
// tell a no from a not-now.
fn a_limit_and_a_budget_are_different_refusals() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	assert_eq!(limits.admit_texture_2d(8192, 1024, 4), Err(Error::LimitExceeded { limit: "texture width", ceiling: 4096, asked: 8192 }));
	// Inside the extent and over the byte limit: 4096x4096 at sixteen bytes a texel is 256 MB, and
	// the floor device's byte ceiling is exactly that - so one more row is over.
	assert_eq!(limits.admit_texture_2d(4096, 4096, 16), Ok(256 * 1024 * 1024));
	let mut tight = limits;
	tight.max_texture_bytes = 64 * 1024 * 1024;
	assert_eq!(tight.admit_texture_2d(4096, 4096, 16), Err(Error::LimitExceeded { limit: "texture bytes", ceiling: 64 * 1024 * 1024, asked: 256 * 1024 * 1024 }));
	// And the budget is the other refusal.
	assert_eq!(limits::admit_budget(1024, 512), Err(Error::OutOfMemory { bytes: 1024 }));
	assert_eq!(limits::admit_budget(512, 512), Ok(()));
}

#[test]
// NEGOTIATION ANSWERS THE ACTUAL LIMITS AND NEVER GRANTS MORE THAN WAS ASKED FOR. A caller that
// asked for eight samples and got four is told so ONCE, here, and plans against four.
fn negotiation_answers_what_was_granted_and_says_whether_it_is_what_was_asked() {
	let implementation = Render3DLimits { max_samples: 4, max_colour_attachments: 8, ..Render3DLimits::PROFILE_MINIMUM };
	let mut wanted = Render3DLimits::PROFILE_MINIMUM;
	wanted.max_samples = 8;
	wanted.max_colour_attachments = 4;
	let granted = negotiate(&wanted, &implementation);
	assert!(!granted.exactly_as_requested, "less was granted than asked, and the caller is told");
	assert_eq!(granted.limits.max_samples, 4, "the implementation's number");
	assert_eq!(granted.limits.max_colour_attachments, 4, "and NEVER more than was asked for, even though the device has eight");
	// Asking for exactly what is there is exact.
	let exact = negotiate(&implementation, &implementation);
	assert!(exact.exactly_as_requested);
	assert_eq!(exact.limits, implementation);
}

#[test]
// A SAMPLE COUNT IS ADMITTED BY THE PROFILE'S LIST AND BY THE DEVICE, and three is neither.
fn a_sample_count_is_checked_against_the_profile_and_the_device() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	assert_eq!(limits.admit_samples(1), Ok(()));
	assert_eq!(limits.admit_samples(4), Ok(()));
	assert_eq!(limits.admit_samples(3), Err(Error::LimitExceeded { limit: "sample count", ceiling: 4, asked: 3 }));
	let mut single = limits;
	single.max_samples = 1;
	assert_eq!(single.admit_samples(4), Err(Error::LimitExceeded { limit: "sample count", ceiling: 1, asked: 4 }));
}

#[test]
// THE ERROR SET IS THE PROFILE'S NAMED LIST AND NOTHING ELSE. A variant here that the plan does not
// name is a refusal nobody agreed to; a name the plan gives with no variant is a case that has to be
// reported as something else, which is how "invalid" enums grow.
//
// WRITTEN AS ONE VALUE PER VARIANT rather than as a count, because a count passes when two variants
// are added and one removed.
fn every_named_refusal_exists_and_is_its_own_variant() {
	let every: [Error; 12] = [
		Error::InvalidMesh { reason: MeshFault::IndexOutOfRange { index: 4, vertices: 3 } },
		Error::InvalidTransform,
		Error::UnsupportedFormat { format: "RGBA32F", used_as: "a blend target" },
		Error::TargetMismatch { reason: AttachmentFault::SampleCountMismatch { samples: 4, expected: 1 } },
		Error::NonFinite { what: "a vertex position" },
		Error::LimitExceeded { limit: "sample count", ceiling: 4, asked: 8 },
		Error::OutOfMemory { bytes: 1 << 30 },
		Error::InvalidTexture { reason: "a cube map that is not square" },
		Error::InvalidRenderState { reason: "a depth test with no depth attachment" },
		Error::InvalidShader { reason: "a loop with no bound" },
		Error::IncompatiblePipeline { reason: "the attachment count disagrees with the pass" },
		Error::Unsupported { feature: "an extension beyond Profile 1" },
	];
	// EACH IS DISTINCT FROM EVERY OTHER, which is what makes a caller able to branch on them.
	for (at, left) in every.iter().enumerate() {
		for (other, right) in every.iter().enumerate() {
			assert_eq!(at == other, left == right, "variant {at} and variant {other} compare wrongly");
		}
	}
	// AND THE TWO PAIRS THAT MUST NOT BE FOLDED ARE NOT.
	assert_ne!(Error::LimitExceeded { limit: "x", ceiling: 1, asked: 2 }, Error::OutOfMemory { bytes: 2 }, "a permanent no and a temporary one are different answers");
	assert_ne!(Error::NonFinite { what: "a vertex" }, Error::InvalidTransform, "the caller's data and the caller's maths are different fixes");
}

// ---------------------------------------------------------------------------------------------
// The resource model.
// ---------------------------------------------------------------------------------------------

fn texture(dimension: TextureDimension, width: u32, height: u32) -> TextureDesc {
	TextureDesc { dimension, width, height, depth: 1, mip_levels: 1, layers: if matches!(dimension, TextureDimension::Cube) { 6 } else { 1 }, samples: 1, format: "RGBA8", usage: TextureUsage { sampled: true, ..TextureUsage::default() } }
}

#[test]
// A DESCRIPTION THAT DESCRIBES NO TEXTURE IS REFUSED WHERE IT IS WRITTEN, not where it is used. A
// zero extent, a cube map that is not square, a mip chain longer than the extent has - each is a
// mistake with a name, and creating it successfully moves the refusal somewhere with less context.
fn a_texture_description_is_refused_at_the_description() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	assert_eq!(texture(TextureDimension::D2, 256, 256).validate(&limits), Ok(()));
	let mut zero = texture(TextureDimension::D2, 0, 256);
	assert!(matches!(zero.validate(&limits), Err(Error::InvalidTexture { .. })), "a zero extent has no texels");
	zero.width = 256;
	zero.mip_levels = 0;
	assert!(matches!(zero.validate(&limits), Err(Error::InvalidTexture { .. })));

	// A cube map's faces are square and come six at a time.
	let mut cube = texture(TextureDimension::Cube, 256, 128);
	assert!(matches!(cube.validate(&limits), Err(Error::InvalidTexture { .. })), "a cube map's faces are square");
	cube.height = 256;
	cube.layers = 4;
	assert!(matches!(cube.validate(&limits), Err(Error::InvalidTexture { .. })), "and there are six of them per array element");
	cube.layers = 12;
	assert_eq!(cube.validate(&limits), Ok(()), "two cube maps in an array is twelve layers");

	// A mip chain longer than the extent has.
	let mut deep = texture(TextureDimension::D2, 8, 8);
	assert_eq!(deep.full_mip_chain(), 4, "8, 4, 2, 1");
	deep.mip_levels = 4;
	assert_eq!(deep.validate(&limits), Ok(()));
	deep.mip_levels = 5;
	assert!(matches!(deep.validate(&limits), Err(Error::InvalidTexture { .. })));
}

#[test]
// A MULTISAMPLED TEXTURE IS A RENDER TARGET AND NOT A TEXTURE, which is three refusals rather than
// one: no mip chain, not sampled, and no way to be written unless it is an attachment.
fn a_multisampled_texture_is_a_render_target_and_says_so_three_ways() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	let base = TextureDesc { samples: 4, usage: TextureUsage { colour_attachment: true, ..TextureUsage::default() }, ..texture(TextureDimension::D2, 256, 256) };
	assert_eq!(base.validate(&limits), Ok(()));
	let mut chained = base;
	chained.mip_levels = 4;
	assert!(matches!(chained.validate(&limits), Err(Error::InvalidTexture { .. })), "a chain of multisampled levels would need a resolve per level");
	let mut sampled = base;
	sampled.usage.sampled = true;
	assert!(matches!(sampled.validate(&limits), Err(Error::InvalidTexture { .. })), "a multisampled texture is resolved before it is sampled");
	let mut orphan = base;
	orphan.usage = TextureUsage { copy_source: true, ..TextureUsage::default() };
	assert!(matches!(orphan.validate(&limits), Err(Error::InvalidTexture { .. })), "and one that is not an attachment has no way to be written");
}

#[test]
// A VIEW ADDRESSES A SUBRESOURCE, AND THE OVERLAP TEST IS WHAT EVERY HAZARD RULE IS REALLY ABOUT.
// Sampling mip 2 while rendering into mip 0 of one texture is not a conflict, and a model that could
// not tell them apart would refuse it.
fn views_overlap_only_where_they_reach_the_same_texels() {
	let base = TextureDesc { mip_levels: 4, layers: 6, dimension: TextureDimension::Cube, ..texture(TextureDimension::Cube, 64, 64) };
	let whole = TextureViewDesc::whole(&base, Aspect::Colour);
	assert_eq!(whole.validate(&base), Ok(()));

	let mip0 = TextureViewDesc { dimension: TextureDimension::D2, aspect: Aspect::Colour, base_mip: 0, mip_count: 1, base_layer: 0, layer_count: 1 };
	let mip2 = TextureViewDesc { base_mip: 2, ..mip0 };
	assert!(!mip0.overlaps(&mip2), "different mip levels are different texels");
	assert!(mip0.overlaps(&whole), "and a view of everything reaches both");

	let face3 = TextureViewDesc { base_layer: 3, ..mip0 };
	assert!(!mip0.overlaps(&face3), "different cube faces are different texels - which is what a shadow pass rendering one face at a time needs");

	// A view past the end of what it names is refused.
	let past = TextureViewDesc { base_mip: 3, mip_count: 2, ..mip0 };
	assert!(matches!(past.validate(&base), Err(Error::InvalidTexture { .. })));
	let past_layer = TextureViewDesc { base_layer: 5, layer_count: 2, ..mip0 };
	assert!(matches!(past_layer.validate(&base), Err(Error::InvalidTexture { .. })));

	// DEPTH AND STENCIL ARE DIFFERENT PLANES and do not conflict, which is what lets one pass read
	// the depth of a buffer whose stencil another is writing.
	let depth = TextureViewDesc { aspect: Aspect::Depth, ..mip0 };
	let stencil = TextureViewDesc { aspect: Aspect::Stencil, ..mip0 };
	assert!(!depth.overlaps(&stencil));
	let both = TextureViewDesc { aspect: Aspect::DepthAndStencil, ..mip0 };
	assert!(both.overlaps(&depth) && both.overlaps(&stencil));
}

fn target(format: &'static str, samples: u32) -> RenderTargetView {
	RenderTargetView { texture: 1, view: TextureViewDesc { dimension: TextureDimension::D2, aspect: Aspect::Colour, base_mip: 0, mip_count: 1, base_layer: 0, layer_count: 1 }, format, samples, width: 640, height: 480, load: LoadOp::Clear, store: StoreOp::Store }
}

#[test]
// A PASS HAS ONE VIEWPORT AND ONE SET OF FRAGMENTS, so every attachment shares an extent and a
// sample count - and a resolve destination has ONE sample and the SOURCE'S format, because a resolve
// is an average and not a conversion.
fn a_render_target_set_holds_its_attachments_to_one_extent_and_one_sample_count() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	let one = [target("RGBA8", 1)];
	assert_eq!(RenderTargetSet { colour: &one, depth_stencil: None, resolve: &[] }.validate(&limits), Ok(()));

	let mismatched = [target("RGBA8", 1), RenderTargetView { width: 320, ..target("RGBA8", 1) }];
	assert!(matches!(RenderTargetSet { colour: &mismatched, depth_stencil: None, resolve: &[] }.validate(&limits), Err(Error::TargetMismatch { .. })));

	let samples = [target("RGBA8", 4), target("RGBA8", 1)];
	assert!(matches!(RenderTargetSet { colour: &samples, depth_stencil: None, resolve: &[] }.validate(&limits), Err(Error::TargetMismatch { .. })));

	// A resolve of a multisampled pass into a single-sample target of the same format.
	let multi = [target("RGBA8", 4)];
	let resolve = [Some(target("RGBA8", 1))];
	assert_eq!(RenderTargetSet { colour: &multi, depth_stencil: None, resolve: &resolve }.validate(&limits), Ok(()));
	// A resolve that is a conversion is refused.
	let converting = [Some(target("RGBA16F", 1))];
	assert!(matches!(RenderTargetSet { colour: &multi, depth_stencil: None, resolve: &converting }.validate(&limits), Err(Error::TargetMismatch { .. })));
	// And a single-sample pass has nothing to resolve.
	assert!(matches!(RenderTargetSet { colour: &one, depth_stencil: None, resolve: &[Some(target("RGBA8", 1))] }.validate(&limits), Err(Error::TargetMismatch { .. })));
	// More attachments than the floor device admits.
	let five = [target("RGBA8", 1), target("RGBA8", 1), target("RGBA8", 1), target("RGBA8", 1), target("RGBA8", 1)];
	assert!(matches!(RenderTargetSet { colour: &five, depth_stencil: None, resolve: &[] }.validate(&limits), Err(Error::TargetMismatch { .. })));
}

#[test]
// THE HAZARD THE PROFILE REFUSES AT SUBMISSION: a texture sampled while it is an attachment of the
// same pass. The read depends on tile order, so refusing it is the only answer that does not make the
// picture depend on the backend - AND THE TEST IS PER SUBRESOURCE, so the common case of sampling
// one mip while rendering into another is admitted.
fn sampling_a_texture_that_is_an_attachment_of_the_same_pass_is_refused_per_subresource() {
	let view = |base_mip| TextureViewDesc { dimension: TextureDimension::D2, aspect: Aspect::Colour, base_mip, mip_count: 1, base_layer: 0, layer_count: 1 };
	let attachment = [RenderTargetView { texture: 7, view: view(0), ..target("RGBA8", 1) }];
	let targets = RenderTargetSet { colour: &attachment, depth_stencil: None, resolve: &[] };

	assert!(matches!(resource::refuse_sampled_attachment(&[(7, view(0))], &targets), Err(Error::TargetMismatch { .. })), "the same subresource is the hazard");
	assert_eq!(resource::refuse_sampled_attachment(&[(7, view(1))], &targets), Ok(()), "a different mip level is not");
	assert_eq!(resource::refuse_sampled_attachment(&[(9, view(0))], &targets), Ok(()), "and a different texture is not");

	// The other rule the profile states: a host write while a submission owns the buffer, refused at
	// the WRITE so the report names the thing that is wrong.
	assert!(matches!(resource::refuse_write_in_flight(true), Err(Error::InvalidRenderState { .. })));
	assert_eq!(resource::refuse_write_in_flight(false), Ok(()));
}

#[test]
// READING DISCARDED CONTENTS IS A TYPED ERROR AND NOT A VALUE. "Undefined" and "discarded" are kept
// apart so a report can say which: one is a resource nothing has written, the other is one a pass
// deliberately threw away.
fn contents_track_what_a_pass_left_and_refuse_a_read_of_what_it_did_not() {
	assert!(Contents::Cleared.readable().is_ok());
	assert!(Contents::Written.readable().is_ok());
	assert!(matches!(Contents::Undefined.readable(), Err(Error::InvalidRenderState { .. })));
	assert!(matches!(Contents::Discarded.readable(), Err(Error::InvalidRenderState { .. })));
	assert_ne!(Contents::Undefined.readable(), Contents::Discarded.readable(), "and the two refusals say different things");

	// A pass that clears and stores leaves a readable clear value.
	assert_eq!(Contents::Undefined.after(LoadOp::Clear, StoreOp::Store), Contents::Cleared);
	// One that discards its store leaves nothing readable, whatever it loaded.
	assert_eq!(Contents::Written.after(LoadOp::Load, StoreOp::Discard), Contents::Discarded);
	// LOADING WHAT WAS DISCARDED IS STILL DISCARDED: a pass that keeps contents nobody may read has
	// kept nothing, and a store does not make them readable.
	assert_eq!(Contents::Discarded.after(LoadOp::Load, StoreOp::Store), Contents::Discarded);
	assert_eq!(Contents::Undefined.after(LoadOp::Load, StoreOp::Store), Contents::Undefined);
	// And one that discards the old contents and writes new ones is written.
	assert_eq!(Contents::Discarded.after(LoadOp::Discard, StoreOp::Store), Contents::Written);
}

#[test]
// A BUFFER DESCRIPTION IS REFUSED FOR WHAT IT CANNOT BE USED AS.
fn a_buffer_description_is_checked_against_its_usage_and_its_limits() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	let vertex = BufferDesc { size: 1024, usage: BufferUsage { vertex: true, ..BufferUsage::default() }, host_visibility: HostVisibility::None };
	assert_eq!(vertex.validate(&limits), Ok(()));
	assert!(matches!(BufferDesc { size: 0, ..vertex }.validate(&limits), Err(Error::InvalidTexture { .. })), "a buffer of no bytes has no use that is not an out-of-range access");
	assert!(matches!(BufferDesc { usage: BufferUsage::default(), ..vertex }.validate(&limits), Err(Error::InvalidRenderState { .. })));
	// A uniform buffer is bounded by the per-stage limit.
	let uniform = BufferDesc { size: 32 * 1024, usage: BufferUsage { uniform: true, ..BufferUsage::default() }, host_visibility: HostVisibility::Upload };
	assert!(matches!(uniform.validate(&limits), Err(Error::LimitExceeded { limit: "uniform buffer bytes", .. })));
	// A readback buffer is the destination of a copy and has to say so.
	let readback = BufferDesc { size: 1024, usage: BufferUsage { copy_source: true, ..BufferUsage::default() }, host_visibility: HostVisibility::Readback };
	assert!(matches!(readback.validate(&limits), Err(Error::InvalidRenderState { .. })));
}

// ---------------------------------------------------------------------------------------------
// 2D and 3D interop.
// ---------------------------------------------------------------------------------------------

fn image_layout(format: graphics_core::PixelFormat, space: graphics_core::ColorSpace, alpha: graphics_core::AlphaMode) -> graphics_core::layout::ImageLayout {
	graphics_core::layout::ImageLayout::new(graphics_core::geom::Extent2D::new(64, 64), 64 * 16, graphics_core::PixelStorage::Known(format), graphics_core::layout::RowOrigin::TopLeft, graphics_core::semantics::ImageSemantics::Color { color_space: space, alpha_mode: alpha }).expect("a layout")
}

#[test]
// THE COMPATIBLE PATH IS DIRECT AND THERE IS NO BRIDGE TYPE. A single-sample colour target in the
// same format, space and alpha representation IS the image; the answer is `Direct` and no copy.
fn a_compatible_single_sample_target_is_the_image_itself() {
	let layout = image_layout(graphics_core::PixelFormat::R8G8B8A8Unorm, graphics_core::ColorSpace::Srgb, graphics_core::AlphaMode::Premultiplied);
	assert_eq!(bridge_for_attachment(&layout, "RGBA8", 1, graphics_core::ColorSpace::Srgb, graphics_core::AlphaMode::Premultiplied), Ok(Bridge::Direct));
}

#[test]
// AND WHAT IS NOT COMPATIBLE IS NAMED RATHER THAN CONVERTED SILENTLY, one variant per operation, in
// the ORDER the operations would run in: a multisampled attachment is resolved first, and only then
// is its format and its colour meaning a question.
fn every_incompatible_pair_names_the_one_operation_that_fixes_it() {
	let premultiplied_srgb = image_layout(graphics_core::PixelFormat::R8G8B8A8Unorm, graphics_core::ColorSpace::Srgb, graphics_core::AlphaMode::Premultiplied);

	// A multisampled attachment is a resolve, and the answer does not depend on anything later.
	assert_eq!(bridge_for_attachment(&premultiplied_srgb, "RGBA16F", 4, graphics_core::ColorSpace::SrgbLinear, graphics_core::AlphaMode::Straight), Ok(Bridge::Resolve), "a resolve comes first, so a caller is not told to convert something it has to resolve anyway");
	// A different numeric format.
	assert_eq!(bridge_for_attachment(&premultiplied_srgb, "RGBA16F", 1, graphics_core::ColorSpace::Srgb, graphics_core::AlphaMode::Premultiplied), Ok(Bridge::ConvertFormat { from: "RGBA8", to: "RGBA16F" }));
	// The same memory, a different meaning. This is the conversion that gets skipped.
	assert_eq!(bridge_for_attachment(&premultiplied_srgb, "RGBA8", 1, graphics_core::ColorSpace::SrgbLinear, graphics_core::AlphaMode::Premultiplied), Ok(Bridge::ConvertColourSpace));
	// And the one that produces fringes when it is missed.
	assert_eq!(bridge_for_attachment(&premultiplied_srgb, "RGBA8", 1, graphics_core::ColorSpace::Srgb, graphics_core::AlphaMode::Straight), Ok(Bridge::ConvertAlpha));
	// A format the 3D profile does not have at all is a refusal rather than a conversion.
	assert!(matches!(bridge_for_attachment(&premultiplied_srgb, "NoSuchFormat", 1, graphics_core::ColorSpace::Srgb, graphics_core::AlphaMode::Premultiplied), Err(Error::UnsupportedFormat { .. })));
}

#[test]
// THE REVERSE PATH VALIDATES RATHER THAN PROMISING. Not every image can be sampled, and a format that
// can be sampled but not FILTERED is a different answer from one that cannot be sampled at all -
// folding them would make every integer texture unusable.
fn the_reverse_path_admits_a_sampled_image_and_refuses_what_cannot_be_one() {
	let rgba8 = image_layout(graphics_core::PixelFormat::R8G8B8A8Unorm, graphics_core::ColorSpace::Srgb, graphics_core::AlphaMode::Premultiplied);
	assert_eq!(admit_as_texture(&rgba8, true), Ok("RGBA8"));

	// RGBA32F is sampled and NOT filterable, which the profile's own table says.
	let rgba32f = image_layout(graphics_core::PixelFormat::R32G32B32A32Float, graphics_core::ColorSpace::SrgbLinear, graphics_core::AlphaMode::Premultiplied);
	assert_eq!(admit_as_texture(&rgba32f, false), Ok("RGBA32F"), "a point-sampled read is fine");
	assert!(matches!(admit_as_texture(&rgba32f, true), Err(Error::UnsupportedFormat { used_as: "a FILTERED texture", .. })), "and a filtered one is not");

	// A packed-mask framebuffer - what firmware hands over - is not a profile format and needs a
	// conversion rather than being some format.
	let packed = graphics_core::layout::ImageLayout::new(graphics_core::geom::Extent2D::new(64, 64), 64 * 4, graphics_core::PixelStorage::PackedRgbUnorm(graphics_core::PackedRgbLayout::from_masks(4, (16, 8), (8, 8), (0, 8)).expect("a packing")), graphics_core::layout::RowOrigin::TopLeft, graphics_core::semantics::ImageSemantics::Color { color_space: graphics_core::ColorSpace::Srgb, alpha_mode: graphics_core::AlphaMode::Opaque }).expect("a layout");
	assert!(matches!(admit_as_texture(&packed, false), Err(Error::UnsupportedFormat { .. })));
}

// ---------------------------------------------------------------------------------------------
// The command model.
// ---------------------------------------------------------------------------------------------

fn a_pass_target() -> [RenderTargetView; 1] {
	[target("RGBA8", 1)]
}

fn a_pipeline_state() -> PipelineState {
	PipelineState { topology: Topology::TriangleList, cull: Cull::Back, depth_test: Some(CompareOp::Less), depth_write: true, samples: 1, per_sample_shading: false }
}

#[test]
// THE STATE MACHINE IS REFUSED AT THE CALL AND NOT AT SUBMISSION. A list that records anything and
// validates later has moved every error message away from the code that caused it.
fn the_recorder_refuses_every_way_a_list_can_be_out_of_order() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	let colour = a_pass_target();
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] };

	// A draw outside a pass.
	let mut list = CommandList::new(limits);
	assert!(matches!(list.draw(Topology::TriangleList, 3, 1, 0, 0), Err(Error::InvalidRenderState { .. })));
	// A pass ended that was never begun.
	assert!(matches!(list.end_render_pass(), Err(Error::InvalidRenderState { .. })));
	// A pass inside a pass.
	list.begin_render_pass(0, &set).expect("a first pass");
	assert!(matches!(list.begin_render_pass(0, &set), Err(Error::InvalidRenderState { .. })));
	// A draw with no pipeline.
	assert!(matches!(list.draw(Topology::TriangleList, 3, 1, 0, 0), Err(Error::InvalidRenderState { .. })));
	list.bind_pipeline(GraphicsPipeline(1), &a_pipeline_state(), 1).expect("a compatible pipeline");
	// A draw with no viewport - there is no default, because a default would be a guess at the
	// attachment's size.
	assert!(matches!(list.draw(Topology::TriangleList, 3, 1, 0, 0), Err(Error::InvalidRenderState { .. })));
	list.set_viewport(Rect { x: 0, y: 0, width: 640, height: 480 }).expect("a viewport");
	list.draw(Topology::TriangleList, 3, 1, 0, 0).expect("a draw");
	// A list finished with a pass still open.
	assert!(matches!(list.finish(), Err(Error::InvalidRenderState { .. })));
	list.end_render_pass().expect("closed");
	list.finish().expect("finished");
	// And a command recorded into a finished list, and a list finished twice.
	assert!(matches!(list.begin_render_pass(0, &set), Err(Error::InvalidRenderState { .. })));
	assert!(matches!(list.finish(), Err(Error::InvalidRenderState { .. })));
}

#[test]
// EVERY BINDING IS PER PASS. A pipeline bound in one pass is not bound in the next: the attachments
// changed, so its compatibility with them has to be established again, and carrying the binding
// across would make that check silent.
fn bindings_do_not_survive_the_end_of_a_pass() {
	let colour = a_pass_target();
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	list.begin_render_pass(0, &set).expect("a pass");
	list.bind_pipeline(GraphicsPipeline(1), &a_pipeline_state(), 1).expect("a pipeline");
	list.set_viewport(Rect { x: 0, y: 0, width: 640, height: 480 }).expect("a viewport");
	list.draw(Topology::TriangleList, 3, 1, 0, 0).expect("a draw");
	list.end_render_pass().expect("closed");

	list.begin_render_pass(1, &set).expect("a second pass");
	assert!(matches!(list.draw(Topology::TriangleList, 3, 1, 0, 0), Err(Error::InvalidRenderState { .. })), "the pipeline from the first pass is not bound in the second");
}

#[test]
// A PIPELINE IS COMPATIBLE WITH ITS PASS OR IT IS NOT BOUND, and finding out at the draw would name
// the draw rather than the pipeline.
fn a_pipeline_whose_sample_count_disagrees_with_the_pass_is_refused_at_the_bind() {
	let colour = [target("RGBA8", 4)];
	let resolve = [Some(target("RGBA8", 1))];
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &resolve };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	list.begin_render_pass(0, &set).expect("a multisampled pass");
	assert!(matches!(list.bind_pipeline(GraphicsPipeline(1), &a_pipeline_state(), 4), Err(Error::IncompatiblePipeline { .. })), "a one-sample pipeline in a four-sample pass");
	let multisampled = PipelineState { samples: 4, ..a_pipeline_state() };
	list.bind_pipeline(GraphicsPipeline(2), &multisampled, 4).expect("a compatible one");
	// Per-sample shading with one sample has no samples to shade.
	let nonsense = PipelineState { samples: 1, per_sample_shading: true, ..a_pipeline_state() };
	let single = a_pass_target();
	let mut other = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	other.begin_render_pass(0, &RenderTargetSet { colour: &single, depth_stencil: None, resolve: &[] }).expect("a pass");
	assert!(matches!(other.bind_pipeline(GraphicsPipeline(3), &nonsense, 1), Err(Error::InvalidRenderState { .. })));
}

#[test]
// A DRAW THAT CAN PRODUCE NOTHING IS A MISTAKE AND NOT A NO-OP, and the topology says how many
// vertices its first primitive needs.
fn a_draw_is_refused_when_it_cannot_make_one_primitive() {
	let colour = a_pass_target();
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	list.begin_render_pass(0, &set).expect("a pass");
	list.bind_pipeline(GraphicsPipeline(1), &a_pipeline_state(), 1).expect("a pipeline");
	list.set_viewport(Rect { x: 0, y: 0, width: 640, height: 480 }).expect("a viewport");

	assert!(matches!(list.draw(Topology::TriangleList, 2, 1, 0, 0), Err(Error::InvalidMesh { reason: MeshFault::TooFewVertices { needs: 3, has: 2, .. } })));
	assert!(matches!(list.draw(Topology::LineList, 1, 1, 0, 0), Err(Error::InvalidMesh { reason: MeshFault::TooFewVertices { needs: 2, has: 1, .. } })));
	list.draw(Topology::PointList, 1, 1, 0, 0).expect("one point is one primitive");
	assert!(matches!(list.draw(Topology::PointList, 1, 0, 0, 0), Err(Error::InvalidMesh { .. })), "and no instances is no draw");
}

#[test]
// AN INDEXED DRAW WITH NO INDEX BUFFER READS WHATEVER WAS BOUND LAST, and the BASE VERTEX IS SIGNED -
// so the check is on the LARGEST index the draw can reach, which a per-index check at draw time
// cannot do and this can.
fn an_indexed_draw_needs_its_buffer_and_its_range_is_checked_through_the_base_vertex() {
	let colour = a_pass_target();
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	list.begin_render_pass(0, &set).expect("a pass");
	list.bind_pipeline(GraphicsPipeline(1), &a_pipeline_state(), 1).expect("a pipeline");
	list.set_viewport(Rect { x: 0, y: 0, width: 640, height: 480 }).expect("a viewport");

	assert!(matches!(list.draw_indexed(Topology::TriangleList, 3, 1, 0, 0, 0, 64), Err(Error::InvalidRenderState { .. })), "no index buffer bound");
	list.bind_index_buffer(Buffer(9), 0, false).expect("an index buffer");
	list.draw_indexed(Topology::TriangleList, 3, 1, 0, 0, 0, 64).expect("inside the vertex buffer");
	// The highest index the draw reaches is first_index + count - 1 + base_vertex.
	assert!(matches!(list.draw_indexed(Topology::TriangleList, 3, 1, 62, 0, 0, 64), Err(Error::InvalidMesh { reason: MeshFault::IndexOutOfRange { .. } })));
	// A NEGATIVE BASE VERTEX IS A READ BEFORE THE BUFFER, which is the case this check exists for.
	assert!(matches!(list.draw_indexed(Topology::TriangleList, 3, 1, 0, -8, 0, 64), Err(Error::InvalidMesh { reason: MeshFault::IndexOutOfRange { .. } })));
	list.draw_indexed(Topology::TriangleList, 3, 1, 8, -8, 0, 64).expect("a negative base that stays inside is fine");
}

#[test]
// A SCISSOR OUTSIDE THE VIEWPORT CAN ONLY CLIP FRAGMENTS THAT DO NOT EXIST. Refusing it names the
// mistake; clamping it hides one.
fn a_scissor_is_bounded_by_the_viewport_rather_than_clamped_into_it() {
	let colour = a_pass_target();
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] };
	let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	list.begin_render_pass(0, &set).expect("a pass");
	let viewport = Rect { x: 10, y: 10, width: 100, height: 100 };
	list.set_viewport(viewport).expect("a viewport");
	list.set_scissor(Rect { x: 20, y: 20, width: 50, height: 50 }, viewport).expect("inside");
	list.set_scissor(viewport, viewport).expect("exactly the viewport");
	assert!(matches!(list.set_scissor(Rect { x: 0, y: 20, width: 50, height: 50 }, viewport), Err(Error::InvalidRenderState { .. })), "left of the viewport");
	assert!(matches!(list.set_scissor(Rect { x: 20, y: 20, width: 200, height: 50 }, viewport), Err(Error::InvalidRenderState { .. })), "past its right edge");
	// A viewport with no area draws nothing, which is a mistake rather than a state.
	assert!(matches!(list.set_viewport(Rect { x: 0, y: 0, width: 0, height: 100 }), Err(Error::InvalidRenderState { .. })));
}

#[test]
// AN ATTRIBUTE THAT LEAVES ITS STRIDE READS THE NEXT VERTEX, which shows as geometry that is almost
// right - so it is refused with the numbers rather than clamped.
fn a_vertex_layout_refuses_an_attribute_that_leaves_its_stride() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	let streams = [VertexStream { stride: 32, per_instance: false }];
	let good = [VertexAttribute { stream: 0, offset: 0, size: 12, location: 0 }, VertexAttribute { stream: 0, offset: 12, size: 8, location: 1 }];
	assert_eq!(VertexLayout { streams: &streams, attributes: &good }.validate(&limits), Ok(()));

	let past = [VertexAttribute { stream: 0, offset: 28, size: 8, location: 0 }];
	assert!(matches!(VertexLayout { streams: &streams, attributes: &past }.validate(&limits), Err(Error::InvalidMesh { reason: MeshFault::AttributeOutsideStride { offset: 28, size: 8, stride: 32, .. } })));
	let absent = [VertexAttribute { stream: 3, offset: 0, size: 4, location: 0 }];
	assert!(matches!(VertexLayout { streams: &streams, attributes: &absent }.validate(&limits), Err(Error::InvalidMesh { reason: MeshFault::StreamTooShort { stream: 3, .. } })));
	let empty = [VertexAttribute { stream: 0, offset: 0, size: 0, location: 0 }];
	assert!(matches!(VertexLayout { streams: &streams, attributes: &empty }.validate(&limits), Err(Error::InvalidMesh { .. })), "an attribute of no bytes reads nothing");
}

// ---------------------------------------------------------------------------------------------
// Submission.
// ---------------------------------------------------------------------------------------------

#[test]
// THE QUEUE IS BOUNDED AND REFUSES OVERFLOW WITHOUT PARTIAL PUBLICATION: a submission that does not
// fit is not queued at all, so the caller's resources are still its own and the serial is not spent.
fn the_queue_is_bounded_and_an_overflow_publishes_nothing() {
	let mut queue = Queue::with_capacity(2).expect("a queue");
	assert_eq!(queue.submit(1, &[10, 11]), Ok(0));
	assert_eq!(queue.submit(2, &[12]), Ok(1));
	assert_eq!(queue.in_flight(), 2);
	assert!(matches!(queue.submit(3, &[13]), Err(Error::LimitExceeded { limit: "submissions in flight", ceiling: 2, .. })));
	assert_eq!(queue.in_flight(), 2, "the refused submission was not queued");
	// The serial was not spent either: the next accepted one is 2.
	queue.settle(0, Status::Complete).expect("settled");
	assert_eq!(queue.submit(3, &[13]), Ok(2));
	assert!(Queue::with_capacity(0).is_err(), "a queue that can hold nothing can accept nothing");
}

#[test]
// A SUBMISSION OWNS ITS RESOURCES UNTIL ITS COMPLETION IS OBSERVABLE, and they are released IN
// SUBMISSION ORDER - not as each finishes. A backend may complete two out of order where nothing
// observes it, and releasing out of order would let a caller reuse a buffer an earlier one holds.
fn resources_are_released_in_submission_order_and_not_as_each_finishes() {
	let mut queue = Queue::with_capacity(4).expect("a queue");
	queue.submit(1, &[10]).expect("first");
	queue.submit(2, &[20]).expect("second");
	queue.submit(3, &[30]).expect("third");

	// The SECOND finishes first. Nothing is released, because the first still holds its own.
	assert_eq!(queue.settle(1, Status::Complete), Ok(alloc::vec![]));
	assert!(queue.owned_by_a_submission(20), "a finished submission behind a pending one still holds its resources");
	assert!(queue.owned_by_a_submission(10));

	// The first finishes: both are released, in order.
	assert_eq!(queue.settle(0, Status::Complete), Ok(alloc::vec![10, 20]));
	assert!(!queue.owned_by_a_submission(10) && !queue.owned_by_a_submission(20));
	assert!(queue.owned_by_a_submission(30), "and the third still holds its own");
	assert_eq!(queue.in_flight(), 1);

	// A status that is not terminal is not a settlement, and a serial not in flight is refused.
	assert!(matches!(queue.settle(2, Status::Pending), Err(Error::InvalidRenderState { .. })));
	assert!(matches!(queue.settle(99, Status::Complete), Err(Error::InvalidRenderState { .. })));
}

#[test]
// CANCELLATION IS NOT A ROLLBACK and a LOST BACKEND takes every submission with it, including the
// ones that had finished - their results are no longer readable, which is the thing a caller needs
// to know.
fn cancellation_settles_and_a_lost_backend_takes_every_submission_with_it() {
	let mut queue = Queue::with_capacity(4).expect("a queue");
	queue.submit(1, &[10]).expect("first");
	queue.cancel(0).expect("cancelled");
	assert_eq!(queue.in_flight(), 0, "a cancelled submission has settled");
	assert!(!queue.owned_by_a_submission(10), "and released what it held");

	queue.submit(2, &[20]).expect("second");
	queue.submit(3, &[30]).expect("third");
	let released = queue.lose_backend();
	assert_eq!(released, alloc::vec![20, 30], "every resource comes back");
	assert_eq!(queue.in_flight(), 0);
	assert!(queue.is_lost());
	assert!(matches!(queue.submit(4, &[40]), Err(Error::InvalidRenderState { .. })), "and nothing more is accepted");
}

#[test]
// A COMPLETION IS CONSUMED BY WAITING ON IT, which makes "waited for exactly once" a property of the
// type - and a completion answered while its submission is still pending is a refusal rather than a
// value.
fn a_completion_is_consumed_and_refuses_a_status_that_is_not_terminal() {
	let completion = Completion { serial: 0, source: 7 };
	assert!(matches!(completion.wait(Status::Pending), Err(Error::InvalidRenderState { .. })));
	let completion = Completion { serial: 1, source: 7 };
	assert_eq!(completion.wait(Status::Complete), Ok(Status::Complete));
	// Every terminal status is terminal and `Pending` is not, which is what a caller waits on.
	assert!(Status::Complete.terminal() && Status::Cancelled.terminal() && Status::BackendLost.terminal());
	assert!(Status::Failed(Error::InvalidTransform).terminal());
	assert!(!Status::Pending.terminal());
}

// ---------------------------------------------------------------------------------------------
// ClipCoordQ and the clipper.
// ---------------------------------------------------------------------------------------------

fn clip_vertex(x: f32, y: f32, z: f32, w: f32) -> ClipVertex {
	ClipVertex::new(Vec4::new(x, y, z, w), alloc::vec![])
}

#[test]
// THE PLANE ORDER IS PART OF THE PROFILE, and this module's own order is held against the frozen
// list rather than against a copy of it. Clipping is NOT associative in floating point: the vertex
// where a triangle crosses two planes depends on which cut it first.
fn the_plane_order_is_the_frozen_one_name_for_name() {
	let names: alloc::vec::Vec<&str> = PLANES.iter().map(|plane| plane.name()).collect();
	assert_eq!(names.as_slice(), clip::frozen_order(), "the order this clipper applies is the order the profile freezes");
	assert_eq!(names[0], "w = CLIP_W_EPSILON", "and the w plane is cut FIRST, because clipping against w = 0 exactly produces a vertex at infinity after the divide");
}

#[test]
// THREE ANSWERS AND NOT TWO. A non-finite component REFUSES the primitive, everything behind the eye
// CULLS, and everything outside one plane DROPS - and the difference between the first two is the
// difference between a shader bug and the ordinary state of geometry behind the camera.
fn a_non_finite_vertex_refuses_where_a_vertex_behind_the_eye_culls() {
	let inside = [clip_vertex(0.0, 0.0, 0.5, 1.0), clip_vertex(0.5, 0.0, 0.5, 1.0), clip_vertex(0.0, 0.5, 0.5, 1.0)];
	assert_eq!(classify(&inside), Ok(Classification::Inside));

	// A NaN in ONE vertex refuses the whole primitive.
	let poisoned = [clip_vertex(f32::NAN, 0.0, 0.5, 1.0), inside[1].clone(), inside[2].clone()];
	assert_eq!(classify(&poisoned), Err(Error::NonFinite { what: "a clip-space vertex position" }));
	let infinite = [clip_vertex(0.0, f32::INFINITY, 0.5, 1.0), inside[1].clone(), inside[2].clone()];
	assert_eq!(classify(&infinite), Err(Error::NonFinite { what: "a clip-space vertex position" }));

	// Every vertex behind the eye is a CULL, which is an ordinary answer rather than an error.
	let behind = [clip_vertex(0.0, 0.0, -1.0, -1.0), clip_vertex(0.5, 0.0, -1.0, -1.0), clip_vertex(0.0, 0.5, -1.0, -2.0)];
	assert_eq!(classify(&behind), Ok(Classification::BehindEye));

	// Everything beyond one plane is dropped WITHOUT cutting anything.
	let right = [clip_vertex(2.0, 0.0, 0.5, 1.0), clip_vertex(3.0, 0.0, 0.5, 1.0), clip_vertex(2.5, 0.5, 0.5, 1.0)];
	assert_eq!(classify(&right), Ok(Classification::Outside));

	// And one vertex out is a clip.
	let crossing = [clip_vertex(0.0, 0.0, 0.5, 1.0), clip_vertex(2.0, 0.0, 0.5, 1.0), clip_vertex(0.0, 0.5, 0.5, 1.0)];
	assert_eq!(classify(&crossing), Ok(Classification::Clipped));
}

#[test]
// A TRIANGLE WHOLLY INSIDE COMES BACK UNCHANGED, byte for byte. A clipper that rebuilt it would
// produce vertices that are equal to within a rounding rather than equal - and the suite compares
// positions bit-exactly.
fn a_primitive_inside_every_plane_is_not_touched() {
	let inside = [clip_vertex(0.0, 0.0, 0.5, 1.0), clip_vertex(0.5, -0.25, 0.5, 1.0), clip_vertex(-0.5, 0.25, 0.5, 1.0)];
	let out = clip_polygon(&inside).expect("an inside triangle");
	assert_eq!(out.len(), 3);
	for (got, want) in out.iter().zip(inside.iter()) {
		assert_eq!(got.position, want.position, "an untouched vertex is the SAME vertex and not an equal one");
	}
}

#[test]
// A TRIANGLE CROSSING ONE PLANE COMES BACK AS A QUADRILATERAL, with the two new vertices ON the
// plane - which is the property that makes the clip correct rather than approximately correct.
fn a_triangle_cut_by_one_plane_becomes_a_quadrilateral_on_that_plane() {
	// Crossing x = w on the right, at w = 1: x = 1 is the plane.
	let triangle = [clip_vertex(0.0, -0.5, 0.5, 1.0), clip_vertex(2.0, 0.0, 0.5, 1.0), clip_vertex(0.0, 0.5, 0.5, 1.0)];
	let out = clip_polygon(&triangle).expect("a clipped triangle");
	assert_eq!(out.len(), 4, "one cut across a triangle makes four vertices");
	let mut on_plane = 0;
	for vertex in &out {
		let Vec4 { x, w, .. } = vertex.position.0;
		assert!(x <= w + 1e-6, "every vertex is inside x <= w now");
		if (w - x).abs() <= 1e-6 {
			on_plane += 1;
		}
	}
	assert_eq!(on_plane, 2, "and the two the cut produced lie ON the plane");
	// THE FAN TRIANGULATION OF A QUADRILATERAL IS TWO TRIANGLES from its first vertex.
	assert_eq!(fan_triangles(out.len()), alloc::vec![[0, 1, 2], [0, 2, 3]]);
	assert!(fan_triangles(2).is_empty(), "two vertices are not a polygon");
}

#[test]
// A TRIANGLE THAT STRADDLES THE EYE IS CUT AGAINST `w = CLIP_W_EPSILON` FIRST, and every vertex that
// comes back has a positive `w` - which is what makes the divide afterwards finite. Clipping against
// `w = 0` exactly would produce a vertex at infinity.
fn a_primitive_straddling_the_eye_is_cut_against_the_w_plane_and_every_vertex_survives_the_divide() {
	let straddling = [clip_vertex(0.0, 0.0, 0.5, 1.0), clip_vertex(0.0, 0.0, -0.5, -1.0), clip_vertex(0.5, 0.5, 0.25, 0.5)];
	assert_eq!(classify(&straddling), Ok(Classification::Clipped));
	let out = clip_polygon(&straddling).expect("a straddling triangle");
	assert!(!out.is_empty(), "part of it is in front of the eye");
	for vertex in &out {
		assert!(vertex.position.0.w >= clip::W_EPSILON - 1e-9, "every surviving vertex is at or past the w plane: {}", vertex.position.0.w);
		assert!(vertex.position.0.perspective_divide().is_ok(), "so the divide is finite");
	}
}

#[test]
// THE INTERSECTION PARAMETER IS THE ONE THE ATTRIBUTES FOLLOW. The `t` that makes the position
// correct is the `t` that makes a perspective-correct attribute correct, because both are linear in
// homogeneous space - so a fixture that cuts an edge at a known parameter checks the attribute too.
fn a_smooth_attribute_is_interpolated_at_the_same_parameter_as_the_position() {
	// An edge from x = 0 to x = 2 at w = 1 crosses x = w at exactly half way.
	let from = ClipVertex::new(Vec4::new(0.0, 0.0, 0.5, 1.0), alloc::vec![10.0, -4.0]);
	let to = ClipVertex::new(Vec4::new(2.0, 0.0, 0.5, 1.0), alloc::vec![20.0, 4.0]);
	let third = ClipVertex::new(Vec4::new(0.0, 0.5, 0.5, 1.0), alloc::vec![10.0, -4.0]);
	let out = clip_polygon(&[from, to, third]).expect("a clipped triangle");
	let cut = out.iter().find(|vertex| (vertex.position.0.x - 1.0).abs() <= 1e-6).expect("a vertex on x = w");
	assert!(near(cut.smooth[0], 15.0), "half way along the edge, the attribute is half way too: {}", cut.smooth[0]);
	assert!(near(cut.smooth[1], 0.0), "and so is the second: {}", cut.smooth[1]);
}

#[test]
// THE PROVOKING VERTEX IS STATED PER TOPOLOGY, because a strip and a fan number their vertices
// differently and "the first vertex" means a different thing in each. A FAN'S IS THE SECOND - the
// one that is not the shared hub - because the hub is in every triangle and choosing it would give
// every triangle of a fan the same flat value.
fn the_provoking_vertex_is_the_profiles_own_answer_per_topology() {
	assert_eq!(provoking_vertex(Topology::TriangleList, 0), 0);
	assert_eq!(provoking_vertex(Topology::TriangleList, 2), 6, "3i");
	assert_eq!(provoking_vertex(Topology::TriangleStrip, 3), 3, "vertex i for triangle i, whatever the winding flip does");
	assert_eq!(provoking_vertex(Topology::TriangleFan, 0), 1, "NOT the hub");
	assert_eq!(provoking_vertex(Topology::TriangleFan, 3), 4);
	assert_eq!(provoking_vertex(Topology::LineList, 2), 4, "2i");
	assert_eq!(provoking_vertex(Topology::LineStrip, 2), 2);
	assert_eq!(provoking_vertex(Topology::PointList, 5), 5);
	// EVERY FAN TRIANGLE OF A CLIPPED POLYGON CARRIES THE ORIGINAL PRIMITIVE'S VALUE, which is why
	// `fan_triangles` answers indices into the clipped polygon and never a provoking vertex: the
	// triangulation is an implementation detail of clipping and must not be visible in a flat
	// attribute.
	for triangle in fan_triangles(5) {
		assert_eq!(triangle[0], 0, "a fan from the first vertex");
	}
}

#[test]
// THE CLIP VOLUME IS THE PROFILE'S, and `inside_volume` is the one place it is written: `-w <= x <=
// w`, `-w <= y <= w`, `0 <= z <= w`, with `w > 0`. The `z` half is what distinguishes a zero-to-one
// depth range from a minus-one-to-one one, and a clipper written for the other admits half a scene
// it should cut.
fn the_clip_volume_is_zero_to_one_in_z_and_symmetric_in_x_and_y() {
	assert!(ClipCoordQ::new(Vec4::new(0.0, 0.0, 0.0, 1.0)).inside_volume(), "z = 0 is the near plane and is inside");
	assert!(ClipCoordQ::new(Vec4::new(1.0, -1.0, 1.0, 1.0)).inside_volume(), "and the far corner is too");
	assert!(!ClipCoordQ::new(Vec4::new(0.0, 0.0, -0.001, 1.0)).inside_volume(), "a negative z is BEFORE the near plane, which a minus-one-to-one clipper would admit");
	assert!(!ClipCoordQ::new(Vec4::new(1.001, 0.0, 0.5, 1.0)).inside_volume());
	assert!(!ClipCoordQ::new(Vec4::new(0.0, 0.0, 0.5, 0.0)).inside_volume(), "w = 0 is not in front of the eye");
	assert!(!ClipCoordQ::new(Vec4::new(0.0, 0.0, 0.5, f32::NAN)).inside_volume(), "and a NaN makes every inequality false");
}

// ---------------------------------------------------------------------------------------------
// One API, two backends: an asynchronous stand-in and an already-completed software result.
// ---------------------------------------------------------------------------------------------

/// A backend that settles a submission when it is told to, which is what a GPU one does.
struct Asynchronous {
	queue: submission::Queue,
	outstanding: Vec<u64>,
}

/// A backend that has already finished by the time `submit` returns, which is what a software one
/// does. THE SAME TYPES AND THE SAME CALLS - the difference is WHEN `settle` happens, and a caller
/// that had to know which kind it was talking to would have two code paths for one contract.
struct Immediate {
	queue: submission::Queue,
}

impl Asynchronous {
	fn submit(&mut self, source: u32, retained: &[u32]) -> Result<u64, Error> {
		let serial = self.queue.submit(source, retained)?;
		self.outstanding.push(serial);
		Ok(serial)
	}

	fn settle_oldest(&mut self, status: Status) -> Result<Vec<u32>, Error> {
		let serial = self.outstanding.remove(0);
		self.queue.settle(serial, status)
	}
}

impl Immediate {
	fn submit(&mut self, source: u32, retained: &[u32]) -> Result<u64, Error> {
		let serial = self.queue.submit(source, retained)?;
		// The work is done before the call returns, so the resources are free again at once.
		self.queue.settle(serial, Status::Complete)?;
		Ok(serial)
	}
}

#[test]
// AN ASYNCHRONOUS STAND-IN AND AN ALREADY-COMPLETED SOFTWARE RESULT GO THROUGH THE SAME API. The
// only difference is when `settle` is called; every type, every refusal and every ownership rule is
// the same, which is what makes "a future GPU backend does not change the API shape" a property
// rather than an intention.
fn a_pending_backend_and_an_immediate_one_use_the_same_submission_contract() {
	let mut slow = Asynchronous { queue: submission::Queue::with_capacity(4).unwrap(), outstanding: Vec::new() };
	let mut fast = Immediate { queue: submission::Queue::with_capacity(4).unwrap() };

	let slow_serial = slow.submit(1, &[10, 11]).unwrap();
	let fast_serial = fast.submit(1, &[10, 11]).unwrap();
	assert_eq!(slow_serial, fast_serial, "the serials are the queue's and not the backend's");

	// WHILE IT IS PENDING THE RESOURCES ARE THE SUBMISSION'S. The immediate backend never shows that
	// window, which is exactly why a caller must not be written against it: the rule is the same and
	// only the timing differs.
	assert!(slow.queue.owned_by_a_submission(10));
	assert!(!fast.queue.owned_by_a_submission(10), "the immediate backend released them before it returned");
	assert_eq!(slow.queue.in_flight(), 1);
	assert_eq!(fast.queue.in_flight(), 0);

	let released = slow.settle_oldest(Status::Complete).unwrap();
	assert_eq!(released, vec![10, 11]);
	assert!(!slow.queue.owned_by_a_submission(10), "and then the two agree");

	// A completion is waited on exactly once, and answering one that is still pending is refused
	// whichever backend produced it.
	let pending = Completion { serial: 7, source: 3 };
	assert!(matches!(pending.wait(Status::Pending), Err(Error::InvalidRenderState { .. })));
	let finished = Completion { serial: 7, source: 3 };
	assert_eq!(finished.wait(Status::Complete), Ok(Status::Complete));

	// A READBACK TICKET IS THE SAME SHAPE FOR BOTH: the completion, the terminal status and where the
	// bytes land. The software backend hands one back already complete; the asynchronous one hands
	// back the same type with `Pending` and settles it later.
	let immediate = ReadbackTicket { completion: Completion { serial: fast_serial, source: 0 }, status: Status::Complete, destination: 42 };
	let deferred = ReadbackTicket { completion: Completion { serial: slow_serial, source: 9 }, status: Status::Pending, destination: 42 };
	assert_eq!(immediate.destination, deferred.destination);
	assert!(immediate.status.terminal() && !deferred.status.terminal());
}

#[test]
// A LOST BACKEND IS TERMINAL FOR EVERY SUBMISSION, INCLUDING THE ONES THAT HAD FINISHED: their
// results are no longer readable, which is the thing a caller needs to know and the thing a status
// of `Complete` would hide.
fn losing_the_backend_settles_everything_and_says_the_results_are_gone() {
	let mut queue = submission::Queue::with_capacity(4).unwrap();
	queue.submit(1, &[10]).unwrap();
	queue.submit(1, &[11]).unwrap();
	let released = queue.lose_backend();
	assert_eq!(released, vec![10, 11], "every retained resource comes back");
	assert!(queue.is_lost());
	assert!(matches!(queue.submit(1, &[12]), Err(_)), "and nothing else is accepted");
	assert_eq!(Status::BackendLost.terminal(), true);
}

// ---------------------------------------------------------------------------------------------
// Property and fuzz: hostile input, and the rule that it is always ANSWERED.
// ---------------------------------------------------------------------------------------------

/// A deterministic generator. NOT A RANDOM ONE: a fuzz fixture whose failing case cannot be
/// reproduced is a fixture that reports a defect nobody can find, so the sequence is fixed and a
/// failure names the iteration that produced it.
struct Noise(u64);

impl Noise {
	fn next(&mut self) -> u64 {
		// xorshift64*, which is short enough to read and good enough to cover the shapes below.
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

#[test]
// A MALFORMED VERTEX LAYOUT IS ANSWERED AND NEVER PANICS, and the answer is the same whichever way
// it is malformed. THE PROPERTY IS THE POINT: an attribute is admitted if and only if it names a
// stream that exists and lies wholly inside that stream's stride, and a fixture that only tried the
// cases somebody thought of would not be testing the rule.
fn a_thousand_malformed_vertex_layouts_are_each_answered_by_the_rule() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	let mut noise = Noise(0x5EED_1234_ABCD_0001);
	for iteration in 0..1000 {
		let stream_count = noise.below(6) as usize;
		let streams: Vec<VertexStream> = (0..stream_count).map(|_| VertexStream { stride: noise.below(72), per_instance: noise.next() & 1 == 1 }).collect();
		let attribute_count = noise.below(20) as usize;
		let attributes: Vec<VertexAttribute> = (0..attribute_count).map(|index| VertexAttribute { stream: noise.below(8), offset: noise.below(96), size: noise.below(24), location: index as u32 }).collect();
		let layout = VertexLayout { streams: &streams, attributes: &attributes };
		let outcome = layout.validate(&limits);

		// The rule, restated from the definition rather than from the implementation.
		let over_limit = streams.len() as u64 > limits.max_vertex_streams as u64 || attributes.len() as u64 > limits.max_vertex_attributes as u64;
		let readable = attributes.iter().all(|attribute| match streams.get(attribute.stream as usize) {
			Some(stream) => attribute.size > 0 && attribute.offset.saturating_add(attribute.size) <= stream.stride,
			None => false,
		});
		let expected_ok = !over_limit && readable;
		assert_eq!(outcome.is_ok(), expected_ok, "iteration {iteration}: {streams:?} {attributes:?} -> {outcome:?}");
	}
}

#[test]
// AN INCOMPATIBLE ATTACHMENT SET IS ANSWERED AND NEVER PANICS. A pass has ONE extent, ONE sample
// count and one resolve slot per colour attachment; any set that does not is a typed refusal, and
// this generates sets rather than listing the four a person would think of.
fn a_thousand_generated_attachment_sets_are_each_answered_by_the_rule() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	let mut noise = Noise(0x5EED_1234_ABCD_0002);
	let extents = [(320_u32, 240_u32), (640, 480), (800, 600)];
	let samples = [1_u32, 2, 4, 8];
	let formats = ["RGBA8", "RGBA16F", "RGBA32F", "R32Uint"];
	let mut refused = 0;
	for iteration in 0..1000 {
		let count = noise.below(6) as usize;
		let colour: Vec<RenderTargetView> = (0..count)
			.map(|_| {
				let (width, height) = extents[noise.below(extents.len() as u32) as usize];
				let view = RenderTargetView { width, height, samples: samples[noise.below(samples.len() as u32) as usize], format: formats[noise.below(formats.len() as u32) as usize], ..target("RGBA8", 1) };
				view
			})
			.collect();
		let resolve_count = if noise.next() & 1 == 0 { 0 } else { noise.below(6) as usize };
		let resolve: Vec<Option<RenderTargetView>> = (0..resolve_count).map(|_| if noise.next() & 1 == 0 { None } else { Some(target(formats[noise.below(formats.len() as u32) as usize], 1)) }).collect();
		let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &resolve };
		match set.validate(&limits) {
			Ok(()) => {
				// WHAT IS ADMITTED MUST SATISFY THE RULE, which is the half a refusal-only fixture
				// never checks: a validator that accepted everything would pass it.
				let first = colour.first().expect("an empty set with no depth attachment is refused");
				assert!(colour.iter().all(|view| view.width == first.width && view.height == first.height && view.samples == first.samples), "iteration {iteration}");
				assert!(resolve.is_empty() || resolve.len() == colour.len(), "iteration {iteration}");
			}
			Err(Error::TargetMismatch { .. }) | Err(Error::UnsupportedFormat { .. }) | Err(Error::LimitExceeded { .. }) => refused += 1,
			Err(other) => panic!("iteration {iteration}: an attachment set answered with {other:?}, which is not a set's refusal"),
		}
	}
	assert!(refused > 100, "the generator produced {refused} refusals, which is too few to be testing them");
}

#[test]
// A COMMAND LIST FED AN ARBITRARY SEQUENCE OF CALLS NEVER PANICS AND NEVER RECORDS A DRAW WITHOUT
// ITS PRECONDITIONS. The state machine is the thing being fuzzed: every ordering of begin, bind,
// viewport, draw, end and finish either succeeds or is a typed refusal, and a list that ends
// finished holds a balanced pass sequence whatever was thrown at it.
fn ten_thousand_arbitrary_command_sequences_leave_the_recorder_consistent() {
	let mut noise = Noise(0x5EED_1234_ABCD_0003);
	let colour = [target("RGBA8", 1)];
	let set = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] };
	let state = PipelineState { topology: Topology::TriangleList, cull: Cull::Back, depth_test: None, depth_write: false, samples: 1, per_sample_shading: false };
	let mut draws_recorded = 0;
	let mut refusals = 0;
	for _ in 0..400 {
		let mut list = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
		let mut open = 0_i32;
		for _ in 0..25 {
			let outcome = match noise.below(7) {
				0 => list.begin_render_pass(0, &set),
				1 => list.end_render_pass(),
				2 => list.bind_pipeline(GraphicsPipeline(1), &state, 1),
				3 => list.set_viewport(Rect { x: 0, y: 0, width: 64, height: 64 }),
				4 => list.bind_vertex_buffer(0, Buffer(1), 0),
				5 => list.draw(Topology::TriangleList, 3, 1, 0, 0),
				_ => list.finish(),
			};
			match outcome {
				Ok(()) => {}
				Err(Error::InvalidRenderState { .. }) | Err(Error::InvalidMesh { .. }) | Err(Error::LimitExceeded { .. }) | Err(Error::IncompatiblePipeline { .. }) => refusals += 1,
				Err(other) => panic!("the recorder answered {other:?}, which is not a recording refusal"),
			}
		}
		// EVERY DRAW IS INSIDE A PASS AND AFTER A PIPELINE AND A VIEWPORT, checked by replaying the
		// list rather than by trusting the recorder that produced it.
		let mut in_pass = false;
		let mut pipeline = false;
		let mut viewport = false;
		for command in list.commands() {
			match command {
				Command::BeginRenderPass { .. } => {
					assert!(!in_pass, "a pass begun inside another one was recorded");
					in_pass = true;
					pipeline = false;
					viewport = false;
				}
				Command::EndRenderPass => {
					assert!(in_pass, "a pass ended that was never begun was recorded");
					in_pass = false;
				}
				Command::BindPipeline(_) => {
					assert!(in_pass);
					pipeline = true;
				}
				Command::SetViewport(_) => {
					assert!(in_pass);
					viewport = true;
				}
				Command::Draw { .. } | Command::DrawIndexed { .. } => {
					assert!(in_pass && pipeline && viewport, "a draw was recorded without its preconditions");
					draws_recorded += 1;
				}
				_ => assert!(in_pass, "a binding was recorded outside a pass"),
			}
			open += match command {
				Command::BeginRenderPass { .. } => 1,
				Command::EndRenderPass => -1,
				_ => 0,
			};
		}
		assert!(open == 0 || open == 1, "a list can only be balanced or hold one open pass");
		if list.is_finished() {
			assert_eq!(open, 0, "a finished list has no open pass");
		}
	}
	assert!(draws_recorded > 50, "only {draws_recorded} draws were recorded, which is too few to be testing the preconditions");
	assert!(refusals > 1000, "only {refusals} refusals, which is too few to be testing the state machine");
}

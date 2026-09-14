//! COMPOSITING: the twelve Porter-Duff operators, additive `Plus`, and every blend mode.
//!
//! ONE DRAWING TELLS ALL THIRTEEN OPERATORS APART. An opaque backdrop on the left, a partly
//! transparent source on the right, overlapping in the middle - and three probes: where only the
//! backdrop is, where both are, and where only the source is. Every operator has its own signature
//! across those three, because that is exactly what the pair of factors decides.
//!
//! THE SOURCE ALPHA IS SIX TENTHS AND NOT A HALF, deliberately. At a half, `as` and `1 - as` are the
//! same number, and four pairs of operators become indistinguishable - which is how a suite passes an
//! implementation that has `Xor` and `DestinationAtop` the wrong way round.
//!
//! AND EVERY BLEND MODE IS ITS EQUATION EVALUATED AT ONE POINT, written out in the scene. The
//! backdrop and source are chosen with a different value in each channel, so a mode that is right in
//! one channel and wrong in another cannot pass.

use crate::Outcome;
use crate::harness::{Frame, draw, near, rect_path, solid};
use graphics_core::geom::RectF;
use render2d::blend::{BlendMode, Operator};
use render2d::path::FillRule;

/// What a probe should show: an alpha, and a straight colour when there is one to see.
struct Expect {
	alpha: f32,
	colour: Option<[f32; 3]>,
}

const NOTHING: Expect = Expect { alpha: 0.0, colour: None };

/// An opaque blue backdrop on the left, a six-tenths red source on the right, drawn with `operator`.
fn composed(operator: Operator) -> Result<Frame, crate::Trouble> {
	draw(32, 8, |canvas| {
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 20.0, 8.0)), solid(0.0, 0.0, 1.0, 1.0), FillRule::NonZero)?;
		canvas.set_operator(operator);
		canvas.fill_path(rect_path(RectF::new(12.0, 0.0, 20.0, 8.0)), solid(1.0, 0.0, 0.0, 0.6), FillRule::NonZero)
	})
}

fn probe(frame: &Frame, x: u32, expect: &Expect, where_: &str) -> Outcome {
	let pixel = frame.pixel(x, 4);
	require!(near(pixel[3], expect.alpha), "{where_}: the alpha should be {} and is {pixel:?}", expect.alpha);
	if let Some(colour) = expect.colour {
		require!(near(pixel[0], colour[0]) && near(pixel[1], colour[1]) && near(pixel[2], colour[2]), "{where_}: the colour should be {colour:?} and is {pixel:?}");
	}
	Ok(())
}

/// Check one operator at the three probes. THE BACKDROP-ONLY PROBE IS ALWAYS THE BACKDROP: an
/// operator applies where the source SHAPE is, and a `Clear` that wiped the whole surface would be a
/// drawing API with no bounded drawing in it.
fn operator_scene(operator: Operator, middle: Expect, source_only: Expect) -> Outcome {
	let frame = composed(operator)?;
	probe(&frame, 4, &Expect { alpha: 1.0, colour: Some([0.0, 0.0, 1.0]) }, "outside the source shape")?;
	probe(&frame, 16, &middle, "where both are")?;
	probe(&frame, 28, &source_only, "where only the source is")
}

// @covers: CompositeClear
/// `Clear` writes transparency wherever the source is, which is what an eraser is.
pub fn composite_clear() -> Outcome {
	operator_scene(Operator::Clear, NOTHING, NOTHING)
}

// @covers: CompositeSource
/// `Source` REPLACES: the backdrop is gone, alpha and all, so the result is the source's own six
/// tenths rather than an opaque blend.
pub fn composite_source() -> Outcome {
	operator_scene(Operator::Src, Expect { alpha: 0.6, colour: Some([1.0, 0.0, 0.0]) }, Expect { alpha: 0.6, colour: Some([1.0, 0.0, 0.0]) })
}

// @covers: CompositeDestination
/// `Destination` keeps what was there and draws nothing - which is not a no-op, because where there
/// was no backdrop there is still nothing.
pub fn composite_destination() -> Outcome {
	operator_scene(Operator::Dst, Expect { alpha: 1.0, colour: Some([0.0, 0.0, 1.0]) }, NOTHING)
}

// @covers: CompositeSourceOver
/// `SourceOver` is the ordinary one: `0.6` of red over blue is `co = 0.6*red + 0.4*blue` at full
/// alpha, and where there is no backdrop it is the source at its own alpha.
pub fn composite_source_over() -> Outcome {
	operator_scene(Operator::SrcOver, Expect { alpha: 1.0, colour: Some([0.6, 0.0, 0.4]) }, Expect { alpha: 0.6, colour: Some([1.0, 0.0, 0.0]) })
}

// @covers: CompositeDestinationOver
/// `DestinationOver` puts the new drawing UNDER the old one, so an opaque backdrop hides it entirely
/// and it shows only where there was nothing.
pub fn composite_destination_over() -> Outcome {
	operator_scene(Operator::DstOver, Expect { alpha: 1.0, colour: Some([0.0, 0.0, 1.0]) }, Expect { alpha: 0.6, colour: Some([1.0, 0.0, 0.0]) })
}

// @covers: CompositeSourceIn
/// `SourceIn` keeps the source only where the backdrop was, and takes the backdrop's alpha with it.
pub fn composite_source_in() -> Outcome {
	operator_scene(Operator::SrcIn, Expect { alpha: 0.6, colour: Some([1.0, 0.0, 0.0]) }, NOTHING)
}

// @covers: CompositeDestinationIn
/// `DestinationIn` keeps the BACKDROP where the source was, at the source's alpha - which is how a
/// mask is applied to something already drawn.
pub fn composite_destination_in() -> Outcome {
	operator_scene(Operator::DstIn, Expect { alpha: 0.6, colour: Some([0.0, 0.0, 1.0]) }, NOTHING)
}

// @covers: CompositeSourceOut
/// `SourceOut` keeps the source only where the backdrop was NOT.
pub fn composite_source_out() -> Outcome {
	operator_scene(Operator::SrcOut, NOTHING, Expect { alpha: 0.6, colour: Some([1.0, 0.0, 0.0]) })
}

// @covers: CompositeDestinationOut
/// `DestinationOut` keeps the backdrop where the source was not - it is the eraser whose shape is the
/// source, and six tenths of a source erases six tenths.
pub fn composite_destination_out() -> Outcome {
	operator_scene(Operator::DstOut, Expect { alpha: 0.4, colour: Some([0.0, 0.0, 1.0]) }, NOTHING)
}

// @covers: CompositeSourceAtop
/// `SourceAtop` draws the source over the backdrop but clipped to it: the same colour as `SourceOver`
/// where the backdrop is, and nothing at all where it is not.
pub fn composite_source_atop() -> Outcome {
	operator_scene(Operator::SrcAtop, Expect { alpha: 1.0, colour: Some([0.6, 0.0, 0.4]) }, NOTHING)
}

// @covers: CompositeDestinationAtop
/// `DestinationAtop` keeps the backdrop where the source is, at the source's alpha, and the source
/// where the backdrop is not.
pub fn composite_destination_atop() -> Outcome {
	operator_scene(Operator::DstAtop, Expect { alpha: 0.6, colour: Some([0.0, 0.0, 1.0]) }, Expect { alpha: 0.6, colour: Some([1.0, 0.0, 0.0]) })
}

// @covers: CompositeXor
/// `Xor` keeps each where the other is not, which leaves `1 - as` of the backdrop in the overlap -
/// four tenths here, and NOT the six tenths `DestinationAtop` leaves.
pub fn composite_xor() -> Outcome {
	operator_scene(Operator::Xor, Expect { alpha: 0.4, colour: Some([0.0, 0.0, 1.0]) }, Expect { alpha: 0.6, colour: Some([1.0, 0.0, 0.0]) })
}

// @covers: CompositePlus
/// `Plus` ADDS, which is not one of the twelve: the alphas sum past one and are clamped, and the
/// colour is the sum of the premultiplied values. It is what a glow, a lightening brush and an
/// additive particle are.
pub fn composite_plus() -> Outcome {
	operator_scene(Operator::Plus, Expect { alpha: 1.0, colour: Some([0.6, 0.0, 1.0]) }, Expect { alpha: 0.6, colour: Some([1.0, 0.0, 0.0]) })
}

/// A blend of an opaque source over an opaque backdrop, both with a DIFFERENT value in each channel -
/// so a mode that is right in one channel and wrong in another cannot pass.
///
/// backdrop `(0.8, 0.4, 0.2)`, source `(0.4, 0.6, 0.5)`.
fn blended(mode: BlendMode) -> Result<Frame, crate::Trouble> {
	draw(16, 8, |canvas| {
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 8.0)), solid(0.8, 0.4, 0.2, 1.0), FillRule::NonZero)?;
		canvas.set_blend_mode(mode);
		canvas.fill_path(rect_path(RectF::new(0.0, 0.0, 16.0, 8.0)), solid(0.4, 0.6, 0.5, 1.0), FillRule::NonZero)
	})
}

fn blend_scene(mode: BlendMode, expected: [f32; 3]) -> Outcome {
	let frame = blended(mode)?;
	let pixel = frame.pixel(8, 4);
	require!(pixel[3] == 255, "an opaque source over an opaque backdrop is opaque: {pixel:?}");
	require!(near(pixel[0], expected[0]) && near(pixel[1], expected[1]) && near(pixel[2], expected[2]), "the equation gives {expected:?} and the pixel is {pixel:?}");
	Ok(())
}

// @covers: BlendMultiply
/// `cb * cs`.
pub fn blend_multiply() -> Outcome {
	blend_scene(BlendMode::Multiply, [0.32, 0.24, 0.10])
}

// @covers: BlendScreen
/// `cb + cs - cb * cs`.
pub fn blend_screen() -> Outcome {
	blend_scene(BlendMode::Screen, [0.88, 0.76, 0.60])
}

// @covers: BlendOverlay
/// `HardLight` with the operands swapped: the BACKDROP decides which branch each channel takes.
pub fn blend_overlay() -> Outcome {
	blend_scene(BlendMode::Overlay, [0.76, 0.48, 0.20])
}

// @covers: BlendDarken
/// `min(cb, cs)`, per channel.
pub fn blend_darken() -> Outcome {
	blend_scene(BlendMode::Darken, [0.4, 0.4, 0.2])
}

// @covers: BlendLighten
/// `max(cb, cs)`, per channel.
pub fn blend_lighten() -> Outcome {
	blend_scene(BlendMode::Lighten, [0.8, 0.6, 0.5])
}

// @covers: BlendColorDodge
/// `cb / (1 - cs)`, clamped at one - so a channel whose source is six tenths and backdrop four tenths
/// reaches exactly one, and the red channel saturates.
pub fn blend_color_dodge() -> Outcome {
	blend_scene(BlendMode::ColorDodge, [1.0, 1.0, 0.4])
}

// @covers: BlendColorBurn
/// `1 - min(1, (1 - cb) / cs)`, which is the dodge of the complement.
pub fn blend_color_burn() -> Outcome {
	blend_scene(BlendMode::ColorBurn, [0.5, 0.0, 0.0])
}

// @covers: BlendHardLight
/// Multiply or screen depending on the SOURCE, which is the branch `Overlay` takes on the backdrop.
pub fn blend_hard_light() -> Outcome {
	blend_scene(BlendMode::HardLight, [0.64, 0.52, 0.20])
}

// @covers: BlendSoftLight
/// The W3C soft light, with its own curve `D(cb)` below a quarter - the one place where three blend
/// modes that otherwise agree come apart.
pub fn blend_soft_light() -> Outcome {
	blend_scene(BlendMode::SoftLight, [0.768, 0.4465, 0.2])
}

// @covers: BlendDifference
/// `|cb - cs|`.
pub fn blend_difference() -> Outcome {
	blend_scene(BlendMode::Difference, [0.4, 0.2, 0.3])
}

// @covers: BlendExclusion
/// `cb + cs - 2 * cb * cs`, which is difference's softer relative: the same at the ends and a half in
/// the middle.
pub fn blend_exclusion() -> Outcome {
	blend_scene(BlendMode::Exclusion, [0.56, 0.52, 0.50])
}

/// The profile's own luminance coefficients, which is what the four non-separable modes are defined
/// in terms of - so a scene reads the SPEC's number rather than restating one.
fn luminance(colour: [f32; 3]) -> f32 {
	let (red, green, blue) = graphics_profile::compositing::model::LUMINANCE_COEFFICIENTS;
	colour[0] * red as f32 + colour[1] * green as f32 + colour[2] * blue as f32
}

fn saturation(colour: [f32; 3]) -> f32 {
	let high = colour[0].max(colour[1]).max(colour[2]);
	let low = colour[0].min(colour[1]).min(colour[2]);
	high - low
}

/// Which channel is the largest, which is the coarse form of "whose hue is this".
fn strongest(colour: [f32; 3]) -> usize {
	let mut index = 0;
	for candidate in 1..3 {
		if colour[candidate] > colour[index] {
			index = candidate;
		}
	}
	index
}

/// The blended pixel as three floats.
fn blended_colour(mode: BlendMode) -> Result<[f32; 3], crate::Trouble> {
	let frame = blended(mode)?;
	let pixel = frame.pixel(8, 4);
	Ok([pixel[0] as f32 / 255.0, pixel[1] as f32 / 255.0, pixel[2] as f32 / 255.0])
}

const BACKDROP: [f32; 3] = [0.8, 0.4, 0.2];
const SOURCE: [f32; 3] = [0.4, 0.6, 0.5];

// @covers: BlendHue
/// THE SOURCE'S HUE, THE BACKDROP'S SATURATION AND LUMINANCE. Checked as the DEFINITION rather than
/// as three numbers, because that is what makes the check independent of the implementation's
/// arithmetic: the result's luminance is the backdrop's, and its strongest channel is the source's.
pub fn blend_hue() -> Outcome {
	let result = blended_colour(BlendMode::Hue)?;
	require!((luminance(result) - luminance(BACKDROP)).abs() < 0.02, "the luminance is the backdrop's: {:?} against {:?}", luminance(result), luminance(BACKDROP));
	require!(strongest(result) == strongest(SOURCE), "and the hue is the source's: {result:?}");
	Ok(())
}

// @covers: BlendSaturation
/// The source's SATURATION, with the backdrop's hue and luminance.
pub fn blend_saturation() -> Outcome {
	let result = blended_colour(BlendMode::Saturation)?;
	require!((luminance(result) - luminance(BACKDROP)).abs() < 0.02, "the luminance is the backdrop's: {result:?}");
	require!((saturation(result) - saturation(SOURCE)).abs() < 0.03, "the saturation is the source's: {} against {}", saturation(result), saturation(SOURCE));
	require!(strongest(result) == strongest(BACKDROP), "and the hue is the backdrop's: {result:?}");
	Ok(())
}

// @covers: BlendColor
/// The source's hue AND saturation, with the backdrop's luminance - which is what tinting a
/// photograph while keeping its shading is.
pub fn blend_color() -> Outcome {
	let result = blended_colour(BlendMode::Color)?;
	require!((luminance(result) - luminance(BACKDROP)).abs() < 0.02, "the luminance is the backdrop's: {result:?}");
	require!(strongest(result) == strongest(SOURCE), "and both hue and saturation are the source's: {result:?}");
	require!((saturation(result) - saturation(SOURCE)).abs() < 0.06, "including the saturation: {} against {}", saturation(result), saturation(SOURCE));
	Ok(())
}

// @covers: BlendLuminosity
/// The exact opposite: the source's LUMINANCE with the backdrop's hue and saturation.
pub fn blend_luminosity() -> Outcome {
	let result = blended_colour(BlendMode::Luminosity)?;
	require!((luminance(result) - luminance(SOURCE)).abs() < 0.02, "the luminance is the source's: {} against {}", luminance(result), luminance(SOURCE));
	require!(strongest(result) == strongest(BACKDROP), "and the hue is the backdrop's: {result:?}");
	Ok(())
}

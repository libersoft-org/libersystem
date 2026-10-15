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
//!
//! EVERY PROBE IS A PIXEL THE SHAPES COVER WHOLLY OR NOT AT ALL, so the frozen edge policy compares it
//! EXACTLY - against the byte the target stores the expected value as, which is the value moved by the
//! profile's ordered dither at that pixel and rounded (see `harness::stored`). A tolerance here would be a
//! colour error wearing an antialiasing tolerance.

use crate::Outcome;
use crate::harness::{Frame, draw, rect_path, solid};
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
	// NOTHING THERE IS NOTHING AT ALL, colour included: a transparent pixel stores no colour.
	let colour = expect.colour.unwrap_or([0.0; 3]);
	require!(frame.holds(x, 4, colour, expect.alpha), "{where_}: the pixel should hold exactly {colour:?} at alpha {} and is {pixel:?}", expect.alpha);
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
	require!(frame.holds(8, 4, expected, 1.0), "an opaque source over an opaque backdrop is opaque, and the equation gives exactly {expected:?}: the pixel is {pixel:?}");
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

/// THE NON-SEPARABLE MODES' OWN FUNCTIONS, AT f64: `Lum`, `Sat`, `SetLum`, `SetSat` and `ClipColor` as
/// `graphics_profile::compositing::model` states them, with the SPEC'S coefficients read from there rather
/// than restated - so each of the four scenes compares the pixel with its equation's value, exactly, and not
/// with a property of it a tolerance would let a wrong equation share.
fn lum(colour: [f64; 3]) -> f64 {
	let (red, green, blue) = graphics_profile::compositing::model::LUMINANCE_COEFFICIENTS;
	colour[0] * red + colour[1] * green + colour[2] * blue
}

fn sat(colour: [f64; 3]) -> f64 {
	colour[0].max(colour[1]).max(colour[2]) - colour[0].min(colour[1]).min(colour[2])
}

/// "with l = Lum(C), n = min channel and x = max channel: if n < 0, C = l + (C - l) * l / (l - n); if x > 1,
/// C = l + (C - l) * (1 - l) / (x - l)".
fn clip_color(colour: [f64; 3]) -> [f64; 3] {
	let light = lum(colour);
	let (low, high) = (colour[0].min(colour[1]).min(colour[2]), colour[0].max(colour[1]).max(colour[2]));
	let mut out = colour;
	if low < 0.0 {
		out = out.map(|channel| light + (channel - light) * light / (light - low));
	}
	if high > 1.0 {
		out = out.map(|channel| light + (channel - light) * (1.0 - light) / (high - light));
	}
	out
}

/// "add l - Lum(C) to every channel, then ClipColor".
fn set_lum(colour: [f64; 3], light: f64) -> [f64; 3] {
	let difference = light - lum(colour);
	clip_color(colour.map(|channel| channel + difference))
}

/// "order the channels; the smallest becomes 0, the largest s, the middle (Cmid - Cmin) * s / (Cmax - Cmin);
/// if Cmax == Cmin every channel becomes 0".
fn set_sat(colour: [f64; 3], saturation: f64) -> [f64; 3] {
	let mut order = [0usize, 1, 2];
	order.sort_unstable_by(|left, right| colour[*left].total_cmp(&colour[*right]));
	let (low, middle, high) = (order[0], order[1], order[2]);
	let mut out = [0.0; 3];
	if colour[high] > colour[low] {
		out[middle] = (colour[middle] - colour[low]) * saturation / (colour[high] - colour[low]);
		out[high] = saturation;
	}
	out
}

const BACKDROP: [f64; 3] = [0.8, 0.4, 0.2];
const SOURCE: [f64; 3] = [0.4, 0.6, 0.5];

/// A non-separable mode's scene: the pixel is the equation's value, as the target stores it.
fn non_separable_scene(mode: BlendMode, expected: [f64; 3]) -> Outcome {
	blend_scene(mode, expected.map(|channel| channel as f32))
}

// @covers: BlendHue
/// THE SOURCE'S HUE, THE BACKDROP'S SATURATION AND LUMINANCE: `SetLum(SetSat(Cs, Sat(Cb)), Lum(Cb))`.
pub fn blend_hue() -> Outcome {
	non_separable_scene(BlendMode::Hue, set_lum(set_sat(SOURCE, sat(BACKDROP)), lum(BACKDROP)))
}

// @covers: BlendSaturation
/// The source's SATURATION, with the backdrop's hue and luminance: `SetLum(SetSat(Cb, Sat(Cs)), Lum(Cb))`.
pub fn blend_saturation() -> Outcome {
	non_separable_scene(BlendMode::Saturation, set_lum(set_sat(BACKDROP, sat(SOURCE)), lum(BACKDROP)))
}

// @covers: BlendColor
/// The source's hue AND saturation, with the backdrop's luminance - which is what tinting a photograph
/// while keeping its shading is: `SetLum(Cs, Lum(Cb))`.
pub fn blend_color() -> Outcome {
	non_separable_scene(BlendMode::Color, set_lum(SOURCE, lum(BACKDROP)))
}

// @covers: BlendLuminosity
/// The exact opposite: the source's LUMINANCE with the backdrop's hue and saturation: `SetLum(Cb, Lum(Cs))`.
pub fn blend_luminosity() -> Outcome {
	non_separable_scene(BlendMode::Luminosity, set_lum(BACKDROP, lum(SOURCE)))
}

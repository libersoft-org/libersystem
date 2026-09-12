//! THE EQUATIONS. Every Porter-Duff operator and every blend mode, written out.
//!
//! "SUPPORTS THE STANDARD BLEND MODES" IS NOT A CONTRACT. Two implementations satisfy that sentence
//! and disagree on `ColorBurn` at zero, on `SoftLight` below a quarter, and on all four of the
//! non-separable modes - which need a luminance model, a saturation model and a gamut-clipping rule
//! that the mode's name does not imply. Each is a value here.
//!
//! EVERYTHING IS PREMULTIPLIED AND LINEAR. Compositing in an encoded space is the defect that makes
//! a half-transparent black edge look grey instead of dark, and it is invisible until somebody
//! compares two implementations side by side.

/// One Porter-Duff operator, as its two coverage factors.
///
/// THE FACTORS ARE THE OPERATOR. `co = as*Fa*Cs + ab*Fb*Cb` and `ao = as*Fa + ab*Fb` is the whole
/// definition, and writing each operator as its pair is what makes them checkable against one
/// another rather than thirteen separate paragraphs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Operator {
	pub name: &'static str,
	/// The factor on the SOURCE, in terms of `as` and `ab`.
	pub source_factor: &'static str,
	/// The factor on the BACKDROP.
	pub backdrop_factor: &'static str,
}

/// Every compositing operator `Render2D Core Profile 1` requires.
pub const OPERATORS: &[Operator] = &[
	Operator { name: "Clear", source_factor: "0", backdrop_factor: "0" },
	Operator { name: "Src", source_factor: "1", backdrop_factor: "0" },
	Operator { name: "Dst", source_factor: "0", backdrop_factor: "1" },
	Operator { name: "SrcOver", source_factor: "1", backdrop_factor: "1 - as" },
	Operator { name: "DstOver", source_factor: "1 - ab", backdrop_factor: "1" },
	Operator { name: "SrcIn", source_factor: "ab", backdrop_factor: "0" },
	Operator { name: "DstIn", source_factor: "0", backdrop_factor: "as" },
	Operator { name: "SrcOut", source_factor: "1 - ab", backdrop_factor: "0" },
	Operator { name: "DstOut", source_factor: "0", backdrop_factor: "1 - as" },
	Operator { name: "SrcAtop", source_factor: "ab", backdrop_factor: "1 - as" },
	Operator { name: "DstAtop", source_factor: "1 - ab", backdrop_factor: "as" },
	Operator { name: "Xor", source_factor: "1 - ab", backdrop_factor: "1 - as" },
	// PLUS IS NOT ONE OF PORTER AND DUFF'S TWELVE and is here anyway, because additive light is what
	// a glow, a highlight and an emissive overlay are - and an implementation without it grows a
	// private one.
	Operator { name: "Plus", source_factor: "1", backdrop_factor: "1" },
];

/// The general compositing equation every operator above is a pair of factors for.
pub const COMPOSITING_EQUATION: &str = "co = as*Fa*Cs + ab*Fb*Cb ; ao = as*Fa + ab*Fb, with colours premultiplied and linear";

/// One SEPARABLE blend mode: a function applied to each colour channel independently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Blend {
	pub name: &'static str,
	/// `B(Cb, Cs)`, per channel, on unpremultiplied values in `0..=1`.
	pub equation: &'static str,
}

/// Every separable blend mode the profile requires.
pub const BLENDS: &[Blend] = &[
	Blend { name: "Normal", equation: "Cs" },
	Blend { name: "Multiply", equation: "Cb * Cs" },
	Blend { name: "Screen", equation: "Cb + Cs - Cb * Cs" },
	Blend { name: "Overlay", equation: "HardLight(Cs, Cb): the operands swapped" },
	Blend { name: "Darken", equation: "min(Cb, Cs)" },
	Blend { name: "Lighten", equation: "max(Cb, Cs)" },
	// THE TWO WITH A DIVISION, and their endpoints are where implementations disagree: the zero and
	// one cases are stated rather than left to whatever the division produces.
	Blend { name: "ColorDodge", equation: "Cb == 0 -> 0 ; Cs == 1 -> 1 ; otherwise min(1, Cb / (1 - Cs))" },
	Blend { name: "ColorBurn", equation: "Cb == 1 -> 1 ; Cs == 0 -> 0 ; otherwise 1 - min(1, (1 - Cb) / Cs)" },
	Blend { name: "HardLight", equation: "Cs <= 0.5 -> Multiply(Cb, 2*Cs) ; otherwise Screen(Cb, 2*Cs - 1)" },
	// SOFTLIGHT'S LOWER BRANCH IS THE ONE THAT IS GOT WRONG: its `D(Cb)` is a piecewise function of
	// the BACKDROP, not of the source, and the quarter threshold is on the backdrop.
	Blend { name: "SoftLight", equation: "Cs <= 0.5 -> Cb - (1 - 2*Cs) * Cb * (1 - Cb) ; otherwise Cb + (2*Cs - 1) * (D(Cb) - Cb), where D(Cb) = Cb <= 0.25 ? ((16*Cb - 12) * Cb + 4) * Cb : sqrt(Cb)" },
	Blend { name: "Difference", equation: "abs(Cb - Cs)" },
	Blend { name: "Exclusion", equation: "Cb + Cs - 2 * Cb * Cs" },
];

/// One NON-SEPARABLE blend mode, which mixes the channels and therefore needs a colour model.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NonSeparableBlend {
	pub name: &'static str,
	/// `B(Cb, Cs)` over the whole colour, in terms of the model below.
	pub equation: &'static str,
}

/// The four non-separable modes.
pub const NON_SEPARABLE_BLENDS: &[NonSeparableBlend] = &[
	NonSeparableBlend { name: "Hue", equation: "SetLum(SetSat(Cs, Sat(Cb)), Lum(Cb))" },
	NonSeparableBlend { name: "Saturation", equation: "SetLum(SetSat(Cb, Sat(Cs)), Lum(Cb))" },
	NonSeparableBlend { name: "Color", equation: "SetLum(Cs, Lum(Cb))" },
	NonSeparableBlend { name: "Luminosity", equation: "SetLum(Cb, Lum(Cs))" },
];

/// THE LUMINANCE AND SATURATION MODEL the four non-separable modes are defined in.
///
/// ITS COEFFICIENTS ARE NOT THE COLOUR SPACE'S. `Lum` here is the compositing specification's own
/// fixed triple, and it stays fixed in every colour space - which looks wrong and is right: using the
/// destination space's luminance would make `Luminosity` produce a different result for the same two
/// colours depending on which space they were tagged with, and the mode is defined as an operation on
/// the numbers rather than on the light.
pub mod model {
	/// `Lum(C) = 0.3*R + 0.59*G + 0.11*B`, fixed in every colour space.
	pub const LUMINANCE_COEFFICIENTS: (f64, f64, f64) = (0.30, 0.59, 0.11);
	/// `Sat(C) = max(R,G,B) - min(R,G,B)`.
	pub const SATURATION: &str = "max(R, G, B) - min(R, G, B)";
	/// `SetLum(C, l)` adds `l - Lum(C)` to every channel and then CLIPS.
	pub const SET_LUMINANCE: &str = "add l - Lum(C) to every channel, then ClipColor";
	/// `SetSat(C, s)` maps the channels onto `0..=s` by their order, with the middle placed
	/// proportionally - and a colour whose maximum equals its minimum becomes black, because there is
	/// no direction to spread it in.
	pub const SET_SATURATION: &str = "order the channels; the smallest becomes 0, the largest s, the middle (Cmid - Cmin) * s / (Cmax - Cmin); if Cmax == Cmin every channel becomes 0";
	/// THE GAMUT CLIP, which is the step the mode's name does not imply and which every
	/// implementation that omits it gets wrong on saturated colours.
	pub const CLIP_COLOR: &str = "with l = Lum(C), n = min channel and x = max channel: if n < 0, C = l + (C - l) * l / (l - n); if x > 1, C = l + (C - l) * (1 - l) / (x - l). Luminance is preserved and the chroma is reduced";
}

/// SUBPIXEL TEXT, which needs two numbers the word "LCD" does not carry.
pub mod lcd {
	/// The five-tap FIR applied across the subpixel triple, as NINTHS. Without a filter, subpixel
	/// coverage produces coloured fringes on every stem; with a different filter it produces
	/// different fringes, which is why the taps are stated rather than described.
	pub const FILTER_NINTHS: [u8; 5] = [1, 2, 3, 2, 1];
	/// THE GAMMA COVERAGE IS BLENDED THROUGH. Blending coverage linearly makes light-on-dark text
	/// look bolder than dark-on-light at the same weight, which is the artefact that gets called
	/// "the font renders too thin" - and it is a gamma question rather than a font question.
	pub const COVERAGE_GAMMA: f64 = 2.2;
}

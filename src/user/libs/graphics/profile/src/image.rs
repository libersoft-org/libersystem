//! `Image and Colour Profile 1`: what a pixel IS, what it MEANS, and the exact numbers every
//! conversion between two of them uses.
//!
//! "SUPPORTS COLOUR MANAGEMENT" IS NOT A CONTRACT. Two implementations can satisfy that sentence and
//! produce visibly different images, which is the failure this registry exists to end: the primaries,
//! the white point, the adaptation matrix, the transfer functions and their inverses, the tone
//! mapping operator, the gamut-mapping rule, the dither matrix and its phase, and the rounding at
//! every boundary are all VALUES here rather than choices left to a backend.
//!
//! A FORMAT SET CHOSEN FOR A SCANOUT IS THE WRONG SET. Eight-bit RGB is what a display takes; it is
//! not what a coverage mask, a glyph cache, a filter intermediate, a wide-gamut composite or an HDR
//! target is made of, and adding those formats after the compositing code exists means rewriting the
//! compositing code.
//!
//! AN IMAGE'S MEANING IS PART OF IT. A normal map, a roughness map, a coverage mask and a depth
//! buffer are all just bytes, and running any of them through an sRGB decode corrupts them silently -
//! the artefact looks like a lighting bug, which is where the days go. Semantics are carried beside
//! storage, and a transfer function applies to nothing but colour.
//!
//! THE VALUES ARE EXACT RATIONALS WHERE THE STANDARDS STATE THEM AS SUCH. `PQ`'s constants are
//! twelve-bit fractions and `HLG`'s are decimal; writing the first as decimals loses the identity
//! that makes two implementations agree at the endpoints, so both forms are carried and the exact one
//! is normative.

/// The version this registry publishes. A refusal, the generated document and a conformance run all
/// name it, and a change to the list is a change to this number in the same edit.
pub const IMAGE_COLOR_PROFILE_VERSION: u32 = 1;

/// How a channel's bits are laid out in storage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
	/// Unsigned normalised: the integer range maps linearly onto `0.0..=1.0`.
	Unorm,
	/// IEEE 754 binary16 or binary32, as the channel width says.
	Float,
	/// An unsigned integer, carried as itself. Never sampled and never filtered.
	Uint,
}

impl Encoding {
	pub const fn name(self) -> &'static str {
		match self {
			Encoding::Unorm => "unorm",
			Encoding::Float => "float",
			Encoding::Uint => "uint",
		}
	}
}

/// WHAT A FORMAT'S CHANNELS ARE, which is not the same question as how many it has.
///
/// PREMULTIPLICATION IS A RELATION BETWEEN COLOUR AND ALPHA. A format with no colour has nothing to
/// have been multiplied, and one with no alpha has nothing to multiply BY - so the alpha modes a
/// format admits fall out of this rather than being listed per format and getting out of step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Carries {
	/// Colour and alpha together.
	ColourAlpha,
	/// Colour with no alpha - an `X8` format, or a scanout.
	Colour,
	/// Alpha and nothing else: a coverage mask.
	Alpha,
	/// Measurements. Never colour, never filtered through a transfer function.
	Data,
}

impl Carries {
	pub const fn has_alpha(self) -> bool {
		matches!(self, Carries::ColourAlpha | Carries::Alpha)
	}

	pub const fn has_colour(self) -> bool {
		matches!(self, Carries::ColourAlpha | Carries::Colour)
	}

	pub const fn name(self) -> &'static str {
		match self {
			Carries::ColourAlpha => "colour+alpha",
			Carries::Colour => "colour",
			Carries::Alpha => "alpha",
			Carries::Data => "data",
		}
	}
}

/// One storage format in the mandatory set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Format {
	pub name: &'static str,
	/// The channels in the order the NAME states them, which is the order in memory. Never host
	/// endianness: a format called `B8G8R8A8` has blue in its first byte on every machine.
	pub channels: &'static str,
	/// Bits per channel, in that order. A packed format states its own.
	pub bits: &'static [u8],
	pub encoding: Encoding,
	pub bytes_per_pixel: u8,
	/// What the channels ARE, which is what decides the alpha modes the format admits.
	pub carries: Carries,
	/// What it is FOR. A format list with no reason beside each entry is a list somebody extends.
	pub purpose: &'static str,
}

const fn format(name: &'static str, channels: &'static str, bits: &'static [u8], encoding: Encoding, bytes_per_pixel: u8, carries: Carries, purpose: &'static str) -> Format {
	Format { name, channels, bits, encoding, bytes_per_pixel, carries, purpose }
}

/// Every single-plane storage format `Image and Colour Profile 1` requires.
pub const FORMATS: &[Format] = &[
	format("A8_UNORM", "A", &[8], Encoding::Unorm, 1, Carries::Alpha, "coverage masks, glyph masks and alpha-only layers"),
	format("R8_UNORM", "R", &[8], Encoding::Unorm, 1, Carries::Data, "single-channel data images"),
	format("R8G8_UNORM", "RG", &[8, 8], Encoding::Unorm, 2, Carries::Data, "two-channel data images"),
	format("B8G8R8X8_UNORM", "BGRX", &[8, 8, 8, 8], Encoding::Unorm, 4, Carries::Colour, "scanout, and the commonest firmware hand-off"),
	format("R8G8B8X8_UNORM", "RGBX", &[8, 8, 8, 8], Encoding::Unorm, 4, Carries::Colour, "scanout, the `PIXEL_RGB` firmware hand-off"),
	format("B8G8R8A8_UNORM", "BGRA", &[8, 8, 8, 8], Encoding::Unorm, 4, Carries::ColourAlpha, "the ordinary composited surface"),
	format("R8G8B8A8_UNORM", "RGBA", &[8, 8, 8, 8], Encoding::Unorm, 4, Carries::ColourAlpha, "the same channel order a decoder produces"),
	format("R10G10B10A2_UNORM", "RGBA", &[10, 10, 10, 2], Encoding::Unorm, 4, Carries::ColourAlpha, "wide-gamut scanout"),
	format("R16G16B16A16_UNORM", "RGBA", &[16, 16, 16, 16], Encoding::Unorm, 8, Carries::ColourAlpha, "sixteen-bit bounded colour"),
	format("R16G16B16A16_FLOAT", "RGBA", &[16, 16, 16, 16], Encoding::Float, 8, Carries::ColourAlpha, "THE CANONICAL INTERMEDIATE: every layer, filter intermediate and offscreen composite"),
	format("R32_UINT", "R", &[32], Encoding::Uint, 4, Carries::Data, "integer data and object-identity images"),
	format("R32G32B32A32_FLOAT", "RGBA", &[32, 32, 32, 32], Encoding::Float, 16, Carries::ColourAlpha, "full-float host-visible images"),
];

/// THE CANONICAL INTERMEDIATE, named rather than implied.
///
/// REPEATED COMPOSITING THROUGH EIGHT-BIT SRGB BANDS AND LOSES PRECISION, and a blur over an eight-bit
/// intermediate is where it shows first. Every layer, filter intermediate and offscreen composite is
/// premultiplied LINEAR `R16G16B16A16_FLOAT`, and that is a property of the profile rather than of a
/// backend's convenience.
pub const CANONICAL_INTERMEDIATE: &str = "R16G16B16A16_FLOAT";

/// How the alpha channel is to be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlphaMode {
	/// There is no transparency. An alpha channel, where one exists, reads and writes its maximum.
	Opaque,
	/// The colour channels are independent of alpha.
	Straight,
	/// The colour channels are already multiplied by alpha.
	Premultiplied,
}

impl AlphaMode {
	pub const fn name(self) -> &'static str {
		match self {
			AlphaMode::Opaque => "opaque",
			AlphaMode::Straight => "straight",
			AlphaMode::Premultiplied => "premultiplied",
		}
	}
}

/// Which alpha modes a format admits.
///
/// A TYPE ERROR AND NOT AN UNSUPPORTED CASE. `Straight` on a format with no alpha channel is not
/// something a backend might implement later; it is a combination that cannot mean anything, and
/// saying so here is what keeps it out of every backend's match arms.
pub fn alpha_modes(format: &Format) -> &'static [AlphaMode] {
	match (format.carries.has_alpha(), format.carries.has_colour()) {
		// Colour and alpha together: all three, and the choice is a real one.
		(true, true) => &[AlphaMode::Opaque, AlphaMode::Straight, AlphaMode::Premultiplied],
		// ALPHA AND NO COLOUR. Premultiplication is a relation between colour and alpha, and with no
		// colour there is nothing to have been multiplied - so the alpha is itself, and `Opaque` on a
		// format whose only channel IS alpha would mean an image that is alpha-only and fully opaque,
		// which is a constant rather than an image.
		(true, false) => &[AlphaMode::Straight],
		// No alpha channel at all.
		(false, _) => &[AlphaMode::Opaque],
	}
}

/// What an image's numbers MEAN.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Semantics {
	/// Colour, in a stated colour space. The ONLY kind a transfer function applies to.
	Color,
	/// Coverage, in `0.0..=1.0`, already linear. An sRGB decode over a mask lightens every edge.
	Mask,
	/// A tangent-space normal map, with its own stated encoding.
	Normal,
	/// A measurement - roughness, metalness, height, occlusion - carried as a number.
	Data,
	/// Depth, in the projection's own units.
	Depth,
	/// An identity, never filtered and never converted.
	Identity,
}

impl Semantics {
	pub const fn name(self) -> &'static str {
		match self {
			Semantics::Color => "color",
			Semantics::Mask => "mask",
			Semantics::Normal => "normal",
			Semantics::Data => "data",
			Semantics::Depth => "depth",
			Semantics::Identity => "identity",
		}
	}

	/// Whether a transfer function may be applied to an image of this kind.
	pub const fn is_colour(self) -> bool {
		matches!(self, Semantics::Color)
	}
}

/// A chromaticity, as the standards state them: four decimal places, exactly.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Chromaticity {
	pub x: f64,
	pub y: f64,
}

const fn xy(x: f64, y: f64) -> Chromaticity {
	Chromaticity { x, y }
}

/// One set of primaries and its white point.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Primaries {
	pub name: &'static str,
	pub red: Chromaticity,
	pub green: Chromaticity,
	pub blue: Chromaticity,
	pub white: Chromaticity,
}

/// D65, as every one of the mandatory spaces uses it.
pub const D65: Chromaticity = xy(0.3127, 0.3290);

/// Every primary set the profile requires.
pub const PRIMARIES: &[Primaries] = &[
	Primaries { name: "sRGB", red: xy(0.640, 0.330), green: xy(0.300, 0.600), blue: xy(0.150, 0.060), white: D65 },
	Primaries { name: "Display P3", red: xy(0.680, 0.320), green: xy(0.265, 0.690), blue: xy(0.150, 0.060), white: D65 },
	Primaries { name: "Rec. 2020", red: xy(0.708, 0.292), green: xy(0.170, 0.797), blue: xy(0.131, 0.046), white: D65 },
];

/// A transfer function, with the exact form the standard states.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transfer {
	/// The identity. Values are already light.
	Linear,
	/// IEC 61966-2-1, with its linear segment.
	Srgb,
	/// SMPTE ST 2084, absolute, with 1.0 meaning `PQ_PEAK_NITS`.
	Pq,
	/// ITU-R BT.2100 hybrid log-gamma, relative.
	Hlg,
}

impl Transfer {
	pub const fn name(self) -> &'static str {
		match self {
			Transfer::Linear => "linear",
			Transfer::Srgb => "srgb",
			Transfer::Pq => "pq",
			Transfer::Hlg => "hlg",
		}
	}
}

/// One named colour space: primaries plus a transfer function.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ColorSpace {
	pub name: &'static str,
	pub primaries: &'static str,
	pub transfer: Transfer,
}

/// Every colour space `Image and Colour Profile 1` requires.
pub const COLOR_SPACES: &[ColorSpace] = &[
	ColorSpace { name: "srgb", primaries: "sRGB", transfer: Transfer::Srgb },
	ColorSpace { name: "srgb-linear", primaries: "sRGB", transfer: Transfer::Linear },
	ColorSpace { name: "display-p3", primaries: "Display P3", transfer: Transfer::Srgb },
	ColorSpace { name: "display-p3-linear", primaries: "Display P3", transfer: Transfer::Linear },
	ColorSpace { name: "rec2020", primaries: "Rec. 2020", transfer: Transfer::Srgb },
	ColorSpace { name: "rec2020-linear", primaries: "Rec. 2020", transfer: Transfer::Linear },
	ColorSpace { name: "rec2020-pq", primaries: "Rec. 2020", transfer: Transfer::Pq },
	ColorSpace { name: "rec2020-hlg", primaries: "Rec. 2020", transfer: Transfer::Hlg },
];

/// The sRGB transfer function's own constants, which are NOT a pure power law.
///
/// THE LINEAR SEGMENT IS THE PART THAT GETS DROPPED. A pure 2.2 power law is close enough to look
/// right and wrong enough that two implementations disagree in the darks, which is where banding
/// lives.
pub mod srgb {
	/// Below this ENCODED value the function is linear.
	pub const ENCODED_THRESHOLD: f64 = 0.04045;
	/// Below this LINEAR value the inverse is linear.
	pub const LINEAR_THRESHOLD: f64 = 0.0031308;
	pub const SLOPE: f64 = 12.92;
	pub const OFFSET: f64 = 0.055;
	pub const EXPONENT: f64 = 2.4;
}

/// SMPTE ST 2084's constants, as the TWELVE-BIT FRACTIONS the standard states.
///
/// WRITTEN AS FRACTIONS BECAUSE THAT IS WHAT THEY ARE. Rounding them to decimals loses the identity
/// that makes two implementations agree at the endpoints, and the endpoints are where a display's
/// black level and peak white are decided.
pub mod pq {
	/// `2610 / 16384`.
	pub const M1: f64 = 2610.0 / 16384.0;
	/// `2523 / 4096 * 128`.
	pub const M2: f64 = 2523.0 / 4096.0 * 128.0;
	/// `3424 / 4096`.
	pub const C1: f64 = 3424.0 / 4096.0;
	/// `2413 / 4096 * 32`.
	pub const C2: f64 = 2413.0 / 4096.0 * 32.0;
	/// `2392 / 4096 * 32`.
	pub const C3: f64 = 2392.0 / 4096.0 * 32.0;
	/// What an encoded 1.0 means, in cd/m². PQ is ABSOLUTE, which is the whole difference from every
	/// other transfer function here.
	pub const PEAK_NITS: f64 = 10_000.0;
}

/// ITU-R BT.2100 hybrid log-gamma's constants.
///
/// `B` AND `C` ARE DERIVED FROM `A` and are written out anyway: `b = 1 - 4a` and
/// `c = 0.5 - a * ln(4a)`, and a reader checking one implementation against another needs the value
/// rather than the derivation.
pub mod hlg {
	pub const A: f64 = 0.178_832_77;
	pub const B: f64 = 0.284_668_92;
	pub const C: f64 = 0.559_910_73;
	/// Below this LINEAR value the function is a square root rather than a logarithm.
	pub const SPLIT: f64 = 1.0 / 12.0;
}

/// The Bradford chromatic adaptation matrix, and its inverse.
///
/// NAMED AND WRITTEN DOWN, because "adapts between white points" is satisfied by three different
/// matrices in common use and they do not agree. Adaptation happens in this cone space and in no
/// other.
pub mod bradford {
	pub const FORWARD: [[f64; 3]; 3] = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
	pub const INVERSE: [[f64; 3]; 3] = [[0.986_992_9, -0.147_054_3, 0.159_962_7], [0.432_305_3, 0.518_360_3, 0.049_291_2], [-0.008_528_7, 0.040_042_8, 0.968_486_7]];
}

/// The reference white, and what an HDR number means.
pub mod reference {
	/// DIFFUSE WHITE, in cd/m². A relative 1.0 in an SDR space is this much light, which is what makes
	/// an SDR image and an HDR image composable at all.
	pub const DIFFUSE_WHITE_NITS: f64 = 203.0;
	/// Whether HDR values are absolute. `PQ` is; `HLG` and every linear space are relative to
	/// `DIFFUSE_WHITE_NITS`.
	pub const PQ_IS_ABSOLUTE: bool = true;
	/// INSIDE the pipeline a linear value may be negative or above one - a wide-gamut colour in a
	/// narrower space is negative, and a highlight is above one. Clamping happens at OUTPUT and
	/// nowhere else.
	pub const CLAMPED_ONLY_AT_OUTPUT: bool = true;
}

/// THE ONE TONE-MAPPING OPERATOR, with its equation and its parameter.
///
/// EXTENDED REINHARD ON LUMINANCE, chosen over a filmic curve for a reason that is about this profile
/// rather than about taste: it has ONE parameter, it is exactly reproducible in any arithmetic, and
/// it is defined on luminance so it does not shift hue. A filmic curve is prettier and is five
/// constants two implementations will copy from different blog posts.
///
/// `L_out = L * (1 + L / L_white²) / (1 + L)`, applied to the luminance of the linear colour, with
/// the colour scaled by `L_out / L`. Luminance uses the DESTINATION space's own coefficients, not
/// sRGB's, because a Rec. 2020 colour's luminance is not its sRGB luminance.
pub mod tone_map {
	pub const NAME: &str = "extended Reinhard above a knee, on luminance";
	/// The luminance mapped to 1.0, relative to diffuse white.
	pub const WHITE: f64 = 4.0;
	/// The luminance below which the curve is the IDENTITY, relative to diffuse white.
	///
	/// THE THREE THINGS THAT CANNOT ALL HOLD, and this knee is the only shape that reconciles them.
	/// A highlight above one must keep its gradations rather than becoming a flat white rectangle;
	/// a value an author already put inside the range must come back as itself; and the output
	/// range ends at one. The first two need room the third does not have, so something below one
	/// has to move - and the knee is the decision about WHERE, taken once, here.
	///
	/// WITHOUT IT THE CURVE IS 0.53 AT DIFFUSE WHITE. That is right for a scene whose radiances the
	/// author chose and wrong for a compositor handed content already in display space, and the code
	/// compensated with a guard at a luminance of one - which is a 47 per cent STEP, 1.000000 at 1.0
	/// and 0.531280 at 1.0001, running through the middle of every lit surface.
	///
	/// WHAT 0.8 COSTS AND BUYS: everything below 0.8 is returned bit for bit, diffuse white comes
	/// back at 0.900391 instead of 1.0, and the whole of `[1, 4]` keeps its ordering in the top
	/// tenth of the range. That white is no longer the top is not a new decision - this system
	/// already requires a narrow target to COMPRESS additive light rather than clip it, which is
	/// only possible if white sits below the top. The knee makes that continuous instead of a cliff.
	/// It is one number and it is meant to be argued with.
	///
	/// THE CURVE ABOVE IT IS THE SAME OPERATOR AND NOT A SECOND ONE: extended Reinhard on the
	/// remaining range, with its own white at `(WHITE - KNEE) / (1 - KNEE)`. `R` has slope one at
	/// zero, so the shoulder meets the identity with the SAME SLOPE - no kink - and `WHITE` still
	/// maps to exactly one.
	pub const KNEE: f64 = 0.8;

	/// `R`'s white point on the range above the knee, which is what makes `WHITE` land on one.
	pub const fn shoulder_white() -> f64 {
		(WHITE - KNEE) / (1.0 - KNEE)
	}

	/// The operator at a stated white point.
	///
	/// ONE IMPLEMENTATION AND NOT ONE PER CRATE. `graphics-core` needs it for the 2D encode path and
	/// `scene3d` for the 3D resolve, and both already depend on this crate - so the curve lives with
	/// the constants it is made of. Two copies of a curve is how two paths come to disagree about
	/// what a highlight looks like, which is the defect this knee ends.
	pub fn map_with(light: f64, white: f64) -> f64 {
		// AT OR BELOW THE KNEE THE CURVE IS THE IDENTITY, written so a NaN falls through here rather
		// than into the arithmetic below.
		if !(light > KNEE) {
			return light;
		}
		let x = (light - KNEE) / (1.0 - KNEE);
		let shoulder = (white - KNEE) / (1.0 - KNEE);
		KNEE + (1.0 - KNEE) * (x * (1.0 + x / (shoulder * shoulder)) / (1.0 + x))
	}

	/// The operator at the profile's own white point.
	pub fn map(light: f64) -> f64 {
		map_with(light, WHITE)
	}
}

/// THE GAMUT-MAPPING RULE, which is an algorithm and not a preference.
///
/// CLIPPING EACH CHANNEL SHIFTS HUE, and it shifts it most on exactly the saturated colours a
/// wide-gamut image was made for. The rule is to desaturate toward the achromatic colour of the same
/// luminance until the colour is inside the destination, by bisection on the chroma scale - which is
/// deterministic, hue-preserving, and terminates in a stated number of steps rather than "until it
/// converges".
pub mod gamut_map {
	pub const NAME: &str = "hue-preserving desaturation toward equal luminance, by bisection";
	/// How many bisection steps. Fixed, so two implementations produce the same pixels.
	pub const STEPS: u32 = 16;
	/// The chroma scale is resolved to this, which is finer than any output quantisation in the
	/// format set.
	pub const TOLERANCE: f64 = 1.0 / 4096.0;
}

/// THE DITHER, which is ORDERED and nothing else.
///
/// ERROR DIFFUSION CARRIES STATE ACROSS PIXELS, which makes a tile-parallel renderer's output depend
/// on how it decomposed the image. A backend built to render tiles independently and deterministically
/// cannot use it without giving up both properties, so the alternative is not merely vague - it is
/// incompatible with the architecture.
///
/// THE PHASE IS ANCHORED TO THE TARGET'S ORIGIN and never to a tile's. A tile-relative phase makes the
/// pattern visibly restart at every tile boundary, which is the artefact that looks like a seam.
pub mod dither {
	pub const NAME: &str = "ordered, Bayer 8x8";
	/// The matrix, in its conventional integer form. The offset added to a channel before quantisation
	/// is `(MATRIX[y % 8][x % 8] + 0.5) / 64 - 0.5` of one quantisation step.
	pub const MATRIX: [[u8; 8]; 8] = [
		[0, 32, 8, 40, 2, 34, 10, 42],
		[48, 16, 56, 24, 50, 18, 58, 26],
		[12, 44, 4, 36, 14, 46, 6, 38],
		[60, 28, 52, 20, 62, 30, 54, 22],
		[3, 35, 11, 43, 1, 33, 9, 41],
		[51, 19, 59, 27, 49, 17, 57, 25],
		[15, 47, 7, 39, 13, 45, 5, 37],
		[63, 31, 55, 23, 61, 29, 53, 21],
	];
	/// `x` and `y` are the TARGET's coordinates, not a tile's.
	pub const PHASE: &str = "the target's origin";
}

/// What happens at every numeric boundary, stated rather than left to a language's defaults.
pub mod rounding {
	/// Converting a float to an integer channel: CLAMP first, then round half AWAY from zero.
	pub const FLOAT_TO_INTEGER: &str = "clamp, then round half away from zero";
	/// A NaN reaching an integer channel becomes zero rather than whatever a cast produces.
	pub const NAN_TO_INTEGER: &str = "zero";
	/// An infinity clamps to the channel's extreme rather than wrapping or trapping.
	pub const INFINITY_TO_INTEGER: &str = "clamp to the channel extreme";
	/// Converting into `R16G16B16A16_FLOAT`: round to nearest, ties to EVEN, which is IEEE 754's
	/// default and the only rounding two independent implementations agree on without being told.
	pub const FLOAT_TO_HALF: &str = "round to nearest, ties to even";
	/// Half-float subnormals are PRESERVED rather than flushed. Flushing them is a decision that
	/// changes the darkest values of every HDR intermediate.
	pub const HALF_SUBNORMALS: &str = "preserved";
	/// A NaN converted to half becomes one canonical quiet NaN, so two implementations produce the
	/// same bytes.
	pub const HALF_NAN: &str = "one canonical quiet NaN";
	/// An `X8` byte and the reserved bits of a packed layout are ignored on read and written with ALL
	/// BITS SET, so a producer cannot leak stale bytes and a consumer cannot start depending on
	/// uninitialised content.
	pub const RESERVED_BITS: &str = "ignored on read, all bits set on write";
}

/// A multi-plane YUV layout, as a decoder hands one over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct YuvLayout {
	pub name: &'static str,
	/// How many planes, in the order the name states.
	pub planes: u8,
	/// What each plane carries, in order.
	pub order: &'static str,
	/// Chroma subsampling, horizontal and vertical.
	pub subsampling: (u8, u8),
	/// How a plane's own pitch is derived. Stated per layout because an interleaved chroma plane's
	/// row is TWICE its sample count and a planar one's is once, which is the arithmetic that puts a
	/// decoder half a row out.
	pub pitch_rule: &'static str,
	/// Bits per sample.
	pub bits: u8,
	/// Where a sample's bits sit within its storage word, for the formats that do not fill one.
	pub bit_placement: &'static str,
}

/// The YUV layouts the profile requires.
pub const YUV_LAYOUTS: &[YuvLayout] = &[
	YuvLayout { name: "NV12", planes: 2, order: "Y, then interleaved CbCr", subsampling: (2, 2), bits: 8, bit_placement: "the whole byte", pitch_rule: "luma at least the width; chroma at least the width rounded up to an even number, because its two samples are interleaved" },
	YuvLayout { name: "I420", planes: 3, order: "Y, then Cb, then Cr", subsampling: (2, 2), bits: 8, bit_placement: "the whole byte", pitch_rule: "luma at least the width; each chroma plane at least half the width rounded up" },
	YuvLayout { name: "P010", planes: 2, order: "Y, then interleaved CbCr", subsampling: (2, 2), bits: 10, bit_placement: "the HIGH ten bits of a little-endian 16-bit word; the low six are zero on write and ignored on read", pitch_rule: "luma at least twice the width; chroma at least twice the width rounded up to an even number" },
];

/// One YUV matrix: the two coefficients every other one is derived from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct YuvMatrix {
	pub name: &'static str,
	pub kr: f64,
	pub kb: f64,
}

/// The YUV matrices the profile requires.
pub const YUV_MATRICES: &[YuvMatrix] = &[YuvMatrix { name: "BT.601", kr: 0.299, kb: 0.114 }, YuvMatrix { name: "BT.709", kr: 0.2126, kb: 0.0722 }, YuvMatrix { name: "BT.2020", kr: 0.2627, kb: 0.0593 }];

/// How a YUV image's samples use their range, and where its chroma sample sits.
pub mod yuv {
	/// Limited range, eight-bit: luma and chroma do not use the whole byte, which is the single
	/// commonest cause of washed-out or crushed video.
	pub const LIMITED_LUMA_8: (u16, u16) = (16, 235);
	pub const LIMITED_CHROMA_8: (u16, u16) = (16, 240);
	pub const FULL_8: (u16, u16) = (0, 255);
	/// The same at ten bits, which is the range scaled rather than re-derived.
	pub const LIMITED_LUMA_10: (u16, u16) = (64, 940);
	pub const LIMITED_CHROMA_10: (u16, u16) = (64, 960);
	pub const FULL_10: (u16, u16) = (0, 1023);
	/// WHERE THE CHROMA SAMPLE IS. Getting this wrong shifts the colour half a pixel against the
	/// luma, which reads as a coloured fringe on every hard edge.
	pub const SITING: &str = "left-sited horizontally, centre-sited vertically, as MPEG-2 and every codec in this tree produce";
	/// How chroma is brought back to full resolution. Named, because a bilinear and a nearest
	/// reconstruction disagree on every edge in the image.
	pub const RECONSTRUCTION: &str = "bilinear, with the siting above, edge samples replicated";
	/// THE ORDER OF OPERATIONS, which is the part a recipe written for RGB gets wrong: planes are
	/// reconstructed and the matrix applied FIRST, producing ENCODED RGB; only then does transfer
	/// decoding, primary conversion and premultiplication happen. Applying an RGB sampling recipe
	/// directly to YUV bytes decodes the transfer function of a signal that is not yet a colour.
	pub const ORDER: &str = "reconstruct planes, apply the matrix to encoded RGB, then decode the transfer function, then convert primaries, then premultiply";
	/// An odd extent's chroma plane is the ceiling of half the luma extent, and the last column or row
	/// is replicated rather than read past.
	pub const ODD_EXTENT: &str = "chroma extent is the ceiling of half the luma extent; the final sample is replicated";
	/// A crop must land on a chroma sample, or the crop's own chroma is between two of them.
	pub const CROP_ALIGNMENT: &str = "a crop origin and extent are multiples of the subsampling factor";
}

/// THE THREE BYTE SPANS, which are three different questions about one image.
///
/// CONFUSING THEM IS HOW A BUFFER IS ACCEPTED THAT CANNOT HOLD THE IMAGE - or how one that is exactly
/// big enough is refused. They are not "the size", and an implementation that stores one number
/// answers all three with it and is wrong about two.
pub mod spans {
	/// WHAT A BORROWED CPU VIEW NEEDS: `(height - 1) * pitch + minimum_row_bytes`.
	///
	/// The last row's PADDING is not part of it, so a legal final row with no padding after it is
	/// accepted - and a validator demanding `pitch * height` refuses buffers that are exactly big
	/// enough for the image they hold.
	pub const MINIMUM_VISIBLE_BYTES: &str = "(height - 1) * pitch + minimum_row_bytes";
	/// WHAT THE SELECTED BACKEND MAY TOUCH, stated separately because it is not the same span.
	///
	/// A display or DMA backend may read the final row's pitch padding - a scanout engine fetching
	/// whole rows does - and a driver may never touch beyond this. A presentable or DMA image must
	/// OWN at least this much, which is the check that separates "the image fits" from "the hardware
	/// will not read past the allocation".
	pub const BACKEND_ACCESS_SPAN: &str = "every byte the selected display or DMA backend may touch, including the final row's pitch padding where it reads that";
	/// THE ACTUAL OWNED ALLOCATION, which is a fact about the memory rather than about the image.
	///
	/// It is not a second stored answer to "how big is the image": it is at least the backend access
	/// span for a presentable or DMA image, and it may be larger for reasons - alignment, a pool,
	/// a suballocation - that are nobody else's business.
	pub const ALLOCATION_LEN: &str = "the owned allocation's own length, at least the backend access span for a presentable or DMA image";
}

/// The two ROW quantities, which are not spans and are what the spans are computed from.
pub mod rows {
	/// `width * bytes_per_pixel`, checked. A pitch below this is a refusal.
	pub const MINIMUM_ROW_BYTES: &str = "width * bytes_per_pixel";
	/// What the layout states. At least the minimum row, and not otherwise constrained.
	pub const PITCH: &str = "the layout's own, at least the minimum row";
}

/// WHAT MAY BE DONE TO AN IMAGE, which is not the same list for every kind of image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Operation {
	/// Read a sample at a coordinate, without filtering.
	Sample,
	/// Read a filtered sample - bilinear, bicubic, or a minification chain.
	Filter,
	/// Decode or encode a transfer function.
	Transfer,
	/// Blend, under a Porter-Duff operator or a blend mode.
	Blend,
	/// Convert between primaries, which implies chromatic adaptation.
	ConvertPrimaries,
	/// Multiply or divide the colour channels by alpha.
	Premultiply,
	/// Add the ordered dither offset on the way to a narrower target.
	Dither,
}

impl Operation {
	pub const fn name(self) -> &'static str {
		match self {
			Operation::Sample => "sample",
			Operation::Filter => "filter",
			Operation::Transfer => "transfer",
			Operation::Blend => "blend",
			Operation::ConvertPrimaries => "convert-primaries",
			Operation::Premultiply => "premultiply",
			Operation::Dither => "dither",
		}
	}
}

/// Which operations an image of a given kind admits.
///
/// A COLOUR-MANAGED PIPELINE THAT CANNOT TELL A COLOUR FROM A MEASUREMENT WILL TRANSFORM THE
/// MEASUREMENT, and the artefact looks like a lighting bug. FILTERING an identity image averages two
/// object ids into a third that names a different object; DITHERING a mask adds noise to coverage,
/// which is visible as a speckled edge; and a transfer function on anything but colour is the
/// classic one. Each refusal below is a defect somebody has shipped.
pub fn operations(semantics: Semantics) -> &'static [Operation] {
	match semantics {
		Semantics::Color => &[Operation::Sample, Operation::Filter, Operation::Transfer, Operation::Blend, Operation::ConvertPrimaries, Operation::Premultiply, Operation::Dither],
		// A mask is coverage and is already linear: it filters and blends, and a transfer function
		// over it lightens every edge.
		Semantics::Mask => &[Operation::Sample, Operation::Filter, Operation::Blend],
		// A normal filters - that is what a mipmap of one is for - and is never a colour.
		Semantics::Normal => &[Operation::Sample, Operation::Filter],
		Semantics::Data => &[Operation::Sample, Operation::Filter],
		// Depth is compared, not blended, and a filtered depth is a value at no surface.
		Semantics::Depth => &[Operation::Sample],
		// AN IDENTITY IS NEVER FILTERED. The average of two object ids is a third, which names a
		// different object - and the bug that follows is a click landing on the wrong thing.
		Semantics::Identity => &[Operation::Sample],
	}
}

/// What a `PixelStorage` may be used FOR, which the two arms do not share.
pub mod storage {
	/// `Known(PixelFormat)` - the mandatory list - is everything: a texture, a presentable surface,
	/// something a renderer samples, and something it draws into.
	pub const KNOWN: &str = "any image: sampled, filtered, drawn into, presented";
	/// `PackedRgbUnorm` exists for the BOOT AND SCANOUT ADAPTERS - the firmware hand-off and the
	/// terminal - and is not a texture format, not a presentable application surface format, and not
	/// something a renderer samples. Narrowing the model to fixed formats would reintroduce a defect
	/// this tree already fixed: a literal 32 where a firmware-reported 24- or 16-bit layout belonged
	/// gave a diagonal smear rather than a picture.
	pub const PACKED_RGB_UNORM: &str = "a boot or scanout adapter's destination only: never sampled, never a texture, never a presentable application surface";
}

/// How an image's bytes are laid out in an allocation, and what the bytes nobody draws contain.
pub mod allocation {
	/// A row starts at a multiple of this, so a vector store at the start of a row is aligned on every
	/// architecture this tree targets.
	pub const ROW_ALIGNMENT_BYTES: u32 = 16;
	/// PADDING IS PART OF THE IMAGE'S BYTES AND NOT PART OF THE IMAGE. It is never read as pixel
	/// content, and on EXPORT it is written as zero - so two exports of one image are the same bytes,
	/// which is what makes an image hashable and a golden comparison meaningful.
	pub const EXPORTED_PADDING: &str = "zero";
	/// An allocation whose size does not reach the minimum visible bytes is a typed refusal, computed
	/// with checked arithmetic - a width times a height times a byte count is exactly the product that
	/// overflows on a crafted layout.
	pub const SHORT_ALLOCATION: &str = "a typed refusal, from checked arithmetic";
	/// A zero extent is a refusal rather than an empty image: every consumer that divides by an
	/// extent would have to check, and one of them will not.
	pub const ZERO_EXTENT: &str = "a typed refusal";
}

/// The metadata an HDR transfer function needs beside the pixels.
///
/// WITHOUT IT A PQ IMAGE IS UNTONE-MAPPABLE. PQ is absolute, so a display that is dimmer than the
/// content has to know what the content's peak actually was - and "assume ten thousand" tone-maps
/// every image as though it were the brightest one ever made.
pub mod hdr_metadata {
	/// For PQ: the mastering display's luminance range, in cd/m², and the content light levels.
	pub const PQ: &str = "mastering display minimum and maximum luminance in cd/m², plus maximum content light level and maximum frame-average light level; absent metadata is a refusal to tone-map rather than an assumed peak";
	/// For HLG: the nominal peak the relative signal is referred to.
	pub const HLG: &str = "the nominal peak luminance in cd/m² the relative signal is referred to; its system gamma follows from that peak";
	/// And for everything else there is none, which is not missing metadata.
	pub const SDR: &str = "none: an SDR signal is relative to diffuse white, which the profile states";
}

/// THE GUARANTEED MINIMA: what a conforming implementation promises to be able to do.
///
/// A PROFILE WITH NO MINIMA PROMISES NOTHING. "Supports large images" is satisfied by an
/// implementation that refuses at 513 pixels, so an application written against the profile has to
/// discover the real limit by being refused - which it will do in front of a user. These are the
/// numbers an application may assume without asking.
pub mod minima {
	/// The largest image extent, in either direction, that must be accepted.
	pub const IMAGE_EXTENT: u32 = 16_384;
	/// The largest pitch that must be accepted, which is the widest image at the widest format plus
	/// room for alignment.
	pub const PITCH_BYTES: u32 = IMAGE_EXTENT * 16 + 64;
	/// How many planes a multi-plane image may have.
	pub const PLANES: u32 = 3;
	/// How many colour stops a gradient must accept, which is where a profile that said nothing
	/// leaves an author guessing.
	pub const GRADIENT_STOPS: u32 = 256;
}

/// THE CONFORMANCE TOLERANCES, so that "passes conformance" has a boundary rather than a judgement.
///
/// AN EXACT COMPARISON IS THE WRONG TEST FOR A TRANSCENDENTAL, and a judgement is not a test at all.
/// Every value below is the largest difference a conforming implementation may show from the
/// reference, in the units the entry is compared in - and each is stated because the alternative is
/// a reviewer deciding, once, differently each time.
pub mod tolerance {
	/// A transfer function and its inverse, applied in turn: the result against the input, as a
	/// fraction of the full range. Tight, because both directions are stated exactly.
	pub const TRANSFER_ROUND_TRIP: f64 = 1.0 / 65_536.0;
	/// A colour converted between two spaces and back, in linear light.
	pub const PRIMARY_ROUND_TRIP: f64 = 1.0 / 4_096.0;
	/// A tone-mapped value against the reference curve.
	pub const TONE_MAP: f64 = 1.0 / 4_096.0;
	/// A YUV image converted to RGB, in CODE VALUES at the source's own bit depth. One code value,
	/// because the matrix and the range are stated exactly and the only freedom left is rounding.
	pub const YUV_CODE_VALUES: f64 = 1.0;
	/// THE DITHER IS EXACT. Its matrix and its phase are both stated, so two implementations that
	/// disagree by anything disagree about the rule rather than about arithmetic.
	pub const DITHER: f64 = 0.0;
	/// A filtered sample against the reference filter, as a fraction of the full range.
	pub const FILTERED_SAMPLE: f64 = 1.0 / 1_024.0;
}

/// WHAT AN IMAGE'S BYTES ARE BEFORE ANYTHING DRAWS INTO THEM.
///
/// THE UNINITIALISED CASE IS A DISCLOSURE, not an aesthetic problem. A presentable image whose bytes
/// were never written shows whatever the allocator handed over - which in a system with a shared page
/// pool is somebody else's pixels - and the bug reads as a flicker rather than as a leak, so it is
/// not reported.
pub mod initialisation {
	/// A newly allocated image's CONTENT is undefined, and that is stated rather than left implied: a
	/// consumer that assumed zero would be right on most allocators and wrong on the one that matters.
	pub const NEW_IMAGE: &str = "undefined; a reader before a writer is a defect in the reader";
	/// A PRESENTABLE image must be fully written or cleared before its first present. Not "should":
	/// a present of an unwritten surface is a disclosure of whatever the pool held.
	pub const BEFORE_FIRST_PRESENT: &str = "fully written or cleared; presenting an unwritten surface is refused";
	/// Padding is never content, so it is not part of what must be written - and on EXPORT it is
	/// zeroed, which is what makes two exports of one image the same bytes.
	pub const PADDING: &str = "not content; never read, zeroed on export";
}

/// A format by name, for a report and a gate. A name the profile does not carry is `None` rather than
/// a default: a format nobody declared is not a format.
pub fn format_named(name: &str) -> Option<&'static Format> {
	FORMATS.iter().find(|entry| entry.name == name)
}

/// A colour space by name.
pub fn color_space(name: &str) -> Option<&'static ColorSpace> {
	COLOR_SPACES.iter().find(|entry| entry.name == name)
}

/// A primary set by name.
pub fn primaries(name: &str) -> Option<&'static Primaries> {
	PRIMARIES.iter().find(|entry| entry.name == name)
}

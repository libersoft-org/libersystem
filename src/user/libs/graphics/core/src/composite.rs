//! THE COMPOSITING ARITHMETIC, once, from the frozen equations.
//!
//! "SUPPORTS THE STANDARD BLEND MODES" IS NOT A CONTRACT. Two implementations satisfy that sentence
//! and disagree on `ColorBurn` at zero, on `SoftLight` below a quarter, and on all four of the
//! non-separable modes. Every equation here is READ from `graphics-profile`'s registry - the tables
//! are the specification and this is the code that implements them, not a second copy of the list.
//!
//! EVERYTHING IS PREMULTIPLIED AND LINEAR unless the caller states otherwise through its `Working`
//! space. Compositing in an encoded space is what makes a half-transparent black edge look grey
//! instead of dark.

use crate::pixel::Rgba;

/// A Porter-Duff operator, plus additive `Plus`.
///
/// THE FACTORS ARE THE OPERATOR: `co = as*Fa*Cs + ab*Fb*Cb` and `ao = as*Fa + ab*Fb` is the whole
/// definition, which is why each one is a pair of factors here rather than its own equation.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Operator {
	Clear,
	Src,
	Dst,
	#[default]
	SrcOver,
	DstOver,
	SrcIn,
	DstIn,
	SrcOut,
	DstOut,
	SrcAtop,
	DstAtop,
	Xor,
	/// NOT ONE OF PORTER AND DUFF'S TWELVE and here anyway: additive light is what a glow, a
	/// highlight and an emissive overlay are, and an implementation without it grows a private one.
	Plus,
}

/// Every operator, in the frozen registry's own order.
pub const ALL_OPERATORS: [Operator; 13] = [
	Operator::Clear,
	Operator::Src,
	Operator::Dst,
	Operator::SrcOver,
	Operator::DstOver,
	Operator::SrcIn,
	Operator::DstIn,
	Operator::SrcOut,
	Operator::DstOut,
	Operator::SrcAtop,
	Operator::DstAtop,
	Operator::Xor,
	Operator::Plus,
];

impl Operator {
	pub const fn name(self) -> &'static str {
		match self {
			Operator::Clear => "Clear",
			Operator::Src => "Src",
			Operator::Dst => "Dst",
			Operator::SrcOver => "SrcOver",
			Operator::DstOver => "DstOver",
			Operator::SrcIn => "SrcIn",
			Operator::DstIn => "DstIn",
			Operator::SrcOut => "SrcOut",
			Operator::DstOut => "DstOut",
			Operator::SrcAtop => "SrcAtop",
			Operator::DstAtop => "DstAtop",
			Operator::Xor => "Xor",
			Operator::Plus => "Plus",
		}
	}

	/// Its factors as the registry states them, for a document or a test to read.
	pub fn factors(self) -> (&'static str, &'static str) {
		let entry = graphics_profile::compositing::OPERATORS.iter().find(|entry| entry.name == self.name());
		match entry {
			Some(entry) => (entry.source_factor, entry.backdrop_factor),
			None => ("", ""),
		}
	}

	/// The pair `(Fa, Fb)` for a given source and backdrop alpha.
	fn coverage(self, source_alpha: f32, backdrop_alpha: f32) -> (f32, f32) {
		match self {
			Operator::Clear => (0.0, 0.0),
			Operator::Src => (1.0, 0.0),
			Operator::Dst => (0.0, 1.0),
			Operator::SrcOver => (1.0, 1.0 - source_alpha),
			Operator::DstOver => (1.0 - backdrop_alpha, 1.0),
			Operator::SrcIn => (backdrop_alpha, 0.0),
			Operator::DstIn => (0.0, source_alpha),
			Operator::SrcOut => (1.0 - backdrop_alpha, 0.0),
			Operator::DstOut => (0.0, 1.0 - source_alpha),
			Operator::SrcAtop => (backdrop_alpha, 1.0 - source_alpha),
			Operator::DstAtop => (1.0 - backdrop_alpha, source_alpha),
			Operator::Xor => (1.0 - backdrop_alpha, 1.0 - source_alpha),
			Operator::Plus => (1.0, 1.0),
		}
	}
}

/// A blend mode: what the source colour becomes before the operator composites it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BlendMode {
	#[default]
	Normal,
	Multiply,
	Screen,
	Overlay,
	Darken,
	Lighten,
	ColorDodge,
	ColorBurn,
	HardLight,
	SoftLight,
	Difference,
	Exclusion,
	/// The four that MIX CHANNELS, and therefore need a luminance and a saturation model.
	Hue,
	Saturation,
	Color,
	Luminosity,
}

/// Every blend mode, separable ones first, in the registry's own order.
pub const ALL_BLEND_MODES: [BlendMode; 16] = [
	BlendMode::Normal,
	BlendMode::Multiply,
	BlendMode::Screen,
	BlendMode::Overlay,
	BlendMode::Darken,
	BlendMode::Lighten,
	BlendMode::ColorDodge,
	BlendMode::ColorBurn,
	BlendMode::HardLight,
	BlendMode::SoftLight,
	BlendMode::Difference,
	BlendMode::Exclusion,
	BlendMode::Hue,
	BlendMode::Saturation,
	BlendMode::Color,
	BlendMode::Luminosity,
];

impl BlendMode {
	pub const fn name(self) -> &'static str {
		match self {
			BlendMode::Normal => "Normal",
			BlendMode::Multiply => "Multiply",
			BlendMode::Screen => "Screen",
			BlendMode::Overlay => "Overlay",
			BlendMode::Darken => "Darken",
			BlendMode::Lighten => "Lighten",
			BlendMode::ColorDodge => "ColorDodge",
			BlendMode::ColorBurn => "ColorBurn",
			BlendMode::HardLight => "HardLight",
			BlendMode::SoftLight => "SoftLight",
			BlendMode::Difference => "Difference",
			BlendMode::Exclusion => "Exclusion",
			BlendMode::Hue => "Hue",
			BlendMode::Saturation => "Saturation",
			BlendMode::Color => "Color",
			BlendMode::Luminosity => "Luminosity",
		}
	}

	pub const fn is_separable(self) -> bool {
		!matches!(self, BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity)
	}

	/// The equation as the registry states it.
	pub fn equation(self) -> &'static str {
		if self.is_separable() { graphics_profile::compositing::BLENDS.iter().find(|blend| blend.name == self.name()).map(|blend| blend.equation).unwrap_or("") } else { graphics_profile::compositing::NON_SEPARABLE_BLENDS.iter().find(|blend| blend.name == self.name()).map(|blend| blend.equation).unwrap_or("") }
	}
}

/// `B(Cb, Cs)` on UNPREMULTIPLIED channels in `0..=1`.
pub fn blend(mode: BlendMode, backdrop: [f32; 3], source: [f32; 3]) -> [f32; 3] {
	if mode.is_separable() {
		let mut out = [0.0f32; 3];
		for channel in 0..3 {
			out[channel] = separable(mode, backdrop[channel], source[channel]);
		}
		out
	} else {
		non_separable(mode, backdrop, source)
	}
}

fn separable(mode: BlendMode, backdrop: f32, source: f32) -> f32 {
	match mode {
		BlendMode::Normal => source,
		BlendMode::Multiply => backdrop * source,
		BlendMode::Screen => backdrop + source - backdrop * source,
		// Overlay is HardLight with the operands SWAPPED, which is the definition and not a
		// coincidence - writing it out twice is how the two come to disagree at the threshold.
		BlendMode::Overlay => separable(BlendMode::HardLight, source, backdrop),
		BlendMode::Darken => backdrop.min(source),
		BlendMode::Lighten => backdrop.max(source),
		// THE TWO WITH A DIVISION. Their endpoints are stated rather than left to whatever the
		// division produces, which is where implementations disagree.
		BlendMode::ColorDodge => {
			if backdrop <= 0.0 {
				0.0
			} else if source >= 1.0 {
				1.0
			} else {
				(backdrop / (1.0 - source)).min(1.0)
			}
		}
		BlendMode::ColorBurn => {
			if backdrop >= 1.0 {
				1.0
			} else if source <= 0.0 {
				0.0
			} else {
				1.0 - ((1.0 - backdrop) / source).min(1.0)
			}
		}
		BlendMode::HardLight => {
			if source <= 0.5 {
				separable(BlendMode::Multiply, backdrop, 2.0 * source)
			} else {
				separable(BlendMode::Screen, backdrop, 2.0 * source - 1.0)
			}
		}
		// SOFTLIGHT'S LOWER BRANCH IS THE ONE THAT IS GOT WRONG: `D(Cb)` is a piecewise function of
		// the BACKDROP, and the quarter threshold is on the backdrop and not on the source.
		BlendMode::SoftLight => {
			if source <= 0.5 {
				backdrop - (1.0 - 2.0 * source) * backdrop * (1.0 - backdrop)
			} else {
				let d = if backdrop <= 0.25 { ((16.0 * backdrop - 12.0) * backdrop + 4.0) * backdrop } else { sqrt_inline(backdrop) };
				backdrop + (2.0 * source - 1.0) * (d - backdrop)
			}
		}
		BlendMode::Difference => (backdrop - source).abs(),
		BlendMode::Exclusion => backdrop + source - 2.0 * backdrop * source,
		BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => source,
	}
}

/// `Lum(C)`, at the compositing specification's own FIXED coefficients.
///
/// FIXED IN EVERY COLOUR SPACE, which looks wrong and is right: using the destination space's
/// luminance would make `Luminosity` produce a different result for the same two colours depending on
/// which space they were tagged with, and the mode is defined as an operation on the NUMBERS.
fn luminance(colour: [f32; 3]) -> f32 {
	let (red, green, blue) = graphics_profile::compositing::model::LUMINANCE_COEFFICIENTS;
	colour[0] * red as f32 + colour[1] * green as f32 + colour[2] * blue as f32
}

fn saturation(colour: [f32; 3]) -> f32 {
	colour[0].max(colour[1]).max(colour[2]) - colour[0].min(colour[1]).min(colour[2])
}

/// THE GAMUT CLIP, which is the step the mode's name does not imply and which every implementation
/// that omits it gets wrong on saturated colours: luminance is preserved and the chroma reduced.
fn clip_color(colour: [f32; 3]) -> [f32; 3] {
	let light = luminance(colour);
	let low = colour[0].min(colour[1]).min(colour[2]);
	let high = colour[0].max(colour[1]).max(colour[2]);
	let mut out = colour;
	if low < 0.0 && light - low != 0.0 {
		for channel in out.iter_mut() {
			*channel = light + (*channel - light) * light / (light - low);
		}
	}
	if high > 1.0 && high - light != 0.0 {
		for channel in out.iter_mut() {
			*channel = light + (*channel - light) * (1.0 - light) / (high - light);
		}
	}
	// AND THE RESIDUE IS CLAMPED. The algebra lands a channel exactly on zero or one and floating
	// point lands it a few parts in a hundred million past, which is not a colour - and a negative
	// premultiplied channel propagates through every stage after this one.
	for channel in out.iter_mut() {
		*channel = channel.clamp(0.0, 1.0);
	}
	out
}

fn set_luminance(colour: [f32; 3], light: f32) -> [f32; 3] {
	let difference = light - luminance(colour);
	clip_color([colour[0] + difference, colour[1] + difference, colour[2] + difference])
}

/// `SetSat(C, s)`: the channels mapped onto `0..=s` BY THEIR ORDER, with the middle placed
/// proportionally - and a colour whose maximum equals its minimum becomes black, because there is no
/// direction to spread it in.
fn set_saturation(colour: [f32; 3], saturation: f32) -> [f32; 3] {
	let mut order = [0usize, 1, 2];
	// A three-element ordering written out: a sort here would be a comparator over floats, and the
	// NaN case of one of those is a panic in a compositor.
	if colour[order[0]] > colour[order[1]] {
		order.swap(0, 1);
	}
	if colour[order[1]] > colour[order[2]] {
		order.swap(1, 2);
	}
	if colour[order[0]] > colour[order[1]] {
		order.swap(0, 1);
	}
	let (low, middle, high) = (order[0], order[1], order[2]);
	let mut out = [0.0f32; 3];
	if colour[high] > colour[low] {
		out[middle] = (colour[middle] - colour[low]) * saturation / (colour[high] - colour[low]);
		out[high] = saturation;
	}
	out[low] = 0.0;
	out
}

fn non_separable(mode: BlendMode, backdrop: [f32; 3], source: [f32; 3]) -> [f32; 3] {
	match mode {
		BlendMode::Hue => set_luminance(set_saturation(source, saturation(backdrop)), luminance(backdrop)),
		BlendMode::Saturation => set_luminance(set_saturation(backdrop, saturation(source)), luminance(backdrop)),
		BlendMode::Color => set_luminance(source, luminance(backdrop)),
		BlendMode::Luminosity => set_luminance(backdrop, luminance(source)),
		_ => source,
	}
}

/// THE WHOLE COMPOSITE: blend, then the operator, on PREMULTIPLIED colours.
///
/// THE BLEND IS APPLIED TO THE UNPREMULTIPLIED COLOURS AND WEIGHTED BY THE BACKDROP'S ALPHA, which is
/// the part that is skipped by implementations that blend premultiplied values directly: a blend mode
/// is defined on colours, and where the backdrop is transparent there is no colour to blend with, so
/// the result falls back to the source.
pub fn composite(operator: Operator, mode: BlendMode, source: Rgba, backdrop: Rgba) -> Rgba {
	let straight = |colour: Rgba| -> [f32; 3] { if colour.alpha > 0.0 { [colour.red / colour.alpha, colour.green / colour.alpha, colour.blue / colour.alpha] } else { [0.0, 0.0, 0.0] } };
	let source_colour = straight(source);
	let blended = if matches!(mode, BlendMode::Normal) {
		source_colour
	} else {
		let backdrop_colour = straight(backdrop);
		let mixed = blend(mode, backdrop_colour, source_colour);
		let weight = backdrop.alpha;
		[
			source_colour[0] * (1.0 - weight) + mixed[0] * weight,
			source_colour[1] * (1.0 - weight) + mixed[1] * weight,
			source_colour[2] * (1.0 - weight) + mixed[2] * weight,
		]
	};
	let source = Rgba::new(blended[0] * source.alpha, blended[1] * source.alpha, blended[2] * source.alpha, source.alpha);
	let (source_factor, backdrop_factor) = operator.coverage(source.alpha, backdrop.alpha);
	Rgba::new(source.red * source_factor + backdrop.red * backdrop_factor, source.green * source_factor + backdrop.green * backdrop_factor, source.blue * source_factor + backdrop.blue * backdrop_factor, source.alpha * source_factor + backdrop.alpha * backdrop_factor)
}

/// THE SHARED SQUARE ROOT, AS A CALL THAT CANNOT BE INLINED AWAY.
///
/// `#[inline(never)]` IS A LIBRARY-GRAPH DECISION AND NOT AN OPTIMISATION ONE. `render2d` is a
/// separate shared library and this is the only thing it reads from here; left to the compiler,
/// x86_64 and aarch64 inlined the call and riscv64 emitted a dynamic import, so ONE image failed to
/// link with "import ... has no direct provider" while the other two were clean and a provider row
/// declared to fix it would have failed the opposite check on those two. Forcing the call makes the
/// three targets agree by construction. This crate's own hot paths do not pay for it: they call
/// `sqrt_inline` below, which is the same body.
#[inline(never)]
pub fn sqrt_f32(value: f32) -> f32 {
	sqrt_inline(value)
}

/// The same square root, inlinable, for this crate's own per-pixel paths.
///
/// THE ENCODE TABLE IS INDEXED BY IT, three times per pixel of every frame, which is what made the
/// four-iteration Newton version it replaced most of what a frame cost. A call there would be a
/// smaller regression than that one and still a regression measured work had removed.
#[inline]
pub(crate) fn sqrt_inline(value: f32) -> f32 {
	if !matches!(value.partial_cmp(&0.0), Some(core::cmp::Ordering::Greater)) {
		return 0.0;
	}
	// `libm`'s AND NOT FOUR NEWTON ITERATIONS, which is what this was.
	//
	// THE OLD ONE WAS FOUR SERIALLY DEPENDENT DIVISIONS. `estimate = 0.5 * (estimate + value /
	// estimate)`, four times, each waiting on the last - an f32 divide is a dozen cycles and cannot
	// be pipelined behind itself. That is fine in a corner and ruinous on the path it was actually
	// on: the encode table is indexed by the square root of its input, so EVERY CHANNEL OF EVERY
	// PIXEL of every frame ran it three times. Measured on the probe that isolates the tile round
	// trip, it was most of what a frame cost.
	//
	// AND IT IS MORE ACCURATE, not less: this is a correctly rounded square root where four
	// iterations from a bit-twiddled seed are an approximation, and on every target this system
	// builds for it lowers to the hardware instruction.
	libm::sqrtf(value)
}

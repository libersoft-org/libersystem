//! DEPTH AND STENCIL: the mapping from clip depth, the rounding, the packing, the clear, the
//! comparison, the bias and the stencil operations - each exactly as the profile freezes it.
//!
//! THE `Depth32F` QUESTION IS ANSWERED AND THIS IMPLEMENTS THE ANSWER. The stored floats are
//! compared and the incoming depth is NOT quantised: quantisation is what a normalised format does
//! because its storage cannot hold anything else, and imposing it on a float format throws away the
//! precision the format exists for. `DEPTH32F_ANSWER` in the profile is the sentence; `store` and
//! `compare` below are it in code.
//!
//! BIT-EXACT FOR EVERY FORMAT. `Depth16` and `Depth24` round once, by the rule below, and compare
//! stored integers; `Depth32F` compares floats. Nothing here is allowed a backend-specific rounding
//! or a fused multiply-add, because the conformance suite compares depth exactly.

use crate::error::Error;

/// A depth or depth-stencil format, as its own closed enumeration.
///
/// THE NAMES ARE THE PROFILE'S, and `name()` answers the string the frozen table is keyed by - so a
/// reader can go from a value here to the row that defines it without a second mapping.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DepthFormat {
	Depth16,
	Depth24,
	Depth32F,
	Depth24Stencil8,
	Depth32FStencil8,
}

pub const ALL_DEPTH_FORMATS: &[DepthFormat] = &[DepthFormat::Depth16, DepthFormat::Depth24, DepthFormat::Depth32F, DepthFormat::Depth24Stencil8, DepthFormat::Depth32FStencil8];

impl DepthFormat {
	pub const fn name(self) -> &'static str {
		match self {
			Self::Depth16 => "Depth16",
			Self::Depth24 => "Depth24",
			Self::Depth32F => "Depth32F",
			Self::Depth24Stencil8 => "Depth24Stencil8",
			Self::Depth32FStencil8 => "Depth32FStencil8",
		}
	}

	/// The row of the frozen table this format is defined by. A format this crate has and the
	/// profile does not would be a format nothing defines, which is what this lookup refuses.
	pub fn frozen(self) -> Result<&'static graphics_profile::render3d_spec::DepthFormat, Error> {
		graphics_profile::render3d_spec::DEPTH_FORMATS.iter().find(|entry| entry.name == self.name()).ok_or(Error::UnsupportedFormat { format: self.name(), used_as: "a depth attachment" })
	}

	/// How many bits the depth is stored in. `None` for the floating-point formats, whose precision
	/// is not a width.
	pub const fn normalised_bits(self) -> Option<u32> {
		match self {
			Self::Depth16 => Some(16),
			Self::Depth24 | Self::Depth24Stencil8 => Some(24),
			Self::Depth32F | Self::Depth32FStencil8 => None,
		}
	}

	pub const fn has_stencil(self) -> bool {
		matches!(self, Self::Depth24Stencil8 | Self::Depth32FStencil8)
	}

	/// The largest value the storage holds, for a normalised format.
	pub const fn normalised_maximum(self) -> Option<u32> {
		match self.normalised_bits() {
			Some(16) => Some(65_535),
			Some(24) => Some(16_777_215),
			_ => None,
		}
	}
}

/// A depth value as it is STORED, which is a different thing from a depth in `[0, 1]`.
///
/// TWO SHAPES AND NOT ONE NUMBER, because a normalised format compares INTEGERS and a float format
/// compares FLOATS - and a type that held both as `f32` would let a comparison be written once and
/// be wrong for one of them.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Stored {
	Normalised(u32),
	Float(f32),
}

/// Convert a fragment's depth in `[0, 1]` into what the format stores.
///
/// ROUNDED HALF AWAY FROM ZERO, ONCE. A normalised depth is `round(d * max)`, which is the rule the
/// stored range in the frozen table implies and which is stated here so two implementations cannot
/// round differently at a half. The input is clamped first: a depth outside `[0, 1]` has already
/// been clamped to the viewport's range by the caller, and clamping again costs nothing and removes
/// the case where a NaN becomes an arbitrary integer.
pub fn store(format: DepthFormat, depth: f32) -> Stored {
	let clamped = if depth.is_nan() { 0.0 } else { depth.clamp(0.0, 1.0) };
	match format.normalised_maximum() {
		Some(maximum) => Stored::Normalised(round_half_away(clamped * maximum as f32).min(maximum)),
		// NOT QUANTISED. See the note at the top of this file and `DEPTH32F_ANSWER`.
		None => Stored::Float(clamped),
	}
}

/// The stored value read back as an `f32` in `[0, 1]`.
///
/// THE PROFILE SAYS A READBACK ANSWERS CONVERTED VALUES AND NOT RAW INTEGERS, so a caller does not
/// have to know the storage to read a depth buffer.
pub fn readback(format: DepthFormat, stored: Stored) -> f32 {
	match (stored, format.normalised_maximum()) {
		(Stored::Normalised(value), Some(maximum)) => value as f32 / maximum as f32,
		(Stored::Float(value), _) => value,
		(Stored::Normalised(value), None) => value as f32,
	}
}

/// The comparison a depth or stencil test performs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompareOp {
	Never,
	Less,
	Equal,
	LessOrEqual,
	Greater,
	NotEqual,
	GreaterOrEqual,
	Always,
}

pub const ALL_COMPARE_OPS: &[CompareOp] = &[
	CompareOp::Never,
	CompareOp::Less,
	CompareOp::Equal,
	CompareOp::LessOrEqual,
	CompareOp::Greater,
	CompareOp::NotEqual,
	CompareOp::GreaterOrEqual,
	CompareOp::Always,
];

impl CompareOp {
	/// `incoming OP stored`, which is the order the profile states and the one a reader gets wrong
	/// half the time if it is not written down.
	pub fn test<T: PartialOrd + PartialEq>(self, incoming: T, stored: T) -> bool {
		match self {
			Self::Never => false,
			Self::Less => incoming < stored,
			Self::Equal => incoming == stored,
			Self::LessOrEqual => incoming <= stored,
			Self::Greater => incoming > stored,
			Self::NotEqual => incoming != stored,
			Self::GreaterOrEqual => incoming >= stored,
			Self::Always => true,
		}
	}
}

/// The depth test, performed in the format's own numeric space.
///
/// A NORMALISED FORMAT COMPARES INTEGERS AND A FLOAT FORMAT COMPARES FLOATS, which is the whole
/// reason `Stored` has two shapes. Comparing an incoming `f32` against a converted-back integer is a
/// different test at every value the conversion is not exact at.
pub fn depth_test(format: DepthFormat, op: CompareOp, incoming: f32, stored: Stored) -> bool {
	match (store(format, incoming), stored) {
		(Stored::Normalised(incoming), Stored::Normalised(stored)) => op.test(incoming, stored),
		(Stored::Float(incoming), Stored::Float(stored)) => op.test(incoming, stored),
		// A mismatch is a buffer read with the wrong format, which cannot happen through this API and
		// is answered rather than panicking.
		_ => false,
	}
}

/// Apply the depth bias, in the units the profile's equation states.
///
/// `offset = constant_factor * r + slope_factor * m`, where `m` is the maximum of `|dz/dx|` and
/// `|dz/dy|` over the primitive and `r` is the smallest representable difference at the format's
/// precision near this fragment's depth. ADDED AFTER THE DEPTH IS COMPUTED AND BEFORE THE TEST AND
/// THE WRITE, and the caller clamps to the viewport range after.
///
/// `r` IS WHY THE FORMAT IS A PARAMETER. For a normalised format it is one unit of storage, the same
/// everywhere; for a float format it is `2^(exponent(z) - 23)`, which changes across the buffer - so
/// a bias that used one number for both would be far too small near zero on a float buffer and far
/// too large near one.
pub fn bias(format: DepthFormat, depth: f32, maximum_slope: f32, constant_factor: f32, slope_factor: f32, clamp: f32) -> f32 {
	let r = match format.normalised_maximum() {
		Some(maximum) => 1.0 / maximum as f32,
		None => {
			// `2^(exponent(z) - 23)`, from the bits rather than from a logarithm.
			let magnitude = depth.abs();
			if magnitude == 0.0 || !magnitude.is_finite() {
				f32::from_bits(1) // the smallest subnormal, which is the step near zero
			} else {
				let exponent = ((magnitude.to_bits() >> 23) & 0xff) as i32 - 127;
				exponent_to_scale(exponent - 23)
			}
		}
	};
	let offset = constant_factor * r + slope_factor * maximum_slope;
	let bounded = if clamp > 0.0 {
		offset.clamp(-clamp, clamp)
	} else if clamp < 0.0 {
		offset.clamp(clamp, -clamp)
	} else {
		offset
	};
	depth + bounded
}

/// What a stencil test does to the stored value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StencilOp {
	Keep,
	Zero,
	Replace,
	/// Saturating: `255` stays `255` rather than wrapping to zero, which is what makes a counter
	/// that overflows stop rather than start again.
	IncrementClamp,
	DecrementClamp,
	Invert,
	IncrementWrap,
	DecrementWrap,
}

pub const ALL_STENCIL_OPS: &[StencilOp] = &[
	StencilOp::Keep,
	StencilOp::Zero,
	StencilOp::Replace,
	StencilOp::IncrementClamp,
	StencilOp::DecrementClamp,
	StencilOp::Invert,
	StencilOp::IncrementWrap,
	StencilOp::DecrementWrap,
];

impl StencilOp {
	pub fn apply(self, stored: u8, reference: u8) -> u8 {
		match self {
			Self::Keep => stored,
			Self::Zero => 0,
			Self::Replace => reference,
			Self::IncrementClamp => stored.saturating_add(1),
			Self::DecrementClamp => stored.saturating_sub(1),
			Self::Invert => !stored,
			Self::IncrementWrap => stored.wrapping_add(1),
			Self::DecrementWrap => stored.wrapping_sub(1),
		}
	}
}

/// One face's stencil state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StencilFace {
	pub compare: CompareOp,
	/// ANDed with both the reference and the stored value before the comparison.
	pub read_mask: u8,
	/// Which bits a stencil write may change.
	pub write_mask: u8,
	pub reference: u8,
	/// What to do when the STENCIL test fails.
	pub on_fail: StencilOp,
	/// What to do when the stencil test passed and the DEPTH test failed.
	pub on_depth_fail: StencilOp,
	/// What to do when both passed.
	pub on_pass: StencilOp,
}

impl Default for StencilFace {
	fn default() -> Self {
		Self { compare: CompareOp::Always, read_mask: 0xff, write_mask: 0xff, reference: 0, on_fail: StencilOp::Keep, on_depth_fail: StencilOp::Keep, on_pass: StencilOp::Keep }
	}
}

/// What one fragment did to the depth and stencil buffers.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Outcome {
	/// Whether the fragment survives to be written to the colour attachments.
	pub passed: bool,
	/// The stencil value to store, already masked. `None` when nothing is written.
	pub stencil: Option<u8>,
	/// The depth value to store. `None` when nothing is written.
	pub depth: Option<Stored>,
}

/// The whole fragment test, in the frozen ORDER: stencil first, then depth.
///
/// THE ORDER IS THE CONTRACT AND SO IS WHICH OPERATION EACH OUTCOME SELECTS: `on_fail` when the
/// stencil test fails, `on_depth_fail` when the stencil passed and the depth failed, `on_pass` when
/// both passed. An implementation that ran depth first would write a different stencil for every
/// fragment the two tests disagree about, which is most of the fragments a stencil is used for.
///
/// AND A WRITE HAPPENS ONLY WHEN THE TEST IT BELONGS TO PASSED. A fragment discarded by the fragment
/// stage writes neither, which the caller expresses by not calling this.
pub fn test(format: DepthFormat, depth_compare: CompareOp, depth_write: bool, incoming_depth: f32, stored_depth: Stored, stencil: Option<(&StencilFace, u8)>) -> Outcome {
	let (stencil_passed, stencil_state) = match stencil {
		Some((face, stored)) => (face.compare.test(face.reference & face.read_mask, stored & face.read_mask), Some((face, stored))),
		None => (true, None),
	};
	let depth_passed = stencil_passed && depth_test(format, depth_compare, incoming_depth, stored_depth);

	let stencil_result = stencil_state.map(|(face, stored)| {
		let operation = if !stencil_passed {
			face.on_fail
		} else if !depth_passed {
			face.on_depth_fail
		} else {
			face.on_pass
		};
		let updated = operation.apply(stored, face.reference);
		// THE WRITE MASK SELECTS BITS, not a whole value: the bits it does not cover keep what was
		// stored, which is what makes two passes able to use different halves of one buffer.
		(stored & !face.write_mask) | (updated & face.write_mask)
	});

	Outcome { passed: depth_passed, stencil: stencil_result, depth: if depth_passed && depth_write { Some(store(format, incoming_depth)) } else { None } }
}

/// A depth clear value, converted by the same rule a fragment's depth is.
pub fn clear_depth(format: DepthFormat, value: f32) -> Stored {
	store(format, value)
}

/// A stencil clear takes a `u32` and stores its low eight bits, which is what the profile says.
pub const fn clear_stencil(value: u32) -> u8 {
	(value & 0xff) as u8
}

fn round_half_away(value: f32) -> u32 {
	if !(value > 0.0) {
		return 0;
	}
	(value + 0.5) as u32
}

/// `2^exponent` for the range a depth bias reaches, without a maths library.
fn exponent_to_scale(exponent: i32) -> f32 {
	if exponent < -126 {
		// Subnormal territory: the smallest step there is the smallest subnormal itself.
		return f32::from_bits(1);
	}
	if exponent > 127 {
		return f32::INFINITY;
	}
	f32::from_bits(((exponent + 127) as u32) << 23)
}

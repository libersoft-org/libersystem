//! The numeric form every run value is in: 26.6 fixed point.
//!
//! ONE REPRESENTATION, FIXED, because two implementations of the same shaping must produce the same
//! bytes. A signed 32-bit value with six fractional bits is 1/64 of a pixel, which is the
//! granularity hinting-free subpixel positioning needs, and it is what offsets, advances, origins
//! and sizes are carried in.
//!
//! ROUNDING HAPPENS AT ONE PLACE AND IS ROUND-HALF-TO-EVEN: where a scaled font unit becomes a run
//! value, and nowhere else. Round-half-up would make the result depend on the sign of the value, and
//! rounding a second time somewhere downstream would make it depend on the order two libraries were
//! written in.
//!
//! OVERFLOW IS A REFUSAL AND NEVER A SATURATION. A line whose advance does not fit is one this layer
//! will not draw; saturating would hand the renderer a position that is silently wrong, which is the
//! failure that cannot be found from the picture.

/// The fractional bits: 1/64 of a pixel.
pub const FRACTIONAL_BITS: u32 = 6;

/// One unit of the fractional part, as a whole 26.6 value.
pub const ONE: Fixed266 = Fixed266(1 << FRACTIONAL_BITS);

/// A 26.6 fixed-point quantity: pixels times 64.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Fixed266(i32);

/// Why a value could not be expressed as a run value.
///
/// ITS OWN TYPE RATHER THAN `None`, so a refusal that reaches a caller says which of the two things
/// went wrong: a quantity outside 26.6 at all, or a sum of admissible quantities that left it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overflow {
	/// A scaled font unit that does not fit 26.6 - including a non-finite one.
	Value,
	/// Two admissible values whose sum does not fit; an advance accumulated along a line.
	Sum,
}

impl Fixed266 {
	pub const ZERO: Self = Self(0);

	/// The raw 26.6 encoding, which is what crosses a seam.
	pub const fn raw(self) -> i32 {
		self.0
	}

	/// A value from its raw 26.6 encoding.
	pub const fn from_raw(raw: i32) -> Self {
		Self(raw)
	}

	/// Whole pixels, exactly.
	pub const fn from_pixels(pixels: i16) -> Self {
		Self((pixels as i32) << FRACTIONAL_BITS)
	}

	/// THE ONE ROUNDING SITE: a scaled font unit becomes a run value here and nowhere else.
	///
	/// Round-half-to-even, computed on the integer pair rather than through a float, so the result
	/// does not depend on the host's floating-point behaviour. `units` is the value in font units
	/// and `units_per_em` the face's design grid; `size` is the em size in 26.6.
	pub fn from_font_units(units: i32, units_per_em: u16, size: Fixed266) -> Result<Self, Overflow> {
		if units_per_em == 0 {
			return Err(Overflow::Value);
		}
		// units * size / units_per_em, in 26.6 throughout: `size` is already scaled by 64, so the
		// product is the numerator of a 26.6 value and the division is the only place a fraction is
		// lost - which is where the rounding rule applies.
		let numerator = i64::from(units) * i64::from(size.0);
		let denominator = i64::from(units_per_em);
		let rounded = divide_round_half_to_even(numerator, denominator);
		match i32::try_from(rounded) {
			Ok(value) => Ok(Self(value)),
			Err(_) => Err(Overflow::Value),
		}
	}

	/// A sum that refuses rather than wraps or saturates.
	pub fn checked_add(self, other: Self) -> Result<Self, Overflow> {
		match self.0.checked_add(other.0) {
			Some(value) => Ok(Self(value)),
			None => Err(Overflow::Sum),
		}
	}

	/// The whole-pixel part, rounding toward negative infinity - the pixel a value lies in.
	pub const fn floor_pixels(self) -> i32 {
		self.0 >> FRACTIONAL_BITS
	}

	/// The fractional part, always in `0..64` however negative the value is. This is what a subpixel
	/// phase is quantised from.
	pub const fn fraction(self) -> u32 {
		(self.0 & ((1 << FRACTIONAL_BITS) - 1)) as u32
	}
}

/// Integer division with round-half-to-even, for a positive divisor.
///
/// WRITTEN OUT RATHER THAN LEFT TO A FLOAT. `(a as f64 / b as f64).round()` is round-half-away and
/// loses exactness above 2^53; this is exact for every input the caller can construct, and it
/// rounds the way the contract says at the tie.
fn divide_round_half_to_even(numerator: i64, denominator: i64) -> i64 {
	let negative = numerator < 0;
	let magnitude = numerator.unsigned_abs();
	let denominator = denominator.unsigned_abs();
	let quotient = magnitude / denominator;
	let remainder = magnitude % denominator;
	let twice = remainder * 2;
	let rounded = if twice > denominator || (twice == denominator && quotient % 2 == 1) { quotient + 1 } else { quotient };
	let rounded = rounded as i64;
	if negative { -rounded } else { rounded }
}

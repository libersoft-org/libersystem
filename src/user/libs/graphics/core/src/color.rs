//! THE COLOUR MODEL, computed from the frozen constants.
//!
//! NONE OF THE NUMBERS ARE HERE. The primaries, the white point, the Bradford matrix, the sRGB
//! segments and the PQ and HLG constants all live in `graphics-profile`'s registry, where they are
//! published, hashed and held to their own standards' identities by fixtures. This file is the
//! ARITHMETIC over them - the RGB-to-XYZ derivation, the adaptation, the transfer functions and their
//! inverses - and a second copy of a matrix here would be a second answer to a question the profile
//! already froze.

use graphics_profile::image::{self, Transfer};
use libm::{exp, log, pow, sqrt};

use crate::Error;

/// A named colour space: primaries, white point and transfer function.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ColorSpace {
	Srgb,
	SrgbLinear,
	DisplayP3,
	DisplayP3Linear,
	Rec2020,
	Rec2020Linear,
	Rec2020Pq,
	Rec2020Hlg,
}

/// Every space, in the registry's order.
pub const ALL_SPACES: [ColorSpace; 8] = [
	ColorSpace::Srgb,
	ColorSpace::SrgbLinear,
	ColorSpace::DisplayP3,
	ColorSpace::DisplayP3Linear,
	ColorSpace::Rec2020,
	ColorSpace::Rec2020Linear,
	ColorSpace::Rec2020Pq,
	ColorSpace::Rec2020Hlg,
];

impl ColorSpace {
	/// The registry name, which is the only name this space has.
	pub const fn name(self) -> &'static str {
		match self {
			ColorSpace::Srgb => "srgb",
			ColorSpace::SrgbLinear => "srgb-linear",
			ColorSpace::DisplayP3 => "display-p3",
			ColorSpace::DisplayP3Linear => "display-p3-linear",
			ColorSpace::Rec2020 => "rec2020",
			ColorSpace::Rec2020Linear => "rec2020-linear",
			ColorSpace::Rec2020Pq => "rec2020-pq",
			ColorSpace::Rec2020Hlg => "rec2020-hlg",
		}
	}

	fn described(self) -> &'static image::ColorSpace {
		image::color_space(self.name()).unwrap_or(&image::COLOR_SPACES[0])
	}

	pub fn transfer(self) -> Transfer {
		self.described().transfer
	}

	pub fn primaries(self) -> &'static image::Primaries {
		image::primaries(self.described().primaries).unwrap_or(&image::PRIMARIES[0])
	}

	/// Whether values in this space are already light.
	pub fn is_linear(self) -> bool {
		matches!(self.transfer(), Transfer::Linear)
	}

	/// The LINEAR space with these primaries, which is the space colours are composited in.
	///
	/// A WORKING SPACE IS A LINEAR ONE OR IT IS NOT A WORKING SPACE. Blending in an encoded space is
	/// the defect that makes a half-transparent black edge look grey, and the three Rec. 2020
	/// transfers - plain, PQ and HLG - all share one set of primaries, so all three work in the same
	/// linear space and differ only in how they are stored.
	pub const fn linear_counterpart(self) -> ColorSpace {
		match self {
			ColorSpace::Srgb | ColorSpace::SrgbLinear => ColorSpace::SrgbLinear,
			ColorSpace::DisplayP3 | ColorSpace::DisplayP3Linear => ColorSpace::DisplayP3Linear,
			ColorSpace::Rec2020 | ColorSpace::Rec2020Linear | ColorSpace::Rec2020Pq | ColorSpace::Rec2020Hlg => ColorSpace::Rec2020Linear,
		}
	}
}

/// Decode one channel from its encoded value to LINEAR light.
///
/// THE LINEAR SEGMENT OF sRGB IS THE PART THAT GETS DROPPED, and a pure power law is close enough to
/// look right and wrong enough that two implementations disagree in the darks - which is where
/// banding lives.
pub fn decode(transfer: Transfer, value: f64) -> f64 {
	match transfer {
		Transfer::Linear => value,
		Transfer::Srgb => {
			if value <= image::srgb::ENCODED_THRESHOLD {
				value / image::srgb::SLOPE
			} else {
				pow((value + image::srgb::OFFSET) / (1.0 + image::srgb::OFFSET), image::srgb::EXPONENT)
			}
		}
		Transfer::Pq => {
			// ABSOLUTE, so the result is a fraction of the format's ten thousand nits rather than of
			// diffuse white - which is the difference a caller must not have to remember.
			let powed = pow(value.max(0.0), 1.0 / image::pq::M2);
			let numerator = (powed - image::pq::C1).max(0.0);
			let denominator = image::pq::C2 - image::pq::C3 * powed;
			if denominator <= 0.0 { 0.0 } else { pow(numerator / denominator, 1.0 / image::pq::M1) }
		}
		Transfer::Hlg => {
			// The INVERSE of the OETF: HLG's forward direction is scene-referred, so decoding is the
			// half that is written out less often and got wrong more often.
			if value <= 0.5 { value * value / 3.0 } else { (exp((value - image::hlg::C) / image::hlg::A) + image::hlg::B) / 12.0 }
		}
	}
}

/// Encode one channel from linear light.
pub fn encode(transfer: Transfer, value: f64) -> f64 {
	match transfer {
		Transfer::Linear => value,
		Transfer::Srgb => {
			if value <= image::srgb::LINEAR_THRESHOLD {
				value * image::srgb::SLOPE
			} else {
				(1.0 + image::srgb::OFFSET) * pow(value, 1.0 / image::srgb::EXPONENT) - image::srgb::OFFSET
			}
		}
		Transfer::Pq => {
			let powed = pow(value.max(0.0), image::pq::M1);
			pow((image::pq::C1 + image::pq::C2 * powed) / (1.0 + image::pq::C3 * powed), image::pq::M2)
		}
		Transfer::Hlg => {
			if value <= image::hlg::SPLIT {
				sqrt(3.0 * value.max(0.0))
			} else {
				image::hlg::A * log(12.0 * value - image::hlg::B) + image::hlg::C
			}
		}
	}
}

/// A three-by-three matrix over linear light.
pub type Matrix3 = [[f64; 3]; 3];

/// The matrix taking a space's LINEAR RGB to XYZ, derived from its primaries and white point.
///
/// DERIVED AND NOT TABULATED. A tabulated matrix is a fourth place the primaries live, and the first
/// one somebody updates without the others; the derivation is six numbers of arithmetic over the
/// chromaticities the profile already froze.
pub fn rgb_to_xyz(space: ColorSpace) -> Result<Matrix3, Error> {
	let primaries = space.primaries();
	let column = |c: image::Chromaticity| -> Result<[f64; 3], Error> {
		if c.y == 0.0 {
			return Err(Error::UnknownColorSpace);
		}
		Ok([c.x / c.y, 1.0, (1.0 - c.x - c.y) / c.y])
	};
	let red = column(primaries.red)?;
	let green = column(primaries.green)?;
	let blue = column(primaries.blue)?;
	let white = column(primaries.white)?;
	// The per-primary scale that makes the three primaries sum to the white point.
	let basis: Matrix3 = [[red[0], green[0], blue[0]], [red[1], green[1], blue[1]], [red[2], green[2], blue[2]]];
	let inverse = invert(&basis).ok_or(Error::UnknownColorSpace)?;
	let scale = multiply_vector(&inverse, white);
	Ok([
		[red[0] * scale[0], green[0] * scale[1], blue[0] * scale[2]],
		[red[1] * scale[0], green[1] * scale[1], blue[1] * scale[2]],
		[red[2] * scale[0], green[2] * scale[1], blue[2] * scale[2]],
	])
}

/// The matrix converting LINEAR RGB in one space to LINEAR RGB in another, with Bradford adaptation
/// between their white points.
///
/// THE ADAPTATION IS SKIPPED WHEN THE WHITE POINTS MATCH, and that is not an optimisation: applying
/// an identity adaptation through a matrix and its inverse accumulates rounding, and every one of the
/// profile's spaces is D65 - so the common case would otherwise pay a conversion that changes nothing
/// except in the last bits.
pub fn convert(from: ColorSpace, to: ColorSpace) -> Result<Matrix3, Error> {
	let source = rgb_to_xyz(from)?;
	let destination = rgb_to_xyz(to)?;
	let to_rgb = invert(&destination).ok_or(Error::UnknownColorSpace)?;
	let source_white = from.primaries().white;
	let destination_white = to.primaries().white;
	if source_white == destination_white {
		return Ok(multiply(&to_rgb, &source));
	}
	let adaptation = bradford(source_white, destination_white)?;
	Ok(multiply(&to_rgb, &multiply(&adaptation, &source)))
}

/// The Bradford adaptation matrix between two white points, built from the frozen cone matrices.
fn bradford(from: image::Chromaticity, to: image::Chromaticity) -> Result<Matrix3, Error> {
	let white = |c: image::Chromaticity| -> Result<[f64; 3], Error> {
		if c.y == 0.0 {
			return Err(Error::UnknownColorSpace);
		}
		Ok([c.x / c.y, 1.0, (1.0 - c.x - c.y) / c.y])
	};
	let source = multiply_vector(&image::bradford::FORWARD, white(from)?);
	let destination = multiply_vector(&image::bradford::FORWARD, white(to)?);
	let mut scale: Matrix3 = [[0.0; 3]; 3];
	for index in 0..3 {
		if source[index] == 0.0 {
			return Err(Error::UnknownColorSpace);
		}
		scale[index][index] = destination[index] / source[index];
	}
	Ok(multiply(&image::bradford::INVERSE, &multiply(&scale, &image::bradford::FORWARD)))
}

pub fn multiply(left: &Matrix3, right: &Matrix3) -> Matrix3 {
	let mut out: Matrix3 = [[0.0; 3]; 3];
	for (row, slot) in out.iter_mut().enumerate() {
		for (column, value) in slot.iter_mut().enumerate() {
			*value = (0..3).map(|inner| left[row][inner] * right[inner][column]).sum();
		}
	}
	out
}

pub fn multiply_vector(matrix: &Matrix3, vector: [f64; 3]) -> [f64; 3] {
	let mut out = [0.0; 3];
	for (row, slot) in out.iter_mut().enumerate() {
		*slot = (0..3).map(|inner| matrix[row][inner] * vector[inner]).sum();
	}
	out
}

/// The inverse, or `None` for a singular matrix - which a primary set with two collinear primaries
/// would give, and which is a refusal rather than a division by zero somewhere later.
pub fn invert(matrix: &Matrix3) -> Option<Matrix3> {
	let [[a, b, c], [d, e, f], [g, h, i]] = *matrix;
	let determinant = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
	if determinant == 0.0 || !determinant.is_finite() {
		return None;
	}
	let scale = 1.0 / determinant;
	Some([
		[(e * i - f * h) * scale, (c * h - b * i) * scale, (b * f - c * e) * scale],
		[(f * g - d * i) * scale, (a * i - c * g) * scale, (c * d - a * f) * scale],
		[(d * h - e * g) * scale, (b * g - a * h) * scale, (a * e - b * d) * scale],
	])
}

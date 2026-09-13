//! THE MULTI-PLANE IMAGE MODEL: `NV12`, `I420` and `P010`, and the one path that draws them.
//!
//! WITHOUT IT A VIDEO PLAYER CONVERTS EVERY FRAME TO RGBA BEFORE ANYTHING WILL DRAW IT - a full-frame
//! conversion on the CPU, once per frame, for the one workload where that cost is least affordable.
//! A decoder, a camera and a hardware pipeline all hand over planes; what is missing is not a
//! conversion but a type that says what the planes ARE.
//!
//! IT IS A SEPARATE TYPE AND NOT A WIDER `ImageLayout`. A single-plane image would otherwise grow a
//! plane count, a subsampling factor, a chroma siting and a range that it can never use, and every
//! consumer of the common case would have to decide what those mean for it.
//!
//! AND THE ORDER OF OPERATIONS IS THE PROFILE'S, which is the part a recipe written for RGB gets
//! wrong: planes are reconstructed and the matrix applied FIRST, producing ENCODED RGB; only then
//! does transfer decoding, primary conversion and premultiplication happen. Applying an RGB sampling
//! recipe directly to YUV bytes decodes the transfer function of a signal that is not yet a colour.

use crate::Error;
use crate::color::ColorSpace;
use crate::geom::Extent2D;
use crate::pixel::Rgba;

/// A planar layout from the frozen registry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlanarFormat {
	/// Two planes: luma, then interleaved CbCr at half resolution on both axes.
	Nv12,
	/// Three planes: luma, then Cb, then Cr, each at half resolution on both axes.
	I420,
	/// `NV12`'s shape at ten bits, in the HIGH ten bits of a little-endian sixteen-bit word.
	P010,
}

/// Every planar format, in the registry's own order.
pub const ALL_PLANAR_FORMATS: [PlanarFormat; 3] = [PlanarFormat::Nv12, PlanarFormat::I420, PlanarFormat::P010];

impl PlanarFormat {
	pub const fn name(self) -> &'static str {
		match self {
			PlanarFormat::Nv12 => "NV12",
			PlanarFormat::I420 => "I420",
			PlanarFormat::P010 => "P010",
		}
	}

	/// Its entry in the frozen registry. Every property below is READ from there.
	pub fn described(self) -> &'static graphics_profile::image::YuvLayout {
		graphics_profile::image::YUV_LAYOUTS.iter().find(|layout| layout.name == self.name()).unwrap_or(&graphics_profile::image::YUV_LAYOUTS[0])
	}

	pub fn planes(self) -> usize {
		self.described().planes as usize
	}

	pub fn bits(self) -> u8 {
		self.described().bits
	}

	/// Bytes per luma sample: one at eight bits, two at ten.
	pub const fn luma_bytes(self) -> u32 {
		match self {
			PlanarFormat::Nv12 | PlanarFormat::I420 => 1,
			PlanarFormat::P010 => 2,
		}
	}

	/// How many chroma SAMPLES a chroma plane's row carries per pixel of that plane, which is two for
	/// an interleaved plane and one for a planar one.
	pub const fn chroma_samples_per_pixel(self) -> u32 {
		match self {
			PlanarFormat::Nv12 | PlanarFormat::P010 => 2,
			PlanarFormat::I420 => 1,
		}
	}

	pub fn subsampling(self) -> (u32, u32) {
		let (x, y) = self.described().subsampling;
		(x as u32, y as u32)
	}

	/// The minimum row bytes of one plane, by the registry's own pitch rule.
	pub fn minimum_row_bytes(self, plane: usize, extent: Extent2D) -> Option<u32> {
		let (horizontal, _) = self.subsampling();
		match (self, plane) {
			(_, 0) => extent.width.checked_mul(self.luma_bytes()),
			// AN INTERLEAVED CHROMA ROW IS TWICE ITS SAMPLE COUNT and a planar one is once, which is
			// the arithmetic that puts a decoder half a row out.
			(PlanarFormat::Nv12 | PlanarFormat::P010, 1) => extent.width.div_ceil(horizontal).checked_mul(self.chroma_samples_per_pixel())?.checked_mul(self.luma_bytes()),
			(PlanarFormat::I420, 1 | 2) => extent.width.div_ceil(horizontal).checked_mul(self.luma_bytes()),
			_ => None,
		}
	}

	/// The extent of one plane. AN ODD EXTENT'S CHROMA PLANE IS THE CEILING OF HALF THE LUMA EXTENT,
	/// which is the profile's own rule and the one that decides whether the last column exists.
	pub fn plane_extent(self, plane: usize, extent: Extent2D) -> Option<Extent2D> {
		let (horizontal, vertical) = self.subsampling();
		match plane {
			0 => Some(extent),
			plane if plane < self.planes() => Some(Extent2D::new(extent.width.div_ceil(horizontal).max(1), extent.height.div_ceil(vertical).max(1))),
			_ => None,
		}
	}
}

/// Which matrix takes this image's YUV to RGB.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum YuvMatrix {
	Bt601,
	Bt709,
	Bt2020,
}

pub const ALL_YUV_MATRICES: [YuvMatrix; 3] = [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020];

impl YuvMatrix {
	pub const fn name(self) -> &'static str {
		match self {
			YuvMatrix::Bt601 => "BT.601",
			YuvMatrix::Bt709 => "BT.709",
			YuvMatrix::Bt2020 => "BT.2020",
		}
	}

	/// The two coefficients every other one is derived from, READ from the registry.
	pub fn coefficients(self) -> (f32, f32) {
		let entry = graphics_profile::image::YUV_MATRICES.iter().find(|matrix| matrix.name == self.name()).unwrap_or(&graphics_profile::image::YUV_MATRICES[0]);
		(entry.kr as f32, entry.kb as f32)
	}
}

/// Whether the samples use the whole range of their bits.
///
/// LIMITED RANGE IS THE SINGLE COMMONEST CAUSE OF WASHED-OUT OR CRUSHED VIDEO, and it is not
/// detectable from the samples: an image that happens to use only the middle of its range is legal
/// full-range content, and one that uses all of it is legal limited-range content that clipped.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum YuvRange {
	#[default]
	Limited,
	Full,
}

/// A multi-plane image's description: what the planes are, what they mean, and where they sit.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MultiPlaneLayout {
	pub extent: Extent2D,
	pub format: PlanarFormat,
	pub matrix: YuvMatrix,
	pub range: YuvRange,
	/// The colour space the RGB comes out IN, which is the primaries and transfer function the
	/// content was authored against - the matrix is a channel mix and says nothing about either.
	pub color_space: ColorSpace,
	/// Each plane's pitch, in bytes.
	pub pitches: [u32; 3],
}

impl MultiPlaneLayout {
	/// Build a layout, CHECKING what a consumer would otherwise have to trust.
	///
	/// A MALFORMED PLANE IS A REFUSAL AND NOT A CLAMP. A chroma pitch half a row short is a decoder's
	/// arithmetic error, and accepting it draws a picture that shears - which looks like a codec bug
	/// and is not one.
	pub fn new(extent: Extent2D, format: PlanarFormat, matrix: YuvMatrix, range: YuvRange, color_space: ColorSpace, pitches: [u32; 3]) -> Result<Self, Error> {
		if extent.is_empty() {
			return Err(Error::ZeroExtent);
		}
		for plane in 0..format.planes() {
			let minimum = format.minimum_row_bytes(plane, extent).ok_or(Error::Overflow)?;
			if pitches.get(plane).copied().unwrap_or(0) < minimum {
				return Err(Error::PitchTooSmall);
			}
		}
		Ok(Self { extent, format, matrix, range, color_space, pitches })
	}

	/// What a borrowed CPU view of one plane needs.
	pub fn plane_visible_bytes(&self, plane: usize) -> Option<u64> {
		let extent = self.format.plane_extent(plane, self.extent)?;
		let row = self.format.minimum_row_bytes(plane, self.extent)? as u64;
		let rows = (extent.height as u64).checked_sub(1)?;
		rows.checked_mul(self.pitches.get(plane).copied()? as u64)?.checked_add(row)
	}
}

/// A borrowed multi-plane image.
pub struct MultiPlaneView<'a> {
	layout: MultiPlaneLayout,
	planes: [&'a [u8]; 3],
}

impl<'a> MultiPlaneView<'a> {
	/// Borrow planes as an image, or refuse.
	pub fn new(layout: MultiPlaneLayout, planes: [&'a [u8]; 3]) -> Result<Self, Error> {
		for (plane, bytes) in planes.iter().enumerate().take(layout.format.planes()) {
			let needed = layout.plane_visible_bytes(plane).ok_or(Error::Overflow)?;
			if (bytes.len() as u64) < needed {
				return Err(Error::BufferTooShort);
			}
		}
		Ok(Self { layout, planes })
	}

	pub fn layout(&self) -> &MultiPlaneLayout {
		&self.layout
	}

	/// One sample of one plane, as a code value.
	fn sample(&self, plane: usize, x: u32, y: u32, interleaved_offset: u32) -> Option<u16> {
		let extent = self.layout.format.plane_extent(plane, self.layout.extent)?;
		// THE FINAL SAMPLE IS REPLICATED rather than read past, which is the profile's own answer for
		// an odd extent and is what stops a crafted extent becoming an out-of-bounds read.
		let x = x.min(extent.width.saturating_sub(1));
		let y = y.min(extent.height.saturating_sub(1));
		let pitch = self.layout.pitches.get(plane).copied()? as usize;
		let bytes = self.layout.format.luma_bytes() as usize;
		let samples = if plane == 0 { 1 } else { self.layout.format.chroma_samples_per_pixel() };
		let start = y as usize * pitch + (x * samples + interleaved_offset) as usize * bytes;
		let data = self.planes.get(plane)?;
		match bytes {
			1 => data.get(start).map(|byte| *byte as u16),
			_ => {
				let pair = data.get(start..start + 2)?;
				// THE HIGH TEN BITS OF A LITTLE-ENDIAN SIXTEEN-BIT WORD; the low six are zero on write
				// and IGNORED on read, which is what makes a `P010` sample a ten-bit value and not a
				// sixteen-bit one that happens to be large.
				Some(u16::from_le_bytes([pair[0], pair[1]]) >> 6)
			}
		}
	}

	/// The luma and chroma code values at a pixel, with chroma RECONSTRUCTED bilinearly at the
	/// profile's own siting: left-sited horizontally, centre-sited vertically.
	fn code_values(&self, x: u32, y: u32) -> (f32, f32, f32) {
		let luma = self.sample(0, x, y, 0).unwrap_or(0) as f32;
		let (horizontal, vertical) = self.layout.format.subsampling();
		// LEFT-SITED HORIZONTALLY means the chroma sample sits on the even luma column, so the
		// fraction is which of the two luma columns this pixel is; CENTRE-SITED VERTICALLY means it
		// sits between the two luma rows, so the fraction is offset by half a chroma row.
		let chroma_x = x as f32 / horizontal as f32;
		let chroma_y = (y as f32 + 0.5) / vertical as f32 - 0.5;
		let (x0, y0) = (chroma_x as u32, if chroma_y > 0.0 { chroma_y as u32 } else { 0 });
		let fx = chroma_x - x0 as f32;
		let fy = if chroma_y > 0.0 { chroma_y - y0 as f32 } else { 0.0 };
		let chroma = |offset: u32| -> f32 {
			let (plane, interleaved) = match self.layout.format {
				PlanarFormat::Nv12 | PlanarFormat::P010 => (1, offset),
				PlanarFormat::I420 => (1 + offset as usize, 0),
			};
			let at = |x: u32, y: u32| self.sample(plane, x, y, interleaved).unwrap_or(0) as f32;
			let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
			let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
			top * (1.0 - fy) + bottom * fy
		};
		(luma, chroma(0), chroma(1))
	}

	/// One pixel, as ENCODED RGB in the image's own colour space - which is where the conversion from
	/// planes ENDS. What happens after it is the ordinary pipeline: decode the transfer function,
	/// convert primaries, premultiply.
	pub fn encoded_rgb(&self, x: u32, y: u32) -> Rgba {
		let (luma, cb, cr) = self.code_values(x, y);
		let bits = self.layout.format.bits();
		let (luma_range, chroma_range) = ranges(bits, self.layout.range);
		let y_normalised = (luma - luma_range.0 as f32) / (luma_range.1 as f32 - luma_range.0 as f32);
		let centre = (chroma_range.0 as f32 + chroma_range.1 as f32) * 0.5;
		let chroma_span = chroma_range.1 as f32 - chroma_range.0 as f32;
		let cb = (cb - centre) / chroma_span;
		let cr = (cr - centre) / chroma_span;
		let (kr, kb) = self.layout.matrix.coefficients();
		let kg = 1.0 - kr - kb;
		// THE INVERSE MATRIX, DERIVED FROM THE TWO COEFFICIENTS rather than tabulated: a tabulated
		// matrix is a second place the coefficients live, and the first one somebody updates without
		// the other.
		let red = y_normalised + 2.0 * (1.0 - kr) * cr;
		let blue = y_normalised + 2.0 * (1.0 - kb) * cb;
		let green = y_normalised - (2.0 * (1.0 - kb) * kb / kg) * cb - (2.0 * (1.0 - kr) * kr / kg) * cr;
		Rgba::new(red.clamp(0.0, 1.0), green.clamp(0.0, 1.0), blue.clamp(0.0, 1.0), 1.0)
	}
}

/// The code-value range of luma and chroma at a bit depth, from the frozen constants.
fn ranges(bits: u8, range: YuvRange) -> ((u16, u16), (u16, u16)) {
	use graphics_profile::image::yuv;
	match (bits, range) {
		(10, YuvRange::Limited) => (yuv::LIMITED_LUMA_10, yuv::LIMITED_CHROMA_10),
		(10, YuvRange::Full) => (yuv::FULL_10, yuv::FULL_10),
		(_, YuvRange::Limited) => (yuv::LIMITED_LUMA_8, yuv::LIMITED_CHROMA_8),
		(_, YuvRange::Full) => (yuv::FULL_8, yuv::FULL_8),
	}
}

/// Whether a crop lands where a crop may land.
///
/// A CROP MUST LAND ON A CHROMA SAMPLE, or the crop's own chroma is between two of them - which is
/// half a pixel of colour shift that appears only in the cropped copy.
pub fn crop_is_aligned(format: PlanarFormat, origin: (u32, u32), extent: Extent2D) -> bool {
	let (horizontal, vertical) = format.subsampling();
	origin.0.is_multiple_of(horizontal) && origin.1.is_multiple_of(vertical) && extent.width.is_multiple_of(horizontal) && extent.height.is_multiple_of(vertical)
}

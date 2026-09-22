//! THE ONE PIXEL PIPELINE: decode, premultiply, convert, encode, quantise.
//!
//! IT LIVES HERE BECAUSE FOUR LAYERS NEED IT AND THREE OF THEM HAD THEIR OWN. `pix` blends
//! straight-alpha bytes in an ENCODED space, a compositor needs linear light, a software renderer
//! needs both plus filtering, and a display path needs the conversion at the end - and each private
//! copy disagreed with the others about the stage ORDER, which is what decides whether a
//! half-transparent black edge comes out dark or grey.
//!
//! THE ORDER IS FIXED ONCE, HERE:
//!
//! ```text
//! decode transfer function -> straight to PREMULTIPLIED -> sample and filter -> blend in the
//! stated working space -> convert to the target's alpha mode -> encode transfer function ->
//! quantise with dithering
//! ```
//!
//! FILTERING PREMULTIPLIES BEFORE INTERPOLATING. Interpolating straight alpha pulls the colour of
//! fully transparent texels into the result, which rings dark halos around every transparent edge -
//! and it is invisible on opaque test images, so it ships.
//!
//! AND THE WORKING SPACE IS PART OF THE CONTRACT rather than an assumption. A caller that composites
//! encoded sRGB bytes is doing something this module can do and says so; a caller that wants light
//! says that instead. What is not available is leaving it unstated.

use crate::Error;
use crate::color::{self, ColorSpace, Matrix3};
use crate::format::{AlphaMode, PackedRgbLayout, PixelFormat, PixelStorage};
use crate::semantics::ImageSemantics;
use crate::view::{ImageView, ImageViewMut};

/// Four channels, as numbers rather than as bytes. What every stage above the storage works in.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rgba {
	pub red: f32,
	pub green: f32,
	pub blue: f32,
	pub alpha: f32,
}

impl Rgba {
	pub const TRANSPARENT: Self = Self { red: 0.0, green: 0.0, blue: 0.0, alpha: 0.0 };

	pub const fn new(red: f32, green: f32, blue: f32, alpha: f32) -> Self {
		Self { red, green, blue, alpha }
	}

	/// Scale every channel, which on a PREMULTIPLIED colour is what an opacity or a coverage is.
	pub fn scaled(self, factor: f32) -> Self {
		Self { red: self.red * factor, green: self.green * factor, blue: self.blue * factor, alpha: self.alpha * factor }
	}

	/// Channel-wise addition. NOT `core::ops::Add`, deliberately: an operator on a premultiplied
	/// colour would read as arithmetic that is meaningful for any two colours, and adding two
	/// premultiplied colours is meaningful only where a filter is accumulating weighted taps.
	pub fn plus(self, other: Self) -> Self {
		Self { red: self.red + other.red, green: self.green + other.green, blue: self.blue + other.blue, alpha: self.alpha + other.alpha }
	}
}

/// Where a colour's numbers live while it is being worked on.
///
/// THE TWO THAT EXIST IN THIS TREE. `LinearPremultiplied` is what a correct compositor uses and what
/// this profile's layers and filters are defined in; `EncodedStraight` is what `pix` does to
/// sRGB-encoded bytes, which is cheap, wrong in the darks, and the right answer for a boot console
/// that has no colour management to be wrong about. Naming the second is what stops it being the
/// silent default somewhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Working {
	LinearPremultiplied(ColorSpace),
	EncodedStraight(ColorSpace),
}

impl Working {
	/// The working space for a target in `space`: its own primaries, in LINEAR light.
	pub const fn linear(space: ColorSpace) -> Self {
		Working::LinearPremultiplied(space.linear_counterpart())
	}

	pub const fn space(self) -> ColorSpace {
		match self {
			Working::LinearPremultiplied(space) | Working::EncodedStraight(space) => space,
		}
	}

	pub const fn is_linear(self) -> bool {
		matches!(self, Working::LinearPremultiplied(_))
	}

	/// A LINEAR WORKING SPACE CARRYING AN ENCODED SPACE IS A CONTRADICTION, and one that would be
	/// found as a picture that is too dark rather than as an error: every decode would apply the
	/// transfer function and every encode would apply it again.
	pub fn validate(self) -> Result<(), Error> {
		match self {
			Working::LinearPremultiplied(space) if !space.is_linear() => Err(Error::UnknownColorSpace),
			_ => Ok(()),
		}
	}
}

/// Read one pixel's channels AS STORED, normalised to `0..=1` for the integer formats and left alone
/// for the float ones.
///
/// NO TRANSFER FUNCTION AND NO ALPHA MODE ARE APPLIED HERE. This is the storage layer and nothing
/// else; a caller that wants light uses a `Decoder`, and one that wants the bytes gets the bytes.
pub fn read(view: &ImageView<'_>, x: u32, y: u32) -> Option<Rgba> {
	let layout = view.layout();
	if x >= layout.extent.width || y >= layout.extent.height {
		return None;
	}
	let row = view.row(y)?;
	let bytes_per_pixel = layout.storage.bytes_per_pixel() as usize;
	let start = (x as usize).checked_mul(bytes_per_pixel)?;
	let pixel = row.get(start..start + bytes_per_pixel)?;
	match layout.storage {
		PixelStorage::Known(format) => Some(read_known(format, pixel)),
		PixelStorage::PackedRgbUnorm(packed) => Some(read_packed(packed, pixel)),
	}
}

/// Read a horizontal run of pixels, finding the row ONCE.
///
/// THE ROW LOOKUP IS THE COST, not the pixel. `row(y)` validates and computes the row's start and its
/// visible length; doing that per pixel is most of the time a conversion spends, and it answers the
/// same question for every pixel of the row.
pub fn read_row(view: &ImageView<'_>, x: u32, y: u32, out: &mut [Rgba]) {
	let layout = *view.layout();
	let Some(row) = view.row(y) else {
		out.fill(Rgba::TRANSPARENT);
		return;
	};
	let width = layout.storage.bytes_per_pixel() as usize;
	let start = (x as usize) * width;
	// THE STORAGE IS MATCHED ONCE FOR THE ROW AND NOT ONCE PER PIXEL, and the run is bounds-checked
	// once rather than per pixel.
	//
	// The loop used to ask `row.get(..)` and then match the storage enum for EVERY pixel - a branch,
	// a panic path and a second dispatch inside `read_known`, on the path that converts a whole
	// frame's worth of pixels twice per frame. A decision that is the same for every pixel of a row
	// belongs outside the loop; what is left is a fixed-size window the compiler can widen.
	let Some(run) = row.get(start..start + out.len() * width) else {
		out.fill(Rgba::TRANSPARENT);
		return;
	};
	// THE TWO FOUR-BYTE ORDERS ARE SPELT OUT, and every other format goes through the general path.
	//
	// WHY IT IS WORTH A SPECIAL CASE: `read_known` reads each channel with `get(index).copied()
	// .unwrap_or(0)`, which is a bounds check and a branch PER CHANNEL - four per pixel - and the
	// compiler cannot remove them, because `chunks_exact(width)` has a runtime chunk size and it
	// cannot prove index three is inside it. `chunks_exact(4)` with a literal has a chunk size the
	// compiler knows, so the checks go and the loop can be widened. These two are what every target
	// in this tree actually presents; the general path below is unchanged and still answers for the
	// rest.
	match layout.storage {
		PixelStorage::Known(PixelFormat::B8G8R8A8Unorm) if width == 4 => {
			for (slot, pixel) in out.iter_mut().zip(run.chunks_exact(4)) {
				*slot = Rgba::new(U8_NORMALISED[pixel[2] as usize], U8_NORMALISED[pixel[1] as usize], U8_NORMALISED[pixel[0] as usize], U8_NORMALISED[pixel[3] as usize]);
			}
		}
		PixelStorage::Known(PixelFormat::R8G8B8A8Unorm) if width == 4 => {
			for (slot, pixel) in out.iter_mut().zip(run.chunks_exact(4)) {
				*slot = Rgba::new(U8_NORMALISED[pixel[0] as usize], U8_NORMALISED[pixel[1] as usize], U8_NORMALISED[pixel[2] as usize], U8_NORMALISED[pixel[3] as usize]);
			}
		}
		PixelStorage::Known(format) => {
			for (slot, pixel) in out.iter_mut().zip(run.chunks_exact(width)) {
				*slot = read_known(format, pixel);
			}
		}
		PixelStorage::PackedRgbUnorm(packed) => {
			for (slot, pixel) in out.iter_mut().zip(run.chunks_exact(width)) {
				*slot = read_packed(packed, pixel);
			}
		}
	}
}

/// Write a horizontal run of pixels, finding the row once. The storage is matched once - see
/// `read_row`.
pub fn write_row(view: &mut ImageViewMut<'_>, x: u32, y: u32, values: &[Rgba]) {
	let layout = *view.layout();
	let width = layout.storage.bytes_per_pixel() as usize;
	let start = (x as usize) * width;
	let Some(row) = view.row_mut(y) else { return };
	let Some(run) = row.get_mut(start..start + values.len() * width) else { return };
	// The same two orders spelt out, for the reason `read_row` gives.
	match layout.storage {
		PixelStorage::Known(PixelFormat::B8G8R8A8Unorm) if width == 4 => {
			for (value, pixel) in values.iter().zip(run.chunks_exact_mut(4)) {
				pixel[0] = quantise_u8(value.blue);
				pixel[1] = quantise_u8(value.green);
				pixel[2] = quantise_u8(value.red);
				pixel[3] = quantise_u8(value.alpha);
			}
		}
		PixelStorage::Known(PixelFormat::R8G8B8A8Unorm) if width == 4 => {
			for (value, pixel) in values.iter().zip(run.chunks_exact_mut(4)) {
				pixel[0] = quantise_u8(value.red);
				pixel[1] = quantise_u8(value.green);
				pixel[2] = quantise_u8(value.blue);
				pixel[3] = quantise_u8(value.alpha);
			}
		}
		PixelStorage::Known(format) => {
			for (value, pixel) in values.iter().zip(run.chunks_exact_mut(width)) {
				write_known(format, pixel, *value);
			}
		}
		PixelStorage::PackedRgbUnorm(packed) => {
			for (value, pixel) in values.iter().zip(run.chunks_exact_mut(width)) {
				write_packed(packed, pixel, *value);
			}
		}
	}
}

/// Write one pixel's channels AS STORED, clamping and rounding by the profile's own rules.
pub fn write(view: &mut ImageViewMut<'_>, x: u32, y: u32, value: Rgba) -> Option<()> {
	let layout = *view.layout();
	if x >= layout.extent.width || y >= layout.extent.height {
		return None;
	}
	let bytes_per_pixel = layout.storage.bytes_per_pixel() as usize;
	let start = (x as usize).checked_mul(bytes_per_pixel)?;
	let row = view.row_mut(y)?;
	let pixel = row.get_mut(start..start + bytes_per_pixel)?;
	match layout.storage {
		PixelStorage::Known(format) => write_known(format, pixel, value),
		PixelStorage::PackedRgbUnorm(packed) => write_packed(packed, pixel, value),
	}
	Some(())
}

/// ONE PACKED PIXEL, read from the masks firmware described it with.
///
/// PUBLIC BECAUSE A BOOT CONSOLE AND A SCANOUT ADAPTER NEED EXACTLY THIS AND NOTHING ELSE AROUND IT.
/// They have a slice, an element size and three masks - not an image, not a layout and not a view -
/// and making them build one to write a pixel is what leaves a second, private packer behind in each
/// of them. That second packer is where the literal 32 that once gave a diagonal smear lived.
pub fn read_packed(packed: PackedRgbLayout, pixel: &[u8]) -> Rgba {
	let mut element = 0u64;
	for (index, byte) in pixel.iter().enumerate().take(8) {
		element |= (*byte as u64) << (index * 8);
	}
	let channel = |channel: crate::format::PackedChannel| -> f32 {
		if channel.bits == 0 {
			return 0.0;
		}
		let mask = (1u64 << channel.bits) - 1;
		((element >> channel.shift) & mask) as f32 / mask as f32
	};
	Rgba::new(channel(packed.red), channel(packed.green), channel(packed.blue), 1.0)
}

/// ONE PACKED PIXEL, written to the masks firmware described it with.
pub fn write_packed(packed: PackedRgbLayout, pixel: &mut [u8], value: Rgba) {
	let mut element = 0u64;
	let mut place = |channel: crate::format::PackedChannel, value: f32| {
		if channel.bits == 0 {
			return;
		}
		let mask = (1u64 << channel.bits) - 1;
		element |= (quantise(value, mask as f32) as u64 & mask) << channel.shift;
	};
	place(packed.red, value.red);
	place(packed.green, value.green);
	place(packed.blue, value.blue);
	// THE RESERVED BITS ARE WRITTEN WITH ALL BITS SET, which is what the format registry says and is
	// not the same as leaving them alone: a scanout that reads them as alpha shows a transparent
	// picture when they happen to be zero.
	if packed.reserved.bits != 0 {
		let mask = (1u64 << packed.reserved.bits) - 1;
		element |= mask << packed.reserved.shift;
	}
	for (index, byte) in pixel.iter_mut().enumerate().take(8) {
		*byte = (element >> (index * 8)) as u8;
	}
}

/// EVERY BYTE'S NORMALISED VALUE, computed once at build time instead of per channel per fetch.
///
/// `v as f32 / 255.0` IS A DIVISION, and Rust may not turn it into a multiply by the reciprocal
/// because the two are not the same number - measured, 126 of the 256 values differ in the last
/// place. So the division stayed, four of them per texel, on the path a BILINEAR sample walks four
/// times and a BICUBIC one sixteen times for every pixel of an image draw.
///
/// A TABLE IS THE SAME NUMBER BY CONSTRUCTION. Each entry is `v as f32 / 255.0` evaluated by the
/// same compiler that would have evaluated it at run time, so this is bit-identical and not an
/// approximation - which is the only kind of change this path is allowed.
const U8_NORMALISED: [f32; 256] = {
	let mut table = [0.0f32; 256];
	let mut index = 0usize;
	while index < 256 {
		table[index] = index as f32 / 255.0;
		index += 1;
	}
	table
};

/// One packed pixel's channels, for a caller that has already found the bytes.
///
/// PUBLIC BECAUSE THE SAMPLER PREPARES ITS OWN FETCH: it computes the offset from constants it took
/// once, which is the whole point, and then needs exactly this and nothing else.
pub fn read_known_pixel(format: PixelFormat, pixel: &[u8]) -> Rgba {
	read_known(format, pixel)
}

fn read_known(format: PixelFormat, pixel: &[u8]) -> Rgba {
	let u8_at = |index: usize| U8_NORMALISED[pixel.get(index).copied().unwrap_or(0) as usize];
	let u16_at = |index: usize| {
		let bytes = [pixel.get(index * 2).copied().unwrap_or(0), pixel.get(index * 2 + 1).copied().unwrap_or(0)];
		u16::from_le_bytes(bytes)
	};
	let f32_at = |index: usize| {
		let mut bytes = [0u8; 4];
		bytes.copy_from_slice(&pixel[index * 4..index * 4 + 4]);
		f32::from_le_bytes(bytes)
	};
	match format {
		PixelFormat::A8Unorm => Rgba::new(0.0, 0.0, 0.0, u8_at(0)),
		PixelFormat::R8Unorm => Rgba::new(u8_at(0), 0.0, 0.0, 1.0),
		PixelFormat::R8G8Unorm => Rgba::new(u8_at(0), u8_at(1), 0.0, 1.0),
		PixelFormat::B8G8R8X8Unorm => Rgba::new(u8_at(2), u8_at(1), u8_at(0), 1.0),
		PixelFormat::R8G8B8X8Unorm => Rgba::new(u8_at(0), u8_at(1), u8_at(2), 1.0),
		PixelFormat::B8G8R8A8Unorm => Rgba::new(u8_at(2), u8_at(1), u8_at(0), u8_at(3)),
		PixelFormat::R8G8B8A8Unorm => Rgba::new(u8_at(0), u8_at(1), u8_at(2), u8_at(3)),
		PixelFormat::R10G10B10A2Unorm => {
			let mut bytes = [0u8; 4];
			bytes.copy_from_slice(&pixel[0..4]);
			let element = u32::from_le_bytes(bytes);
			let ten = |shift: u32| ((element >> shift) & 0x3ff) as f32 / 1023.0;
			Rgba::new(ten(0), ten(10), ten(20), ((element >> 30) & 0x3) as f32 / 3.0)
		}
		PixelFormat::R16G16B16A16Unorm => Rgba::new(u16_at(0) as f32 / 65535.0, u16_at(1) as f32 / 65535.0, u16_at(2) as f32 / 65535.0, u16_at(3) as f32 / 65535.0),
		PixelFormat::R16G16B16A16Float => Rgba::new(half_to_f32(u16_at(0)), half_to_f32(u16_at(1)), half_to_f32(u16_at(2)), half_to_f32(u16_at(3))),
		// A DATA FORMAT IS NOT A COLOUR and reading it as one is how an object id becomes a pixel. It
		// is returned as its own bits so a caller that means to move them can, and the semantics type
		// is what refuses to sample it.
		PixelFormat::R32Uint => {
			let mut bytes = [0u8; 4];
			bytes.copy_from_slice(&pixel[0..4]);
			Rgba::new(u32::from_le_bytes(bytes) as f32, 0.0, 0.0, 1.0)
		}
		PixelFormat::R32G32B32A32Float => Rgba::new(f32_at(0), f32_at(1), f32_at(2), f32_at(3)),
	}
}

fn write_known(format: PixelFormat, pixel: &mut [u8], value: Rgba) {
	let put_u8 = |pixel: &mut [u8], index: usize, value: f32| {
		if let Some(byte) = pixel.get_mut(index) {
			*byte = quantise_u8(value);
		}
	};
	match format {
		PixelFormat::A8Unorm => put_u8(pixel, 0, value.alpha),
		PixelFormat::R8Unorm => put_u8(pixel, 0, value.red),
		PixelFormat::R8G8Unorm => {
			put_u8(pixel, 0, value.red);
			put_u8(pixel, 1, value.green);
		}
		PixelFormat::B8G8R8X8Unorm | PixelFormat::B8G8R8A8Unorm => {
			put_u8(pixel, 0, value.blue);
			put_u8(pixel, 1, value.green);
			put_u8(pixel, 2, value.red);
			// AN `X8` LANE IS WRITTEN OPAQUE rather than left alone: a scanout that reads it as alpha
			// shows nothing where the bytes happened to be zero.
			put_u8(pixel, 3, if matches!(format, PixelFormat::B8G8R8A8Unorm) { value.alpha } else { 1.0 });
		}
		PixelFormat::R8G8B8X8Unorm | PixelFormat::R8G8B8A8Unorm => {
			put_u8(pixel, 0, value.red);
			put_u8(pixel, 1, value.green);
			put_u8(pixel, 2, value.blue);
			put_u8(pixel, 3, if matches!(format, PixelFormat::R8G8B8A8Unorm) { value.alpha } else { 1.0 });
		}
		PixelFormat::R10G10B10A2Unorm => {
			let element = (quantise(value.red, 1023.0) as u32) | (quantise(value.green, 1023.0) as u32) << 10 | (quantise(value.blue, 1023.0) as u32) << 20 | (quantise(value.alpha, 3.0) as u32) << 30;
			pixel[0..4].copy_from_slice(&element.to_le_bytes());
		}
		PixelFormat::R16G16B16A16Unorm => {
			for (index, channel) in [value.red, value.green, value.blue, value.alpha].into_iter().enumerate() {
				let quantised = quantise(channel, 65535.0) as u16;
				pixel[index * 2..index * 2 + 2].copy_from_slice(&quantised.to_le_bytes());
			}
		}
		PixelFormat::R16G16B16A16Float => {
			for (index, channel) in [value.red, value.green, value.blue, value.alpha].into_iter().enumerate() {
				pixel[index * 2..index * 2 + 2].copy_from_slice(&f32_to_half(channel).to_le_bytes());
			}
		}
		PixelFormat::R32Uint => {
			let element = if value.red.is_finite() { value.red.clamp(0.0, u32::MAX as f32) as u32 } else { 0 };
			pixel[0..4].copy_from_slice(&element.to_le_bytes());
		}
		PixelFormat::R32G32B32A32Float => {
			for (index, channel) in [value.red, value.green, value.blue, value.alpha].into_iter().enumerate() {
				pixel[index * 4..index * 4 + 4].copy_from_slice(&channel.to_le_bytes());
			}
		}
	}
}

/// Float to an integer channel, by the profile's own rule: CLAMP, then round half AWAY from zero,
/// with a NaN becoming zero rather than whatever a cast produces.
pub fn quantise(value: f32, maximum: f32) -> f32 {
	if !value.is_finite() {
		// An infinity clamps to the extreme; a NaN is zero. Both are stated rather than left to a
		// language's cast, which is unspecified for exactly these two.
		return if value > 0.0 { maximum } else { 0.0 };
	}
	let scaled = (value * maximum).clamp(0.0, maximum);
	let rounded = if scaled >= 0.0 { (scaled + 0.5) as u32 as f32 } else { -((-scaled + 0.5) as u32 as f32) };
	rounded.clamp(0.0, maximum)
}

/// Float to an 8-bit channel, by the same rule and without the trip through `f32`.
///
/// THE BYTE WRITERS WANT A BYTE. Going through the general quantiser gives `f32 -> u32 -> f32 -> u8`
/// for every channel of every pixel, and three of those four conversions exist only because the
/// general form has to return a float for the wider formats.
pub fn quantise_u8(value: f32) -> u8 {
	if !value.is_finite() {
		return if value > 0.0 { 255 } else { 0 };
	}
	let scaled = value * 255.0;
	if scaled <= 0.0 {
		return 0;
	}
	if scaled >= 255.0 {
		return 255;
	}
	(scaled + 0.5) as u8
}

/// How many quantisation steps a channel of this storage has, or `None` when it is floating point -
/// which is the case where dithering is not merely unnecessary but wrong, because there is no step
/// to spread the error over.
pub fn quantisation_steps(storage: PixelStorage) -> Option<f32> {
	match storage {
		PixelStorage::Known(format) => match format {
			PixelFormat::A8Unorm | PixelFormat::R8Unorm | PixelFormat::R8G8Unorm | PixelFormat::B8G8R8X8Unorm | PixelFormat::R8G8B8X8Unorm | PixelFormat::B8G8R8A8Unorm | PixelFormat::R8G8B8A8Unorm => Some(255.0),
			PixelFormat::R10G10B10A2Unorm => Some(1023.0),
			PixelFormat::R16G16B16A16Unorm => Some(65535.0),
			PixelFormat::R16G16B16A16Float | PixelFormat::R32G32B32A32Float | PixelFormat::R32Uint => None,
		},
		PixelStorage::PackedRgbUnorm(packed) => {
			let bits = packed.red.bits.min(packed.green.bits).min(packed.blue.bits);
			(bits > 0).then(|| ((1u32 << bits) - 1) as f32)
		}
	}
}

/// The ORDERED dither offset for a target pixel, in units of one quantisation step.
///
/// THE PHASE IS ANCHORED TO THE TARGET'S ORIGIN and never to a tile's, which is the difference
/// between an invisible pattern and a visible seam at every tile boundary.
pub fn dither_offset(x: u32, y: u32) -> f32 {
	let matrix = graphics_profile::image::dither::MATRIX;
	let value = matrix[(y % 8) as usize][(x % 8) as usize] as f32;
	(value + 0.5) / 64.0 - 0.5
}

/// Half-precision to single. Written out because the storage format is half and the arithmetic is
/// not: every subnormal, infinity and NaN has to survive the trip for a round trip to mean anything.
pub fn half_to_f32(bits: u16) -> f32 {
	let sign = (bits as u32 & 0x8000) << 16;
	let exponent = (bits as u32 >> 10) & 0x1f;
	let mantissa = bits as u32 & 0x3ff;
	match exponent {
		0 if mantissa == 0 => f32::from_bits(sign),
		0 => {
			// A SUBNORMAL HALF IS A NORMAL SINGLE, and it is exactly `mantissa * 2^-24`. Computing it
			// as that product rather than by normalising the mantissa by hand is both shorter and the
			// version that is not off by one exponent - which is where a gradient's darkest band goes.
			let value = mantissa as f32 / 16_777_216.0;
			f32::from_bits(value.to_bits() | sign)
		}
		0x1f => f32::from_bits(sign | 0x7f80_0000 | (mantissa << 13)),
		_ => f32::from_bits(sign | ((exponent + 127 - 15) << 23) | (mantissa << 13)),
	}
}

/// Single to half, rounding to NEAREST EVEN - which is the rule the rest of this tree rounds by, and
/// leaving it as truncation would bias every accumulated colour downward.
pub fn f32_to_half(value: f32) -> u16 {
	let bits = value.to_bits();
	let sign = ((bits >> 16) & 0x8000) as u16;
	let exponent = ((bits >> 23) & 0xff) as i32;
	let mantissa = bits & 0x7f_ffff;
	if exponent == 0xff {
		// An infinity stays an infinity and a NaN stays a NaN with a non-zero mantissa, because a NaN
		// whose mantissa is truncated to zero becomes an infinity.
		let payload = if mantissa != 0 { 0x200 } else { 0 };
		return sign | 0x7c00 | payload;
	}
	let unbiased = exponent - 127 + 15;
	if unbiased >= 0x1f {
		return sign | 0x7c00;
	}
	if unbiased <= 0 {
		if unbiased < -10 {
			return sign;
		}
		let mantissa = mantissa | 0x80_0000;
		let shift = (14 - unbiased) as u32;
		let rounded = round_shift(mantissa, shift);
		return sign | rounded as u16;
	}
	let rounded = round_shift(mantissa, 13);
	let (exponent, rounded) = if rounded > 0x3ff { (unbiased + 1, 0) } else { (unbiased, rounded) };
	if exponent >= 0x1f {
		return sign | 0x7c00;
	}
	sign | ((exponent as u16) << 10) | rounded as u16
}

fn round_shift(value: u32, shift: u32) -> u32 {
	if shift >= 32 {
		return 0;
	}
	let dropped = value & ((1 << shift) - 1);
	let kept = value >> shift;
	let halfway = 1 << (shift - 1);
	if dropped > halfway || (dropped == halfway && kept & 1 == 1) { kept + 1 } else { kept }
}

/// THE TRANSFER FUNCTIONS AS TABLES, built once and shared by every conversion in a frame.
///
/// A POWER PER CHANNEL PER PIXEL IS THE WHOLE COST OF A CORRECT PIPELINE. A 640x480 frame decodes and
/// re-encodes nearly two million channels; at fifty nanoseconds for a `powf` that is a tenth of a
/// second, which is six frames' worth of budget spent on arithmetic whose answer is the same every
/// time. The exact functions stay exactly where the definition is - these tables are built FROM them.
///
/// THE ENCODE TABLE IS INDEXED BY THE SQUARE ROOT of the linear value, and that is the part that
/// makes it accurate rather than merely fast: `x^(1/2.4)` has an unbounded second derivative at zero,
/// so a table linear in `x` is worst exactly where dark colours live. In `u = sqrt(x)` the curve is
/// gentle everywhere, and a square root is one instruction.
pub struct TransferTable {
	transfer: graphics_profile::image::Transfer,
	decode: [f32; DECODE_ENTRIES + 1],
	encode: [f32; ENCODE_ENTRIES + 1],
}

const DECODE_ENTRIES: usize = 1024;
const ENCODE_ENTRIES: usize = 4096;

impl TransferTable {
	pub fn new(transfer: graphics_profile::image::Transfer) -> Self {
		let mut table = Self { transfer, decode: [0.0; DECODE_ENTRIES + 1], encode: [0.0; ENCODE_ENTRIES + 1] };
		for (index, slot) in table.decode.iter_mut().enumerate() {
			*slot = color::decode(transfer, index as f64 / DECODE_ENTRIES as f64) as f32;
		}
		for (index, slot) in table.encode.iter_mut().enumerate() {
			let u = index as f64 / ENCODE_ENTRIES as f64;
			*slot = color::encode(transfer, u * u) as f32;
		}
		table
	}

	pub fn transfer(&self) -> graphics_profile::image::Transfer {
		self.transfer
	}

	/// Encoded to linear. OUTSIDE `0..=1` THE EXACT FUNCTION IS USED, because an extended-range value
	/// is rare, is not what the table covers, and must not be silently clamped into it.
	pub fn decode(&self, value: f32) -> f32 {
		if !(value.is_finite() && (0.0..=1.0).contains(&value)) {
			return color::decode(self.transfer, value as f64) as f32;
		}
		let position = value * DECODE_ENTRIES as f32;
		let index = position as usize;
		let fraction = position - index as f32;
		let low = self.decode[index.min(DECODE_ENTRIES)];
		let high = self.decode[(index + 1).min(DECODE_ENTRIES)];
		low + (high - low) * fraction
	}

	/// Linear to encoded.
	pub fn encode(&self, value: f32) -> f32 {
		if !(value.is_finite() && (0.0..=1.0).contains(&value)) {
			return color::encode(self.transfer, value as f64) as f32;
		}
		let position = crate::composite::sqrt_inline(value) * ENCODE_ENTRIES as f32;
		let index = position as usize;
		let fraction = position - index as f32;
		let low = self.encode[index.min(ENCODE_ENTRIES)];
		let high = self.encode[(index + 1).min(ENCODE_ENTRIES)];
		low + (high - low) * fraction
	}
}

/// Turns an image's stored pixels into the working space.
///
/// BUILT ONCE PER IMAGE AND USED PER PIXEL, because the colour-space matrix is six numbers of
/// arithmetic over chromaticities and doing it per pixel is how a conversion comes to cost more than
/// the drawing.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Decoder {
	transfer: graphics_profile::image::Transfer,
	matrix: Option<Matrix3>,
	alpha: AlphaMode,
	working_is_linear: bool,
}

impl Decoder {
	/// The transfer function this decoder applies, so a caller can build the table it wants.
	pub fn transfer(&self) -> graphics_profile::image::Transfer {
		self.transfer
	}

	pub fn new(semantics: &ImageSemantics, working: Working) -> Result<Self, Error> {
		let space = semantics.color_space().ok_or(Error::NotColour)?;
		let alpha = semantics.alpha_mode().ok_or(Error::NotColour)?;
		working.validate()?;
		let matrix = conversion(space, working.space())?;
		Ok(Self { transfer: space.transfer(), matrix, alpha, working_is_linear: working.is_linear() })
	}

	/// Stage one and two, through a prepared table: the same answer, without a power per channel.
	pub fn decode_tabled(&self, table: &TransferTable, raw: Rgba) -> Rgba {
		self.decode_inner(raw, Some(table))
	}

	/// Stage one and two: decode the transfer function, then STRAIGHT TO PREMULTIPLIED.
	pub fn decode(&self, raw: Rgba) -> Rgba {
		self.decode_inner(raw, None)
	}

	/// A WHOLE RUN, WITH EVERY DECISION TAKEN ONCE.
	///
	/// `decode` answers for one pixel and every question it asks is the same for all of them: whether
	/// the working space is linear, what the alpha mode is, whether there is a colour matrix, and
	/// whether the table it was handed is for this transfer function. On a frame that decodes every
	/// pixel of every tile it touches, that is a dozen branches per pixel to re-derive what the
	/// encoder settled when it was built.
	///
	/// THE ANSWER IS IDENTICAL TO `decode` PIXEL FOR PIXEL, which is the property that matters: this
	/// is the same arithmetic in the same order with the conditions lifted out, and the conformance
	/// suite is what holds it to that.
	pub fn decode_row(&self, table: Option<&TransferTable>, values: &mut [Rgba]) {
		let tabled = match table {
			Some(table) if table.transfer() == self.transfer => Some(table),
			_ => None,
		};
		if !self.working_is_linear {
			for value in values.iter_mut() {
				*value = match self.alpha {
					AlphaMode::Opaque => Rgba::new(value.red, value.green, value.blue, 1.0),
					AlphaMode::Straight => Rgba::new(value.red * value.alpha, value.green * value.alpha, value.blue * value.alpha, value.alpha),
					AlphaMode::Premultiplied => *value,
				};
			}
			return;
		}
		let premultiplied = matches!(self.alpha, AlphaMode::Premultiplied);
		for value in values.iter_mut() {
			let raw = *value;
			let mut straight = raw;
			// AN OPAQUE PREMULTIPLIED PIXEL IS ALREADY STRAIGHT, and dividing it by one is three
			// divisions per pixel to compute the number that was already there. The encoder has had
			// this fast path since it was written - "the opaque case is the common one and needs no
			// division at all" - and the decoder did not. Bit-identical, because dividing by exactly
			// one is the identity and this skips only that case.
			if premultiplied && raw.alpha > 0.0 && raw.alpha < 1.0 {
				straight = Rgba::new(raw.red / raw.alpha, raw.green / raw.alpha, raw.blue / raw.alpha, raw.alpha);
			}
			let mut decoded = match tabled {
				Some(table) => Rgba::new(table.decode(straight.red), table.decode(straight.green), table.decode(straight.blue), straight.alpha),
				None => Rgba::new(color::decode(self.transfer, straight.red as f64) as f32, color::decode(self.transfer, straight.green as f64) as f32, color::decode(self.transfer, straight.blue as f64) as f32, straight.alpha),
			};
			if let Some(matrix) = &self.matrix {
				let converted = color::multiply_vector(matrix, [decoded.red as f64, decoded.green as f64, decoded.blue as f64]);
				decoded = Rgba::new(converted[0] as f32, converted[1] as f32, converted[2] as f32, decoded.alpha);
			}
			*value = match self.alpha {
				AlphaMode::Opaque => Rgba::new(decoded.red, decoded.green, decoded.blue, 1.0),
				AlphaMode::Straight | AlphaMode::Premultiplied => Rgba::new(decoded.red * decoded.alpha, decoded.green * decoded.alpha, decoded.blue * decoded.alpha, decoded.alpha),
			};
		}
	}

	fn decode_inner(&self, raw: Rgba, table: Option<&TransferTable>) -> Rgba {
		// A SOURCE THAT IS ALREADY THE WORKING FORMAT IS DECODED BY DOING NOTHING, and saying so is
		// worth a branch because of WHO asks: a pyramid's level zero is exactly this - linear light,
		// premultiplied, no primaries to convert - and a bicubic tap reads one sixteen times a pixel.
		// Without this the identity is computed the long way: divide three channels by alpha, call a
		// transfer function that returns its argument, through `f64` and back, then multiply the three
		// channels by alpha again.
		//
		// AND IT IS THE SAME NUMBER, which is what makes it a shortcut rather than a second answer.
		// The divide and the multiply exist because "a transfer function is not linear" - the comment
		// below says so - and they round-trip a value through `x / a * a` when there is no transfer to
		// correct for. Skipping the pair returns `raw` itself, which is the value that round trip is
		// approximating.
		if self.working_is_linear && matches!(self.transfer, graphics_profile::image::Transfer::Linear) && self.matrix.is_none() && matches!(self.alpha, AlphaMode::Premultiplied) {
			return raw;
		}
		let mut value = raw;
		if self.working_is_linear {
			// THE TRANSFER FUNCTION IS APPLIED TO THE COLOUR AND NOT TO THE COLOUR TIMES ALPHA, so a
			// premultiplied source has its alpha divided out first: a transfer function is not linear,
			// and decoding a premultiplied value gives a colour that is too dark by whatever the curve
			// bends - which is the halo around every antialiased edge on a premultiplied surface.
			if matches!(self.alpha, AlphaMode::Premultiplied) && raw.alpha > 0.0 {
				value = Rgba::new(raw.red / raw.alpha, raw.green / raw.alpha, raw.blue / raw.alpha, raw.alpha);
			}
			let straight = value;
			// THE TABLE IS MATCHED ONCE AND NOT ONCE PER CHANNEL. This is the single-value decode a
			// SAMPLER calls, four times per pixel for a bilinear tap and sixteen for a bicubic one,
			// so three redundant comparisons of a transfer enum per texel is three per channel per
			// texel of every image draw.
			match table {
				Some(table) if table.transfer() == self.transfer => {
					value.red = table.decode(straight.red);
					value.green = table.decode(straight.green);
					value.blue = table.decode(straight.blue);
				}
				_ => {
					value.red = color::decode(self.transfer, straight.red as f64) as f32;
					value.green = color::decode(self.transfer, straight.green as f64) as f32;
					value.blue = color::decode(self.transfer, straight.blue as f64) as f32;
				}
			}
			if let Some(matrix) = &self.matrix {
				let converted = color::multiply_vector(matrix, [value.red as f64, value.green as f64, value.blue as f64]);
				value = Rgba::new(converted[0] as f32, converted[1] as f32, converted[2] as f32, value.alpha);
			}
		}
		match self.alpha {
			AlphaMode::Opaque => Rgba::new(value.red, value.green, value.blue, 1.0),
			AlphaMode::Straight => Rgba::new(value.red * value.alpha, value.green * value.alpha, value.blue * value.alpha, value.alpha),
			// A PREMULTIPLIED SOURCE ALREADY CARRIES THE ALPHA and needs no second multiply. When the
			// working space is linear the channels were divided out before the transfer function was
			// applied above, so multiplying by alpha here is what puts them back.
			AlphaMode::Premultiplied => {
				if self.working_is_linear {
					Rgba::new(value.red * value.alpha, value.green * value.alpha, value.blue * value.alpha, value.alpha)
				} else {
					value
				}
			}
		}
	}
}

/// Turns a working-space colour into a target's stored pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Encoder {
	transfer: graphics_profile::image::Transfer,
	matrix: Option<Matrix3>,
	luminance: (f64, f64, f64),
	alpha: AlphaMode,
	working_is_linear: bool,
	steps: Option<f32>,
	/// One over the step count, so the dither offset is a multiply rather than a division per pixel.
	step: f32,
	/// Whether the target can hold values above one. A narrower target is TONE MAPPED rather than
	/// clipped, which is the difference between a bright window and a white rectangle.
	tone_map: bool,
	/// The luminance this encoder sends to one, relative to diffuse white: the DESTINATION'S when it
	/// reported one, and the profile's constant when it did not.
	tone_white: f64,
}

/// WHAT THE DESTINATION DISPLAY CAN ACTUALLY SHOW, in the terms a tone map needs.
///
/// A COLOUR SPACE NAME IS NOT ENOUGH TO TONE MAP WITH. The operator sends one luminance to one, and
/// WHICH luminance that is depends on the display: a two-thousand-nit highlight shown on a
/// two-hundred-nit panel and on a thousand-nit one are two different curves, and a conversion that
/// does not know the destination's luminance is guessing at the one number that decides how the
/// image looks.
///
/// EVERY FIELD IS OPTIONAL AND `None` IS NOT ZERO. A display that does not report its luminance is
/// the ordinary case - nothing in this system asks a panel yet - and the profile's rule for absent
/// HDR metadata is that it is a refusal to assume rather than a default to invent. What an unknown
/// destination gets is the profile's STATED reference white point, which is one assumption written
/// down in one place rather than one made per conversion.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct OutputLuminance {
	/// What a relative 1.0 is, in cd/m².
	pub sdr_white_nits: Option<f32>,
	pub min_nits: Option<f32>,
	/// The peak the display can show.
	pub max_nits: Option<f32>,
	/// The peak it can hold over a whole frame, which is lower than the peak on most panels.
	pub max_frame_average_nits: Option<f32>,
}

impl OutputLuminance {
	/// A destination that reported nothing.
	pub const UNKNOWN: Self = Self { sdr_white_nits: None, min_nits: None, max_nits: None, max_frame_average_nits: None };

	/// The luminance the tone map sends to 1.0, RELATIVE TO DIFFUSE WHITE.
	///
	/// `max / sdr_white` when the display reported both and the numbers can be believed. A peak below
	/// diffuse white, a zero or a non-finite value is a display description that cannot be true, and
	/// the answer there is the profile's constant rather than a curve computed from nonsense.
	pub fn tone_map_white(&self) -> f64 {
		match (self.sdr_white_nits, self.max_nits) {
			(Some(white), Some(max)) if white > 0.0 && white.is_finite() && max.is_finite() && max >= white => (max as f64) / (white as f64),
			_ => graphics_profile::image::tone_map::WHITE,
		}
	}
}

impl Encoder {
	/// The transfer function this encoder applies.
	pub fn transfer(&self) -> graphics_profile::image::Transfer {
		self.transfer
	}

	/// An encoder for a destination whose luminance is not known, which is the profile's own
	/// reference white point.
	pub fn new(semantics: &ImageSemantics, storage: PixelStorage, working: Working) -> Result<Self, Error> {
		Self::new_for_output(semantics, storage, working, OutputLuminance::UNKNOWN)
	}

	/// An encoder for a destination THAT SAID WHAT IT CAN SHOW.
	///
	/// THE DIFFERENCE IS THE ONE NUMBER THE TONE MAP TURNS ON. Mapping a highlight for a
	/// two-hundred-nit panel and for a thousand-nit one are different curves, and every conversion
	/// that did not ask used the same one.
	pub fn new_for_output(semantics: &ImageSemantics, storage: PixelStorage, working: Working, output: OutputLuminance) -> Result<Self, Error> {
		let space = semantics.color_space().ok_or(Error::NotColour)?;
		let alpha = semantics.alpha_mode().ok_or(Error::NotColour)?;
		working.validate()?;
		let matrix = conversion(working.space(), space)?;
		let steps = quantisation_steps(storage);
		Ok(Self { transfer: space.transfer(), matrix, luminance: luminance_coefficients(space)?, alpha, working_is_linear: working.is_linear(), steps, step: steps.map(|steps| 1.0 / steps).unwrap_or(0.0), tone_map: steps.is_some(), tone_white: output.tone_map_white() })
	}

	/// What this encoder maps to one, relative to diffuse white - the destination's own when it
	/// reported it, and the profile's constant when it did not.
	pub fn tone_map_white(&self) -> f64 {
		self.tone_white
	}

	/// The final stages: tone map where the target is narrower, convert, unpremultiply where the
	/// target wants straight alpha, encode, and add the ordered dither offset.
	///
	/// THE DITHER IS ADDED BEFORE QUANTISATION AND IN UNITS OF ONE STEP, which is what makes it a
	/// dither rather than noise: at eight bits it moves a value by at most half a level.
	/// The final stages, through a prepared table.
	pub fn encode_tabled(&self, table: &TransferTable, value: Rgba, x: u32, y: u32) -> Rgba {
		self.encode_inner(value, x, y, Some(table))
	}

	pub fn encode(&self, value: Rgba, x: u32, y: u32) -> Rgba {
		self.encode_inner(value, x, y, None)
	}

	/// A WHOLE RUN, WITH EVERY DECISION TAKEN ONCE - see `Decoder::decode_row` for why.
	///
	/// `x` is the first pixel's column, because the DITHER PHASE is the target's own x and y: a
	/// tile-relative phase makes the ordered pattern restart at every tile boundary, which is the
	/// artefact that looks like a seam.
	pub fn encode_row(&self, table: Option<&TransferTable>, values: &mut [Rgba], x: u32, y: u32) {
		let tabled = match table {
			Some(table) if table.transfer() == self.transfer => Some(table),
			_ => None,
		};
		// THE DITHER ROW IS THE SAME FOR EVERY PIXEL OF THE ROW, and it was recomputed for each of
		// them: `dither_offset` takes `y % 8` and `x % 8` and indexes a two-dimensional matrix, so a
		// row of a tile did sixty-four pairs of modulos to read eight numbers. `y` is constant here,
		// and `x` cycles with period eight - so the row's eight offsets are taken once and indexed.
		let dither = self.steps.is_some().then(|| {
			let row = graphics_profile::image::dither::MATRIX[(y % 8) as usize];
			let mut offsets = [0.0f32; 8];
			for (slot, cell) in offsets.iter_mut().zip(row.iter()) {
				*slot = ((*cell as f32 + 0.5) / 64.0 - 0.5) * self.step;
			}
			offsets
		});
		for (index, value) in values.iter_mut().enumerate() {
			let raw = *value;
			let alpha = if raw.alpha.is_finite() { raw.alpha.clamp(0.0, 1.0) } else { 0.0 };
			let mut colour = if alpha >= 1.0 {
				Rgba::new(raw.red, raw.green, raw.blue, 1.0)
			} else if alpha > 0.0 {
				let scale = 1.0 / alpha;
				Rgba::new(raw.red * scale, raw.green * scale, raw.blue * scale, alpha)
			} else {
				Rgba::new(0.0, 0.0, 0.0, 0.0)
			};
			if self.working_is_linear {
				if self.tone_map && (colour.red as f64 > KNEE || colour.green as f64 > KNEE || colour.blue as f64 > KNEE) {
					colour = tone_mapped(colour, self.luminance, self.tone_white);
				}
				if let Some(matrix) = &self.matrix {
					let converted = color::multiply_vector(matrix, [colour.red as f64, colour.green as f64, colour.blue as f64]);
					colour = Rgba::new(converted[0] as f32, converted[1] as f32, converted[2] as f32, colour.alpha);
				}
				colour = match tabled {
					Some(table) => Rgba::new(table.encode(colour.red), table.encode(colour.green), table.encode(colour.blue), colour.alpha),
					None => Rgba::new(color::encode(self.transfer, colour.red as f64) as f32, color::encode(self.transfer, colour.green as f64) as f32, color::encode(self.transfer, colour.blue as f64) as f32, colour.alpha),
				};
			}
			if let Some(offsets) = &dither {
				let offset = offsets[((x + index as u32) % 8) as usize];
				colour = Rgba::new(colour.red + offset, colour.green + offset, colour.blue + offset, colour.alpha);
			}
			*value = match self.alpha {
				AlphaMode::Premultiplied => Rgba::new(colour.red * alpha, colour.green * alpha, colour.blue * alpha, alpha),
				AlphaMode::Opaque => Rgba::new(colour.red, colour.green, colour.blue, 1.0),
				AlphaMode::Straight => colour,
			};
		}
	}

	fn encode_inner(&self, value: Rgba, x: u32, y: u32, table: Option<&TransferTable>) -> Rgba {
		// CLAMPED ONLY AT OUTPUT, which is here: the pipeline lets a value go above one so that an
		// additive highlight and a filter's overshoot survive the arithmetic, and the target is where
		// what it cannot hold is resolved.
		let alpha = if value.alpha.is_finite() { value.alpha.clamp(0.0, 1.0) } else { 0.0 };
		// UNPREMULTIPLY BEFORE TONE MAPPING AND BEFORE ENCODING. Tone mapping is an operation on
		// COLOUR, so applying it to a premultiplied value maps a half-transparent bright thing as
		// though it were a dim opaque one; and the transfer function is not linear, so dividing an
		// encoded colour by its alpha is not the inverse of multiplying a linear one.
		let mut colour = if alpha >= 1.0 {
			// THE OPAQUE CASE IS THE COMMON ONE and needs no division at all: a premultiplied colour
			// at full alpha is already the colour.
			Rgba::new(value.red, value.green, value.blue, 1.0)
		} else if alpha > 0.0 {
			// ONE RECIPROCAL AND THREE MULTIPLIES rather than three divisions, which on this path is
			// three pixels' worth of arithmetic saved per pixel.
			let scale = 1.0 / alpha;
			Rgba::new(value.red * scale, value.green * scale, value.blue * scale, alpha)
		} else {
			Rgba::new(0.0, 0.0, 0.0, 0.0)
		};
		if self.working_is_linear {
			// THE TONE MAP IS ONLY REACHED BY A COLOUR THAT NEEDS IT, and what "needs it" is the
			// profile's KNEE rather than one: below the knee the curve is the identity, so the
			// luminance dot product would compute a value that is already the answer. The bound was
			// ONE before the profile named a knee, and a curve that is not the identity at its bound
			// makes a guard like this a 47 per cent STEP - 1.000000 at a luminance of 1.0 and
			// 0.531280 at 1.0001, running through the middle of every lit surface.
			if self.tone_map && (colour.red as f64 > KNEE || colour.green as f64 > KNEE || colour.blue as f64 > KNEE) {
				colour = tone_mapped(colour, self.luminance, self.tone_white);
			}
			if let Some(matrix) = &self.matrix {
				let converted = color::multiply_vector(matrix, [colour.red as f64, colour.green as f64, colour.blue as f64]);
				colour = Rgba::new(converted[0] as f32, converted[1] as f32, converted[2] as f32, colour.alpha);
			}
			let encode = |channel: f32| match table {
				Some(table) if table.transfer() == self.transfer => table.encode(channel),
				_ => color::encode(self.transfer, channel as f64) as f32,
			};
			colour = Rgba::new(encode(colour.red), encode(colour.green), encode(colour.blue), colour.alpha);
		}
		if self.steps.is_some() {
			// THE DITHER IS ADDED BEFORE QUANTISATION AND IN UNITS OF ONE STEP, which is what makes it
			// a dither rather than noise: at eight bits it moves a value by at most half a level.
			let offset = dither_offset(x, y) * self.step;
			colour = Rgba::new(colour.red + offset, colour.green + offset, colour.blue + offset, colour.alpha);
		}
		// A PREMULTIPLIED TARGET HOLDS ENCODED VALUES SCALED BY ALPHA, which is what every
		// premultiplied surface in this tree means - not the encoding of a premultiplied number.
		match self.alpha {
			AlphaMode::Premultiplied => Rgba::new(colour.red * alpha, colour.green * alpha, colour.blue * alpha, alpha),
			AlphaMode::Opaque => Rgba::new(colour.red, colour.green, colour.blue, 1.0),
			AlphaMode::Straight => colour,
		}
	}
}

/// The matrix between two spaces, or `None` when there is nothing to do.
///
/// SAME PRIMARIES AND SAME WHITE POINT MEANS NO MATRIX AT ALL. `srgb` and `srgb-linear` differ only in
/// their transfer function, and deriving the conversion between them numerically gives a matrix that
/// is the identity to within rounding - which is not the same as no matrix: it is nine
/// double-precision multiplies PER PIXEL, on the commonest target there is, to compute the number
/// that was already there. The identity is recognised rather than applied.
fn conversion(from: ColorSpace, to: ColorSpace) -> Result<Option<Matrix3>, Error> {
	if from == to {
		return Ok(None);
	}
	let (from_primaries, to_primaries) = (from.primaries(), to.primaries());
	if from_primaries.red == to_primaries.red && from_primaries.green == to_primaries.green && from_primaries.blue == to_primaries.blue && from_primaries.white == to_primaries.white {
		return Ok(None);
	}
	Ok(Some(color::convert(from, to)?))
}

/// The luminance below which the profile's curve is the identity.
use graphics_profile::image::tone_map::KNEE;

/// The profile's tone map ON LUMINANCE, at this destination's white point.
///
/// ON LUMINANCE AND NOT PER CHANNEL, because a per-channel curve shifts hue - and it shifts it most
/// on exactly the saturated colours a wide-gamut image was made for.
///
/// THE CURVE IS THE PROFILE'S OWN FUNCTION AND NOT A COPY OF ITS FORMULA. It was the formula
/// written out here under a guard at a luminance of one, which made this the identity below diffuse
/// white and extended Reinhard above it - two functions meeting at a step. The profile names a KNEE
/// now, the curve is the identity below it and joins the shoulder there with the same slope, and
/// `scene3d` calls the same function so the two paths cannot drift.
fn tone_mapped(colour: Rgba, luminance: (f64, f64, f64), white: f64) -> Rgba {
	let light = colour.red as f64 * luminance.0 + colour.green as f64 * luminance.1 + colour.blue as f64 * luminance.2;
	// WRITTEN OUT BECAUSE EVERY COMPARISON WITH NaN IS FALSE: a NaN luminance must fall through
	// unmapped rather than be scaled by a NaN ratio. A luminance at or below the knee falls through
	// because the curve is the identity there, which also keeps the ratio below away from zero.
	if !matches!(light.partial_cmp(&KNEE), Some(core::cmp::Ordering::Greater)) {
		return colour;
	}
	let mapped = graphics_profile::image::tone_map::map_with(light, white);
	let scale = (mapped / light) as f32;
	Rgba::new(colour.red * scale, colour.green * scale, colour.blue * scale, colour.alpha)
}

/// A space's OWN luminance coefficients: the second row of its RGB-to-XYZ matrix.
///
/// THE DESTINATION'S AND NOT sRGB'S, which the tone-mapping rule states outright: a Rec. 2020
/// colour's luminance is not its sRGB luminance, and using one set of coefficients for every space
/// makes a wide-gamut image tone-map to the wrong brightness. They are DERIVED from the primaries the
/// profile froze rather than tabulated here, because a tabulated triple is a second place the
/// primaries live and the first one somebody updates without the other.
///
/// This is a different question from the four non-separable blend modes, whose `Lum` is the
/// compositing specification's own fixed triple in every space - that one is an operation on the
/// numbers, and this one is an operation on the light.
fn luminance_coefficients(space: ColorSpace) -> Result<(f64, f64, f64), Error> {
	let matrix = color::rgb_to_xyz(space)?;
	Ok((matrix[1][0], matrix[1][1], matrix[1][2]))
}

/// Copy one image into another, converting every stage on the way.
///
/// THE ONE CONVERSION PATH. A display's copy-and-scale, a screenshot's readback and a decoder's
/// hand-over are the same operation with different arguments, and each private copy of it disagreed
/// with the others about the stage order.
pub fn convert_image(source: &ImageView<'_>, target: &mut ImageViewMut<'_>, working: Working) -> Result<(), Error> {
	let decoder = Decoder::new(&source.layout().semantics, working)?;
	let encoder = Encoder::new(&target.layout().semantics, target.layout().storage, working)?;
	let extent = target.layout().extent;
	let source_extent = source.layout().extent;
	for y in 0..extent.height.min(source_extent.height) {
		for x in 0..extent.width.min(source_extent.width) {
			let Some(raw) = read(source, x, y) else { continue };
			let value = decoder.decode(raw);
			let encoded = encoder.encode(value, x, y);
			write(target, x, y, encoded);
		}
	}
	Ok(())
}

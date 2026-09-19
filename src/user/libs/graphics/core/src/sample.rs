//! THE ONE SAMPLER: nearest, bilinear, bicubic, a pyramid under them, and a bounded anisotropic tap.
//!
//! A GENERAL IMAGE RENDERER THAT ONLY HAS BILINEAR PRODUCES SHIMMERING THUMBNAILS, which is the most
//! visible defect a 2D library can ship with: a photograph scaled to an eighth samples one texel in
//! sixty-four, so the pixels that survive change as the image moves. The pyramid is what fixes it and
//! it belongs here rather than in a backend, because a compositor's scale and a renderer's image
//! pattern are the same operation.
//!
//! EVERY TEXEL IS DECODED AND PREMULTIPLIED BEFORE IT IS INTERPOLATED. Interpolating straight alpha
//! pulls the colour of fully transparent texels into the result and rings dark halos around
//! transparent edges - and it looks correct on every opaque test image, which is how it ships.

use alloc::vec::Vec;

use crate::Error;
use crate::color::ColorSpace;
use crate::format::{AlphaMode, PixelFormat, PixelStorage};
use crate::layout::{ImageLayout, RowOrigin};
use crate::owned::OwnedImage;
use crate::pixel::{Decoder, Rgba, TransferTable, Working, read, write};
use crate::semantics::ImageSemantics;
use crate::view::ImageView;

/// How an image is sampled.
///
/// THE MINIFICATION CHAIN IS PART OF THE ENUMERATION rather than a backend's private choice, because
/// two backends that disagree about whether a downscale is filtered disagree visibly.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Quality {
	/// Nearest neighbour: for pixel art, and for anything that must not be softened.
	Nearest,
	#[default]
	Bilinear,
	Bicubic,
	/// Bilinear with a pyramid under it, which is what a large downscale needs to stop aliasing.
	Mipmapped,
}

/// How sampling continues outside the image.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Spread {
	#[default]
	Clamp,
	Repeat,
	Mirror,
}

impl Spread {
	/// Map a texel coordinate into the image, which is what makes a pattern a pattern.
	pub fn wrap(self, coordinate: i64, extent: u32) -> Option<u32> {
		let extent = extent as i64;
		if extent <= 0 {
			return None;
		}
		let wrapped = match self {
			Spread::Clamp => coordinate.clamp(0, extent - 1),
			Spread::Repeat => coordinate.rem_euclid(extent),
			Spread::Mirror => {
				let period = extent * 2;
				let position = coordinate.rem_euclid(period);
				if position < extent { position } else { period - 1 - position }
			}
		};
		Some(wrapped as u32)
	}
}

/// Samples one image, in the working space, with a stated spread.
pub struct Sampler<'a> {
	source: Source<'a>,
	decoder: Decoder,
	spread: Spread,
	/// THE TRANSFER FUNCTION AS A TABLE, when the caller has one. A power per channel per texel is
	/// most of the cost of sampling an encoded image, and a bilinear tap reads four texels.
	table: Option<&'a TransferTable>,
	/// WHAT A PACKED TEXEL FETCH NEEDS, WORKED OUT ONCE. `None` for a planar source, which fetches
	/// through its own reconstruction.
	///
	/// THE ROW LOOKUP IS THE COST AND THIS LAYER ALREADY KNEW IT. `read_row`'s own documentation says
	/// "`row(y)` validates and computes the row's start and its visible length; doing that per pixel
	/// is most of the time a conversion spends" - and a sampler did exactly that per TEXEL, which a
	/// bilinear tap does four times and a bicubic sixteen times for every pixel it draws. Each one
	/// re-derived the row's start from the origin and the pitch, asked the STORAGE ENUM for the
	/// minimum row bytes, took two bounds-checked slices and matched the format again to read the
	/// channels. A sampler is built once per image per frame; all of that is the same answer every
	/// time.
	packed: Option<Packed>,
}

/// The constants a packed fetch needs, so it is an offset and a read.
#[derive(Clone, Copy)]
struct Packed {
	format: PixelFormat,
	pitch: usize,
	bytes_per_pixel: usize,
	top_left: bool,
	height: u32,
}

/// What a sampler is reading.
///
/// THE TWO SOURCES DIFFER ONLY IN HOW A TEXEL IS FETCHED. Everything after that - the transfer
/// function, the primaries, the premultiply, the filter and the pyramid - is the same code, which is
/// what "drawn through the same pipeline" has to mean if a video frame is to composite identically to
/// an image of it.
enum Source<'a> {
	Packed(ImageView<'a>),
	/// PLANES, reconstructed and matrixed to ENCODED RGB before anything else touches them. Applying
	/// an RGB sampling recipe directly to YUV bytes decodes the transfer function of a signal that is
	/// not yet a colour.
	Planar(crate::planar::MultiPlaneView<'a>),
}

impl<'a> Sampler<'a> {
	pub fn new(view: ImageView<'a>, working: Working, spread: Spread) -> Result<Self, Error> {
		Self::with_table(view, working, spread, None)
	}

	pub fn with_table(view: ImageView<'a>, working: Working, spread: Spread, table: Option<&'a TransferTable>) -> Result<Self, Error> {
		// A PACKED FIRMWARE LAYOUT IS NEVER SAMPLED. The profile says so and the refusal is here
		// rather than at each of the places that would otherwise have to remember.
		if !view.layout().storage.is_sampleable() {
			return Err(Error::UnknownFormat);
		}
		let decoder = Decoder::new(&view.layout().semantics, working)?;
		let layout = *view.layout();
		// A KNOWN FORMAT AND A DESCRIBABLE ROW, or no fast path at all - the general one below still
		// answers for everything this cannot prepare for.
		let packed = match (layout.storage, layout.storage.bytes_per_pixel(), layout.minimum_row_bytes()) {
			(PixelStorage::Known(format), bytes_per_pixel, Some(_)) if bytes_per_pixel > 0 => Some(Packed { format, pitch: layout.pitch as usize, bytes_per_pixel: bytes_per_pixel as usize, top_left: matches!(layout.origin, RowOrigin::TopLeft), height: layout.extent.height }),
			_ => None,
		};
		Ok(Self { source: Source::Packed(view), decoder, spread, table, packed })
	}

	/// A sampler over PLANES.
	///
	/// ITS DECODER IS BUILT FROM THE PLANAR LAYOUT'S OWN COLOUR SPACE, with straight alpha and an
	/// opaque image: what the plane reconstruction produces is encoded RGB in that space, which is
	/// exactly what the ordinary decoder's first stage expects.
	pub fn planar(view: crate::planar::MultiPlaneView<'a>, working: Working, spread: Spread, table: Option<&'a TransferTable>) -> Result<Self, Error> {
		let semantics = ImageSemantics::Color { color_space: view.layout().color_space, alpha_mode: AlphaMode::Opaque };
		let decoder = Decoder::new(&semantics, working)?;
		Ok(Self { source: Source::Planar(view), decoder, spread, table, packed: None })
	}

	/// The extent this sampler reads, whatever its source is.
	pub fn extent(&self) -> crate::geom::Extent2D {
		match &self.source {
			Source::Packed(view) => view.layout().extent,
			Source::Planar(view) => view.layout().extent,
		}
	}

	/// The single-plane layout, for a caller that knows it has one.
	pub fn layout(&self) -> Option<&ImageLayout> {
		match &self.source {
			Source::Packed(view) => Some(view.layout()),
			Source::Planar(_) => None,
		}
	}

	/// One texel, wrapped by the spread mode, decoded and premultiplied.
	pub fn texel(&self, x: i64, y: i64) -> Rgba {
		let extent = self.extent();
		let (Some(x), Some(y)) = (self.spread.wrap(x, extent.width), self.spread.wrap(y, extent.height)) else {
			return Rgba::TRANSPARENT;
		};
		let raw = match &self.source {
			// THE PREPARED FETCH: one offset and one bounds check - see `Sampler::packed`.
			Source::Packed(view) => match &self.packed {
				Some(packed) => {
					let row = if packed.top_left { y } else { packed.height.saturating_sub(1).saturating_sub(y) };
					let start = row as usize * packed.pitch + x as usize * packed.bytes_per_pixel;
					view.bytes().get(start..start + packed.bytes_per_pixel).map(|pixel| crate::pixel::read_known_pixel(packed.format, pixel))
				}
				None => read(view, x, y),
			},
			// RECONSTRUCT AND MATRIX FIRST, which produces the encoded RGB the decoder below takes -
			// the same decoder, applying the same transfer function and the same primaries.
			Source::Planar(view) => Some(view.encoded_rgb(x, y)),
		};
		match raw {
			Some(raw) => match self.table {
				Some(table) => self.decoder.decode_tabled(table, raw),
				None => self.decoder.decode(raw),
			},
			None => Rgba::TRANSPARENT,
		}
	}

	/// Sample at a point in TEXEL coordinates, where an integer coordinate is a texel's CORNER.
	///
	/// THE HALF-TEXEL IS PART OF THE CONTRACT. A sampler that treats an integer as a texel's centre
	/// shifts every scaled image by half a pixel, which is the offset that shows up as a blurry
	/// one-to-one blit.
	pub fn sample(&self, x: f32, y: f32, quality: Quality) -> Rgba {
		match quality {
			Quality::Nearest => self.texel(floor_i64(x), floor_i64(y)),
			Quality::Bilinear | Quality::Mipmapped => self.bilinear(x, y),
			Quality::Bicubic => self.bicubic(x, y),
		}
	}

	fn bilinear(&self, x: f32, y: f32) -> Rgba {
		let (x, y) = (x - 0.5, y - 0.5);
		let (x0, y0) = (floor_i64(x), floor_i64(y));
		let (fx, fy) = (x - x0 as f32, y - y0 as f32);
		let top = lerp(self.texel(x0, y0), self.texel(x0 + 1, y0), fx);
		let bottom = lerp(self.texel(x0, y0 + 1), self.texel(x0 + 1, y0 + 1), fx);
		lerp(top, bottom, fy)
	}

	fn bicubic(&self, x: f32, y: f32) -> Rgba {
		let (x, y) = (x - 0.5, y - 0.5);
		let (x0, y0) = (floor_i64(x), floor_i64(y));
		let (fx, fy) = (x - x0 as f32, y - y0 as f32);
		let mut rows = [Rgba::TRANSPARENT; 4];
		for (index, row) in rows.iter_mut().enumerate() {
			let sample_y = y0 - 1 + index as i64;
			let mut accumulated = Rgba::TRANSPARENT;
			for column in 0..4i64 {
				let weight = mitchell(column as f32 - 1.0 - fx);
				accumulated = accumulated.plus(self.texel(x0 - 1 + column, sample_y).scaled(weight));
			}
			*row = accumulated;
		}
		let mut out = Rgba::TRANSPARENT;
		for (index, row) in rows.iter().enumerate() {
			out = out.plus(row.scaled(mitchell(index as f32 - 1.0 - fy)));
		}
		// A CUBIC KERNEL OVERSHOOTS, which is what makes it look sharp, and a negative alpha or a
		// colour above its own alpha is not a premultiplied colour any more.
		clamp_premultiplied(out)
	}
}

/// A pyramid of progressively halved levels, in the canonical premultiplied linear float format.
///
/// BUILT ONCE IN `prepare` AND NEVER DURING A DRAW. It is the "no steady-state allocation" rule
/// meeting the fact that a pyramid is an allocation: it happens when the drawing is prepared, and
/// replaying the same drawing then allocates nothing.
pub struct Pyramid {
	levels: Vec<OwnedImage>,
}

impl Pyramid {
	/// THE BOX FILTER IS THE ONE THE PROFILE CAN STATE. Every level is the average of four texels of
	/// the level above, in PREMULTIPLIED LINEAR light - averaging encoded values makes a downscaled
	/// image darker than the original, by exactly the amount the transfer function bends.
	pub fn build(base: &ImageView<'_>, working: Working) -> Result<Self, Error> {
		Self::build_with(base, working, None)
	}

	/// Build, decoding the base level through a prepared table.
	pub fn build_with(base: &ImageView<'_>, working: Working, table: Option<&TransferTable>) -> Result<Self, Error> {
		let sampler = Sampler::with_table(ImageView::new(*base.layout(), base.bytes())?, working, Spread::Clamp, table)?;
		Self::from_sampler(&sampler, working)
	}

	/// Build LEVEL ZERO ALONE, decoding the base through a prepared table.
	pub fn base_with(base: &ImageView<'_>, working: Working, table: Option<&TransferTable>) -> Result<Self, Error> {
		let sampler = Sampler::with_table(ImageView::new(*base.layout(), base.bytes())?, working, Spread::Clamp, table)?;
		Self::base_from_sampler(&sampler, working)
	}

	/// LEVEL ZERO ALONE: the source decoded into the working format, with no chain under it.
	///
	/// A MAGNIFYING DRAW READS LEVEL ZERO AND NOTHING ELSE. Bilinear and bicubic sampling take the
	/// source at its own resolution, so the halvings below it would be prepare time and prepared
	/// memory that nothing reads - minification is their only reader, and `Mipmapped` is the quality
	/// that asks for it.
	pub fn base_from_sampler(sampler: &Sampler<'_>, working: Working) -> Result<Self, Error> {
		working.validate()?;
		let extent = sampler.extent();
		let mut levels: Vec<OwnedImage> = Vec::new();
		// LEVEL ZERO IS THE SOURCE IN THE WORKING FORMAT, so every level after it is filtered from
		// decoded light rather than from the source's own encoding - and so one sampling path serves
		// every level, including the base.
		let mut base_level = OwnedImage::new(pyramid_layout(extent, working.space())?)?;
		{
			let mut target = base_level.view_mut();
			for y in 0..extent.height {
				for x in 0..extent.width {
					write(&mut target, x, y, sampler.texel(x as i64, y as i64));
				}
			}
		}
		levels.push(base_level);
		Ok(Self { levels })
	}

	/// Build from any sampler, which is what lets a PLANAR source have a pyramid without a second
	/// filtering path: the levels are decoded light either way.
	pub fn from_sampler(sampler: &Sampler<'_>, working: Working) -> Result<Self, Error> {
		let mut pyramid = Self::base_from_sampler(sampler, working)?;
		let space = working.space();
		let mut extent = sampler.extent();
		let levels = &mut pyramid.levels;
		while extent.width > 1 || extent.height > 1 {
			let next = crate::geom::Extent2D { width: (extent.width / 2).max(1), height: (extent.height / 2).max(1) };
			let mut level = OwnedImage::new(pyramid_layout(next, space)?)?;
			{
				let previous = levels.last().ok_or(Error::Allocation)?.view();
				let source_extent = previous.layout().extent;
				let mut target = level.view_mut();
				for y in 0..next.height {
					for x in 0..next.width {
						let mut sum = Rgba::TRANSPARENT;
						let mut count = 0.0f32;
						for (offset_x, offset_y) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
							let (sample_x, sample_y) = (x * 2 + offset_x, y * 2 + offset_y);
							if sample_x >= source_extent.width || sample_y >= source_extent.height {
								continue;
							}
							// THE LEVELS ARE ALREADY DECODED AND PREMULTIPLIED, so the filter is a
							// plain average: this is the one place where reading the stored values
							// without a decoder is correct, and it is correct because the level above
							// was written by this same function.
							if let Some(texel) = read(&previous, sample_x, sample_y) {
								sum = sum.plus(texel);
								count += 1.0;
							}
						}
						if count > 0.0 {
							write(&mut target, x, y, sum.scaled(1.0 / count));
						}
					}
				}
			}
			levels.push(level);
			extent = next;
			// A PYRAMID HAS AS MANY LEVELS AS ITS LARGEST SIDE HAS HALVINGS, and 32 of them is an
			// image wider than four billion texels - so the bound is stated rather than trusted to
			// the loop's own arithmetic on a crafted extent.
			if levels.len() >= 32 {
				break;
			}
		}
		Ok(pyramid)
	}

	pub fn levels(&self) -> usize {
		self.levels.len()
	}

	pub fn level(&self, index: usize) -> Option<ImageView<'_>> {
		self.levels.get(index).map(|level| level.view())
	}

	/// Sample at a level of detail, interpolating BETWEEN the two levels around it.
	///
	/// TRILINEAR AND NOT NEAREST-LEVEL, because the point at which a nearest-level sampler switches
	/// levels is visible as a band across a gradient of scale - a ground plane, a zoom animation, a
	/// thumbnail grid at mixed sizes.
	pub fn sample(&self, x: f32, y: f32, level_of_detail: f32, spread: Spread) -> Rgba {
		let clamped = level_of_detail.clamp(0.0, (self.levels.len().saturating_sub(1)) as f32);
		let low = clamped as usize;
		let high = (low + 1).min(self.levels.len().saturating_sub(1));
		let fraction = clamped - low as f32;
		let sample_at = |index: usize| -> Rgba {
			let Some(view) = self.level(index) else { return Rgba::TRANSPARENT };
			let scale = 1.0 / (1u32 << index) as f32;
			// NO DECODER AND NO SAMPLER ARE BUILT HERE. A level of this pyramid is ALREADY in the
			// working space, premultiplied and linear - `build` put it there - so a texel is a read
			// and nothing else. Constructing a sampler per tap meant deriving a colour-space matrix,
			// with its Bradford adaptation, once per texel of every scaled image on the screen.
			bilinear_working(&view, x * scale, y * scale, spread)
		};
		if fraction <= 0.0 {
			return sample_at(low);
		}
		lerp(sample_at(low), sample_at(high), fraction)
	}

	/// THE ANISOTROPIC SAMPLE: several taps along the MAJOR axis of the sampled footprint, each at the
	/// level of detail the MINOR axis implies.
	///
	/// A PROJECTIVE TRANSFORM MAKES A SQUARE PIXEL INTO A LONG THIN QUADRILATERAL, and a single
	/// trilinear sample of it either blurs along the short axis or aliases along the long one. The tap
	/// count is BOUNDED, because the footprint of a pixel near the horizon is unbounded.
	pub fn sample_anisotropic(&self, x: f32, y: f32, x_step: (f32, f32), y_step: (f32, f32), spread: Spread, maximum_taps: u32) -> Rgba {
		let maximum_taps = maximum_taps.min(MAXIMUM_ANISOTROPIC_TAPS);
		let length = |step: (f32, f32)| crate::composite::sqrt_inline(step.0 * step.0 + step.1 * step.1);
		let (along_x, along_y) = (length(x_step), length(y_step));
		let (major, minor, step) = if along_x >= along_y { (along_x, along_y, x_step) } else { (along_y, along_x, y_step) };
		if !major.is_finite() || major <= 0.0 {
			return self.sample(x, y, 0.0, spread);
		}
		let ratio = if minor > 0.0 { (major / minor).clamp(1.0, maximum_taps.max(1) as f32) } else { maximum_taps.max(1) as f32 };
		let taps = (ratio as u32).max(1);
		let level_of_detail = log2_f32(if minor > 0.0 { minor } else { major / ratio }).max(0.0);
		let mut accumulated = Rgba::TRANSPARENT;
		for tap in 0..taps {
			// The taps are spread SYMMETRICALLY about the sample point, so the result does not drift
			// toward one end of the footprint as the tap count changes.
			let offset = (tap as f32 + 0.5) / taps as f32 - 0.5;
			accumulated = accumulated.plus(self.sample(x + step.0 * offset, y + step.1 * offset, level_of_detail, spread));
		}
		accumulated.scaled(1.0 / taps as f32)
	}
}

/// A bilinear tap over a level that is already in the working space.
fn bilinear_working(view: &ImageView<'_>, x: f32, y: f32, spread: Spread) -> Rgba {
	let extent = view.layout().extent;
	let texel = |x: i64, y: i64| -> Rgba {
		let (Some(x), Some(y)) = (spread.wrap(x, extent.width), spread.wrap(y, extent.height)) else { return Rgba::TRANSPARENT };
		read(view, x, y).unwrap_or(Rgba::TRANSPARENT)
	};
	let (x, y) = (x - 0.5, y - 0.5);
	let (x0, y0) = (floor_i64(x), floor_i64(y));
	let (fx, fy) = (x - x0 as f32, y - y0 as f32);
	let top = lerp(texel(x0, y0), texel(x0 + 1, y0), fx);
	let bottom = lerp(texel(x0, y0 + 1), texel(x0 + 1, y0 + 1), fx);
	lerp(top, bottom, fy)
}

fn pyramid_layout(extent: crate::geom::Extent2D, space: ColorSpace) -> Result<ImageLayout, Error> {
	let storage = PixelStorage::Known(PixelFormat::R16G16B16A16Float);
	let pitch = storage.minimum_row_bytes(extent.width).ok_or(Error::Overflow)?;
	let semantics = ImageSemantics::Color { color_space: space, alpha_mode: AlphaMode::Premultiplied };
	ImageLayout::new(extent, pitch, storage, RowOrigin::TopLeft, semantics)
}

/// THE BOUND ON THE MULTI-TAP APPROXIMATION.
///
/// FOUR, because the footprint of a pixel near a projective horizon is unbounded and the profile asks
/// for "anisotropic sampling OR a bounded multi-tap approximation" - this is the second, and the
/// bound is stated rather than left to whatever a caller passes. Each tap is a trilinear sample, so
/// four taps is thirty-two texels for one pixel.
pub const MAXIMUM_ANISOTROPIC_TAPS: u32 = 4;

/// The level of detail a scale implies: `log2` of how many texels one pixel covers.
pub fn level_of_detail(texels_per_pixel: f32) -> f32 {
	log2_f32(texels_per_pixel).max(0.0)
}

fn log2_f32(value: f32) -> f32 {
	if !(value.is_finite() && value > 0.0) {
		return 0.0;
	}
	// From the exponent and a polynomial on the mantissa: no math crate in this layer, and the
	// accuracy a level-of-detail needs is a fraction of a level.
	let bits = value.to_bits();
	let exponent = ((bits >> 23) & 0xff) as i32 - 127;
	let mantissa = f32::from_bits((bits & 0x007f_ffff) | 0x3f80_0000);
	let approximation = -0.344_845 * mantissa * mantissa + 2.024_658 * mantissa - 1.674_873;
	exponent as f32 + approximation
}

fn floor_i64(value: f32) -> i64 {
	if !value.is_finite() {
		return 0;
	}
	let truncated = value as i64;
	if value < truncated as f32 { truncated - 1 } else { truncated }
}

fn lerp(from: Rgba, to: Rgba, t: f32) -> Rgba {
	let t = t.clamp(0.0, 1.0);
	from.scaled(1.0 - t).plus(to.scaled(t))
}

/// A premultiplied colour whose channels exceed its alpha is not one. The cubic kernel's overshoot is
/// what produces them, and clamping here is what keeps the overshoot a sharpening rather than a
/// colour nothing can display.
fn clamp_premultiplied(colour: Rgba) -> Rgba {
	let alpha = colour.alpha.clamp(0.0, 1.0);
	Rgba::new(colour.red.clamp(0.0, alpha), colour.green.clamp(0.0, alpha), colour.blue.clamp(0.0, alpha), alpha)
}

fn mitchell(distance: f32) -> f32 {
	// MITCHELL-NETRAVALI AT B = C = 1/3, which is the cubic that the graphics literature settles on
	// for image resampling: a Catmull-Rom ringing less, a B-spline blurring less.
	let (b, c) = (1.0 / 3.0f32, 1.0 / 3.0f32);
	let x = distance.abs();
	let value = if x < 1.0 {
		(12.0 - 9.0 * b - 6.0 * c) * x * x * x + (-18.0 + 12.0 * b + 6.0 * c) * x * x + (6.0 - 2.0 * b)
	} else if x < 2.0 {
		(-b - 6.0 * c) * x * x * x + (6.0 * b + 30.0 * c) * x * x + (-12.0 * b - 48.0 * c) * x + (8.0 * b + 24.0 * c)
	} else {
		0.0
	};
	value / 6.0
}

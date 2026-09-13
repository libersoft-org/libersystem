//! GLYPHS: every kind the profile lists, a bounded cache, and the mask cache under it.
//!
//! THE CACHE KEY IS THE ONE `font-contract` DEFINES and this file does not restate it. It is eleven
//! fields because each of them distinguishes two glyphs that would otherwise share an entry - the
//! glyph index, the face's index within a collection, the variation coordinates, the strike, the
//! palette and the rasterisation mode were each a stale-pixel bug before they were fields. Linking
//! the type is what makes "neither side may restate it" a property of the build.
//!
//! THE OUTLINES COME FROM A PROVIDER AND NOT FROM A FONT PARSER HERE. A rasteriser that opened font
//! files would be a text stack with a rasteriser attached; what this needs is the decoded form for a
//! key, and who decodes it is the caller's business.
//!
//! AND AN LCD MASK IS FILTERED BY THE FROZEN FIVE-TAP FIR. Without a filter, subpixel coverage
//! produces coloured fringes on every stem; with a different filter it produces different fringes,
//! which is why the taps are the profile's and not this file's.

use alloc::vec::Vec;

use font_contract::cache::GlyphCacheKey;
use font_contract::glyph::{GlyphKind, RasterisationMode};
use graphics_core::OwnedImage;
use graphics_core::pixel::Rgba;
use render2d::paint::Color;
use render2d::path::Path;

/// One decoded glyph, in the form its kind implies.
///
/// POSITIONED RELATIVE TO THE PEN, in device pixels, with the subpixel phase from the key already
/// applied. A provider that returned font units would make every consumer scale them, and two
/// consumers that scale differently disagree about where a glyph sits by a fraction of a pixel -
/// which is exactly what the phase in the key exists to pin down.
pub enum GlyphImage {
	/// An outline to fill, in device pixels relative to the pen.
	Outline(Path),
	/// A coverage mask. `left` and `top` are its offset from the pen.
	Mask { left: i32, top: i32, width: u32, height: u32, coverage: Vec<u8>, mode: RasterisationMode },
	/// A bitmap strike, which is already colour and is composited rather than filled.
	Bitmap { left: i32, top: i32, image: OwnedImage },
	/// `COLR` layers: outlines with their own palette colours, drawn in order.
	Layers(Vec<(Path, Color)>),
	/// A glyph the provider has no form for. IT DRAWS NOTHING rather than a substitute box: a
	/// renderer inventing a glyph is a renderer lying about what the text says.
	Missing,
}

impl GlyphImage {
	/// What holding this costs, which is what the cache's bound is measured in.
	pub fn bytes(&self) -> u64 {
		match self {
			GlyphImage::Outline(path) => (core::mem::size_of_val(path.points()) + path.verbs().len()) as u64,
			GlyphImage::Mask { coverage, .. } => coverage.len() as u64,
			GlyphImage::Bitmap { image, .. } => image.allocation_len() as u64,
			GlyphImage::Layers(layers) => layers.iter().map(|(path, _)| (core::mem::size_of_val(path.points()) + path.verbs().len()) as u64).sum(),
			GlyphImage::Missing => 0,
		}
	}

	/// The kind this form is, which a fixture holds against the key it was cached under.
	pub fn kind(&self) -> Option<GlyphKind> {
		match self {
			GlyphImage::Outline(_) => Some(GlyphKind::Outline),
			GlyphImage::Mask { mode: RasterisationMode::Grayscale, .. } => Some(GlyphKind::GrayscaleMask),
			GlyphImage::Mask { .. } => Some(GlyphKind::SubpixelMask),
			GlyphImage::Bitmap { .. } => Some(GlyphKind::BitmapStrike),
			GlyphImage::Layers(_) => Some(GlyphKind::ColrLayers),
			GlyphImage::Missing => None,
		}
	}
}

/// Where decoded glyphs come from.
pub trait GlyphProvider {
	/// The decoded form for a key, or `Missing` when the face has no such glyph.
	fn glyph(&self, key: &GlyphCacheKey) -> GlyphImage;
}

/// A provider for a drawing with no text.
pub struct NoGlyphs;

impl GlyphProvider for NoGlyphs {
	fn glyph(&self, _key: &GlyphCacheKey) -> GlyphImage {
		GlyphImage::Missing
	}
}

/// The bounded glyph cache.
///
/// BOUNDED AND EVICTING IN INSERTION ORDER, which for text is close to least-recently-used and is
/// deterministic: a cache whose eviction depends on a clock or on a hash iteration order makes two
/// runs of one drawing produce different work, and the profile requires a given input to produce a
/// given output.
pub struct GlyphRaster {
	entries: Vec<(GlyphCacheKey, GlyphImage)>,
	bytes: u64,
	ceiling: u64,
	/// Bumped when the cache is cleared, which is what a prepared list is bound to.
	generation: u64,
}

impl Default for GlyphRaster {
	fn default() -> Self {
		Self::new(graphics_profile::RENDER2D_PROFILE_1_MINIMA.max_cache_bytes)
	}
}

impl GlyphRaster {
	pub fn new(ceiling: u64) -> Self {
		Self { entries: Vec::new(), bytes: 0, ceiling, generation: 1 }
	}

	pub fn generation(&self) -> u64 {
		self.generation
	}

	pub fn bytes(&self) -> u64 {
		self.bytes
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	/// Drop everything, which is what a replaced face's generation means for everything derived from
	/// it - and it bumps the cache generation, so every prepared list that was bound to the old one
	/// is refused by name rather than replayed against entries that are gone.
	pub fn clear(&mut self) {
		self.entries.clear();
		self.bytes = 0;
		self.generation = self.generation.wrapping_add(1);
	}

	/// The cached form for a key, decoding it through the provider on a miss.
	pub fn get(&mut self, key: &GlyphCacheKey, provider: &dyn GlyphProvider) -> &GlyphImage {
		if let Some(position) = self.entries.iter().position(|(cached, _)| cached == key) {
			return &self.entries[position].1;
		}
		let image = provider.glyph(key);
		let cost = image.bytes();
		// EVICT UNTIL IT FITS, and evict from the FRONT: the oldest entry is the one least likely to
		// be the glyph the next word needs.
		while self.bytes + cost > self.ceiling && !self.entries.is_empty() {
			let (_, evicted) = self.entries.remove(0);
			self.bytes = self.bytes.saturating_sub(evicted.bytes());
		}
		self.bytes += cost;
		self.entries.push((*key, image));
		&self.entries[self.entries.len() - 1].1
	}
}

/// Apply the frozen LCD filter across a row of subpixel coverage.
///
/// THE TAPS ARE NINTHS AND THE GAMMA IS THE PROFILE'S. Blending coverage linearly makes light text on
/// a dark background look bolder than the reverse at the same weight, which is the artefact that gets
/// called "the font renders too thin" - and it is a gamma question rather than a font question.
pub fn filter_subpixel(triples: &[u8]) -> Vec<u8> {
	let taps = graphics_profile::compositing::lcd::FILTER_NINTHS;
	let mut out = alloc::vec![0u8; triples.len()];
	for (index, slot) in out.iter_mut().enumerate() {
		let mut sum = 0u32;
		for (offset, weight) in taps.iter().enumerate() {
			let tap = index as i64 + offset as i64 - 2;
			let value = if tap >= 0 && (tap as usize) < triples.len() { triples[tap as usize] as u32 } else { 0 };
			sum += value * *weight as u32;
		}
		*slot = (sum / 9).min(255) as u8;
	}
	out
}

/// Coverage to the value it composites at, through the profile's own coverage gamma.
pub fn coverage_through_gamma(coverage: f32) -> f32 {
	let gamma = graphics_profile::compositing::lcd::COVERAGE_GAMMA as f32;
	if coverage <= 0.0 {
		return 0.0;
	}
	libm::powf(coverage.min(1.0), 1.0 / gamma)
}

/// A subpixel triple as the three channel coverages a mask composites with.
///
/// THE LAYOUT DECIDES WHICH COVERAGE IS WHICH CHANNEL, and a layout nobody named falls back to
/// grayscale rather than guessing - which is the rule everywhere in this tree.
pub fn subpixel_channels(mode: RasterisationMode, triple: [f32; 3]) -> Rgba {
	use font_contract::glyph::SubpixelLayout;
	match mode {
		RasterisationMode::Grayscale => Rgba::new(triple[0], triple[0], triple[0], triple[0]),
		RasterisationMode::Subpixel(layout) => match layout {
			SubpixelLayout::RgbHorizontal | SubpixelLayout::RgbVertical => Rgba::new(triple[0], triple[1], triple[2], (triple[0] + triple[1] + triple[2]) / 3.0),
			SubpixelLayout::BgrHorizontal | SubpixelLayout::BgrVertical => Rgba::new(triple[2], triple[1], triple[0], (triple[0] + triple[1] + triple[2]) / 3.0),
			SubpixelLayout::None => Rgba::new(triple[0], triple[0], triple[0], triple[0]),
		},
	}
}

//! TEXTURES AND SAMPLERS: every addressing mode, every filter, mip selection, cube faces and
//! anisotropy - each against the rule the profile freezes, because every one of them is a place two
//! backends disagree silently.
//!
//! TEXEL CENTRES ARE AT `(i + 0.5) / extent`. A sampler that put them at `i / extent` is off by half
//! a texel everywhere, which on a screen-aligned blit is a visible blur and nothing else.
//!
//! `repeat` IS THE EUCLIDEAN REMAINDER, so `-0.25` and `0.75` sample the SAME texel. The language's
//! `%` gives `-0.25` for the first, which lands on a different texel - and the difference only shows
//! where a coordinate goes negative, which is exactly where nobody looks.
//!
//! sRGB TEXELS ARE DECODED BEFORE FILTERING AND PREMULTIPLIED BEFORE INTERPOLATION. Filtering
//! encoded values averages in the wrong space, which makes every edge between two colours darker
//! than either; interpolating unpremultiplied alpha bleeds the colour of a transparent texel into
//! its neighbours, which is the halo around every cut-out leaf ever rendered.
//!
//! AND A `Data` OR `Normal` IMAGE IS NEVER TRANSFER-DECODED. A roughness value and a tangent-space
//! normal are numbers, not light; running an sRGB decode over them changes what they mean.

use alloc::vec;
use alloc::vec::Vec;

use graphics_profile::image::{Semantics, Transfer};
use render3d::Error;

/// One texel, in whatever numeric space its level holds.
pub type Texel = [f32; 4];

/// How a coordinate outside `[0, 1)` is brought back in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wrap {
	ClampToEdge,
	Repeat,
	MirroredRepeat,
	ClampToBorder,
}

/// The three border colours the profile admits. NOT AN ARBITRARY COLOUR: an arbitrary border needs a
/// whole colour to travel with the sampler, and these three cover every use a border wrap has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Border {
	TransparentBlack,
	OpaqueBlack,
	OpaqueWhite,
}

impl Border {
	pub const fn texel(self) -> Texel {
		match self {
			Self::TransparentBlack => [0.0, 0.0, 0.0, 0.0],
			Self::OpaqueBlack => [0.0, 0.0, 0.0, 1.0],
			Self::OpaqueWhite => [1.0, 1.0, 1.0, 1.0],
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
	Nearest,
	Linear,
}

/// A sampler: how a texture is read.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sampler {
	pub wrap_u: Wrap,
	pub wrap_v: Wrap,
	pub wrap_w: Wrap,
	pub border: Border,
	pub minify: Filter,
	pub magnify: Filter,
	pub mip: Filter,
	pub lod_bias: f32,
	pub min_lod: f32,
	pub max_lod: f32,
	/// Up to the device's `max_anisotropy`. ONE COLLAPSES THE RULE TO THE ISOTROPIC ONE, which is
	/// what makes anisotropy an addition rather than a second sampler.
	pub anisotropy: u32,
	/// The reference a depth-compare sample is tested against, or `None` for an ordinary read.
	pub compare: Option<render3d::CompareOp>,
}

impl Sampler {
	pub const NEAREST: Self = Self { wrap_u: Wrap::Repeat, wrap_v: Wrap::Repeat, wrap_w: Wrap::Repeat, border: Border::TransparentBlack, minify: Filter::Nearest, magnify: Filter::Nearest, mip: Filter::Nearest, lod_bias: 0.0, min_lod: 0.0, max_lod: 1000.0, anisotropy: 1, compare: None };

	pub const LINEAR: Self = Self { minify: Filter::Linear, magnify: Filter::Linear, mip: Filter::Linear, ..Self::NEAREST };
}

/// One mip level's texels.
#[derive(Clone, PartialEq, Debug)]
pub struct Level {
	pub width: u32,
	pub height: u32,
	pub depth: u32,
	pub texels: Vec<Texel>,
}

impl Level {
	pub fn new(width: u32, height: u32, depth: u32) -> Self {
		Self { width, height, depth, texels: vec![[0.0; 4]; (width * height * depth) as usize] }
	}

	pub fn at(&self, x: u32, y: u32, z: u32) -> Texel {
		let index = ((z * self.height + y) * self.width + x) as usize;
		self.texels.get(index).copied().unwrap_or([0.0; 4])
	}

	pub fn set(&mut self, x: u32, y: u32, z: u32, texel: Texel) {
		let index = ((z * self.height + y) * self.width + x) as usize;
		if let Some(slot) = self.texels.get_mut(index) {
			*slot = texel;
		}
	}
}

/// What kind of texture, which decides how a coordinate is read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Dim2,
	/// Six faces in the order +X, -X, +Y, -Y, +Z, -Z, stored as layers.
	Cube,
	/// Layers addressed by an explicit index rather than by a filtered coordinate.
	Array,
	Dim3,
}

/// A texture: its shape, its levels and what its numbers MEAN.
#[derive(Clone, PartialEq, Debug)]
pub struct Texture {
	/// The caller's own identifier. ONLY THE CACHE READS IT - a texture is otherwise named by the
	/// reference the sampler was given - and two textures sharing one identifier would serve each
	/// other's texels.
	pub id: u32,
	pub kind: Kind,
	/// One entry per level; for a cube or an array, each level holds every layer in `depth`.
	pub levels: Vec<Level>,
	/// The transfer function its texels are encoded with.
	pub transfer: Transfer,
	/// WHAT THE NUMBERS ARE. `Data` and `Normal` are never transfer-decoded whatever the transfer
	/// says, because they are measurements and directions rather than light.
	pub semantics: Semantics,
	/// Whether alpha is already multiplied into the colour. If it is not, a filtered read
	/// premultiplies BEFORE interpolating.
	pub premultiplied: bool,
}

impl Texture {
	pub fn level(&self, index: u32) -> Option<&Level> {
		self.levels.get(index as usize)
	}

	pub fn levels(&self) -> u32 {
		self.levels.len() as u32
	}

	/// Decode one texel into LINEAR PREMULTIPLIED form, which is the space every filter below works
	/// in.
	fn linear(&self, texel: Texel) -> Texel {
		let decoded = match self.semantics {
			// A MEASUREMENT AND A DIRECTION ARE NOT LIGHT. Decoding them changes what they mean.
			Semantics::Data | Semantics::Normal | Semantics::Depth | Semantics::Identity | Semantics::Mask => texel,
			Semantics::Color => [
				graphics_core::color::decode(self.transfer, texel[0] as f64) as f32,
				graphics_core::color::decode(self.transfer, texel[1] as f64) as f32,
				graphics_core::color::decode(self.transfer, texel[2] as f64) as f32,
				// ALPHA IS NEVER TRANSFER-ENCODED. It is coverage, not light.
				texel[3],
			],
		};
		if self.premultiplied || !matches!(self.semantics, Semantics::Color) {
			return decoded;
		}
		[decoded[0] * decoded[3], decoded[1] * decoded[3], decoded[2] * decoded[3], decoded[3]]
	}
}

/// A DIRECT-MAPPED TEXEL CACHE.
///
/// WHAT IT ACTUALLY BUYS, because "a texture cache" without that is a claim. A tap costs an address
/// computation, a bounds-and-wrap decision per axis, and - for an sRGB texture that has not had its
/// mip chain generated - a transfer-function decode per channel, which is a `pow`. A bilinear tap
/// takes four of those and a trilinear one eight, and neighbouring pixels of a surface hit the SAME
/// texels: the four taps of pixel `n` and of pixel `n+1` overlap in two of them whenever the surface
/// is magnified at all. So the cache is over the fetch, keyed by the texel's address.
///
/// DIRECT-MAPPED AND NOT ASSOCIATIVE. An associative cache needs a replacement policy, and a
/// replacement policy is a decision that changes which taps are fast without changing which are
/// correct - so it is a knob with no answer. Direct-mapped has one behaviour, and a conflict costs
/// exactly what no cache would have cost.
///
/// AND IT IS THE CALLER'S. A cache inside the texture would need interior mutability on a value
/// several threads may read; a cache the caller passes in belongs to whatever is doing the shading.
pub struct Cache {
	entries: [Entry; CACHE_ENTRIES],
	hits: u32,
	misses: u32,
}

/// How many texels the cache holds. A power of two, so the index is a mask rather than a modulo.
pub const CACHE_ENTRIES: usize = 256;

/// The largest texel-space coordinate the sampler will convert to an integer.
///
/// `2^24` IS WHERE AN `f32` STOPS NAMING ADJACENT INTEGERS, so a coordinate beyond it cannot select
/// one texel rather than its neighbour whatever the wrap mode does. Clamping there loses nothing a
/// shader could have meant and makes `as i64` exact - which is what stops the `+1` that finds the
/// neighbouring texel from overflowing.
pub const TEXEL_COORD_BOUND: f32 = 16_777_216.0;

#[derive(Clone, Copy)]
struct Entry {
	/// The texel's address, or `u64::MAX` for an empty slot - which is not a legal address, because
	/// the texture identifier is bounded well below the top of the range.
	key: u64,
	texel: Texel,
}

impl Default for Cache {
	fn default() -> Self {
		Self::new()
	}
}

impl Cache {
	pub const fn new() -> Self {
		Self { entries: [Entry { key: u64::MAX, texel: [0.0; 4] }; CACHE_ENTRIES], hits: 0, misses: 0 }
	}

	/// Empty it. NEEDED WHEN A TEXTURE'S CONTENTS CHANGE: the key names an address and not a
	/// version, so a render-to-texture pass that rewrites a texture must invalidate what was read
	/// from it - otherwise the next frame samples the previous one.
	pub fn clear(&mut self) {
		for entry in &mut self.entries {
			entry.key = u64::MAX;
		}
	}

	pub fn hits(&self) -> u32 {
		self.hits
	}

	pub fn misses(&self) -> u32 {
		self.misses
	}

	fn lookup(&mut self, key: u64) -> Option<Texel> {
		let slot = (key as usize) & (CACHE_ENTRIES - 1);
		if self.entries[slot].key == key {
			self.hits += 1;
			return Some(self.entries[slot].texel);
		}
		self.misses += 1;
		None
	}

	fn store(&mut self, key: u64, texel: Texel) {
		let slot = (key as usize) & (CACHE_ENTRIES - 1);
		self.entries[slot] = Entry { key, texel };
	}
}

/// A texel's address, as one number: the texture, the level and the coordinates.
///
/// MIXED SO NEIGHBOURING TEXELS LAND IN DIFFERENT SLOTS. `x` in the low bits and `y` above it would
/// make a whole cache line's worth of a column collide; interleaving the low bits of the two is what
/// makes a 2D neighbourhood - which is what a bilinear tap reads - map to distinct slots.
fn texel_key(texture: u32, level: u32, x: u32, y: u32, z: u32) -> u64 {
	let interleaved = ((x & 0xf) | ((y & 0xf) << 4)) as u64;
	let rest = ((x >> 4) as u64) | (((y >> 4) as u64) << 12) | ((z as u64) << 24) | ((level as u64) << 32) | ((texture as u64) << 40);
	interleaved | (rest << 8)
}

/// Bring a normalised coordinate into range, per axis, and say whether it landed on the border.
///
/// APPLIED AFTER THE LEVEL IS CHOSEN, so a wrapped coordinate does not change the derivative the
/// level came from - which is the ordering the profile states and the one that keeps a repeating
/// texture from picking the coarsest level at every seam.
fn address(coordinate: f32, extent: u32, wrap: Wrap) -> Option<i64> {
	let size = extent as i64;
	if size == 0 {
		return None;
	}
	if coordinate.is_nan() {
		return None;
	}
	let index = libm::floorf(coordinate.clamp(-TEXEL_COORD_BOUND, TEXEL_COORD_BOUND)) as i64;
	match wrap {
		Wrap::ClampToEdge => Some(index.clamp(0, size - 1)),
		// THE EUCLIDEAN REMAINDER, so a negative coordinate lands where its positive twin does.
		Wrap::Repeat => Some(index.rem_euclid(size)),
		Wrap::MirroredRepeat => {
			let period = 2 * size;
			let folded = index.rem_euclid(period);
			Some(if folded < size { folded } else { period - 1 - folded })
		}
		Wrap::ClampToBorder => {
			if index < 0 || index >= size {
				None
			} else {
				Some(index)
			}
		}
	}
}

/// One texel of one level, after addressing, decoding and premultiplication.
fn fetch(texture: &Texture, level: &Level, level_index: u32, x: i64, y: i64, z: i64, sampler: &Sampler, cache: &mut Option<&mut Cache>) -> Texel {
	let Some(x) = address(x as f32, level.width, sampler.wrap_u) else { return sampler.border.texel() };
	let Some(y) = address(y as f32, level.height, sampler.wrap_v) else { return sampler.border.texel() };
	let Some(z) = address(z as f32, level.depth, sampler.wrap_w) else { return sampler.border.texel() };
	// THE KEY IS THE ADDRESS AFTER WRAPPING, so two coordinates that name the same texel through
	// different wrap modes share one entry - which is the whole point of caching the fetch rather
	// than the coordinate.
	let key = texel_key(texture.id, level_index, x as u32, y as u32, z as u32);
	if let Some(cache) = cache.as_deref_mut() {
		if let Some(texel) = cache.lookup(key) {
			return texel;
		}
		let texel = texture.linear(level.at(x as u32, y as u32, z as u32));
		cache.store(key, texel);
		return texel;
	}
	texture.linear(level.at(x as u32, y as u32, z as u32))
}

/// The level a footprint selects.
///
/// `lambda = log2(rho) + bias`, where `rho` is the MAXIMUM of the two derivative lengths in texels.
/// The maximum rather than a geometric mean, because the mean under-filters exactly where an
/// anisotropic footprint is worst - a floor seen at a grazing angle, which is where aliasing is most
/// visible.
pub fn lambda(ddx: [f32; 2], ddy: [f32; 2], width: u32, height: u32, sampler: &Sampler, levels: u32) -> f32 {
	let scale = |d: [f32; 2]| {
		let sx = d[0] * width as f32;
		let sy = d[1] * height as f32;
		libm::sqrtf(sx * sx + sy * sy)
	};
	let rho = scale(ddx).max(scale(ddy));
	if rho <= 0.0 || !rho.is_finite() {
		return sampler.min_lod.max(0.0);
	}
	let value = libm::log2f(rho) + sampler.lod_bias;
	value.clamp(sampler.min_lod, sampler.max_lod).clamp(0.0, (levels.max(1) - 1) as f32)
}

/// Sample one level with the sampler's within-level filter.
fn sample_level(texture: &Texture, level_index: u32, coordinate: [f32; 3], sampler: &Sampler, filter: Filter, cache: &mut Option<&mut Cache>) -> Texel {
	let Some(level) = texture.level(level_index) else { return sampler.border.texel() };
	// TEXEL CENTRES AT `(i + 0.5) / extent`, so the sample point in texel units is
	// `coordinate * extent - 0.5`.
	//
	// AND IT IS BOUNDED BEFORE IT BECOMES AN INTEGER. A texture coordinate is SHADER OUTPUT: it can
	// be a NaN, an infinity or `1e30`, and `f32 as i64` saturates at `i64::MAX` - after which the
	// `+1` that finds the neighbouring texel overflows. Beyond `2^24` an `f32` cannot name adjacent
	// texels anyway, so clamping there loses nothing a shader could have meant and makes every
	// integer below exact.
	let texel_space = |value: f32, extent: u32| -> f32 {
		let scaled = value * extent as f32 - 0.5;
		if scaled.is_nan() { 0.0 } else { scaled.clamp(-TEXEL_COORD_BOUND, TEXEL_COORD_BOUND) }
	};
	let u = texel_space(coordinate[0], level.width);
	let v = texel_space(coordinate[1], level.height);
	let w = if level.depth > 1 { texel_space(coordinate[2], level.depth) } else { 0.0 };
	match filter {
		Filter::Nearest => {
			// A TIE GOES TO THE LOWER INDEX, which is what `floor(x + 0.5)` gives and what makes the
			// rule total: two backends round the same way at exactly a half.
			let round = |value: f32| libm::floorf(value + 0.5) as i64;
			fetch(texture, level, level_index, round(u), round(v), round(w), sampler, cache)
		}
		Filter::Linear => {
			let (x0, y0) = (libm::floorf(u) as i64, libm::floorf(v) as i64);
			let (fx, fy) = (u - libm::floorf(u), v - libm::floorf(v));
			let plane = |z: i64, cache: &mut Option<&mut Cache>| {
				let a = fetch(texture, level, level_index, x0, y0, z, sampler, cache);
				let b = fetch(texture, level, level_index, x0 + 1, y0, z, sampler, cache);
				let c = fetch(texture, level, level_index, x0, y0 + 1, z, sampler, cache);
				let d = fetch(texture, level, level_index, x0 + 1, y0 + 1, z, sampler, cache);
				blend(blend(a, b, fx), blend(c, d, fx), fy)
			};
			if level.depth > 1 {
				// EIGHT TAPS IN 3D, which is the four above on each of two planes.
				let z0 = libm::floorf(w) as i64;
				let fz = w - libm::floorf(w);
				blend(plane(z0, cache), plane(z0 + 1, cache), fz)
			} else {
				plane(0, cache)
			}
		}
	}
}

fn blend(from: Texel, to: Texel, at: f32) -> Texel {
	let mut out = [0.0; 4];
	for channel in 0..4 {
		// `(1-t)*a + t*b`, exact at both ends.
		out[channel] = (1.0 - at) * from[channel] + at * to[channel];
	}
	out
}

/// Sample a texture.
///
/// `level` is the `lambda` the caller already computed - from derivatives in a fragment stage, or
/// stated explicitly where no quad exists, which the profile says is the only legal form there.
pub fn sample(texture: &Texture, sampler: &Sampler, coordinate: [f32; 3], level: f32, magnifying: bool) -> Texel {
	sample_cached(texture, sampler, coordinate, level, magnifying, &mut None)
}

/// The same, through a cache the caller keeps.
pub fn sample_cached(texture: &Texture, sampler: &Sampler, coordinate: [f32; 3], level: f32, magnifying: bool, cache: &mut Option<&mut Cache>) -> Texel {
	let within = if magnifying { sampler.magnify } else { sampler.minify };
	let last = texture.levels().saturating_sub(1) as f32;
	// A NON-FINITE LEVEL IS THE SHARPEST ONE. The level is `log2` of a derivative, and a derivative
	// that is a NaN means the shader produced one - `0` is the conservative answer, never samples
	// outside the chain, and cannot carry the NaN into the blend and out through every channel.
	let clamped = if level.is_finite() { level.clamp(0.0, last) } else { 0.0 };
	if sampler.mip == Filter::Nearest || clamped >= last {
		// AT OR ABOVE THE LAST LEVEL THE LAST LEVEL ALONE IS USED and no blend happens, which is the
		// case a naive trilinear blends with a level that does not exist.
		let index = if sampler.mip == Filter::Nearest { libm::floorf(clamped + 0.5) as u32 } else { clamped as u32 };
		return sample_level(texture, index.min(texture.levels().saturating_sub(1)), coordinate, sampler, within, cache);
	}
	let low = libm::floorf(clamped);
	let fraction = clamped - low;
	let first = sample_level(texture, low as u32, coordinate, sampler, within, cache);
	let second = sample_level(texture, low as u32 + 1, coordinate, sampler, within, cache);
	blend(first, second, fraction)
}

/// Sample one LAYER of an array texture.
///
/// A LAYER IS ADDRESSED BY AN INDEX AND IS NEVER FILTERED BETWEEN. Two layers of an array are two
/// unrelated images - a texture atlas page, a shadow cascade, a sprite frame - and blending between
/// them produces a picture of neither. That is the whole difference between an ARRAY and a 3D
/// texture, whose third axis IS filtered, and it is the one a sampler written for the wrong one gets
/// backwards.
///
/// REFUSES A LAYER THE TEXTURE DOES NOT HAVE rather than clamping to the last one, which would draw
/// the wrong page of an atlas and look like an authoring mistake.
pub fn sample_array(texture: &Texture, sampler: &Sampler, coordinate: [f32; 2], layer: u32, level: f32) -> Result<Texel, Error> {
	let index = libm::floorf(level.max(0.0)) as u32;
	let Some(texels) = texture.level(index.min(texture.levels().saturating_sub(1))) else {
		return Err(Error::InvalidTexture { reason: "a sample of a level the texture does not have" });
	};
	if layer >= texels.depth {
		return Err(Error::InvalidTexture { reason: "a sample of an array layer the texture does not have" });
	}
	// The layer names a slice exactly, so the third coordinate is that slice's own centre and the
	// filter never reaches the one beside it.
	let slice = (layer as f32 + 0.5) / texels.depth as f32;
	let within = if sampler.minify == Filter::Nearest { Filter::Nearest } else { Filter::Linear };
	Ok(sample_level(texture, index.min(texture.levels().saturating_sub(1)), [coordinate[0], coordinate[1], slice], sampler, within, &mut None))
}

/// An anisotropic sample: up to `max_anisotropy` taps along the MAJOR axis of the footprint, each a
/// full filtered sample at the level chosen from the MINOR axis.
///
/// THE LEVEL COMES FROM THE MINOR AXIS. Taking it from the major one is the isotropic answer and
/// blurs exactly the direction anisotropy exists to keep sharp.
pub fn sample_anisotropic(texture: &Texture, sampler: &Sampler, coordinate: [f32; 3], ddx: [f32; 2], ddy: [f32; 2], width: u32, height: u32) -> Texel {
	let length = |d: [f32; 2]| {
		let sx = d[0] * width as f32;
		let sy = d[1] * height as f32;
		libm::sqrtf(sx * sx + sy * sy)
	};
	let (long, short, major) = if length(ddx) >= length(ddy) { (length(ddx), length(ddy), ddx) } else { (length(ddy), length(ddx), ddy) };
	if sampler.anisotropy <= 1 || short <= 0.0 || !long.is_finite() {
		let level = lambda(ddx, ddy, width, height, sampler, texture.levels());
		return sample(texture, sampler, coordinate, level, long < 1.0);
	}
	let taps = (libm::ceilf(long / short) as u32).clamp(1, sampler.anisotropy);
	// The level is the MINOR axis's, so the taps along the major axis are what resolves it.
	let minor = [short / width as f32, 0.0];
	let level = lambda(minor, minor, width, height, sampler, texture.levels());
	let mut sum = [0.0_f32; 4];
	for tap in 0..taps {
		// Equal weights, spread along the major axis and centred on the sample point.
		let offset = (tap as f32 + 0.5) / taps as f32 - 0.5;
		let at = [coordinate[0] + major[0] * offset, coordinate[1] + major[1] * offset, coordinate[2]];
		let texel = sample(texture, sampler, at, level, false);
		for channel in 0..4 {
			sum[channel] += texel[channel];
		}
	}
	for channel in &mut sum {
		*channel /= taps as f32;
	}
	sum
}

/// A DEPTH-COMPARE SAMPLE: each tap is compared FIRST, producing zero or one, and the results are
/// then filtered - so a linear depth-compare sample is the FRACTION of taps that passed.
///
/// FILTERING THE DEPTHS AND COMPARING ONCE PRODUCES A HARD EDGE and is the wrong answer: it is what
/// makes a shadow map look like a stencil instead of a soft boundary, and it is the single commonest
/// shadow bug.
pub fn sample_compare(texture: &Texture, sampler: &Sampler, coordinate: [f32; 2], level: u32, reference: f32) -> Result<f32, Error> {
	let compare = sampler.compare.ok_or(Error::InvalidRenderState { reason: "a depth-compare sample through a sampler with no comparison" })?;
	let Some(texels) = texture.level(level) else {
		return Err(Error::InvalidTexture { reason: "a depth-compare sample of a level the texture does not have" });
	};
	let bound = |value: f32, extent: u32| -> f32 {
		let scaled = value * extent as f32 - 0.5;
		if scaled.is_nan() { 0.0 } else { scaled.clamp(-TEXEL_COORD_BOUND, TEXEL_COORD_BOUND) }
	};
	let u = bound(coordinate[0], texels.width);
	let v = bound(coordinate[1], texels.height);
	let (x0, y0) = (libm::floorf(u) as i64, libm::floorf(v) as i64);
	let (fx, fy) = (u - libm::floorf(u), v - libm::floorf(v));
	let passed = |x: i64, y: i64| -> f32 {
		let stored = fetch(texture, texels, level, x, y, 0, sampler, &mut None)[0];
		f32::from(compare.test(reference, stored))
	};
	if sampler.minify == Filter::Nearest {
		let round = |value: f32| libm::floorf(value + 0.5) as i64;
		return Ok(passed(round(u), round(v)));
	}
	let top = (1.0 - fx) * passed(x0, y0) + fx * passed(x0 + 1, y0);
	let bottom = (1.0 - fx) * passed(x0, y0 + 1) + fx * passed(x0 + 1, y0 + 1);
	Ok((1.0 - fy) * top + fy * bottom)
}

/// RENDER TO TEXTURE: turn a colour attachment's resolved contents into a texture.
///
/// A RESOLVE FIRST AND NOT A RAW SAMPLE ARRAY. A multisampled attachment holds several values per
/// pixel and a texture holds one; sampling the first of them would make a render-to-texture pass
/// aliased in a way nothing else in the frame is, and averaging at fetch time would make every tap
/// cost the sample count.
///
/// THE RESULT IS DECLARED LINEAR AND PREMULTIPLIED, because that is what the blend path wrote. A
/// texture that claimed otherwise would be decoded a second time on its first fetch, which is the
/// double-decode that makes a render-to-texture chain darker at every step.
pub fn from_attachment(attachment: &crate::pass::Colour, id: u32) -> Result<Texture, Error> {
	let resolved = attachment.resolve()?;
	let mut level = Level::new(attachment.width, attachment.height, 1);
	for (index, value) in resolved.iter().enumerate() {
		let (x, y) = (index as u32 % attachment.width, index as u32 / attachment.width);
		level.set(x, y, 0, [value.x, value.y, value.z, value.w]);
	}
	Ok(Texture {
		id,
		kind: Kind::Dim2,
		levels: vec![level],
		transfer: Transfer::Linear,
		// AN IDENTITY ATTACHMENT IS NEVER FILTERED AND NEVER CONVERTED, which is what its semantics
		// say - so a pass that samples one gets the number it wrote.
		semantics: if attachment.integer { Semantics::Identity } else { Semantics::Color },
		premultiplied: true,
	})
}

// ---------------------------------------------------------------------------------------------
// Cube maps.
// ---------------------------------------------------------------------------------------------

/// Which face a direction selects, and the face-local coordinates - the standard cube-map table, so
/// a cube built for any other system loads without a flip.
///
/// THE MAJOR AXIS DECIDES THE FACE and the other two become `s` and `t` with the signs the table
/// gives. Getting one sign wrong mirrors one face, which in a reflection looks like a modelling
/// error rather than a sampler one.
pub fn cube_face(direction: [f32; 3]) -> (u32, [f32; 2]) {
	let [x, y, z] = direction;
	let (ax, ay, az) = (libm::fabsf(x), libm::fabsf(y), libm::fabsf(z));
	let (face, major, sc, tc) = if ax >= ay && ax >= az {
		if x > 0.0 { (0, ax, -z, -y) } else { (1, ax, z, -y) }
	} else if ay >= az {
		if y > 0.0 { (2, ay, x, z) } else { (3, ay, x, -z) }
	} else if z > 0.0 {
		(4, az, x, -y)
	} else {
		(5, az, -x, -y)
	};
	if major == 0.0 {
		return (face, [0.5, 0.5]);
	}
	(face, [(sc / major + 1.0) * 0.5, (tc / major + 1.0) * 0.5])
}

/// Sample a cube map through a direction.
///
/// A LINEAR FILTER WHOSE FOOTPRINT CROSSES A FACE EDGE TAKES THE TAPS FROM THE NEIGHBOURING FACE.
/// Clamping is what makes the seam visible; the taps are gathered by re-projecting each tap's own
/// direction, which is the construction that needs no table of edge adjacencies and cannot get one
/// of them backwards.
pub fn sample_cube(texture: &Texture, sampler: &Sampler, direction: [f32; 3], level: f32) -> Texel {
	let (face, coordinate) = cube_face(direction);
	let Some(texels) = texture.level(libm::floorf(level) as u32) else { return sampler.border.texel() };
	if sampler.minify == Filter::Nearest && sampler.magnify == Filter::Nearest {
		return sample_face(texture, sampler, face, coordinate, level, Filter::Nearest);
	}
	// The four taps, each re-projected through its own direction so one that leaves the face lands
	// on the neighbour rather than on a clamped edge texel.
	let step_u = 1.0 / texels.width as f32;
	let step_v = 1.0 / texels.height as f32;
	let u = coordinate[0] * texels.width as f32 - 0.5;
	let v = coordinate[1] * texels.height as f32 - 0.5;
	let (fx, fy) = (u - libm::floorf(u), v - libm::floorf(v));
	let base_u = (libm::floorf(u) + 0.5) * step_u;
	let base_v = (libm::floorf(v) + 0.5) * step_v;
	let tap = |du: f32, dv: f32| -> Texel {
		let point = [base_u + du * step_u, base_v + dv * step_v];
		let (tap_face, tap_coordinate) = if (0.0..=1.0).contains(&point[0]) && (0.0..=1.0).contains(&point[1]) { (face, point) } else { cube_face(direction_of(face, point)) };
		sample_face(texture, sampler, tap_face, tap_coordinate, level, Filter::Nearest)
	};
	blend(blend(tap(0.0, 0.0), tap(1.0, 0.0), fx), blend(tap(0.0, 1.0), tap(1.0, 1.0), fx), fy)
}

fn sample_face(texture: &Texture, sampler: &Sampler, face: u32, coordinate: [f32; 2], level: f32, filter: Filter) -> Texel {
	let index = libm::floorf(level) as u32;
	let Some(texels) = texture.level(index) else { return sampler.border.texel() };
	// A cube's faces are layers, so the third coordinate names the face rather than being filtered.
	let depth = if texels.depth == 0 { 1.0 } else { texels.depth as f32 };
	let layer = (face as f32 + 0.5) / depth;
	sample_level(texture, index, [coordinate[0], coordinate[1], layer], sampler, filter, &mut None)
}

/// The direction a face-local coordinate names - the inverse of `cube_face`, which is what lets a
/// tap that leaves a face be re-projected onto its neighbour.
fn direction_of(face: u32, coordinate: [f32; 2]) -> [f32; 3] {
	let sc = coordinate[0] * 2.0 - 1.0;
	let tc = coordinate[1] * 2.0 - 1.0;
	match face {
		0 => [1.0, -tc, -sc],
		1 => [-1.0, -tc, sc],
		2 => [sc, 1.0, tc],
		3 => [sc, -1.0, -tc],
		4 => [sc, -tc, 1.0],
		_ => [-sc, -tc, -1.0],
	}
}

// ---------------------------------------------------------------------------------------------
// Mip generation.
// ---------------------------------------------------------------------------------------------

/// Generate the mip chain of a level, by a 2x2 BOX filter of the level above.
///
/// GENERATED IN THE TEXTURE'S OWN NUMERIC SPACE, which for a colour texture means LINEAR LIGHT: a
/// box filter over sRGB-encoded values averages in the wrong space and every level comes out darker
/// than the one above it, which reads as a texture that dims with distance.
///
/// AN ODD DIMENSION HALVES BY `max(1, floor(n/2))` AND THE BOX TAKES THE TEXELS THAT EXIST. There is
/// no weighting to invent, and inventing one is how two implementations produce different chains
/// from the same image.
pub fn generate_mips(texture: &mut Texture) {
	if texture.levels.is_empty() {
		return;
	}
	// The chain is generated from the decoded top level, so every level is in the same space.
	let decoded = Level { width: texture.levels[0].width, height: texture.levels[0].height, depth: texture.levels[0].depth, texels: texture.levels[0].texels.iter().map(|texel| texture.linear(*texel)).collect() };
	let mut source = decoded.clone();
	texture.levels.truncate(1);
	while source.width > 1 || source.height > 1 || (texture.kind == Kind::Dim3 && source.depth > 1) {
		let width = (source.width / 2).max(1);
		let height = (source.height / 2).max(1);
		let depth = if texture.kind == Kind::Dim3 { (source.depth / 2).max(1) } else { source.depth };
		let mut next = Level::new(width, height, depth);
		for z in 0..depth {
			for y in 0..height {
				for x in 0..width {
					let mut sum = [0.0_f32; 4];
					let mut taps = 0.0_f32;
					for dz in 0..if texture.kind == Kind::Dim3 { 2 } else { 1 } {
						for dy in 0..2 {
							for dx in 0..2 {
								let (sx, sy, sz) = (x * 2 + dx, y * 2 + dy, if texture.kind == Kind::Dim3 { z * 2 + dz } else { z });
								if sx >= source.width || sy >= source.height || sz >= source.depth {
									continue;
								}
								let texel = source.at(sx, sy, sz);
								for channel in 0..4 {
									sum[channel] += texel[channel];
								}
								taps += 1.0;
							}
						}
					}
					if taps > 0.0 {
						for channel in &mut sum {
							*channel /= taps;
						}
					}
					next.set(x, y, z, sum);
				}
			}
		}
		texture.levels.push(next.clone());
		source = next;
	}
	// EVERY GENERATED LEVEL IS ALREADY LINEAR AND PREMULTIPLIED, so the texture says so - otherwise
	// the next fetch would decode them a second time.
	texture.transfer = Transfer::Linear;
	texture.premultiplied = true;
	// AND THE TOP LEVEL IS REPLACED WITH ITS DECODED FORM, so the chain is uniform. Leaving the
	// source's own encoding there while the texture says `Linear` is the worst of both: a magnified
	// fragment reads level zero and gets the ENCODED number as though it were light, which on sRGB
	// is more than twice the value the level below it would have given.
	texture.levels[0] = decoded;
}

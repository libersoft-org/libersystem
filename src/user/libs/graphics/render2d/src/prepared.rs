//! WHAT A PREPARED LIST IS BOUND TO, and what a mismatch answers.
//!
//! A PREPARED LIST IS A CACHE, and a cache whose validity conditions are not enumerated eventually
//! replays a drawing that is not the one recorded. Every dependency the profile names is a field
//! here, so `is_compatible` is a comparison rather than a judgement - and so a test can change each
//! one ALONE and see the refusal it should cause.
//!
//! AND CONTENT IS NOT STRUCTURE. A new video frame in an image the list references changes what the
//! drawing looks like and nothing about what the drawing IS; a list that re-flattened every path for
//! it would be re-preparing sixty times a second for no reason.

use alloc::vec::Vec;

use graphics_core::{ColorSpace, Extent2D, PixelFormat};

use crate::backend::TargetDescription;
use crate::list::DrawList;

/// Everything a preparation is bound to.
#[derive(Clone, PartialEq, Debug)]
pub struct PreparedKey {
	pub profile_version: u32,
	pub list_version: u32,
	pub backend: &'static str,
	pub backend_version: u32,
	pub format: PixelFormat,
	pub color_space: ColorSpace,
	pub extent: Extent2D,
	/// Held as its BITS, because a scale is compared for identity rather than for nearness - and two
	/// scales that differ in the last bit flatten differently.
	pub scale_bits: u32,
	/// One entry per referenced image: the identity and the LAYOUT generation. The content generation
	/// is deliberately NOT here.
	pub image_layouts: Vec<(u64, u64)>,
	/// The glyph caches the list's runs were prepared against.
	pub glyph_cache_generation: u64,
	/// A digest over the filter parameters, which decide the scratch that was reserved.
	pub filter_parameters: u64,
}

/// Why a prepared list cannot be replayed, naming the dependency that changed.
///
/// NAMED AND NEVER A SILENT RE-PREPARE. A caller replaying a list sixty times a second and getting a
/// full preparation each time has a performance bug it cannot see; being told which dependency
/// changed is what makes it findable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RePrepare {
	ProfileVersion,
	ListVersion,
	Backend,
	Format,
	ColorSpace,
	Extent,
	Scale,
	ImageLayout,
	GlyphCache,
	FilterParameters,
}

impl RePrepare {
	pub const fn name(self) -> &'static str {
		match self {
			RePrepare::ProfileVersion => "profile version",
			RePrepare::ListVersion => "list version",
			RePrepare::Backend => "backend identity or version",
			RePrepare::Format => "target format",
			RePrepare::ColorSpace => "target colour space",
			RePrepare::Extent => "physical extent",
			RePrepare::Scale => "scale",
			RePrepare::ImageLayout => "an immutable resource's layout generation",
			RePrepare::GlyphCache => "the glyph cache generation",
			RePrepare::FilterParameters => "filter parameters",
		}
	}
}

impl PreparedKey {
	/// Build the key a list and a target imply.
	pub fn of(list: &DrawList, target: &TargetDescription, backend: (&'static str, u32), glyph_cache_generation: u64) -> Self {
		let image_layouts = list.resources().images.iter().map(|image| (image.identity, image.layout_generation)).collect();
		// A CHEAP, ORDER-DEPENDENT DIGEST over what decides the reserved scratch. Order matters: two
		// graphs with the same nodes in a different order need different scratch.
		let mut filter_parameters = 0xcbf2_9ce4_8422_2325u64;
		for graph in &list.resources().filters {
			for node in graph.nodes() {
				let value = match node {
					crate::filter::FilterNode::Blur { x, y, .. } => (x.to_bits() as u64) << 32 | y.to_bits() as u64,
					crate::filter::FilterNode::Offset { dx, dy, .. } => (dx.to_bits() as u64) << 32 | dy.to_bits() as u64,
					_ => 1,
				};
				filter_parameters ^= value;
				filter_parameters = filter_parameters.wrapping_mul(0x100_0000_01b3);
			}
		}
		Self { profile_version: graphics_profile::image::IMAGE_COLOR_PROFILE_VERSION, list_version: list.version(), backend: backend.0, backend_version: backend.1, format: target.format, color_space: target.color_space, extent: target.extent, scale_bits: target.scale.to_bits(), image_layouts, glyph_cache_generation, filter_parameters }
	}

	/// May a preparation against `self` be replayed against `other`?
	///
	/// ANSWERS WHICH DEPENDENCY CHANGED, first in the order above - which is deliberate: the profile
	/// and the backend come before the target, so a caller told "backend" does not go looking at its
	/// own scale factor.
	pub fn compatible_with(&self, other: &PreparedKey) -> Result<(), RePrepare> {
		if self.profile_version != other.profile_version {
			return Err(RePrepare::ProfileVersion);
		}
		if self.list_version != other.list_version {
			return Err(RePrepare::ListVersion);
		}
		if self.backend != other.backend || self.backend_version != other.backend_version {
			return Err(RePrepare::Backend);
		}
		if self.format != other.format {
			return Err(RePrepare::Format);
		}
		if self.color_space != other.color_space {
			return Err(RePrepare::ColorSpace);
		}
		if self.extent != other.extent {
			return Err(RePrepare::Extent);
		}
		if self.scale_bits != other.scale_bits {
			return Err(RePrepare::Scale);
		}
		if self.image_layouts != other.image_layouts {
			return Err(RePrepare::ImageLayout);
		}
		if self.glyph_cache_generation != other.glyph_cache_generation {
			return Err(RePrepare::GlyphCache);
		}
		if self.filter_parameters != other.filter_parameters {
			return Err(RePrepare::FilterParameters);
		}
		Ok(())
	}
}

/// WHICH IMAGES CHANGED CONTENT rather than structure, between two recordings of one drawing.
///
/// THE ANSWER A VIDEO FRAME NEEDS. The prepared list stays valid; what has to happen is that the
/// named images' upload and sampling caches are refreshed, and nothing is re-flattened.
pub fn content_refreshed(before: &DrawList, after: &DrawList) -> Vec<u64> {
	let mut refreshed = Vec::new();
	for image in &after.resources().images {
		let Some(previous) = before.resources().images.iter().find(|other| other.identity == image.identity) else { continue };
		if previous.layout_generation == image.layout_generation && previous.content_generation != image.content_generation {
			refreshed.push(image.identity);
		}
	}
	refreshed
}

//! THE RESOURCE MODEL: how a resource is created, and what part of it a pass touches.
//!
//! NAMING `Buffer` AND `Texture` IN A COMMAND LIST SETTLES NEITHER. A profile that lists resource
//! kinds has said what exists; it has not said how one is made, which of its subresources a pass
//! writes, or what happens when two commands reach the same bytes. This module is those three.
//!
//! VIEWS, NOT WHOLE TEXTURES, ARE WHAT A PASS RENDERS INTO. A shadow pass renders to ONE CUBE FACE
//! AND ONE MIP, and a model whose attachments are whole textures cannot say that - so
//! `RenderTargetView` and `DepthStencilView` are built from a `TextureViewDesc` and carry the
//! subresource they name. Every hazard rule below is stated about subresources for the same reason:
//! sampling mip 2 while rendering into mip 0 of the same texture is not a conflict, and a model that
//! could not tell them apart would refuse it.
//!
//! THE HAZARD RULES ARE THE PROFILE'S AND THE IMPLEMENTATION IS RESPONSIBLE FOR THEM. An application
//! never inserts a barrier: a profile that required one would have to enumerate every barrier kind
//! and every backend would need the same ones. What this module does is REFUSE the cases the profile
//! says have no defined answer, at the point the profile says to refuse them.

use crate::error::{AttachmentFault, Error};

/// What a buffer may be used for. A usage a buffer was not created with is a refusal rather than a
/// silent promotion: a backend may place a vertex buffer somewhere a uniform read cannot reach.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BufferUsage {
	pub vertex: bool,
	pub index: bool,
	pub uniform: bool,
	pub storage: bool,
	/// The source of a copy.
	pub copy_source: bool,
	/// The destination of a copy, and of `write_buffer`.
	pub copy_destination: bool,
}

impl BufferUsage {
	pub const fn any(self) -> bool {
		self.vertex || self.index || self.uniform || self.storage || self.copy_source || self.copy_destination
	}
}

/// Whether the host can map a buffer, and which way the bytes move.
///
/// A DIRECTION AND NOT A BOOLEAN. A buffer the host writes and a buffer the host reads are placed
/// differently by every backend that has a choice, and "host visible" that did not say which would
/// make both the slow one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HostVisibility {
	/// Device memory. `write_buffer` and copies reach it; `map` does not.
	#[default]
	None,
	/// The host writes, the device reads.
	Upload,
	/// The device writes, the host reads. A readback destination.
	Readback,
}

/// How a buffer is created.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BufferDesc {
	pub size: u64,
	pub usage: BufferUsage,
	pub host_visibility: HostVisibility,
}

impl BufferDesc {
	/// Refuse a description that is not one.
	///
	/// A ZERO-SIZED BUFFER IS A MISTAKE AND NOT AN OPTIMISATION: every use of it is an out-of-range
	/// access, and creating it successfully moves the refusal to somewhere with less context.
	pub fn validate(&self, limits: &crate::limits::Render3DLimits) -> Result<(), Error> {
		if self.size == 0 {
			return Err(Error::InvalidTexture { reason: "a buffer of no bytes has no use that is not an out-of-range access" });
		}
		if !self.usage.any() {
			return Err(Error::InvalidRenderState { reason: "a buffer created for no usage cannot be bound to anything" });
		}
		if self.usage.uniform {
			limits.admit("uniform buffer bytes", self.size, limits.max_uniform_bytes_per_stage as u64)?;
		}
		if matches!(self.host_visibility, HostVisibility::Readback) && !self.usage.copy_destination {
			return Err(Error::InvalidRenderState { reason: "a readback buffer is the destination of a copy and has to say so" });
		}
		Ok(())
	}
}

/// The shape of a texture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextureDimension {
	D2,
	/// Six faces, square, one array element each. A separate dimension rather than an array of six,
	/// because the sampler addresses it by direction.
	Cube,
	/// A 2D array. `layers` is its length.
	D2Array,
	D3,
}

/// What a texture may be used for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TextureUsage {
	pub sampled: bool,
	pub colour_attachment: bool,
	pub depth_stencil_attachment: bool,
	pub storage: bool,
	pub copy_source: bool,
	pub copy_destination: bool,
}

impl TextureUsage {
	pub const fn any(self) -> bool {
		self.sampled || self.colour_attachment || self.depth_stencil_attachment || self.storage || self.copy_source || self.copy_destination
	}

	pub const fn is_attachment(self) -> bool {
		self.colour_attachment || self.depth_stencil_attachment
	}
}

/// How a texture is created.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextureDesc {
	pub dimension: TextureDimension,
	pub width: u32,
	pub height: u32,
	/// Depth for a 3D texture; ignored otherwise.
	pub depth: u32,
	pub mip_levels: u32,
	/// Array layers. Six for a cube map, and the value is CHECKED rather than assumed.
	pub layers: u32,
	pub samples: u32,
	/// The format's name in the profile's own table.
	pub format: &'static str,
	pub usage: TextureUsage,
}

impl TextureDesc {
	/// The number of mip levels a texture of this extent HAS, which is what a level count is checked
	/// against: `floor(log2(max extent)) + 1`.
	pub fn full_mip_chain(&self) -> u32 {
		let mut largest = self.width.max(self.height);
		if matches!(self.dimension, TextureDimension::D3) {
			largest = largest.max(self.depth);
		}
		let mut levels = 1;
		while largest > 1 {
			largest /= 2;
			levels += 1;
		}
		levels
	}

	/// Refuse a description that describes no texture.
	pub fn validate(&self, limits: &crate::limits::Render3DLimits) -> Result<(), Error> {
		if self.width == 0 || self.height == 0 {
			return Err(Error::InvalidTexture { reason: "a texture with a zero extent has no texels" });
		}
		if self.mip_levels == 0 || self.layers == 0 {
			return Err(Error::InvalidTexture { reason: "a texture with no mip levels or no layers has nothing to address" });
		}
		match self.dimension {
			TextureDimension::Cube => {
				if self.width != self.height {
					return Err(Error::InvalidTexture { reason: "a cube map's faces are square" });
				}
				if self.layers % 6 != 0 {
					return Err(Error::InvalidTexture { reason: "a cube map has six faces per array element" });
				}
			}
			TextureDimension::D3 => {
				if self.depth == 0 {
					return Err(Error::InvalidTexture { reason: "a 3D texture with no depth has no texels" });
				}
				if self.layers != 1 {
					return Err(Error::InvalidTexture { reason: "a 3D texture is not an array; its third dimension is depth" });
				}
				limits.admit("texture depth", self.depth as u64, limits.max_texture_extent_3d as u64)?;
			}
			TextureDimension::D2 => {
				if self.layers != 1 {
					return Err(Error::InvalidTexture { reason: "a 2D texture has one layer; an array of them is D2Array" });
				}
			}
			TextureDimension::D2Array => {}
		}
		if self.mip_levels > self.full_mip_chain() {
			return Err(Error::InvalidTexture { reason: "more mip levels than this extent has" });
		}
		// A MULTISAMPLED TEXTURE HAS ONE LEVEL AND IS NOT SAMPLED, which is what makes it a render
		// target rather than a texture: there is no defined filtering of samples, and a mip chain
		// would have to average them - which is the resolve, and a resolve is its own command.
		if self.samples > 1 {
			if self.mip_levels != 1 {
				return Err(Error::InvalidTexture { reason: "a multisampled texture has one mip level; a chain of them would need a resolve per level" });
			}
			if self.usage.sampled {
				return Err(Error::InvalidTexture { reason: "a multisampled texture is resolved before it is sampled" });
			}
			if !self.usage.is_attachment() {
				return Err(Error::InvalidTexture { reason: "a multisampled texture that is not an attachment has no way to be written" });
			}
		}
		limits.admit_samples(self.samples)?;
		let bytes_per_texel = bytes_per_texel(self.format)?;
		limits.admit("texture layers", self.layers as u64, limits.max_texture_layers as u64)?;
		limits.admit_texture_2d(self.width, self.height, bytes_per_texel)?;
		if !self.usage.any() {
			return Err(Error::InvalidRenderState { reason: "a texture created for no usage cannot be bound to anything" });
		}
		Ok(())
	}
}

/// Which part of a texture a view addresses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextureViewDesc {
	pub dimension: TextureDimension,
	pub aspect: Aspect,
	pub base_mip: u32,
	pub mip_count: u32,
	pub base_layer: u32,
	pub layer_count: u32,
}

/// Which plane of a texture a view reaches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Aspect {
	Colour,
	Depth,
	Stencil,
	/// Both planes of a combined format, for a copy or an attachment that uses both.
	DepthAndStencil,
}

impl TextureViewDesc {
	/// The whole texture, which is the commonest view and the one a caller should not have to spell.
	pub const fn whole(texture: &TextureDesc, aspect: Aspect) -> Self {
		Self { dimension: texture.dimension, aspect, base_mip: 0, mip_count: texture.mip_levels, base_layer: 0, layer_count: texture.layers }
	}

	pub fn validate(&self, texture: &TextureDesc) -> Result<(), Error> {
		if self.mip_count == 0 || self.layer_count == 0 {
			return Err(Error::InvalidTexture { reason: "a view of no levels or no layers addresses nothing" });
		}
		if self.base_mip.saturating_add(self.mip_count) > texture.mip_levels {
			return Err(Error::InvalidTexture { reason: "a view past the texture's last mip level" });
		}
		if self.base_layer.saturating_add(self.layer_count) > texture.layers {
			return Err(Error::InvalidTexture { reason: "a view past the texture's last layer" });
		}
		match (self.dimension, texture.dimension) {
			(TextureDimension::D2, _) if self.layer_count == 1 => Ok(()),
			(TextureDimension::Cube, TextureDimension::Cube) if self.layer_count % 6 == 0 => Ok(()),
			(TextureDimension::D2Array, TextureDimension::D2Array | TextureDimension::Cube) => Ok(()),
			(TextureDimension::D3, TextureDimension::D3) => Ok(()),
			_ => Err(Error::InvalidTexture { reason: "a view whose dimension the texture cannot be seen as" }),
		}
	}

	/// Whether two views of ONE texture reach the same texels. The question every hazard rule below
	/// is really about: sampling mip 2 while rendering into mip 0 is not a conflict.
	pub fn overlaps(&self, other: &Self) -> bool {
		let mips = self.base_mip < other.base_mip + other.mip_count && other.base_mip < self.base_mip + self.mip_count;
		let layers = self.base_layer < other.base_layer + other.layer_count && other.base_layer < self.base_layer + self.layer_count;
		let aspects = matches!((self.aspect, other.aspect), (Aspect::Colour, Aspect::Colour) | (Aspect::Depth, Aspect::Depth | Aspect::DepthAndStencil) | (Aspect::Stencil, Aspect::Stencil | Aspect::DepthAndStencil) | (Aspect::DepthAndStencil, _));
		mips && layers && aspects
	}
}

/// One colour attachment: a view, and what the pass does with it at each end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RenderTargetView {
	/// Which texture, as the caller's own identifier. This layer stores no resources.
	pub texture: u32,
	pub view: TextureViewDesc,
	pub format: &'static str,
	pub samples: u32,
	pub width: u32,
	pub height: u32,
	pub load: LoadOp,
	pub store: StoreOp,
}

/// The depth-stencil attachment, which has a load and store per plane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DepthStencilView {
	pub texture: u32,
	pub view: TextureViewDesc,
	pub format: crate::depth::DepthFormat,
	pub samples: u32,
	pub width: u32,
	pub height: u32,
	pub depth_load: LoadOp,
	pub depth_store: StoreOp,
	pub stencil_load: LoadOp,
	pub stencil_store: StoreOp,
}

/// What a pass does with an attachment's existing contents.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoadOp {
	/// Keep what is there. The previous pass's output is visible.
	Load,
	/// Replace it. The previous pass's output is gone and that is fine.
	Clear,
	/// Say nothing about it. NOBODY MAY READ IT AFTERWARDS, which is what makes this different from
	/// `Clear`: a clear has a value and a discard has none, and reading discarded content is how one
	/// backend's uninitialised memory becomes another's black.
	Discard,
}

/// What a pass does with what it wrote.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StoreOp {
	Store,
	/// The contents are not kept. A later read is a typed error, not a value.
	Discard,
}

/// Whether the contents of a subresource may be read.
///
/// THREE STATES AND NOT TWO. "Undefined" and "discarded" are different: the first is a resource
/// nothing has written, the second is one a pass deliberately threw away - and both refuse a read,
/// while a `Cleared` one answers the clear value. Folding the first two into "invalid" would lose
/// which of the two a report should name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Contents {
	/// Never written. A read is refused.
	Undefined,
	/// A pass discarded it. A read is refused, and the report can say it was thrown away rather than
	/// never written.
	Discarded,
	/// A pass cleared it and stored the result.
	Cleared,
	/// A pass rendered into it and stored the result.
	Written,
}

impl Contents {
	/// What a readback of this subresource answers.
	///
	/// THE PROFILE'S RULE, IN ONE PLACE: the clear value where the pass cleared, and a REFUSAL where
	/// it discarded. A backend deciding this locally is how the same frame reads back differently on
	/// two machines.
	pub fn readable(self) -> Result<(), Error> {
		match self {
			Self::Cleared | Self::Written => Ok(()),
			Self::Undefined => Err(Error::InvalidRenderState { reason: "a readback of a subresource nothing has written" }),
			Self::Discarded => Err(Error::InvalidRenderState { reason: "a readback of contents a pass discarded" }),
		}
	}

	/// What a pass's load and store leave behind.
	pub fn after(self, load: LoadOp, store: StoreOp) -> Self {
		match (load, store) {
			(_, StoreOp::Discard) | (LoadOp::Discard, _) if matches!(store, StoreOp::Discard) => Self::Discarded,
			(LoadOp::Clear, StoreOp::Store) => Self::Cleared,
			(LoadOp::Load, StoreOp::Store) => {
				// LOADING WHAT WAS DISCARDED IS STILL DISCARDED. A pass that keeps contents nobody
				// may read has kept nothing, and a store does not make them readable.
				match self {
					Self::Undefined => Self::Undefined,
					Self::Discarded => Self::Discarded,
					other => other,
				}
			}
			(LoadOp::Discard, StoreOp::Store) => Self::Written,
			(_, StoreOp::Discard) => Self::Discarded,
		}
	}
}

/// One pass's attachments, checked against each other.
///
/// A PASS HAS ONE VIEWPORT AND ONE SET OF FRAGMENTS, so every attachment has the same extent and the
/// same sample count. A resolve target has ONE sample and the source's format, because a resolve is
/// an average and not a conversion - a pass that wants both states both.
pub struct RenderTargetSet<'a> {
	pub colour: &'a [RenderTargetView],
	pub depth_stencil: Option<DepthStencilView>,
	/// One resolve destination per colour attachment, or `None` for the ones not resolved.
	pub resolve: &'a [Option<RenderTargetView>],
}

impl RenderTargetSet<'_> {
	pub fn validate(&self, limits: &crate::limits::Render3DLimits) -> Result<(), Error> {
		if self.colour.is_empty() && self.depth_stencil.is_none() {
			return Err(Error::TargetMismatch { reason: AttachmentFault::TooMany { count: 0, ceiling: limits.max_colour_attachments } });
		}
		if self.colour.len() as u64 > limits.max_colour_attachments as u64 {
			return Err(Error::TargetMismatch { reason: AttachmentFault::TooMany { count: self.colour.len() as u32, ceiling: limits.max_colour_attachments } });
		}
		if !self.resolve.is_empty() && self.resolve.len() != self.colour.len() {
			return Err(Error::TargetMismatch { reason: AttachmentFault::ResolveMismatch { reason: "one resolve slot per colour attachment, or none at all" } });
		}
		// The first attachment sets the extent and the sample count every other one is held to.
		let (width, height, samples) = match self.colour.first() {
			Some(first) => (first.width, first.height, first.samples),
			None => {
				let depth = self.depth_stencil.as_ref().expect("checked above");
				(depth.width, depth.height, depth.samples)
			}
		};
		for attachment in self.colour {
			if attachment.width != width || attachment.height != height {
				return Err(Error::TargetMismatch { reason: AttachmentFault::ExtentMismatch { width: attachment.width, height: attachment.height, expected_width: width, expected_height: height } });
			}
			if attachment.samples != samples {
				return Err(Error::TargetMismatch { reason: AttachmentFault::SampleCountMismatch { samples: attachment.samples, expected: samples } });
			}
		}
		if let Some(depth) = &self.depth_stencil {
			if depth.width != width || depth.height != height {
				return Err(Error::TargetMismatch { reason: AttachmentFault::ExtentMismatch { width: depth.width, height: depth.height, expected_width: width, expected_height: height } });
			}
			if depth.samples != samples {
				return Err(Error::TargetMismatch { reason: AttachmentFault::SampleCountMismatch { samples: depth.samples, expected: samples } });
			}
		}
		for (index, slot) in self.resolve.iter().enumerate() {
			let Some(destination) = slot else { continue };
			let source = &self.colour[index];
			if samples == 1 {
				return Err(Error::TargetMismatch { reason: AttachmentFault::ResolveMismatch { reason: "a single-sample pass has nothing to resolve" } });
			}
			if destination.samples != 1 {
				return Err(Error::TargetMismatch { reason: AttachmentFault::ResolveMismatch { reason: "a resolve destination has one sample" } });
			}
			if destination.format != source.format {
				return Err(Error::TargetMismatch { reason: AttachmentFault::ResolveMismatch { reason: "a resolve is an average and not a conversion; the formats must be the same" } });
			}
			if destination.width != width || destination.height != height {
				return Err(Error::TargetMismatch { reason: AttachmentFault::ExtentMismatch { width: destination.width, height: destination.height, expected_width: width, expected_height: height } });
			}
		}
		Ok(())
	}
}

/// THE HAZARD A PASS CANNOT HAVE: a texture sampled while it is an attachment of the same pass.
///
/// REFUSED AT SUBMISSION, which is what the profile says and the only answer that does not make the
/// picture depend on the backend: the read has no defined value because it depends on tile order.
/// THE TEST IS PER SUBRESOURCE: sampling mip 2 while rendering into mip 0 of the same texture is not
/// this hazard, and refusing it would refuse a legitimate and common thing.
pub fn refuse_sampled_attachment(sampled: &[(u32, TextureViewDesc)], targets: &RenderTargetSet<'_>) -> Result<(), Error> {
	for (texture, view) in sampled {
		for attachment in targets.colour {
			if attachment.texture == *texture && view.overlaps(&attachment.view) {
				return Err(Error::TargetMismatch { reason: AttachmentFault::ResolveMismatch { reason: "a texture sampled while it is an attachment of the same pass has no defined value" } });
			}
		}
		if let Some(depth) = &targets.depth_stencil {
			if depth.texture == *texture && view.overlaps(&depth.view) {
				return Err(Error::TargetMismatch { reason: AttachmentFault::ResolveMismatch { reason: "a depth attachment sampled by the pass that writes it has no defined value" } });
			}
		}
	}
	Ok(())
}

/// THE OTHER HAZARD THE PROFILE NAMES: a buffer written by the host while a submission owns it.
///
/// REFUSED AT THE WRITE AND NOT AT THE SUBMISSION, which is what the profile says and what makes the
/// report name the thing that is wrong: a resource handed to a submission is owned by it until its
/// completion is observable.
pub fn refuse_write_in_flight(in_flight: bool) -> Result<(), Error> {
	if in_flight { Err(Error::InvalidRenderState { reason: "a buffer written by the host while a submission owns it" }) } else { Ok(()) }
}

/// The bytes one texel of a profile format occupies.
fn bytes_per_texel(format: &'static str) -> Result<u64, Error> {
	graphics_profile::render3d_spec::COLOUR_FORMATS.iter().find(|entry| entry.name == format).map(|entry| (entry.bits_per_texel as u64).div_ceil(8)).ok_or(Error::UnsupportedFormat { format, used_as: "a texture format" })
}

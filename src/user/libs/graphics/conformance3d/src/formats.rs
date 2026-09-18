//! THE COLOUR FORMATS GROUP: what each one costs, what it may be used for, and what it refuses.
//!
//! A FORMAT IS A CAPABILITY ROW AND NOT A NAME, which is why each scene checks the row rather than
//! the spelling: "supports RGBA32F" is a claim about sampling it, rendering into it, blending it and
//! multisampling it, and three of those four are FALSE for that format - deliberately, with a reason
//! stated beside them. A suite that only checked the name would report a backend as conforming for
//! accepting a format it cannot blend.
//!
//! AND THE REFUSALS ARE THE POINT OF THE THREE `false` CELLS. A backend that filtered `RGBA32F`
//! anyway, or blended an integer format, would produce a picture that is wrong in a way nobody
//! notices until two backends disagree - so what these scenes check is that the path an application
//! takes REFUSES those uses rather than performing them.

use crate::Outcome;
use graphics_core::geom::Extent2D;
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, PixelFormat, PixelStorage};
use graphics_profile::render3d_spec::{COLOUR_FORMATS, FormatCapability};

/// The profile's row for a format, or a failure naming the one that is missing.
fn capability(name: &str) -> Result<&'static FormatCapability, crate::Trouble> {
	COLOUR_FORMATS.iter().find(|entry| entry.name == name).ok_or_else(|| crate::Trouble::Failed(alloc::format!("the profile has no format called {name}")))
}

/// What every colour format owes: a texel size, and a use it is good for.
fn declared(name: &str, bits: u32) -> Result<&'static FormatCapability, crate::Trouble> {
	let row = capability(name)?;
	if row.bits_per_texel != bits {
		return Err(crate::Trouble::Failed(alloc::format!("{name} is {} bits a texel in the profile and {bits} here", row.bits_per_texel)));
	}
	if !row.sampled {
		return Err(crate::Trouble::Failed(alloc::format!("{name} is a colour format and every one of them is sampleable")));
	}
	if !row.renderable || !row.attachment {
		return Err(crate::Trouble::Failed(alloc::format!("{name} is a colour format and every one of them may be rendered into")));
	}
	Ok(row)
}

/// A layout in a stated storage, which is what an application hands the interop boundary.
///
/// THE ALPHA MODE IS PART OF THE FORMAT'S ROW AND NOT A FREE CHOICE: `Straight` on a format with no
/// alpha channel is a combination that cannot mean anything, and the layout boundary refuses it. So
/// a scene states which mode the format admits and the boundary agreeing is part of what is checked.
fn layout(format: PixelFormat, bytes_per_texel: u32, alpha_mode: AlphaMode) -> Result<ImageLayout, crate::Trouble> {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode };
	ImageLayout::new(Extent2D::new(4, 4), 4 * bytes_per_texel, PixelStorage::Known(format), RowOrigin::TopLeft, semantics).map_err(|error| crate::Trouble::Failed(alloc::format!("the layout was refused: {error:?}")))
}

/// A format with no alpha channel takes `Opaque` and REFUSES `Straight`, which is the claim a scene
/// makes about every one of them.
fn opaque_only(format: PixelFormat, bytes_per_texel: u32, name: &str) -> Outcome {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb, alpha_mode: AlphaMode::Straight };
	let refused = ImageLayout::new(Extent2D::new(4, 4), 4 * bytes_per_texel, PixelStorage::Known(format), RowOrigin::TopLeft, semantics);
	require!(refused.is_err(), "{name} carries no alpha, so straight alpha over it is refused rather than ignored");
	Ok(())
}

/// A format that an image can carry is admitted at the interop boundary under its own name.
fn admitted(format: PixelFormat, bytes_per_texel: u32, name: &str, filtered: bool, alpha_mode: AlphaMode) -> Outcome {
	let layout = layout(format, bytes_per_texel, alpha_mode)?;
	let admitted = render3d::admit_as_texture(&layout, filtered).map_err(|error| crate::Trouble::Unsupported(alloc::format!("an image in {name} was refused as a texture: {error:?}")))?;
	require!(admitted == name, "an image in {name} is admitted under that name, and was admitted as {admitted}");
	Ok(())
}

pub fn format_r8() -> Outcome {
	let row = declared("R8", 8)?;
	require!(row.filterable && row.blendable && row.msaa, "R8 is filterable, blendable and multisampled");
	opaque_only(PixelFormat::R8Unorm, 1, "R8")?;
	admitted(PixelFormat::R8Unorm, 1, "R8", true, AlphaMode::Opaque)
}

pub fn format_rg8() -> Outcome {
	let row = declared("RG8", 16)?;
	require!(row.filterable && row.blendable, "RG8 is filterable and blendable");
	opaque_only(PixelFormat::R8G8Unorm, 2, "RG8")?;
	admitted(PixelFormat::R8G8Unorm, 2, "RG8", true, AlphaMode::Opaque)
}

pub fn format_rgba8() -> Outcome {
	// AND IT IS NOT sRGB-ENCODED, which the profile says in the format's own full name: the encoding
	// is `ColorSpace`'s and is never a second storage format. A backend that carried an `RGBA8_SRGB`
	// beside this one would have two formats for one storage and a conversion nobody asked for.
	let row = declared("RGBA8", 32)?;
	require!(row.full_name.contains("NOT sRGB-encoded"), "RGBA8 is not an sRGB format: {}", row.full_name);
	require!(row.filterable && row.blendable && row.msaa, "and it is filterable, blendable and multisampled");
	admitted(PixelFormat::R8G8B8A8Unorm, 4, "RGBA8", true, AlphaMode::Straight)
}

pub fn format_rgb10a2() -> Outcome {
	// TEN BITS A CHANNEL IN THIRTY-TWO, which is what makes it worth having: the same texel size as
	// `RGBA8` and four times the tonal resolution, at two bits of alpha.
	let row = declared("RGB10A2", 32)?;
	require!(row.filterable && row.blendable && row.msaa, "RGB10A2 is filterable, blendable and multisampled");
	require!(row.full_name.contains("10-bit"), "and it is ten bits a channel: {}", row.full_name);
	Ok(())
}

pub fn format_rgba16f() -> Outcome {
	// A HALF-FLOAT FORMAT IS FILTERABLE AND BLENDABLE, which is what makes it the one an HDR pass
	// uses: full float is neither.
	let row = declared("RGBA16F", 64)?;
	require!(row.filterable && row.blendable && row.msaa, "RGBA16F is filterable, blendable and multisampled");
	admitted(PixelFormat::R16G16B16A16Float, 8, "RGBA16F", true, AlphaMode::Straight)
}

pub fn format_rgba32f() -> Outcome {
	// THREE FALSE CELLS, EACH WITH A REASON, and this is the scene that holds them to it. A filtered
	// fetch of 128 bits is four lerps a tap and eight taps for trilinear; a blend is a
	// read-modify-write of 128 bits a sample; multisampling multiplies both. `MinNearest` and
	// `MagNearest` are the conforming way to sample it, and saying so is better than a backend
	// discovering it.
	let row = declared("RGBA32F", 128)?;
	require!(!row.filterable, "RGBA32F is not filterable, and the profile says so");
	require!(!row.blendable, "and it is not blendable");
	require!(!row.msaa, "and it is not multisampled");

	// AND THE REFUSAL IS ON THE PATH AN APPLICATION TAKES rather than only in a table: asking for it
	// as a FILTERED texture is refused, and asking for it unfiltered is not.
	let layout = layout(PixelFormat::R32G32B32A32Float, 16, AlphaMode::Straight)?;
	require!(render3d::admit_as_texture(&layout, true).is_err(), "a filtered read of RGBA32F is refused at the boundary");
	let unfiltered = render3d::admit_as_texture(&layout, false).map_err(|error| crate::Trouble::Unsupported(alloc::format!("an unfiltered read of RGBA32F was refused: {error:?}")))?;
	require!(unfiltered == "RGBA32F", "and an unfiltered one is admitted, as {unfiltered}");
	Ok(())
}

pub fn format_r32_uint() -> Outcome {
	// AN INTEGER FORMAT IS NOT FILTERABLE OR BLENDABLE, and the reason is not performance: there is
	// no correct answer to what the average of two object ids is, and blending them produces an id
	// that identifies nothing. IT IS MULTISAMPLED, because coverage still applies - which is the one
	// cell of the three that is true, and the one a reader would expect to be false.
	let row = declared("R32Uint", 32)?;
	require!(!row.filterable, "an integer format is not filterable");
	require!(!row.blendable, "and not blendable");
	require!(row.msaa, "and IS multisampled, because coverage still applies to an identity");
	require!(row.readback, "and readable back, because picking is what it is for");
	Ok(())
}

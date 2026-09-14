//! 2D AND 3D SHARE ONE IMAGE VOCABULARY, AND THERE IS NO BRIDGE API.
//!
//! `graphics-core` owns the image model - layouts, formats, colour spaces, alpha representation - and
//! both layers consume it. A compatible 2D single-sample colour target IS an `OwnedImage`, and either
//! layer draws into it with no conversion and no adapter type. That is the whole of the compatible
//! path, and it is worth stating because the alternative that gets built is a `Texture::from_image`
//! that copies.
//!
//! WHAT IS NOT COMPATIBLE IS NAMED RATHER THAN CONVERTED SILENTLY. A multisampled attachment, an HDR
//! one, one whose alpha is premultiplied where the other wants straight, or one in a different colour
//! space, needs an EXPLICIT resolve or conversion - and this module's job is to answer WHICH, so a
//! caller is told what to insert instead of discovering that its picture is wrong.
//!
//! AND THE REVERSE PATH VALIDATES RATHER THAN PROMISING. Not every image can be sampled: a format the
//! 3D profile does not have, or one it has but cannot filter, is a refusal at the point the texture
//! is described - not a promise that every image is sampled without conversion.

use crate::error::Error;
use graphics_core::layout::ImageLayout;
use graphics_core::{AlphaMode, ColorSpace, PixelFormat, PixelStorage};

/// What has to happen before an image and a 3D attachment can be the same memory.
///
/// A CLOSED SET AND NOT A BOOLEAN, because "incompatible" leaves a caller with nothing to do. Each
/// variant names the one operation that makes the pair compatible.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bridge {
	/// Nothing. The image IS the attachment.
	Direct,
	/// The attachment is multisampled; a resolve produces the image.
	Resolve,
	/// The numeric formats differ; a conversion produces the image.
	ConvertFormat { from: &'static str, to: &'static str },
	/// The colour spaces differ. A separate variant from the format because the memory can be
	/// identical and the meaning different, which is the conversion that gets skipped.
	ConvertColourSpace,
	/// The alpha representations differ - premultiplied against straight - which is a multiply or a
	/// divide per pixel and is the conversion that produces dark or bright fringes when it is missed.
	ConvertAlpha,
}

/// Whether a `graphics-core` image can be a 3D colour attachment, and what it would take.
///
/// THE ORDER OF THE CHECKS IS THE ORDER THE OPERATIONS WOULD RUN IN: a multisampled attachment is
/// resolved first, and only then is its format and colour meaning a question. Answering the later
/// one first would tell a caller to convert something it has to resolve anyway.
pub fn bridge_for_attachment(layout: &ImageLayout, attachment_format: &'static str, attachment_samples: u32, attachment_space: ColorSpace, attachment_alpha: AlphaMode) -> Result<Bridge, Error> {
	// THE 3D SIDE HAS TO BE A FORMAT THE PROFILE HAS. A caller naming one it does not is refused
	// here rather than at the copy.
	let capability = graphics_profile::render3d_spec::COLOUR_FORMATS.iter().find(|entry| entry.name == attachment_format).ok_or(Error::UnsupportedFormat { format: attachment_format, used_as: "a colour attachment" })?;
	if !capability.attachment {
		return Err(Error::UnsupportedFormat { format: attachment_format, used_as: "a colour attachment" });
	}
	if attachment_samples > 1 {
		return Ok(Bridge::Resolve);
	}
	let image_format = profile_name_for(layout.storage).ok_or(Error::UnsupportedFormat { format: "the image's storage", used_as: "a 3D texture format" })?;
	if image_format != attachment_format {
		return Ok(Bridge::ConvertFormat { from: image_format, to: attachment_format });
	}
	let (space, alpha) = colour_of(layout)?;
	if space != attachment_space {
		return Ok(Bridge::ConvertColourSpace);
	}
	if alpha != attachment_alpha {
		return Ok(Bridge::ConvertAlpha);
	}
	Ok(Bridge::Direct)
}

/// Whether a `graphics-core` image can be SAMPLED by the 3D layer as it stands.
///
/// VALIDATED AND NOT PROMISED. A format the profile does not have is refused; a format it has but
/// cannot filter is admitted for a point-sampled read and named for a filtered one, because those
/// are different answers and folding them would make every integer texture unusable.
pub fn admit_as_texture(layout: &ImageLayout, filtered: bool) -> Result<&'static str, Error> {
	let name = profile_name_for(layout.storage).ok_or(Error::UnsupportedFormat { format: "the image's storage", used_as: "a 3D texture format" })?;
	let capability = graphics_profile::render3d_spec::COLOUR_FORMATS.iter().find(|entry| entry.name == name).ok_or(Error::UnsupportedFormat { format: name, used_as: "a sampled texture" })?;
	if !capability.sampled {
		return Err(Error::UnsupportedFormat { format: name, used_as: "a sampled texture" });
	}
	if filtered && !capability.filterable {
		return Err(Error::UnsupportedFormat { format: name, used_as: "a FILTERED texture" });
	}
	Ok(name)
}

/// The profile's name for a `graphics-core` storage format, or `None` for one the 3D profile has no
/// entry for.
///
/// ONE MAPPING AND NOT A SECOND FORMAT LIST. The names are the profile's; this says which of them a
/// stored image already is, so an image and an attachment can be compared without either side
/// restating the other's vocabulary.
pub fn profile_name_for(storage: PixelStorage) -> Option<&'static str> {
	let PixelStorage::Known(format) = storage else {
		// A PACKED MASK IS NOT A PROFILE FORMAT. It is what a framebuffer a firmware handed over
		// looks like, and the 3D profile's formats are enumerated - so the answer is that it needs a
		// conversion rather than that it is some format.
		return None;
	};
	Some(match format {
		PixelFormat::R8Unorm => "R8",
		PixelFormat::R8G8Unorm => "RG8",
		PixelFormat::R8G8B8A8Unorm => "RGBA8",
		PixelFormat::R16G16B16A16Float => "RGBA16F",
		PixelFormat::R32G32B32A32Float => "RGBA32F",
		_ => return None,
	})
}

fn colour_of(layout: &ImageLayout) -> Result<(ColorSpace, AlphaMode), Error> {
	match layout.semantics {
		graphics_core::semantics::ImageSemantics::Color { color_space, alpha_mode } => Ok((color_space, alpha_mode)),
		_ => Err(Error::UnsupportedFormat { format: "a non-colour image", used_as: "a colour attachment" }),
	}
}

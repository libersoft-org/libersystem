//! THE FORMAT SET, as types, held to the frozen registry by a fixture.
//!
//! THE PROFILE IS THE LIST AND THIS IS THE CODE, and neither restates the other: every name, byte
//! count and alpha rule here comes from `graphics-profile`'s registry, and a fixture requires the
//! enumeration and the registry to describe the same twelve formats. A second copy of a list is a
//! second answer.

use graphics_profile::image::{self, AlphaMode as ProfileAlphaMode, Carries};

use crate::Error;

/// Every single-plane storage format the profile requires.
///
/// CHANNEL ORDER IS DEFINED BY THE NAME AND NEVER BY HOST ENDIANNESS. A format called `B8G8R8A8` has
/// blue in its first byte on every machine, which is what lets a decoder and a scanout agree without
/// either asking what architecture it is on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PixelFormat {
	A8Unorm,
	R8Unorm,
	R8G8Unorm,
	B8G8R8X8Unorm,
	R8G8B8X8Unorm,
	B8G8R8A8Unorm,
	R8G8B8A8Unorm,
	R10G10B10A2Unorm,
	R16G16B16A16Unorm,
	/// THE CANONICAL INTERMEDIATE: every layer, filter intermediate and offscreen composite.
	R16G16B16A16Float,
	R32Uint,
	R32G32B32A32Float,
}

/// Every format, in the registry's own order - so a `for` loop over them is a loop over the profile.
pub const ALL_FORMATS: [PixelFormat; 12] = [
	PixelFormat::A8Unorm,
	PixelFormat::R8Unorm,
	PixelFormat::R8G8Unorm,
	PixelFormat::B8G8R8X8Unorm,
	PixelFormat::R8G8B8X8Unorm,
	PixelFormat::B8G8R8A8Unorm,
	PixelFormat::R8G8B8A8Unorm,
	PixelFormat::R10G10B10A2Unorm,
	PixelFormat::R16G16B16A16Unorm,
	PixelFormat::R16G16B16A16Float,
	PixelFormat::R32Uint,
	PixelFormat::R32G32B32A32Float,
];

impl PixelFormat {
	/// The registry name, which is the only name this format has.
	pub const fn name(self) -> &'static str {
		match self {
			PixelFormat::A8Unorm => "A8_UNORM",
			PixelFormat::R8Unorm => "R8_UNORM",
			PixelFormat::R8G8Unorm => "R8G8_UNORM",
			PixelFormat::B8G8R8X8Unorm => "B8G8R8X8_UNORM",
			PixelFormat::R8G8B8X8Unorm => "R8G8B8X8_UNORM",
			PixelFormat::B8G8R8A8Unorm => "B8G8R8A8_UNORM",
			PixelFormat::R8G8B8A8Unorm => "R8G8B8A8_UNORM",
			PixelFormat::R10G10B10A2Unorm => "R10G10B10A2_UNORM",
			PixelFormat::R16G16B16A16Unorm => "R16G16B16A16_UNORM",
			PixelFormat::R16G16B16A16Float => "R16G16B16A16_FLOAT",
			PixelFormat::R32Uint => "R32_UINT",
			PixelFormat::R32G32B32A32Float => "R32G32B32A32_FLOAT",
		}
	}

	/// Its entry in the frozen registry. Every property below is READ from there.
	pub fn described(self) -> &'static image::Format {
		// The registry is the profile's and this enumeration is held to it by a fixture, so the
		// lookup cannot fail - and it answers with the first entry rather than panicking if the two
		// ever part, because a panic in a const-like accessor is a crash in a caller that did nothing
		// wrong.
		image::format_named(self.name()).unwrap_or(&image::FORMATS[0])
	}

	pub fn bytes_per_pixel(self) -> u32 {
		self.described().bytes_per_pixel as u32
	}

	pub fn carries(self) -> Carries {
		self.described().carries
	}

	/// Whether this format may be used with an alpha mode.
	///
	/// A TYPE ERROR AND NOT AN UNSUPPORTED CASE. `Straight` on a format with no alpha channel is not
	/// something a backend might implement later; it is a combination that cannot mean anything.
	pub fn admits(self, alpha: AlphaMode) -> bool {
		image::alpha_modes(self.described()).contains(&alpha.into_profile())
	}

	/// The minimum bytes one row of `width` pixels needs, CHECKED.
	pub fn minimum_row_bytes(self, width: u32) -> Option<u32> {
		width.checked_mul(self.bytes_per_pixel())
	}
}

/// How the alpha channel is to be read.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AlphaMode {
	Opaque,
	Straight,
	Premultiplied,
}

impl AlphaMode {
	const fn into_profile(self) -> ProfileAlphaMode {
		match self {
			AlphaMode::Opaque => ProfileAlphaMode::Opaque,
			AlphaMode::Straight => ProfileAlphaMode::Straight,
			AlphaMode::Premultiplied => ProfileAlphaMode::Premultiplied,
		}
	}

	pub const fn name(self) -> &'static str {
		self.into_profile().name()
	}
}

/// A channel of a packed layout the firmware described: where its bits are and how many.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct PackedChannel {
	pub shift: u8,
	pub bits: u8,
}

/// A layout firmware handed over, described by masks rather than by a name.
///
/// IT EXISTS BECAUSE A LITERAL 32 ONCE GAVE A DIAGONAL SMEAR. `src/uefi/src/gop.rs` accepts
/// `PIXEL_BIT_MASK` modes whose ELEMENT SIZE comes from the masks, so 16- and 24-bit modes are live;
/// narrowing the model to the named formats would reintroduce that.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PackedRgbLayout {
	pub bytes_per_pixel: u8,
	pub red: PackedChannel,
	pub green: PackedChannel,
	pub blue: PackedChannel,
	/// The bits that are not a channel. Ignored on read; written with all bits set.
	pub reserved: PackedChannel,
}

impl PackedRgbLayout {
	/// Check a firmware-described layout: the channels must be inside the element and must not
	/// overlap.
	///
	/// OVERLAPPING MASKS ARE NOT A LAYOUT. Two channels sharing a bit means one of them changes when
	/// the other is written, which is the shape of a mode nobody tested - and accepting it produces a
	/// picture whose colours shift as the content changes.
	pub fn validate(&self) -> Result<(), Error> {
		if !(1..=8).contains(&self.bytes_per_pixel) {
			return Err(Error::UnknownFormat);
		}
		let element_bits = self.bytes_per_pixel as u32 * 8;
		let mut used: u64 = 0;
		for channel in [self.red, self.green, self.blue, self.reserved] {
			if channel.bits == 0 {
				continue;
			}
			let end = (channel.shift as u32).checked_add(channel.bits as u32).ok_or(Error::UnknownFormat)?;
			if end > element_bits {
				return Err(Error::UnknownFormat);
			}
			let mask = ((1u64 << channel.bits) - 1) << channel.shift;
			if used & mask != 0 {
				return Err(Error::OverlappingChannels);
			}
			used |= mask;
		}
		if self.red.bits == 0 || self.green.bits == 0 || self.blue.bits == 0 {
			return Err(Error::UnknownFormat);
		}
		Ok(())
	}
}

/// How a plane's bytes are described: by a name from the frozen list, or by firmware's masks.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PixelStorage {
	/// The mandatory list: any image at all - sampled, filtered, drawn into, presented.
	Known(PixelFormat),
	/// A BOOT OR SCANOUT ADAPTER'S DESTINATION ONLY. Never sampled, never a texture, and never a
	/// presentable application surface: the profile says so, and so does this type's documentation,
	/// because the alternative is a renderer that grows a second sampling path for the one case.
	PackedRgbUnorm(PackedRgbLayout),
}

impl PixelStorage {
	pub fn bytes_per_pixel(&self) -> u32 {
		match self {
			PixelStorage::Known(format) => format.bytes_per_pixel(),
			PixelStorage::PackedRgbUnorm(layout) => layout.bytes_per_pixel as u32,
		}
	}

	pub fn minimum_row_bytes(&self, width: u32) -> Option<u32> {
		width.checked_mul(self.bytes_per_pixel())
	}

	/// Whether a renderer may SAMPLE this storage. The packed arm may not, and the refusal is here
	/// rather than at each of the places that would otherwise have to remember.
	pub const fn is_sampleable(&self) -> bool {
		matches!(self, PixelStorage::Known(_))
	}

	pub fn validate(&self) -> Result<(), Error> {
		match self {
			PixelStorage::Known(_) => Ok(()),
			PixelStorage::PackedRgbUnorm(layout) => layout.validate(),
		}
	}
}

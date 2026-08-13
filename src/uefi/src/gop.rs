//! The Graphics Output Protocol: which mode the firmware is in, and what its pixels mean.
//!
//! Moved out of the loader so a mock GOP can answer with a mode no machine here has - a
//! `PixelBitMask` format with 16-bit channels, or one with a channel mask that is not contiguous.
//! What the loader does with the answer is architecture-specific and stays there; what the answer
//! MEANS is here.

use core::ffi::c_void;

use crate::{self as uefi, BootServices};

// The active linear framebuffer the firmware's Graphics Output Protocol reports: its
// physical base + byte size and the pixel geometry/format. Architecture-neutral - each
// backend turns it into a `bootproto::Framebuffer` (x86 stores an HHDM virtual `addr`
// it mapped; the device-tree arches store the physical base and let the kernel map it).
pub struct GopFb {
	pub present: bool,
	pub phys: u64,
	// Read only by the x86 backend (to map the framebuffer into the HHDM); the
	// device-tree arches pass the physical base straight through and never map it.
	#[allow(dead_code)]
	pub size: u64,
	pub width: u32,
	pub height: u32,
	pub pitch: u32, // bytes per row
	// Bits per pixel, DERIVED from the mode's own bitmask rather than assumed.
	//
	// The helper below already computed this to get the pitch right for a non-32 bpp mode, and then
	// dropped it: `GopFb` had no field for it and all three backends published the constant `32`.
	// The kernel derives bytes-per-pixel from what they publish, so such a mode got a correct pitch
	// and a wrong pixel stride - the original finding surviving inside its own fix.
	pub bpp: u32,
	pub red_shift: u8,
	pub red_size: u8,
	pub green_shift: u8,
	pub green_size: u8,
	pub blue_shift: u8,
	pub blue_size: u8,
}

impl GopFb {
	pub(crate) const NONE: Self = Self { present: false, phys: 0, size: 0, width: 0, height: 0, pitch: 0, bpp: 0, red_shift: 0, red_size: 0, green_shift: 0, green_size: 0, blue_shift: 0, blue_size: 0 };
}

// Query the Graphics Output Protocol for the active mode's linear framebuffer. Returns
// `GopFb::NONE` on a headless boot (no GOP / no active mode / an unsupported format).
pub fn locate_framebuffer(bs: *mut BootServices) -> GopFb {
	let mut gop: *mut c_void = core::ptr::null_mut();
	let status = unsafe { ((*bs).locate_protocol)(&uefi::GRAPHICS_OUTPUT_PROTOCOL_GUID, core::ptr::null_mut(), &mut gop) };
	if uefi::is_error(status) || gop.is_null() {
		return GopFb::NONE;
	}
	let gop = gop as *mut uefi::GraphicsOutput;
	let mode = unsafe { (*gop).mode };
	if mode.is_null() {
		return GopFb::NONE;
	}
	let info = unsafe { (*mode).info };
	if info.is_null() {
		return GopFb::NONE;
	}
	let (width, height, pitch_px, format, mask) = unsafe { ((*info).horizontal_resolution, (*info).vertical_resolution, (*info).pixels_per_scan_line, (*info).pixel_format, &(*info).pixel_information) };
	// Channel shifts/sizes: the common 32-bpp RGB/BGR modes are fixed layouts; a
	// bit-mask mode is decoded from the reported channel masks.
	let (rs, gs, bs_shift) = match format {
		uefi::PIXEL_RGB => (0u8, 8u8, 16u8),
		uefi::PIXEL_BGR => (16u8, 8u8, 0u8),
		uefi::PIXEL_BIT_MASK => (mask_shift(mask.red), mask_shift(mask.green), mask_shift(mask.blue)),
		_ => return GopFb::NONE,
	};
	let (rz, gz, bz) = match format {
		uefi::PIXEL_BIT_MASK => (mask_size(mask.red), mask_size(mask.green), mask_size(mask.blue)),
		_ => (8u8, 8u8, 8u8),
	};
	// THE PIXEL SIZE COMES FROM THE MASKS in a bit-mask mode, which is what the masks are for. It
	// was the literal 32, and the pitch `pixels_per_scan_line * 4` with it - so a firmware
	// describing a 24- or 16-bit layout got a renderer writing four bytes per pixel into a
	// framebuffer with a different stride, which is a diagonal smear rather than a picture.
	//
	// And the masks are CHECKED rather than assumed contiguous and disjoint: `mask_size` counts
	// the run of ones above the lowest set bit, so a split mask reads as a short one, and two
	// channels claiming the same bits produce a colour neither of them asked for.
	let bpp = match format {
		uefi::PIXEL_BIT_MASK => {
			let all = mask.red | mask.green | mask.blue | mask.reserved;
			if all == 0 {
				return GopFb::NONE;
			}
			// Contiguous: each channel's mask must be exactly the run its shift and size describe.
			//
			// RESERVED IS ONE OF THEM. It contributes to `all` and therefore to the pixel size below,
			// and it was the one mask nothing checked - so a firmware whose reserved mask is split,
			// or overlaps a colour channel, produced an element size derived from a mask that had
			// been through no validation at all. The original defect here was a literal 32; this is
			// the last quarter of the check that replaced it.
			let reserved_shift = mask_shift(mask.reserved);
			let reserved_size = mask_size(mask.reserved);
			for (m, shift, size) in [(mask.red, rs, rz), (mask.green, gs, gz), (mask.blue, bs_shift, bz), (mask.reserved, reserved_shift, reserved_size)] {
				if m != 0 && m != (((1u64 << size) - 1) as u32) << shift {
					return GopFb::NONE;
				}
			}
			// Disjoint: no two channels may claim a bit, reserved included.
			if (mask.red & mask.green) | (mask.green & mask.blue) | (mask.red & mask.blue) != 0 {
				return GopFb::NONE;
			}
			if (mask.reserved & (mask.red | mask.green | mask.blue)) != 0 {
				return GopFb::NONE;
			}
			// The element size is the highest set bit across all four, rounded up to whole bytes.
			(32 - all.leading_zeros()).div_ceil(8) * 8
		}
		_ => 32u32,
	};
	GopFb { present: true, phys: unsafe { (*mode).frame_buffer_base }, size: unsafe { (*mode).frame_buffer_size as u64 }, width, height, pitch: pitch_px * (bpp / 8), bpp, red_shift: rs, red_size: rz, green_shift: gs, green_size: gz, blue_shift: bs_shift, blue_size: bz }
}

// Bit position of the lowest set bit of a channel mask.
pub fn mask_shift(mask: u32) -> u8 {
	if mask == 0 { 0 } else { mask.trailing_zeros() as u8 }
}

// Width in bits of a contiguous channel mask.
pub fn mask_size(mask: u32) -> u8 {
	(mask >> mask_shift(mask)).trailing_ones() as u8
}

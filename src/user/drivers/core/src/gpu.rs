// The display driver's geometry contract, as pure decisions: which dimensions a driver will believe
// and what arithmetic it does on them, tested on the host through the crate's seam.
//
// THE DEBT THAT LIVES HERE (DRV-010). The driver took the display size the DEVICE reported, refused
// only a zero, and then did unchecked arithmetic on it: a pitch of `width * 4` in a `u32`, a union of
// two rectangles as `x + width` in a `u32`, and a transfer offset computed from a rectangle nothing
// had checked was inside the resource. A device - or anything acting as one - chooses those numbers.
//
// WHAT AN OVERFLOW DOES HERE IS NOT A WRONG PICTURE. A wrapped pitch describes a framebuffer whose
// rows are shorter than the pixels in them, and that description is handed to ConsoleService, which
// maps the buffer and draws into it. A wrapped union produces a rectangle whose corner is before its
// origin, and the transfer offset computed from it addresses the resource outside itself.
//
// So the numbers are bounded first and the arithmetic is checked after, and both are decisions rather
// than device operations - which is why they are here, where a host test can hold them.

// The largest extent this driver will accept from a device, on either axis.
//
// SIXTEEN THOUSAND THREE HUNDRED AND EIGHTY-FOUR, which is the image model's own maximum extent: a
// display larger than the largest image the system can describe is not a display this driver can
// back, and the honest answer is the fallback rather than an allocation nobody can use.
pub const MAX_EXTENT: u32 = 16_384;

// Bytes per pixel in the one format this driver's resources use (B8G8R8X8).
pub const BYTES_PER_PIXEL: u32 = 4;

// The display geometry to use, from what the device reported and the driver's own fallback.
//
// A ZERO IS NOT THE ONLY BAD ANSWER. A device that reports four billion is reporting a display that
// cannot be allocated, described or drawn into, and believing it produces a pitch that has wrapped
// before anything notices.
pub fn display_geometry(reported: (u32, u32), fallback: (u32, u32)) -> (u32, u32) {
	match pitch_bytes(reported.0) {
		Some(_) if reported.0 > 0 && reported.1 > 0 && reported.0 <= MAX_EXTENT && reported.1 <= MAX_EXTENT && backing_bytes(reported.0, reported.1).is_some() => reported,
		_ => fallback,
	}
}

// One row's bytes, or `None` when the multiplication does not fit.
pub fn pitch_bytes(width: u32) -> Option<u32> {
	width.checked_mul(BYTES_PER_PIXEL)
}

// The bytes a backing of this geometry needs, or `None` when the product does not fit a `u64` - or
// when it is larger than this driver will ask for.
pub fn backing_bytes(width: u32, height: u32) -> Option<u64> {
	let row = pitch_bytes(width)? as u64;
	let total = row.checked_mul(height as u64)?;
	(width <= MAX_EXTENT && height <= MAX_EXTENT).then_some(total)
}

// The bounding box of two rectangles, SATURATING rather than wrapping.
//
// A WRAPPED UNION IS A RECTANGLE WHOSE CORNER IS BEFORE ITS ORIGIN, and every consumer of it - the
// clamp, the transfer offset, the flush - then computes from a shape that does not exist. Saturating
// gives a rectangle that is too large, which the clip below removes; wrapping gives one that is
// somewhere else.
pub fn union_rect(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> (u32, u32, u32, u32) {
	let x0 = a.0.min(b.0);
	let y0 = a.1.min(b.1);
	let x1 = a.0.saturating_add(a.2).max(b.0.saturating_add(b.2));
	let y1 = a.1.saturating_add(a.3).max(b.1.saturating_add(b.3));
	(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
}

// The part of a rectangle that is inside an extent, or `None` when none of it is.
//
// CLIPPED AND NOT CLAMPED. Clamping the origin moves a rectangle that was outside to the edge, which
// presents pixels nobody asked to present; clipping keeps only what was already inside, and answers
// `None` when that is nothing.
pub fn visible_rect(rect: (u32, u32, u32, u32), extent: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
	let (x, y, width, height) = rect;
	if x >= extent.0 || y >= extent.1 || width == 0 || height == 0 {
		return None;
	}
	let width = width.min(extent.0 - x);
	let height = height.min(extent.1 - y);
	(width > 0 && height > 0).then_some((x, y, width, height))
}

// The byte offset of a rectangle's first pixel in a backing of `stride` pixels, or `None` when the
// arithmetic does not fit.
pub fn transfer_offset(x: u32, y: u32, stride: u32) -> Option<u64> {
	let row = (y as u64).checked_mul(stride as u64)?;
	row.checked_add(x as u64)?.checked_mul(BYTES_PER_PIXEL as u64)
}

#[cfg(test)]
mod tests;

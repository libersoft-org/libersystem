// DRV-010's negatives, each held against the decision that closes it.
use super::{BYTES_PER_PIXEL, MAX_EXTENT, backing_bytes, display_geometry, pitch_bytes, transfer_offset, union_rect, visible_rect};

const FALLBACK: (u32, u32) = (1024, 768);

#[test]
// A DEVICE CHOOSES THE NUMBER THE DRIVER BELIEVES. Refusing only a zero leaves every other
// impossible answer - four billion, or anything whose pitch has already wrapped by the time the
// framebuffer is described to ConsoleService, which maps it and draws into it.
fn an_unbelievable_display_size_falls_back_rather_than_wrapping() {
	assert_eq!(display_geometry((1920, 1080), FALLBACK), (1920, 1080));
	assert_eq!(display_geometry((MAX_EXTENT, MAX_EXTENT), FALLBACK), (MAX_EXTENT, MAX_EXTENT));
	assert_eq!(display_geometry((0, 768), FALLBACK), FALLBACK, "a zero was always refused");
	assert_eq!(display_geometry((1024, 0), FALLBACK), FALLBACK);
	assert_eq!(display_geometry((MAX_EXTENT + 1, 768), FALLBACK), FALLBACK, "one past the bound is past the bound");
	assert_eq!(display_geometry((u32::MAX, u32::MAX), FALLBACK), FALLBACK);
	// THE PITCH IS WHERE IT WRAPS FIRST: `width * 4` leaves the `u32` at a quarter of its range, long
	// before the extent looks absurd.
	assert_eq!(pitch_bytes(u32::MAX / 4 + 1), None);
	assert_eq!(pitch_bytes(1920), Some(1920 * BYTES_PER_PIXEL));
}

#[test]
// THE ALLOCATION IS THE PRODUCT OF THREE NUMBERS, and a product that does not fit is not an
// allocation that fails - it is a smaller one that succeeds, which is a backing shorter than the
// picture it is said to hold.
fn a_backing_size_that_does_not_fit_is_refused() {
	assert_eq!(backing_bytes(1920, 1080), Some(1920 * 1080 * 4));
	assert_eq!(backing_bytes(MAX_EXTENT, MAX_EXTENT), Some(MAX_EXTENT as u64 * MAX_EXTENT as u64 * 4));
	assert_eq!(backing_bytes(u32::MAX, u32::MAX), None);
	assert_eq!(backing_bytes(MAX_EXTENT + 1, 1), None);
}

#[test]
// A WRAPPED UNION IS A RECTANGLE WHOSE CORNER IS BEFORE ITS ORIGIN. The rectangles come off a service
// channel - ConsoleService sends them - so they are input, and `x + width` in a `u32` is where a
// crafted pair stops describing a shape at all.
fn the_union_of_two_rectangles_saturates_rather_than_wrapping() {
	assert_eq!(union_rect((0, 0, 10, 10), (5, 5, 10, 10)), (0, 0, 15, 15));
	assert_eq!(union_rect((10, 10, 5, 5), (0, 0, 2, 2)), (0, 0, 15, 15));
	// The crafted pair: both corners overflow, and the answer is a rectangle that is too LARGE - which
	// the clip then removes - rather than one that is somewhere else.
	let hostile = union_rect((u32::MAX, u32::MAX, u32::MAX, u32::MAX), (0, 0, 1, 1));
	assert_eq!(hostile, (0, 0, u32::MAX, u32::MAX));
	assert!(visible_rect(hostile, (1920, 1080)).is_some(), "a too-large rectangle is clipped to the display");
}

#[test]
// CLIPPED AND NOT CLAMPED. Clamping an origin moves a rectangle that was entirely outside the display
// to its edge, which presents pixels nobody asked to present; clipping keeps what was inside and
// answers nothing when that is nothing.
fn a_rectangle_outside_the_display_presents_nothing() {
	let extent = (1920, 1080);
	assert_eq!(visible_rect((0, 0, 1920, 1080), extent), Some((0, 0, 1920, 1080)));
	assert_eq!(visible_rect((1900, 1060, 100, 100), extent), Some((1900, 1060, 20, 20)), "the part that is inside");
	assert_eq!(visible_rect((1920, 0, 10, 10), extent), None, "the first column past the display is past it");
	assert_eq!(visible_rect((0, 1080, 10, 10), extent), None);
	assert_eq!(visible_rect((0, 0, 0, 10), extent), None, "a zero-width rectangle covers nothing");
	assert_eq!(visible_rect((u32::MAX, u32::MAX, u32::MAX, u32::MAX), extent), None);
}

#[test]
// THE OFFSET IS COMPUTED FROM A RECTANGLE, and an unchecked one addresses the resource outside
// itself. It is the last arithmetic before the device is told where to read.
fn a_transfer_offset_that_does_not_fit_is_refused() {
	assert_eq!(transfer_offset(0, 0, 1920), Some(0));
	assert_eq!(transfer_offset(10, 2, 1920), Some((2 * 1920 + 10) * 4));
	assert_eq!(transfer_offset(u32::MAX, u32::MAX, u32::MAX), None);
}

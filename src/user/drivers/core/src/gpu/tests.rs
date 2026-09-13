// DRV-010's negatives, each held against the decision that closes it.
use super::{BYTES_PER_PIXEL, DamageSet, MAX_DAMAGE_RECTS, MAX_EXTENT, backing_bytes, display_geometry, pitch_bytes, transfer_offset, union_rect, visible_rect};

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

#[test]
// THE DEFECT, WRITTEN AS A TEST. Two opposite corners of a screen have a bounding box of the whole
// screen, and this driver used to transfer that. They stay two rectangles now, and the sum of their
// areas is what moves.
fn two_corners_are_two_transfers_and_not_the_screen_between_them() {
	let mut damage = DamageSet::new();
	damage.add((0, 0, 8, 8));
	damage.add((1016, 760, 8, 8));
	assert_eq!(damage.rects().len(), 2, "a thousand pixels apart is not one rectangle");
	let moved: u64 = damage.rects().iter().map(|rect| rect.2 as u64 * rect.3 as u64).sum();
	assert_eq!(moved, 128, "two corners, and nothing between them");
	assert_eq!(union_rect(damage.rects()[0], damage.rects()[1]), (0, 0, 1024, 768), "which is what the union would have cost");
}

#[test]
// Merging where it costs nothing is the other half of the rule: a client that damages the same
// region twice, or two regions that touch, pays for one transfer.
fn rectangles_that_overlap_or_touch_become_one() {
	let mut damage = DamageSet::new();
	damage.add((10, 10, 20, 20));
	damage.add((15, 15, 20, 20));
	assert_eq!(damage.rects(), &[(10, 10, 25, 25)], "overlapping rectangles merge");

	let mut touching = DamageSet::new();
	touching.add((0, 0, 10, 10));
	touching.add((10, 0, 10, 10));
	assert_eq!(touching.rects(), &[(0, 0, 20, 10)], "two rectangles side by side are one strip");

	let mut contained = DamageSet::new();
	contained.add((0, 0, 100, 100));
	contained.add((10, 10, 5, 5));
	assert_eq!(contained.rects(), &[(0, 0, 100, 100)], "a rectangle inside another adds nothing");
}

#[test]
// An empty rectangle is not damage, and the set stays bounded however many arrive.
fn the_set_is_bounded_and_ignores_what_is_not_damage() {
	let mut damage = DamageSet::new();
	assert!(damage.is_empty());
	damage.add((5, 5, 0, 10));
	damage.add((5, 5, 10, 0));
	assert!(damage.is_empty(), "a rectangle with no pixels is not damage");

	// Thirty-two rectangles far enough apart that none of them merges cheaply: the set holds its cap
	// and the rest are merged into what is there rather than dropped.
	for index in 0..32u32 {
		damage.add((index * 64, index * 32, 8, 8));
	}
	assert_eq!(damage.rects().len(), MAX_DAMAGE_RECTS, "the cap holds");
	// EVERY DAMAGED PIXEL IS STILL COVERED, which is the property that matters: a dropped rectangle
	// would leave the screen showing something that is no longer there.
	for index in 0..32u32 {
		let (x, y) = (index * 64, index * 32);
		assert!(damage.rects().iter().any(|rect| x >= rect.0 && y >= rect.1 && x < rect.0 + rect.2 && y < rect.1 + rect.3), "rectangle {index} is still covered");
	}

	damage.clear();
	assert!(damage.is_empty());
}

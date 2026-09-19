//! The fixtures. Coverage is on the BIT-EXACT side of this stack's split, so its fixtures hold exact
//! integers; shading is on the tolerance side and says so where it appears.

use super::*;

use alloc::vec;
use alloc::vec::Vec;

use render_math::{Vec3, Vec4, Viewport};

use crate::clip::{Varyings, Vertex};
use crate::fixed::{self, FixedFault, Subpixel};
use crate::raster::{self, Facing, PixelBox};

fn near(left: f32, right: f32) -> bool {
	(left - right).abs() <= 1e-4
}

fn viewport(width: f32, height: f32) -> Viewport {
	Viewport::new(0.0, 0.0, width, height)
}

// ---------------------------------------------------------------------------------------------
// The fixed-point grid, and the proof its arithmetic cannot overflow.
// ---------------------------------------------------------------------------------------------

#[test]
// THE BOUND IS A PROOF AND THIS IS THE ARITHMETIC IT IS ABOUT. Two coordinates at the extremes of
// the legal range produce an edge value that fits `i64` with the headroom the header claims, and
// nothing in the calculation is formed in a narrower type on the way.
fn the_edge_function_at_the_extremes_stays_inside_the_proven_bound() {
	let low = Subpixel(0);
	let high = Subpixel(fixed::MAX_SUBPIXEL_COORD);
	assert_eq!(fixed::MAX_SUBPIXEL_COORD, 1 << 22, "the proof is stated for 2^22 subpixel units");

	// The worst case the proof describes: every difference at its maximum magnitude.
	let value = fixed::edge((low, high), (high, low), (high, high));
	assert!(value.abs() <= 1_i64 << 45, "the header's bound is 2^45, and this is {value}");
	assert!(value.abs() > 1_i64 << 43, "and the worst case must actually reach it, or the proof is about nothing: {value}");

	// A COORDINATE EQUAL TO 2^22 IS ADMITTED AND ONE PAST IT IS REFUSED. A check written as "fits in
	// twenty-two bits" would refuse the largest legal value, which is the off-by-one this states.
	assert_eq!(fixed::admit(fixed::MAX_SUBPIXEL_COORD), Ok(high));
	assert_eq!(fixed::admit(fixed::MAX_SUBPIXEL_COORD + 1), Err(FixedFault::OutOfRange { subpixel: fixed::MAX_SUBPIXEL_COORD + 1 }));
	assert_eq!(fixed::admit(-1), Err(FixedFault::OutOfRange { subpixel: -1 }));
}

#[test]
// ROUND-HALF-AWAY-FROM-ZERO, stated because the three architectures this runs on have different
// defaults in their float-to-integer paths, and a coordinate that rounded differently on one of them
// would put a sample on the other side of an edge.
fn a_coordinate_is_quantised_half_away_from_zero_and_a_bad_one_is_refused() {
	let unit = 1.0 / fixed::SUBPIXEL_ONE as f32;
	assert_eq!(fixed::quantise(0.0), Ok(Subpixel(0)));
	assert_eq!(fixed::quantise(1.0), Ok(Subpixel(fixed::SUBPIXEL_ONE)));
	// Exactly half a subpixel rounds AWAY from zero, so up.
	assert_eq!(fixed::quantise(0.5 * unit), Ok(Subpixel(1)));
	assert_eq!(fixed::quantise(1.5 * unit), Ok(Subpixel(2)));
	assert_eq!(fixed::quantise(2.5 * unit), Ok(Subpixel(3)), "half-away, not half-to-even, which would give 2");

	assert_eq!(fixed::quantise(f32::NAN), Err(FixedFault::NotFinite));
	assert_eq!(fixed::quantise(f32::INFINITY), Err(FixedFault::NotFinite));
	assert!(matches!(fixed::quantise(-1.0), Err(FixedFault::OutOfRange { .. })), "a negative coordinate has not been clipped");
	assert!(matches!(fixed::quantise(fixed::MAX_RASTER_EXTENT as f32 + 1.0), Err(FixedFault::OutOfRange { .. })));
}

// ---------------------------------------------------------------------------------------------
// Triangle setup and coverage.
// ---------------------------------------------------------------------------------------------

/// Window-space vertices straight into a setup, with `1/w` of one - the orthographic case, which is
/// where the coverage arithmetic can be checked by hand.
fn flat_setup(points: [(f32, f32); 3], width: u32, height: u32) -> Option<Setup> {
	let window = [Vec3::new(points[0].0, points[0].1, 0.5), Vec3::new(points[1].0, points[1].1, 0.5), Vec3::new(points[2].0, points[2].1, 0.5)];
	raster::setup(window, [1.0, 1.0, 1.0], width, height).expect("the fixture's coordinates are in range")
}

/// Which pixels of a `width` x `height` target one triangle covers at one sample per pixel.
fn covered_pixels(setup: &Setup, width: u32, height: u32) -> Vec<(i64, i64)> {
	let mut out = Vec::new();
	for y in 0..height as i64 {
		for x in 0..width as i64 {
			if raster::coverage(setup, x, y, 1).unwrap() != 0 {
				out.push((x, y));
			}
		}
	}
	out
}

#[test]
// A DEGENERATE TRIANGLE PRODUCES NOTHING, DETERMINISTICALLY. Zero area means no sample can be
// strictly inside all three edges and the fill rule cannot rescue it, so it draws nothing on every
// architecture rather than a line on one of them.
fn a_triangle_with_no_area_is_skipped_and_is_not_an_error() {
	assert!(flat_setup([(0.0, 0.0), (10.0, 10.0), (20.0, 20.0)], 64, 64).is_none(), "three collinear points");
	assert!(flat_setup([(5.0, 5.0), (5.0, 5.0), (5.0, 5.0)], 64, 64).is_none(), "a collapsed vertex");
	// And a coordinate that was never clipped is a REFUSAL rather than a skip.
	let window = [Vec3::new(-1.0, 0.0, 0.5), Vec3::new(10.0, 0.0, 0.5), Vec3::new(0.0, 10.0, 0.5)];
	assert!(raster::setup(window, [1.0; 3], 64, 64).is_err());
}

#[test]
// THE FACING THE RASTERISER DECIDES IS THE ONE `render-math` DECIDES, and the two are stated in
// different spaces: `facing` reads NDC BEFORE the viewport inverts Y, and this reads window space
// AFTER. The inversion reverses apparent winding, so they agree only because this one is written
// against the flipped space - which is exactly the mistake that culls the wrong side while
// satisfying every other rule in the stack.
fn the_rasterisers_facing_agrees_with_the_frozen_winding_before_the_y_flip() {
	let view = viewport(100.0, 100.0);
	// A counter-clockwise triangle in NDC: the FRONT.
	let ndc = [Vec3::new(-0.5, -0.5, 0.5), Vec3::new(0.5, -0.5, 0.5), Vec3::new(0.0, 0.5, 0.5)];
	assert_eq!(render_math::facing(ndc[0], ndc[1], ndc[2]), render_math::Facing::Front);
	let window: Vec<Vec3> = ndc.iter().map(|point| render_math::window_from_ndc(*point, &view).unwrap()).collect();
	let setup = raster::setup([window[0], window[1], window[2]], [1.0; 3], 100, 100).unwrap().unwrap();
	assert_eq!(setup.facing, Facing::Front);

	// The same triangle wound the other way is the BACK, in both spaces.
	assert_eq!(render_math::facing(ndc[0], ndc[2], ndc[1]), render_math::Facing::Back);
	let setup = raster::setup([window[0], window[2], window[1]], [1.0; 3], 100, 100).unwrap().unwrap();
	assert_eq!(setup.facing, Facing::Back);
	// AND THE REWIND MADE THE AREA POSITIVE ANYWAY, so the edge tests are one piece of code.
	assert!(setup.double_area > 0, "a back face is rewound rather than given its own comparisons");

	assert!(raster::culled(Facing::Back, render3d::Cull::Back));
	assert!(!raster::culled(Facing::Front, render3d::Cull::Back));
	assert!(!raster::culled(Facing::Back, render3d::Cull::None) && !raster::culled(Facing::Front, render3d::Cull::None));
}

#[test]
// THE TOP-LEFT RULE, WHICH IS WHAT THIS WHOLE FIXED-POINT APPARATUS IS FOR. Two triangles sharing an
// edge fill every sample EXACTLY ONCE: never twice, which is visible the moment anything is blended,
// and never zero times, which is a seam of background pixels along a diagonal.
fn two_triangles_sharing_an_edge_cover_every_sample_exactly_once() {
	let (width, height) = (32_u32, 32_u32);
	// A quad from (4,4) to (20,20), split along its diagonal.
	let lower = flat_setup([(4.0, 4.0), (20.0, 4.0), (20.0, 20.0)], width, height).unwrap();
	let upper = flat_setup([(4.0, 4.0), (20.0, 20.0), (4.0, 20.0)], width, height).unwrap();

	let mut counts = vec![0_u32; (width * height) as usize];
	for setup in [&lower, &upper] {
		for (x, y) in covered_pixels(setup, width, height) {
			counts[(y as u32 * width + x as u32) as usize] += 1;
		}
	}
	let doubled = counts.iter().filter(|count| **count > 1).count();
	assert_eq!(doubled, 0, "{doubled} samples were filled twice, so the shared edge double-fills");

	// And the union is the quad: every sample strictly inside it is filled once. The sample is at
	// the pixel centre, so pixels 4..=19 on each axis have their centre inside.
	for y in 4..20 {
		for x in 4..20 {
			assert_eq!(counts[(y * width + x) as usize], 1, "the sample at ({x}, {y}) was filled {} times", counts[(y * width + x) as usize]);
		}
	}
	// Nothing outside the quad is touched.
	assert_eq!(counts.iter().filter(|count| **count > 0).count(), 16 * 16);
}

#[test]
// A SUB-PIXEL TRIANGLE IS NOT SKIPPED BY SIZE. It either covers a sample point or it does not.
// Dropping small geometry makes distant objects flicker as the camera moves, which looks like a
// level-of-detail bug and is not one.
fn a_triangle_smaller_than_a_pixel_is_kept_and_answers_by_coverage() {
	// A tiny triangle around the centre of pixel (8, 8), which sits at (8.5, 8.5).
	let covering = flat_setup([(8.3, 8.3), (8.8, 8.3), (8.5, 8.8)], 32, 32).unwrap();
	assert_ne!(raster::coverage(&covering, 8, 8, 1).unwrap(), 0, "it contains the sample point");
	assert_eq!(covered_pixels(&covering, 32, 32).len(), 1);

	// The same size, moved off the sample point: covered by NOTHING, and still not an error.
	let missing = flat_setup([(8.05, 8.05), (8.3, 8.05), (8.15, 8.3)], 32, 32).unwrap();
	assert_eq!(raster::coverage(&missing, 8, 8, 1).unwrap(), 0);
	assert!(covered_pixels(&missing, 32, 32).is_empty(), "no sample point is inside it");
	assert!(missing.double_area > 0, "and it was not discarded as degenerate");
}

#[test]
// MULTISAMPLING TAKES THE PROFILE'S OWN SAMPLE POSITIONS, from `render3d` rather than from a second
// copy of the grid: two copies are two grids that can disagree, and the disagreement is an MSAA edge
// that is right in the reference and wrong in the renderer.
fn partial_coverage_at_four_samples_is_neither_empty_nor_full() {
	// The half-plane `x + y <= 16`, whose edge runs diagonally across the target. Somewhere along it
	// a pixel must have SOME samples covered and not all - which pixel depends on the profile's
	// sample grid, so the fixture looks for one rather than naming one and hard-coding the grid.
	let setup = flat_setup([(0.0, 16.0), (16.0, 0.0), (0.0, 0.0)], 32, 32).unwrap();
	let partial = (0..16_i64).filter(|step| {
		let mask = raster::coverage(&setup, 16 - 1 - step, *step, 4).unwrap();
		mask != 0 && mask != 0b1111
	});
	assert!(partial.count() > 0, "no pixel along the diagonal has partial coverage, so multisampling is not sampling");
	// Well inside, every sample; well outside, none.
	assert_eq!(raster::coverage(&setup, 2, 2, 4).unwrap(), 0b1111);
	assert_eq!(raster::coverage(&setup, 14, 14, 4).unwrap(), 0);
	// A sample count the profile does not have is refused rather than approximated.
	assert!(raster::coverage(&setup, 0, 0, 3).is_err());
}

#[test]
// BINNING PUTS A TRIANGLE IN EVERY TILE ITS BOX TOUCHES, AND THE BINS ARE REUSED. A frame that
// allocated its tile lists again would be allocating per frame, which is the thing the 3D side is
// held to as much as the 2D one.
fn a_triangle_reaches_every_tile_its_box_touches_and_the_bins_are_reused() {
	let mut bins = raster::Bins::new(128, 128);
	assert_eq!((bins.across(), bins.down()), (4, 4));
	// A box spanning tiles (0,0) to (1,1).
	bins.insert(7, &PixelBox { x0: 10, y0: 10, x1: 40, y1: 40 });
	assert_eq!(bins.tile(0, 0), &[7]);
	assert_eq!(bins.tile(1, 1), &[7]);
	assert!(bins.tile(2, 2).is_empty());
	// One wholly inside a single tile reaches only that tile.
	bins.insert(9, &PixelBox { x0: 70, y0: 70, x1: 80, y1: 80 });
	assert_eq!(bins.tile(2, 2), &[9]);
	assert_eq!(bins.tile(0, 0), &[7], "and does not leak into another");

	bins.clear();
	assert!(bins.tile(0, 0).is_empty() && bins.tile(2, 2).is_empty());
	// A resize to the same tile count keeps the grid; a different one changes it.
	bins.resize(120, 120);
	assert_eq!((bins.across(), bins.down()), (4, 4));
	bins.resize(256, 64);
	assert_eq!((bins.across(), bins.down()), (8, 2));
	assert_eq!(bins.tile_box(7, 1, 256, 64), PixelBox { x0: 224, y0: 32, x1: 256, y1: 64 });

	// An empty box bins nowhere rather than panicking.
	bins.insert(3, &PixelBox { x0: 5, y0: 5, x1: 5, y1: 5 });
	assert!(bins.tile(0, 0).is_empty());
}

#[test]
// A VERTEX WITH `w` AT OR BELOW ZERO REACHING THE RASTERISER IS A REFUSAL, because it means the
// clipper did not run. Inventing a coordinate for it would draw a triangle that is not the one
// submitted, and the wrong triangle is harder to find than a refused frame.
fn projection_refuses_what_the_clipper_should_have_removed() {
	let view = viewport(100.0, 100.0);
	assert!(raster::project(Vec4::new(0.0, 0.0, 0.5, 0.0), &view).is_err());
	assert!(raster::project(Vec4::new(0.0, 0.0, 0.5, -1.0), &view).is_err());
	assert!(matches!(raster::project(Vec4::new(f32::NAN, 0.0, 0.5, 1.0), &view), Err(render3d::Error::NonFinite { .. })));

	// A vertex at the centre of the clip volume lands at the centre of the viewport.
	let (window, inverse_w) = raster::project(Vec4::new(0.0, 0.0, 0.5, 1.0), &view).unwrap();
	assert!(near(window.x, 50.0) && near(window.y, 50.0) && near(window.z, 0.5));
	assert!(near(inverse_w, 1.0));
	// And `1/w` is what interpolation needs, not `w`.
	let (_, inverse_w) = raster::project(Vec4::new(0.0, 0.0, 1.0, 4.0), &view).unwrap();
	assert!(near(inverse_w, 0.25));
}

// ---------------------------------------------------------------------------------------------
// Clipping.
// ---------------------------------------------------------------------------------------------

#[test]
// A TRIANGLE WHOLLY INSIDE IS NOT CUT, one wholly outside a plane produces nothing, and one that
// straddles is cut - each decided BEFORE any vertex is produced, so a primitive that never needed
// cutting is not cut.
fn a_triangle_is_cut_only_when_it_straddles_a_plane() {
	let inside = [Vertex::new(Vec4::new(-0.5, -0.5, 0.5, 1.0)), Vertex::new(Vec4::new(0.5, -0.5, 0.5, 1.0)), Vertex::new(Vec4::new(0.0, 0.5, 0.5, 1.0))];
	let clipped = clip::clip_triangle(&inside, Varyings::EMPTY).unwrap();
	assert_eq!(clipped.vertices().len(), 3, "nothing was cut");
	assert_eq!(clipped.triangle(0), Some([0, 1, 2]));

	let outside = [Vertex::new(Vec4::new(5.0, 0.0, 0.5, 1.0)), Vertex::new(Vec4::new(6.0, 0.0, 0.5, 1.0)), Vertex::new(Vec4::new(5.5, 1.0, 0.5, 1.0))];
	assert!(clip::clip_triangle(&outside, Varyings::EMPTY).unwrap().is_empty());

	// Straddling the right plane: the cut adds vertices and the result stays convex.
	let straddling = [Vertex::new(Vec4::new(0.0, -0.5, 0.5, 1.0)), Vertex::new(Vec4::new(2.0, 0.0, 0.5, 1.0)), Vertex::new(Vec4::new(0.0, 0.5, 0.5, 1.0))];
	let clipped = clip::clip_triangle(&straddling, Varyings::EMPTY).unwrap();
	assert!(clipped.vertices().len() > 3, "a cut triangle has more vertices: {}", clipped.vertices().len());
	assert!(clipped.vertices().len() <= clip::MAX_CLIPPED_VERTICES);
	assert!(clipped.vertices().iter().all(|vertex| vertex.position.x <= vertex.position.w + 1e-5), "every vertex is inside the plane it was cut against");
	assert_eq!(clipped.triangle_count(), clipped.vertices().len() - 2);
}

#[test]
// THE BOUND IS PROVEN, NOT CONFIGURED: at most one vertex per half-space, so nine, and fan
// triangulation of nine is seven.
fn the_clipped_vertex_and_triangle_bounds_are_the_numbers_the_proof_gives() {
	assert_eq!(clip::MAX_CLIPPED_VERTICES, 9);
	assert_eq!(clip::MAX_TRIANGLES_AFTER_CLIP, 7);
	// A TRIANGLE THAT CONTAINS THE WHOLE VOLUME COMES BACK AS THE VOLUME'S CROSS-SECTION - four
	// vertices, not nine - which is worth stating because it is the case a reader expects to be the
	// worst one and it is not.
	let containing = [Vertex::new(Vec4::new(-8.0, -8.0, 0.5, 1.0)), Vertex::new(Vec4::new(8.0, -8.0, 0.5, 1.0)), Vertex::new(Vec4::new(0.0, 8.0, 0.5, 1.0))];
	assert_eq!(clip::clip_triangle(&containing, Varyings::EMPTY).unwrap().vertices().len(), 4, "the square cross-section of the volume");

	// The bound is reached by a triangle that CUTS CORNERS rather than swallowing the volume, and by
	// one that leaves through the depth planes as well. A deterministic sweep over such shapes,
	// every one of them held to the bound.
	let mut most = 0;
	for step in 0..24 {
		let angle = step as f32 * 0.25;
		let reach = 1.2 + step as f32 * 0.3;
		let corners = [
			Vertex::new(Vec4::new(-reach, -0.8 + angle * 0.05, -0.4, 1.0)),
			Vertex::new(Vec4::new(reach, -0.7 + angle * 0.05, 1.6, 1.0)),
			Vertex::new(Vec4::new(0.1 * angle, reach, 0.5, 1.0)),
		];
		let clipped = clip::clip_triangle(&corners, Varyings::EMPTY).unwrap();
		assert!(clipped.vertices().len() <= clip::MAX_CLIPPED_VERTICES, "step {step} produced {} vertices", clipped.vertices().len());
		assert!(clipped.triangle_count() <= clip::MAX_TRIANGLES_AFTER_CLIP);
		most = most.max(clipped.vertices().len());
	}
	assert!(most >= 6, "the sweep never got past {most} vertices, so it is not exercising the bound");
}

#[test]
// THE THREE QUALIFIERS ARE CUT BY THREE DIFFERENT RULES. `smooth` follows the homogeneous parameter
// and `noperspective` the PROJECTED one, and on a foreshortened edge the two are different numbers -
// which is the whole point: using one for both puts a kink exactly at the clip edge.
fn smooth_and_noperspective_are_cut_at_different_parameters() {
	// An edge from w = 1 to w = 9, crossing the left plane. The homogeneous and projected parameters
	// differ most where the two `w` values differ most.
	let triangle = [
		Vertex::new(Vec4::new(-2.0, 0.0, 0.5, 1.0)).with_smooth(Varyings::from_slice(&[0.0]).unwrap()).with_noperspective(Varyings::from_slice(&[0.0]).unwrap()),
		Vertex::new(Vec4::new(4.0, 0.0, 0.5, 9.0)).with_smooth(Varyings::from_slice(&[1.0]).unwrap()).with_noperspective(Varyings::from_slice(&[1.0]).unwrap()),
		Vertex::new(Vec4::new(0.0, 1.0, 0.5, 1.0)).with_smooth(Varyings::from_slice(&[0.5]).unwrap()).with_noperspective(Varyings::from_slice(&[0.5]).unwrap()),
	];
	let clipped = clip::clip_triangle(&triangle, Varyings::EMPTY).unwrap();
	let cut = clipped.vertices().iter().find(|vertex| (vertex.position.x + vertex.position.w).abs() < 1e-4).expect("a vertex was produced on the left plane");
	assert!((cut.smooth.as_slice()[0] - cut.noperspective.as_slice()[0]).abs() > 0.05, "the two parameters must differ on a foreshortened edge: smooth {} against noperspective {}", cut.smooth.as_slice()[0], cut.noperspective.as_slice()[0]);
	// The projected parameter is the larger one here, because the far endpoint's `w` pulls it.
	assert!(cut.noperspective.as_slice()[0] > cut.smooth.as_slice()[0]);
}

#[test]
// A `flat` ATTRIBUTE SURVIVES A CLIP THAT REMOVES THE VERTEX IT CAME FROM. It identifies the
// primitive - a material index, an object id - and a clipped triangle is the same triangle. Taking
// it from a vertex the clipper invented would make an id depend on where the camera is.
fn a_flat_attribute_is_carried_through_a_cut_that_removes_its_own_vertex() {
	let triangle = [
		// The provoking vertex, far outside the volume: the clipper will remove it.
		Vertex::new(Vec4::new(-20.0, 0.0, 0.5, 1.0)),
		Vertex::new(Vec4::new(0.5, -0.5, 0.5, 1.0)),
		Vertex::new(Vec4::new(0.5, 0.5, 0.5, 1.0)),
	];
	let identity = Varyings::from_slice(&[4242.0]).unwrap();
	let clipped = clip::clip_triangle(&triangle, identity).unwrap();
	assert!(!clipped.is_empty(), "part of the triangle is inside");
	assert!(clipped.vertices().iter().all(|vertex| vertex.position.x >= -vertex.position.w - 1e-4), "the outside vertex is gone");
	assert_eq!(clipped.flat.as_slice(), identity.as_slice(), "and the identity it carried is unchanged");
}

#[test]
// A NON-FINITE POSITION REFUSES THE PRIMITIVE and a wholly-behind-the-eye one CULLS it. Both are
// `render3d`'s own classification, so this clipper and the API's agree about what is outside.
fn a_non_finite_vertex_refuses_and_geometry_behind_the_eye_culls() {
	let bad = [Vertex::new(Vec4::new(f32::NAN, 0.0, 0.5, 1.0)), Vertex::new(Vec4::new(0.5, -0.5, 0.5, 1.0)), Vertex::new(Vec4::new(0.5, 0.5, 0.5, 1.0))];
	assert!(matches!(clip::clip_triangle(&bad, Varyings::EMPTY), Err(render3d::Error::NonFinite { .. })));

	let behind = [Vertex::new(Vec4::new(0.0, 0.0, -1.0, -1.0)), Vertex::new(Vec4::new(1.0, 0.0, -1.0, -2.0)), Vertex::new(Vec4::new(0.0, 1.0, -1.0, -1.0))];
	assert!(clip::clip_triangle(&behind, Varyings::EMPTY).unwrap().is_empty(), "behind the eye is a cull and not a refusal");
}

// ---------------------------------------------------------------------------------------------
// Interpolation.
// ---------------------------------------------------------------------------------------------

#[test]
// PERSPECTIVE CORRECTION IS WHAT MAKES A TEXTURE STAY ON A SURFACE. On a foreshortened triangle the
// corrected value and the affine one differ substantially, and the affine one is the swimming
// texture that made early software renderers famous.
fn a_smooth_varying_is_perspective_corrected_and_a_noperspective_one_is_not() {
	// Two vertices four times as far away as the third.
	let inverse_w = [1.0, 0.25, 0.25];
	let values = [0.0, 1.0, 1.0];
	let middle = [0.5, 0.5, 0.0];

	let corrected = interp::smooth(middle, inverse_w, values);
	let affine = interp::noperspective(middle, values);
	assert!(near(affine, 0.5), "the affine answer is the plain average: {affine}");
	// At the screen-space midpoint of an edge whose far end is four times further, the surface point
	// is only a fifth of the way along: 0.5*0.25 / (0.5*1 + 0.5*0.25) = 0.2.
	assert!(near(corrected, 0.2), "the perspective-correct answer is {corrected}");

	// With every `w` equal the two agree, which is why the mistake survives an orthographic test.
	let flat = interp::smooth(middle, [1.0, 1.0, 1.0], values);
	assert!(near(flat, affine));
}

#[test]
// DEPTH IS ALREADY LINEAR IN SCREEN SPACE AND IS NOT CORRECTED A SECOND TIME. Applying the
// perspective correction to `z/w` is the defect that makes a depth buffer almost right - near
// geometry correct, far geometry subtly wrong, and z-fighting where nothing overlaps.
fn depth_is_interpolated_linearly_in_screen_space() {
	let inverse_w = [1.0, 0.25, 0.25];
	let depths = [0.1, 0.9, 0.9];
	let middle = [0.5, 0.5, 0.0];
	assert!(near(interp::depth(middle, depths), 0.5), "the plain average, because z/w is already screen-linear");
	assert!(!near(interp::smooth(middle, inverse_w, depths), 0.5), "and it is NOT what the perspective-correct rule gives");

	// `1/w` itself is screen-linear too, which is why the correction can be built from it.
	assert!(near(interp::inverse_w_at(middle, inverse_w), 0.625));
}

#[test]
// THE BARYCENTRIC WEIGHTS ARE THE EDGE VALUES OVER THE AREA, and they sum to one everywhere inside
// the triangle - which is the property every interpolation above depends on.
fn the_barycentric_weights_sum_to_one_and_name_the_right_vertices() {
	let setup = flat_setup([(0.0, 0.0), (16.0, 0.0), (0.0, 16.0)], 32, 32).unwrap();
	for (x, y) in [(1_i64, 1_i64), (4, 2), (2, 8), (7, 7)] {
		let at = (Subpixel(x * fixed::SUBPIXEL_ONE + fixed::SUBPIXEL_HALF), Subpixel(y * fixed::SUBPIXEL_ONE + fixed::SUBPIXEL_HALF));
		let weights = raster::barycentric(&setup, at);
		assert!(near(weights[0] + weights[1] + weights[2], 1.0), "at ({x}, {y}) the weights are {weights:?}");
		assert!(weights.iter().all(|weight| *weight >= -1e-4), "and none is negative inside the triangle: {weights:?}");
	}
	// At a vertex the weight of that vertex is one.
	let corner = (Subpixel(0), Subpixel(0));
	let weights = raster::barycentric(&setup, corner);
	assert!(near(weights[0], 1.0) && near(weights[1], 0.0) && near(weights[2], 0.0), "{weights:?}");
}

// ---------------------------------------------------------------------------------------------
// Primitive assembly, lines and points.
// ---------------------------------------------------------------------------------------------

use crate::geometry::{self, Indices, Primitive};
use render3d::command::Topology;

fn assemble(topology: Topology, indices: Indices<'_>, count: u32, restart: bool) -> Vec<Primitive> {
	let mut out = Vec::new();
	let mut run = Vec::new();
	geometry::assemble(topology, indices, count, 0, restart, &mut out, &mut run).expect("the fixture's indices are in range");
	out
}

fn triangles(primitives: &[Primitive]) -> Vec<[u32; 3]> {
	primitives
		.iter()
		.filter_map(|primitive| match primitive {
			Primitive::Triangle { vertices, .. } => Some(*vertices),
			_ => None,
		})
		.collect()
}

#[test]
// EVERY TOPOLOGY IS ASSEMBLED BY ITS OWN RULE, and a strip ALTERNATES its winding so every triangle
// of it faces the same way - a strip that did not would have every other triangle culled.
fn each_topology_assembles_the_primitives_its_definition_gives() {
	assert_eq!(triangles(&assemble(Topology::TriangleList, Indices::None, 6, false)), vec![[0, 1, 2], [3, 4, 5]]);
	assert_eq!(triangles(&assemble(Topology::TriangleStrip, Indices::None, 5, false)), vec![[0, 1, 2], [2, 1, 3], [2, 3, 4]]);
	assert_eq!(triangles(&assemble(Topology::TriangleFan, Indices::None, 5, false)), vec![[0, 1, 2], [0, 2, 3], [0, 3, 4]]);

	let lines = assemble(Topology::LineList, Indices::None, 4, false);
	assert_eq!(lines.len(), 2);
	assert!(matches!(lines[0], Primitive::Line { vertices: [0, 1], .. }));
	let strip = assemble(Topology::LineStrip, Indices::None, 4, false);
	assert_eq!(strip.len(), 3);
	assert!(matches!(strip[2], Primitive::Line { vertices: [2, 3], .. }));
	assert_eq!(assemble(Topology::PointList, Indices::None, 3, false).len(), 3);

	// A trailing partial primitive produces nothing rather than a degenerate one.
	assert!(triangles(&assemble(Topology::TriangleList, Indices::None, 5, false)).len() == 1);
	assert!(assemble(Topology::TriangleStrip, Indices::None, 2, false).is_empty());
}

#[test]
// PRIMITIVE RESTART ENDS A STRIP OR A FAN AND IS REFUSED ON A LIST, which has nothing to restart: a
// restart in the middle of a list would silently drop a primitive rather than mean anything.
fn primitive_restart_ends_a_strip_and_is_refused_on_a_list() {
	let indices: [u16; 8] = [0, 1, 2, 3, geometry::RESTART_U16, 10, 11, 12];
	let strip = assemble(Topology::TriangleStrip, Indices::U16(&indices), 8, true);
	assert_eq!(triangles(&strip), vec![[0, 1, 2], [2, 1, 3], [10, 11, 12]], "the restart began a new strip");

	let fan = assemble(Topology::TriangleFan, Indices::U16(&indices), 8, true);
	assert_eq!(triangles(&fan), vec![[0, 1, 2], [0, 2, 3], [10, 11, 12]]);

	let mut out = Vec::new();
	assert!(geometry::assemble(Topology::TriangleList, Indices::U16(&indices), 8, 0, true, &mut out, &mut Vec::new()).is_err());
	assert!(geometry::assemble(Topology::PointList, Indices::U16(&indices), 8, 0, true, &mut out, &mut Vec::new()).is_err());
}

#[test]
// THE BASE VERTEX IS SIGNED AND THE SUM IS CHECKED. A negative base with a small index is a read
// before the buffer, which is the case a per-draw bound check cannot catch and a per-index one can.
fn a_base_vertex_shifts_every_index_and_a_negative_sum_is_refused() {
	let indices: [u32; 3] = [0, 1, 2];
	let mut out = Vec::new();
	geometry::assemble(Topology::TriangleList, Indices::U32(&indices), 3, 100, false, &mut out, &mut Vec::new()).unwrap();
	assert_eq!(triangles(&out), vec![[100, 101, 102]]);
	assert!(geometry::assemble(Topology::TriangleList, Indices::U32(&indices), 3, -1, false, &mut out, &mut Vec::new()).is_err(), "index 0 with a base of -1 reads before the buffer");
	// A count past the index buffer is refused rather than reading whatever follows it.
	assert!(geometry::assemble(Topology::TriangleList, Indices::U32(&indices), 9, 0, false, &mut out, &mut Vec::new()).is_err());
}

#[test]
// THE PROVOKING VERTEX IS PER TOPOLOGY, and a fan's is the SECOND vertex - not the hub, which is in
// every triangle and would give every triangle of a fan the same flat value.
fn the_provoking_vertex_is_the_one_each_topology_names() {
	assert_eq!(geometry::provoking(Topology::TriangleList, 2), 6);
	assert_eq!(geometry::provoking(Topology::TriangleStrip, 2), 2);
	assert_eq!(geometry::provoking(Topology::TriangleFan, 2), 3, "not the hub");
	assert_eq!(geometry::provoking(Topology::LineList, 3), 6);
	assert_eq!(geometry::provoking(Topology::LineStrip, 3), 3);
	assert_eq!(geometry::provoking(Topology::PointList, 3), 3);
}

/// A window-space point in subpixel units.
fn point(x: f32, y: f32) -> (Subpixel, Subpixel) {
	(fixed::quantise(x).unwrap(), fixed::quantise(y).unwrap())
}

#[test]
// THE DIAMOND-EXIT RULE, WHICH IS THE LINE'S VERSION OF THE TOP-LEFT RULE. Two segments that meet
// end to end cover the shared pixel EXACTLY once: the first ends inside its diamond and does not
// light it, the second starts there and leaves.
fn two_segments_meeting_end_to_end_light_the_shared_pixel_exactly_once() {
	let a = point(2.5, 8.5);
	let shared = point(10.5, 8.5);
	let b = point(18.5, 8.5);

	let mut counts = vec![0_u32; 32];
	for x in 0..32_i64 {
		if geometry::line_covers_pixel(a, shared, x, 8) {
			counts[x as usize] += 1;
		}
		if geometry::line_covers_pixel(shared, b, x, 8) {
			counts[x as usize] += 1;
		}
	}
	assert!(counts.iter().all(|count| *count <= 1), "a pixel was lit twice: {counts:?}");
	assert_eq!(counts[10], 1, "the shared pixel is lit exactly once");
	// The run is contiguous from the first segment's start to just before the second's end.
	assert_eq!(counts[2..18].iter().filter(|count| **count == 1).count(), 16, "{counts:?}");
	assert_eq!(counts[18], 0, "the final endpoint's own pixel is not lit: the segment ends inside its diamond");
}

#[test]
// A SEGMENT THAT NEVER LEAVES A DIAMOND LIGHTS NOTHING, which is the degenerate case the rule has to
// answer rather than dividing by a zero-length direction.
fn a_segment_shorter_than_a_diamond_lights_nothing() {
	let from = point(8.5, 8.5);
	let to = point(8.6, 8.5);
	assert!(!geometry::line_covers_pixel(from, to, 8, 8));
	assert!(!geometry::line_covers_pixel(from, from, 8, 8), "a zero-length segment");
	// And a diagonal one does light the pixels it crosses.
	let lit = (0..16_i64).filter(|step| geometry::line_covers_pixel(point(0.5, 0.5), point(15.5, 15.5), *step, *step)).count();
	assert!(lit >= 14, "a diagonal segment lights its own pixels: {lit}");
}

#[test]
// THE PARAMETER ALONG A LINE IS THE WINDOW-SPACE DISTANCE, so a `noperspective` varying is linear
// along the drawn line and not along the unprojected one.
fn the_line_parameter_is_the_projection_of_the_pixel_centre_onto_the_segment() {
	let from = point(0.5, 0.5);
	let to = point(16.5, 0.5);
	assert!(near(geometry::line_parameter(from, to, 0, 0), 0.0));
	assert!(near(geometry::line_parameter(from, to, 8, 0), 0.5));
	assert!(near(geometry::line_parameter(from, to, 16, 0), 1.0));
	// Off the end it clamps rather than extrapolating, which would give a varying outside its range.
	assert!(near(geometry::line_parameter(from, to, 24, 0), 1.0));
}

#[test]
// A POINT'S SQUARE IS HALF-OPEN, which is the axis-aligned form of the top-left rule: two adjacent
// points of the same size tile without overlapping.
fn two_adjacent_points_tile_without_overlapping() {
	assert_eq!(geometry::point_size(0.0), 0, "a size of zero draws nothing rather than one pixel");
	assert_eq!(geometry::point_size(-3.0), 0);
	assert_eq!(geometry::point_size(0.4), 1, "and anything positive is clamped up to one");
	assert_eq!(geometry::point_size(3.5), 4, "rounded half away from zero");
	assert_eq!(geometry::point_size(1000.0), crate::geometry::MAX_POINT_SIZE as u32);
	assert_eq!(geometry::point_size(f32::NAN), 0);

	// Two four-pixel points side by side, centred so their squares abut exactly.
	let left = point(8.0, 8.0);
	let right = point(12.0, 8.0);
	let mut counts = vec![0_u32; 32];
	for x in 0..32_i64 {
		let sample = (Subpixel(x * fixed::SUBPIXEL_ONE + fixed::SUBPIXEL_HALF), Subpixel(8 * fixed::SUBPIXEL_ONE + fixed::SUBPIXEL_HALF));
		if geometry::point_covers(left, 4, sample) {
			counts[x as usize] += 1;
		}
		if geometry::point_covers(right, 4, sample) {
			counts[x as usize] += 1;
		}
	}
	assert!(counts.iter().all(|count| *count <= 1), "the squares overlap: {counts:?}");
	assert_eq!(counts.iter().filter(|count| **count == 1).count(), 8, "four pixels each: {counts:?}");
}

// ---------------------------------------------------------------------------------------------
// The shader interpreter.
// ---------------------------------------------------------------------------------------------

use render_shader::ir::{BuiltIn, Output};
use render_shader::{BinaryOp, Binding, Builder, CompareKind, Constant, Module, Op, ScalarType, Stage, Stmt, Transcendental, Type, UnaryOp, Value as IrValue};

use crate::interpreter::{self, Fault, Resources};
use crate::value::Val;

/// A stub environment. Every binding answers from a small table, so a fixture states its inputs
/// beside its expected output rather than in a builder somewhere else.
struct Stub {
	uniforms: Vec<((u32, u32), Val)>,
	attributes: Vec<(u32, Val)>,
	varyings: Vec<(u32, Val)>,
	texel: Option<Val>,
}

impl Stub {
	fn new() -> Self {
		Self { uniforms: Vec::new(), attributes: Vec::new(), varyings: Vec::new(), texel: None }
	}

	fn with_attribute(mut self, location: u32, value: Val) -> Self {
		self.attributes.push((location, value));
		self
	}

	fn with_uniform(mut self, block: u32, member: u32, value: Val) -> Self {
		self.uniforms.push(((block, member), value));
		self
	}
}

impl Resources for Stub {
	fn uniform(&self, block: u32, member: u32) -> Option<Val> {
		self.uniforms.iter().find(|(key, _)| *key == (block, member)).map(|(_, value)| value.clone())
	}

	fn attribute(&self, location: u32) -> Option<Val> {
		self.attributes.iter().find(|(key, _)| *key == location).map(|(_, value)| value.clone())
	}

	fn varying(&self, location: u32) -> Option<Val> {
		self.varyings.iter().find(|(key, _)| *key == location).map(|(_, value)| value.clone())
	}

	fn built_in(&self, which: BuiltIn) -> Option<Val> {
		match which {
			BuiltIn::VertexIndex => Some(Val::scalar_u32(7)),
			BuiltIn::FrontFacing => Some(Val::scalar_bool(true)),
			_ => None,
		}
	}

	fn sample(&self, _texture: u32, _sampler: u32, _coordinate: &Val) -> Option<Val> {
		self.texel.clone()
	}
}

#[test]
// A VERTEX STAGE COMPUTES WHAT ITS IR SAYS. The matrix product is COLUMN-MAJOR and `M * v`, the same
// convention `render-math` fixes - a second convention in the interpreter would make a shader and
// the scene layer disagree about what a transform does, which looks like a wrong model matrix.
fn a_vertex_stage_transforms_its_attribute_by_its_uniform_matrix() {
	let mut builder = Builder::new(Stage::Vertex, "transform");
	let matrix = builder.load(Type::Matrix(4), Binding::Uniform { block: 0, member: 0 });
	let attribute = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let clip = builder.assign(Type::vec(4), Op::Binary(BinaryOp::MatrixProduct, matrix, attribute));
	builder.store(Output::Position, clip);
	let module = builder.finish();

	// A translation by (10, 20, 30), column-major: the translation is the LAST column.
	let translation = Val::matrix(&[[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [10.0, 20.0, 30.0, 1.0]], 4);
	let stub = Stub::new().with_uniform(0, 0, translation).with_attribute(0, Val::vector_f32(&[1.0, 2.0, 3.0, 1.0]));
	let outputs = interpreter::execute(&module, &stub).unwrap();
	let position = outputs.position.expect("a vertex stage writes a position");
	assert!(near(position.f32_at(0), 11.0) && near(position.f32_at(1), 22.0) && near(position.f32_at(2), 33.0) && near(position.f32_at(3), 1.0), "{:?}", position.to_f32());

	// And the same matrix through `render-math` gives the same answer, which is what "one convention"
	// means: two implementations of `M * v` that disagree would each be self-consistent.
	let reference = render_math::Mat4::from_translation(Vec3::new(10.0, 20.0, 30.0)).transform_point(Vec3::new(1.0, 2.0, 3.0));
	assert!(near(position.f32_at(0), reference.x) && near(position.f32_at(1), reference.y) && near(position.f32_at(2), reference.z));
}

#[test]
// EVERY LOOP IS BOUNDED BEFORE IT RUNS, and `break` and `continue` do what their names say. A shader
// cannot hang, which is what makes a frame's worst case computable rather than hoped for.
fn a_bounded_loop_runs_its_trips_and_break_leaves_it() {
	// Sum 1 ten times, breaking after the fifth.
	let mut builder = Builder::new(Stage::Fragment, "loop");
	let zero = builder.constant(Constant::I32(0));
	let _ = zero;
	let one = builder.constant(Constant::F32(1.0));
	let colour = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![one, one, one, one]));
	builder.loop_bounded(10);
	builder.break_loop();
	builder.end_loop(10);
	builder.store(Output::Colour(0), colour);
	let module = builder.finish();
	let outputs = interpreter::execute(&module, &Stub::new()).unwrap();
	assert_eq!(outputs.colour.len(), 1, "the store after the loop ran");

	// A loop whose body assigns is visible afterwards, and the trip count is what bounds it.
	let body = vec![Stmt::Assign(IrValue(1), Op::Binary(BinaryOp::Add, IrValue(0), IrValue(0)))];
	let module = Module {
		stage: Stage::Fragment,
		name: alloc::string::String::from("counting"),
		types: vec![Type::f32(), Type::f32(), Type::vec(4)],
		varyings: vec![],
		body: vec![
			Stmt::Assign(IrValue(0), Op::Const(Constant::F32(2.0))),
			Stmt::Loop { trips: 3, body },
			Stmt::Assign(IrValue(2), Op::Compose(Type::vec(4), vec![IrValue(1), IrValue(1), IrValue(1), IrValue(1)])),
			Stmt::Store(Output::Colour(0), IrValue(2)),
		],
	};
	let outputs = interpreter::execute(&module, &Stub::new()).unwrap();
	assert!(near(outputs.colour[0].1.f32_at(0), 4.0), "the last iteration's value survives the loop");
}

#[test]
// A DISCARDED FRAGMENT WRITES NOTHING - not depth, not stencil, not colour, not an identity - and
// the stage STOPS. Continuing would run stores whose results are thrown away, and a texture read
// among them would be work a discarded fragment paid for.
fn a_discard_ends_the_stage_and_leaves_nothing_written() {
	let mut builder = Builder::new(Stage::Fragment, "cutout");
	let one = builder.constant(Constant::F32(1.0));
	let colour = builder.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![one, one, one, one]));
	builder.discard();
	builder.store(Output::Colour(0), colour);
	let outputs = interpreter::execute(&builder.finish(), &Stub::new()).unwrap();
	assert!(outputs.discarded);
	assert!(outputs.colour.is_empty(), "the store after the discard did not run");
	assert!(outputs.depth.is_none() && outputs.integer.is_empty());
}

#[test]
// A COMPUTED INDEX OUTSIDE ITS ARRAY IS A TYPED REFUSAL AND NOT A READ. Validation decides the
// constant case; this is the one it cannot, because the value is not known until it is computed.
fn a_computed_index_outside_its_array_is_refused_at_the_access() {
	let array = Type::Array(alloc::boxed::Box::new(Type::f32()), 3);
	let module = Module {
		stage: Stage::Fragment,
		name: alloc::string::String::from("indexed"),
		types: vec![array.clone(), Type::Scalar(ScalarType::I32), Type::f32(), Type::vec(4)],
		varyings: vec![],
		body: vec![
			Stmt::Assign(IrValue(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
			Stmt::Assign(IrValue(1), Op::Const(Constant::I32(5))),
			Stmt::Assign(IrValue(2), Op::Index { array: IrValue(0), index: IrValue(1) }),
			Stmt::Assign(IrValue(3), Op::Compose(Type::vec(4), vec![IrValue(2), IrValue(2), IrValue(2), IrValue(2)])),
			Stmt::Store(Output::Colour(0), IrValue(3)),
		],
	};
	let stub = Stub::new().with_uniform(0, 0, Val::new(array, &[1.0_f32.to_bits(), 2.0_f32.to_bits(), 3.0_f32.to_bits()]));
	assert_eq!(interpreter::execute(&module, &stub), Err(Fault::IndexOutOfRange { index: 5, length: 3 }));

	// An index inside it reads the element it names.
	let mut inside = module.clone();
	inside.body[1] = Stmt::Assign(IrValue(1), Op::Const(Constant::I32(2)));
	let outputs = interpreter::execute(&inside, &stub).unwrap();
	assert!(near(outputs.colour[0].1.f32_at(0), 3.0));
}

#[test]
// A VALUE THE TAKEN BRANCH NEVER ASSIGNED IS REFUSED. Validation establishes single assignment over
// the WHOLE body; a branch not taken means the value was never produced, and reading it would be
// whatever the interpreter happened to leave there.
fn a_value_a_branch_did_not_assign_is_refused_rather_than_read() {
	let module = Module {
		stage: Stage::Fragment,
		name: alloc::string::String::from("branchy"),
		types: vec![Type::Scalar(ScalarType::Bool), Type::f32(), Type::vec(4)],
		varyings: vec![],
		body: vec![
			Stmt::Assign(IrValue(0), Op::Const(Constant::Bool(false))),
			Stmt::If { condition: IrValue(0), then_body: vec![Stmt::Assign(IrValue(1), Op::Const(Constant::F32(1.0)))], else_body: vec![] },
			Stmt::Assign(IrValue(2), Op::Compose(Type::vec(4), vec![IrValue(1), IrValue(1), IrValue(1), IrValue(1)])),
			Stmt::Store(Output::Colour(0), IrValue(2)),
		],
	};
	assert_eq!(interpreter::execute(&module, &Stub::new()), Err(Fault::Unassigned { value: 1 }));
}

#[test]
// THE DEGENERATE CASES EVERY INTERPRETER GETS WRONG ONCE, each with the reason it matters.
fn the_arithmetic_answers_the_degenerate_cases_the_way_the_rules_state() {
	let run = |kind: UnaryOp, input: Val| -> Val {
		let module = Module {
			stage: Stage::Fragment,
			name: alloc::string::String::from("unary"),
			types: vec![input.kind.clone(), input.kind.clone(), Type::vec(4)],
			varyings: vec![],
			body: vec![
				Stmt::Assign(IrValue(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
				Stmt::Assign(IrValue(1), Op::Unary(kind, IrValue(0))),
				Stmt::Store(Output::Colour(0), IrValue(1)),
			],
		};
		let stub = Stub::new().with_uniform(0, 0, input);
		interpreter::execute(&module, &stub).unwrap().colour.remove(0).1
	};

	// A ZERO-LENGTH VECTOR NORMALISES TO ITSELF and not to a NaN: one NaN in a lighting term makes a
	// whole surface black.
	let zero = run(UnaryOp::Normalize, Val::vector_f32(&[0.0, 0.0, 0.0]));
	assert!(zero.to_f32().iter().all(|value| *value == 0.0), "{:?}", zero.to_f32());
	let unit = run(UnaryOp::Normalize, Val::vector_f32(&[0.0, 3.0, 4.0]));
	assert!(near(unit.f32_at(1), 0.6) && near(unit.f32_at(2), 0.8));

	// `fract` IS `x - floor(x)`, so a negative coordinate gives a POSITIVE fraction - which is what
	// a repeating texture coordinate needs and what the language's `%` does not give.
	let fraction = run(UnaryOp::Fract, Val::vector_f32(&[-0.25, 0.75, -3.5]));
	assert!(near(fraction.f32_at(0), 0.75) && near(fraction.f32_at(1), 0.75) && near(fraction.f32_at(2), 0.5), "{:?}", fraction.to_f32());
}

#[test]
// A MODULO IS EUCLIDEAN, so the answer does not depend on the sign convention of the host language,
// and an INTEGER DIVISION BY ZERO ANSWERS ZERO rather than trapping: a shader has no way to report
// one and a trap would take the frame down.
fn modulo_is_euclidean_and_an_integer_divide_by_zero_answers_zero() {
	let binary = |op: BinaryOp, left: Val, right: Val| -> Val {
		let kind = left.kind.clone();
		let module = Module {
			stage: Stage::Fragment,
			name: alloc::string::String::from("binary"),
			types: vec![kind.clone(), kind.clone(), kind, Type::vec(4)],
			varyings: vec![],
			body: vec![
				Stmt::Assign(IrValue(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
				Stmt::Assign(IrValue(1), Op::Load(Binding::Uniform { block: 0, member: 1 })),
				Stmt::Assign(IrValue(2), Op::Binary(op, IrValue(0), IrValue(1))),
				Stmt::Store(Output::Colour(0), IrValue(2)),
			],
		};
		let stub = Stub::new().with_uniform(0, 0, left).with_uniform(0, 1, right);
		interpreter::execute(&module, &stub).unwrap().colour.remove(0).1
	};

	let wrapped = binary(BinaryOp::Modulo, Val::scalar_f32(-0.25), Val::scalar_f32(1.0));
	assert!(near(wrapped.f32_at(0), 0.75), "a negative coordinate wraps to a positive fraction: {}", wrapped.f32_at(0));

	let divided = binary(BinaryOp::Divide, Val::scalar_i32(7), Val::scalar_i32(0));
	assert_eq!(divided.i32_at(0), 0);
	let remainder = binary(BinaryOp::Modulo, Val::scalar_i32(-7), Val::scalar_i32(3));
	assert_eq!(remainder.i32_at(0), 2, "euclidean, so the remainder is never negative");

	// A float divide by zero is an infinity, which is IEEE's answer and the profile's.
	let infinite = binary(BinaryOp::Divide, Val::scalar_f32(1.0), Val::scalar_f32(0.0));
	assert!(infinite.f32_at(0).is_infinite() && infinite.f32_at(0) > 0.0);
}

#[test]
// A SELECT WITH A VECTOR CONDITION IS COMPONENT-WISE, which is what makes a per-component mask
// expressible without a branch per component - and a branch per component is what a scalar-only
// select forces a shader author to write.
fn a_vector_condition_selects_component_by_component() {
	let module = Module {
		stage: Stage::Fragment,
		name: alloc::string::String::from("masked"),
		types: vec![Type::Vector(ScalarType::Bool, 4), Type::vec(4), Type::vec(4), Type::vec(4)],
		varyings: vec![],
		body: vec![
			Stmt::Assign(IrValue(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
			Stmt::Assign(IrValue(1), Op::Load(Binding::Uniform { block: 0, member: 1 })),
			Stmt::Assign(IrValue(2), Op::Load(Binding::Uniform { block: 0, member: 2 })),
			Stmt::Assign(IrValue(3), Op::Select { condition: IrValue(0), on_true: IrValue(1), on_false: IrValue(2) }),
			Stmt::Store(Output::Colour(0), IrValue(3)),
		],
	};
	let mask = Val::new(Type::Vector(ScalarType::Bool, 4), &[1, 0, 1, 0]);
	let stub = Stub::new().with_uniform(0, 0, mask).with_uniform(0, 1, Val::vector_f32(&[1.0, 1.0, 1.0, 1.0])).with_uniform(0, 2, Val::vector_f32(&[0.0, 0.0, 0.0, 0.0]));
	let picked = interpreter::execute(&module, &stub).unwrap().colour.remove(0).1;
	assert_eq!(picked.to_f32(), vec![1.0, 0.0, 1.0, 0.0]);
}

#[test]
// A COMPARISON WITH A NaN IS FALSE EXCEPT FOR `NotEqual`, which is IEEE's rule and the one the
// frozen non-finite handling states. A backend that answered otherwise would take a different
// branch, and a position selected by that branch is a different triangle.
fn a_comparison_against_a_nan_follows_the_frozen_rule() {
	let compare = |kind: CompareKind, left: f32, right: f32| -> bool {
		let module = Module {
			stage: Stage::Fragment,
			name: alloc::string::String::from("compare"),
			types: vec![Type::f32(), Type::f32(), Type::Scalar(ScalarType::Bool), Type::vec(4)],
			varyings: vec![],
			body: vec![
				Stmt::Assign(IrValue(0), Op::Load(Binding::Uniform { block: 0, member: 0 })),
				Stmt::Assign(IrValue(1), Op::Load(Binding::Uniform { block: 0, member: 1 })),
				Stmt::Assign(IrValue(2), Op::Compare(kind, IrValue(0), IrValue(1))),
				Stmt::If { condition: IrValue(2), then_body: vec![Stmt::Store(Output::Depth, IrValue(0))], else_body: vec![] },
				Stmt::Assign(IrValue(3), Op::Compose(Type::vec(4), vec![IrValue(0), IrValue(0), IrValue(0), IrValue(0)])),
				Stmt::Store(Output::Colour(0), IrValue(3)),
			],
		};
		let stub = Stub::new().with_uniform(0, 0, Val::scalar_f32(left)).with_uniform(0, 1, Val::scalar_f32(right));
		interpreter::execute(&module, &stub).unwrap().depth.is_some()
	};
	assert!(!compare(CompareKind::Equal, f32::NAN, f32::NAN), "a NaN is not equal to itself");
	assert!(compare(CompareKind::NotEqual, f32::NAN, f32::NAN), "but it IS not-equal to itself");
	assert!(!compare(CompareKind::Less, f32::NAN, 1.0));
	assert!(!compare(CompareKind::GreaterOrEqual, f32::NAN, 1.0));
	assert!(compare(CompareKind::Equal, 1.0, 1.0), "and an ordinary comparison still works");
}

#[test]
// THE TRANSCENDENTALS ARE ON THE TOLERANCE SIDE OF THE SPLIT and this holds them against values
// computed from the definition rather than against themselves.
fn the_transcendentals_answer_within_the_tolerance_they_are_on() {
	let apply = |which: Transcendental, value: f32, second: Option<f32>| -> f32 {
		let mut types = vec![Type::f32(), Type::f32(), Type::vec(4)];
		let mut body = vec![Stmt::Assign(IrValue(0), Op::Load(Binding::Uniform { block: 0, member: 0 }))];
		let second_value = second.map(|_| {
			types.push(Type::f32());
			IrValue(3)
		});
		if second_value.is_some() {
			body.push(Stmt::Assign(IrValue(3), Op::Load(Binding::Uniform { block: 0, member: 1 })));
		}
		body.push(Stmt::Assign(IrValue(1), Op::Transcendental(which, IrValue(0), second_value)));
		body.push(Stmt::Assign(IrValue(2), Op::Compose(Type::vec(4), vec![IrValue(1), IrValue(1), IrValue(1), IrValue(1)])));
		body.push(Stmt::Store(Output::Colour(0), IrValue(2)));
		let module = Module { stage: Stage::Fragment, name: alloc::string::String::from("transcendental"), types, varyings: vec![], body };
		let mut stub = Stub::new().with_uniform(0, 0, Val::scalar_f32(value));
		if let Some(other) = second {
			stub = stub.with_uniform(0, 1, Val::scalar_f32(other));
		}
		interpreter::execute(&module, &stub).unwrap().colour.remove(0).1.f32_at(0)
	};

	assert!(near(apply(Transcendental::Sqrt, 16.0, None), 4.0));
	assert!(near(apply(Transcendental::InverseSqrt, 16.0, None), 0.25));
	assert!(near(apply(Transcendental::Sin, 0.0, None), 0.0));
	assert!(near(apply(Transcendental::Cos, 0.0, None), 1.0));
	assert!(near(apply(Transcendental::Sin, core::f32::consts::FRAC_PI_2, None), 1.0));
	assert!(near(apply(Transcendental::Exp, 0.0, None), 1.0));
	assert!(near(apply(Transcendental::Log, 1.0, None), 0.0));
	assert!(near(apply(Transcendental::Exp2, 10.0, None), 1024.0));
	assert!(near(apply(Transcendental::Log2, 1024.0, None), 10.0));
	assert!(near(apply(Transcendental::Pow, 2.0, Some(10.0)), 1024.0));
	assert!(near(apply(Transcendental::Atan2, 1.0, Some(1.0)), core::f32::consts::FRAC_PI_4));
	assert!(near(apply(Transcendental::Acos, 1.0, None), 0.0));
}

// ---------------------------------------------------------------------------------------------
// Textures and samplers.
// ---------------------------------------------------------------------------------------------

use crate::texture::{self, Border, Filter, Kind, Level, Sampler, Texel, Texture, Wrap};
use graphics_profile::image::{Semantics, Transfer};

fn strip(values: &[Texel]) -> Texture {
	let mut level = Level::new(values.len() as u32, 1, 1);
	for (index, texel) in values.iter().enumerate() {
		level.set(index as u32, 0, 0, *texel);
	}
	Texture { id: 1, kind: Kind::Dim2, levels: vec![level], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: true }
}

fn red(value: f32) -> Texel {
	[value, value, value, 1.0]
}

#[test]
// TEXEL CENTRES ARE AT `(i + 0.5) / extent`. A sampler that put them at `i / extent` is off by half
// a texel everywhere, which on a screen-aligned blit is a visible blur and nothing else.
fn a_nearest_sample_lands_on_the_texel_whose_centre_is_nearest() {
	let texture = strip(&[red(0.0), red(1.0)]);
	let sampler = Sampler { wrap_u: Wrap::ClampToEdge, ..Sampler::NEAREST };
	assert!(near(texture::sample(&texture, &sampler, [0.25, 0.5, 0.0], 0.0, true)[0], 0.0), "the centre of texel 0 is at 0.25");
	assert!(near(texture::sample(&texture, &sampler, [0.75, 0.5, 0.0], 0.0, true)[0], 1.0), "and of texel 1 at 0.75");
	// A TIE GOES TO THE LOWER INDEX, which makes the rule total: two backends round the same way at
	// exactly a half.
	assert!(near(texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 0.0, true)[0], 1.0), "u = 0.5 is texel-space 0.5, which rounds up to texel 1");
	assert!(near(texture::sample(&texture, &sampler, [0.25, 0.5, 0.0], 0.0, true)[0], 0.0));
}

#[test]
// `repeat` IS THE EUCLIDEAN REMAINDER, so `-0.25` and `0.75` sample the SAME texel. The language's
// `%` gives a negative index for the first, which lands somewhere else - and the difference only
// shows where a coordinate goes negative, which is exactly where nobody looks.
fn every_wrap_mode_brings_a_coordinate_back_the_way_its_definition_says() {
	let texture = strip(&[red(0.0), red(0.25), red(0.5), red(0.75)]);
	let read = |wrap: Wrap, u: f32| texture::sample(&texture, &Sampler { wrap_u: wrap, ..Sampler::NEAREST }, [u, 0.5, 0.0], 0.0, true)[0];

	assert!(near(read(Wrap::Repeat, -0.125), read(Wrap::Repeat, 0.875)), "a negative coordinate lands where its positive twin does");
	assert!(near(read(Wrap::Repeat, 1.125), read(Wrap::Repeat, 0.125)));
	// Clamp-to-edge holds the last texel however far outside the coordinate goes.
	assert!(near(read(Wrap::ClampToEdge, 5.0), 0.75));
	assert!(near(read(Wrap::ClampToEdge, -5.0), 0.0));
	// Mirrored repeat folds: just past one it comes back through the texels in reverse.
	assert!(near(read(Wrap::MirroredRepeat, 1.125), read(Wrap::MirroredRepeat, 0.875)));
	// Clamp-to-border answers the BORDER and not an edge texel.
	let bordered = Sampler { wrap_u: Wrap::ClampToBorder, border: Border::OpaqueWhite, ..Sampler::NEAREST };
	assert!(near(texture::sample(&texture, &bordered, [-1.0, 0.5, 0.0], 0.0, true)[0], 1.0));
	let transparent = Sampler { border: Border::TransparentBlack, ..bordered };
	assert!(near(texture::sample(&texture, &transparent, [-1.0, 0.5, 0.0], 0.0, true)[3], 0.0));
}

#[test]
// sRGB TEXELS ARE DECODED BEFORE FILTERING. Filtering encoded values averages in the wrong space,
// which makes every edge between two colours darker than either - and it looks like a texture that
// was authored badly rather than a sampler that is wrong.
fn a_filtered_srgb_sample_averages_in_linear_light_and_a_data_image_is_not_decoded() {
	let mut texture = strip(&[red(0.5), red(1.0)]);
	texture.transfer = Transfer::Srgb;
	let sampler = Sampler { wrap_u: Wrap::ClampToEdge, ..Sampler::LINEAR };
	// Encoded 0.5 decodes to about 0.2140; the average with 1.0 is about 0.6070.
	let filtered = texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 0.0, true)[0];
	assert!((filtered - 0.6070).abs() < 1e-3, "the linear average is 0.607, not the encoded one: {filtered}");
	assert!((filtered - 0.75).abs() > 0.1, "and it is certainly not the encoded average");

	// A MEASUREMENT IS NOT LIGHT. A `Data` image goes through no transfer whatever the transfer says.
	let mut data = texture.clone();
	data.semantics = Semantics::Data;
	let plain = texture::sample(&data, &sampler, [0.5, 0.5, 0.0], 0.0, true)[0];
	assert!(near(plain, 0.75), "the numbers are averaged as numbers: {plain}");
	let mut normal = texture.clone();
	normal.semantics = Semantics::Normal;
	assert!(near(texture::sample(&normal, &sampler, [0.5, 0.5, 0.0], 0.0, true)[0], 0.75));
}

#[test]
// PREMULTIPLIED BEFORE INTERPOLATION. Interpolating unpremultiplied alpha bleeds the colour of a
// transparent texel into its neighbours, which is the halo around every cut-out leaf ever rendered.
fn a_filtered_sample_premultiplies_before_it_interpolates() {
	let mut texture = strip(&[[1.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 0.0]]);
	texture.premultiplied = false;
	let sampler = Sampler { wrap_u: Wrap::ClampToEdge, ..Sampler::LINEAR };
	let filtered = texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 0.0, true);
	// Premultiplied the two texels are (1,0,0,1) and (0,0,0,0), so the average is (0.5,0,0,0.5).
	assert!(near(filtered[0], 0.5) && near(filtered[1], 0.0) && near(filtered[2], 0.0) && near(filtered[3], 0.5), "{filtered:?}");
	// Without the premultiply the white would bleed into the green and blue channels.
	assert!(filtered[1] < 0.01, "the transparent white did not bleed: {filtered:?}");
}

#[test]
// THE LEVEL COMES FROM THE MAXIMUM OF THE TWO DERIVATIVE LENGTHS, not their geometric mean: the mean
// under-filters exactly where an anisotropic footprint is worst, which is a floor at a grazing angle
// and is where aliasing is most visible.
fn the_level_is_log2_of_the_larger_derivative_and_is_clamped_to_the_chain() {
	let sampler = Sampler { max_lod: 1000.0, ..Sampler::LINEAR };
	// One texel per pixel in x, four in y, over a 64-texel texture: rho is 4, so lambda is 2.
	let level = texture::lambda([1.0 / 64.0, 0.0], [0.0, 4.0 / 64.0], 64, 64, &sampler, 7);
	assert!(near(level, 2.0), "{level}");
	// A geometric mean would give 1, which is one level too sharp.
	assert!(!near(level, 1.0));
	// A bias shifts it, and the clamps bound it.
	let biased = texture::lambda([1.0 / 64.0, 0.0], [0.0, 4.0 / 64.0], 64, 64, &Sampler { lod_bias: 1.5, ..sampler }, 7);
	assert!(near(biased, 3.5));
	let clamped = texture::lambda([1.0, 0.0], [0.0, 1.0], 64, 64, &sampler, 3);
	assert!(near(clamped, 2.0), "never past the last level that exists: {clamped}");
	// A zero footprint is the sharpest level rather than a negative infinity.
	assert!(near(texture::lambda([0.0, 0.0], [0.0, 0.0], 64, 64, &sampler, 7), 0.0));
}

#[test]
// TRILINEAR BLENDS TWO LEVELS BY `frac(lambda)`, AND AT OR ABOVE THE LAST LEVEL THE LAST LEVEL ALONE
// IS USED - which is the case a naive implementation blends with a level that does not exist.
fn a_trilinear_sample_blends_two_levels_and_stops_at_the_last_one() {
	let mut texture = strip(&[red(0.0), red(0.0), red(0.0), red(0.0)]);
	texture.levels.push(Level { width: 2, height: 1, depth: 1, texels: vec![red(1.0), red(1.0)] });
	texture.levels.push(Level { width: 1, height: 1, depth: 1, texels: vec![red(0.5)] });
	let sampler = Sampler { wrap_u: Wrap::ClampToEdge, ..Sampler::LINEAR };

	assert!(near(texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 0.0, false)[0], 0.0));
	assert!(near(texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 1.0, false)[0], 1.0));
	assert!(near(texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 0.25, false)[0], 0.25), "a quarter of the way between levels");
	assert!(near(texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 2.0, false)[0], 0.5), "the last level alone");
	assert!(near(texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 9.0, false)[0], 0.5), "and past it, still the last level");

	// A nearest mip filter picks one level and blends nothing.
	let nearest_mip = Sampler { mip: Filter::Nearest, ..sampler };
	assert!(near(texture::sample(&texture, &nearest_mip, [0.5, 0.5, 0.0], 0.4, false)[0], 0.0));
	assert!(near(texture::sample(&texture, &nearest_mip, [0.5, 0.5, 0.0], 0.6, false)[0], 1.0));
}

#[test]
// A DEPTH-COMPARE SAMPLE COMPARES EACH TAP FIRST AND FILTERS THE RESULTS, so a linear one is the
// FRACTION of taps that passed. Filtering the depths and comparing once produces a hard edge, which
// is what makes a shadow map look like a stencil and is the single commonest shadow bug.
fn a_depth_compare_sample_is_the_fraction_of_taps_that_passed() {
	let mut texture = strip(&[[0.2, 0.0, 0.0, 1.0], [0.8, 0.0, 0.0, 1.0]]);
	texture.semantics = Semantics::Depth;
	let sampler = Sampler { wrap_u: Wrap::ClampToEdge, compare: Some(render3d::CompareOp::LessOrEqual), ..Sampler::LINEAR };
	// A reference of 0.5 passes against the far texel and fails against the near one, so halfway
	// between them the answer is a half.
	let fraction = texture::sample_compare(&texture, &sampler, [0.5, 0.5], 0, 0.5).unwrap();
	assert!(near(fraction, 0.5), "{fraction}");
	// Filtering the depths first would compare 0.5 against 0.5 once and answer a hard 1.
	assert!(!near(fraction, 1.0) && !near(fraction, 0.0));
	// At the texel centres it is all or nothing.
	assert!(near(texture::sample_compare(&texture, &sampler, [0.25, 0.5], 0, 0.5).unwrap(), 0.0));
	assert!(near(texture::sample_compare(&texture, &sampler, [0.75, 0.5], 0, 0.5).unwrap(), 1.0));
	// A sampler with no comparison is refused rather than answering a depth.
	assert!(texture::sample_compare(&texture, &Sampler { compare: None, ..sampler }, [0.5, 0.5], 0, 0.5).is_err());
}

#[test]
// THE SIX FACES ARE +X, -X, +Y, -Y, +Z, -Z IN THAT ORDER, with the standard table's sign
// conventions - so a cube built for any other system loads without a flip. Getting one sign wrong
// mirrors one face, which in a reflection looks like a modelling error rather than a sampler one.
fn a_direction_selects_the_face_the_standard_table_gives() {
	assert_eq!(texture::cube_face([1.0, 0.0, 0.0]).0, 0);
	assert_eq!(texture::cube_face([-1.0, 0.0, 0.0]).0, 1);
	assert_eq!(texture::cube_face([0.0, 1.0, 0.0]).0, 2);
	assert_eq!(texture::cube_face([0.0, -1.0, 0.0]).0, 3);
	assert_eq!(texture::cube_face([0.0, 0.0, 1.0]).0, 4);
	assert_eq!(texture::cube_face([0.0, 0.0, -1.0]).0, 5);
	// The major axis decides, so a direction that is mostly +X is the +X face whatever the rest is.
	assert_eq!(texture::cube_face([0.9, 0.3, -0.2]).0, 0);
	// And the face-local coordinate of an axis direction is the face's centre.
	let (_, centre) = texture::cube_face([1.0, 0.0, 0.0]);
	assert!(near(centre[0], 0.5) && near(centre[1], 0.5), "{centre:?}");
	// A zero direction answers a face and a centre rather than dividing by zero.
	let (_, degenerate) = texture::cube_face([0.0, 0.0, 0.0]);
	assert!(near(degenerate[0], 0.5) && near(degenerate[1], 0.5));
}

#[test]
// A LINEAR FILTER WHOSE FOOTPRINT CROSSES A FACE EDGE TAKES ITS TAPS FROM THE NEIGHBOURING FACE.
// Clamping is what makes the seam visible, and a profile that left it open would make every cube map
// look different on two backends.
fn a_cube_sample_at_a_face_edge_takes_taps_from_the_neighbour() {
	// Six 2x2 faces, each a flat colour, so a tap that crosses an edge is visible as a value from a
	// face that is not the one the direction selected.
	let mut level = Level::new(2, 2, 6);
	for face in 0..6_u32 {
		for y in 0..2 {
			for x in 0..2 {
				level.set(x, y, face, [face as f32, 0.0, 0.0, 1.0]);
			}
		}
	}
	let texture = Texture { id: 2, kind: Kind::Cube, levels: vec![level], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: true };
	let sampler = Sampler { wrap_u: Wrap::ClampToEdge, wrap_v: Wrap::ClampToEdge, ..Sampler::LINEAR };

	// Well inside the +Z face: every tap is that face's own value.
	let inside = texture::sample_cube(&texture, &sampler, [0.0, 0.0, 1.0], 0.0);
	assert!(near(inside[0], 4.0), "the +Z face is index 4: {inside:?}");
	// Right at the edge between +Z and +X, the taps come from BOTH faces, so the filtered value lies
	// between them rather than being one of them.
	let seam = texture::sample_cube(&texture, &sampler, [0.999, 0.0, 1.0], 0.0);
	assert!(seam[0] > 0.0 && seam[0] < 4.0, "a clamped seam would answer exactly 4: {seam:?}");
}

#[test]
// MIPS ARE A 2x2 BOX IN THE TEXTURE'S OWN NUMERIC SPACE, which for a colour texture means LINEAR
// LIGHT: a box over sRGB-encoded values makes every level darker than the one above it, which reads
// as a texture that dims with distance.
fn mip_generation_is_a_box_filter_in_linear_light_and_odd_sizes_halve_by_flooring() {
	let mut level = Level::new(4, 4, 1);
	for y in 0..4 {
		for x in 0..4 {
			level.set(x, y, 0, red(if (x + y) % 2 == 0 { 0.0 } else { 1.0 }));
		}
	}
	let mut texture = Texture { id: 1, kind: Kind::Dim2, levels: vec![level], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: true };
	texture::generate_mips(&mut texture);
	assert_eq!(texture.levels(), 3, "4 -> 2 -> 1");
	assert_eq!((texture.levels[1].width, texture.levels[1].height), (2, 2));
	// Every 2x2 block of a checkerboard averages to a half.
	assert!(near(texture.levels[1].at(0, 0, 0)[0], 0.5));
	assert!(near(texture.levels[2].at(0, 0, 0)[0], 0.5));

	// AN ODD DIMENSION HALVES BY `max(1, floor(n/2))` AND THE BOX TAKES THE TEXELS THAT EXIST.
	let mut odd = Texture { id: 3, kind: Kind::Dim2, levels: vec![Level::new(5, 3, 1)], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: true };
	for y in 0..3 {
		for x in 0..5 {
			odd.levels[0].set(x, y, 0, red(1.0));
		}
	}
	texture::generate_mips(&mut odd);
	assert_eq!((odd.levels[1].width, odd.levels[1].height), (2, 1));
	assert!(near(odd.levels[1].at(0, 0, 0)[0], 1.0), "a partial box averages what exists, not what does not");

	// An sRGB texture is decoded once on the way in and the chain says so, or the next fetch would
	// decode it a second time.
	let mut encoded = Texture { id: 4, kind: Kind::Dim2, levels: vec![Level::new(2, 2, 1)], transfer: Transfer::Srgb, semantics: Semantics::Color, premultiplied: true };
	for y in 0..2 {
		for x in 0..2 {
			encoded.levels[0].set(x, y, 0, red(0.5));
		}
	}
	texture::generate_mips(&mut encoded);
	assert_eq!(encoded.transfer, Transfer::Linear);
	assert!((encoded.levels[1].at(0, 0, 0)[0] - 0.2140).abs() < 1e-3, "the box averaged decoded values: {}", encoded.levels[1].at(0, 0, 0)[0]);
	// AND LEVEL ZERO IS DECODED TOO, which is the half a chain check does not reach: every level
	// below it is built from decoded light, so a top level left in the source's own encoding while
	// the texture says `Linear` is read as light by every magnified fragment - more than twice the
	// value, and a step between level zero and level one that no filter put there.
	assert!((encoded.levels[0].at(0, 0, 0)[0] - 0.2140).abs() < 1e-3, "the top level is the decoded one: {}", encoded.levels[0].at(0, 0, 0)[0]);
	assert!(encoded.premultiplied, "and it says so, or the next fetch decodes it again");

	// AND A SOURCE THAT WAS NOT PREMULTIPLIED IS, because that is the other half of what the chain
	// declares: `linear` premultiplies on the way in, so every level including the top carries
	// colour already multiplied by its alpha.
	let mut straight = Texture { id: 5, kind: Kind::Dim2, levels: vec![Level::new(2, 2, 1)], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: false };
	for y in 0..2 {
		for x in 0..2 {
			straight.levels[0].set(x, y, 0, [0.8, 0.8, 0.8, 0.5]);
		}
	}
	texture::generate_mips(&mut straight);
	assert!(straight.premultiplied);
	assert!(near(straight.levels[0].at(0, 0, 0)[0], 0.4), "the top level is premultiplied: {}", straight.levels[0].at(0, 0, 0)[0]);
	assert!(near(straight.levels[1].at(0, 0, 0)[0], 0.4), "and so is the level under it: {}", straight.levels[1].at(0, 0, 0)[0]);
}

#[test]
// ANISOTROPY TAKES UP TO `max_anisotropy` TAPS ALONG THE MAJOR AXIS, AT THE LEVEL THE MINOR AXIS
// CHOOSES. Taking the level from the major axis is the isotropic answer and blurs exactly the
// direction anisotropy exists to keep sharp.
fn an_anisotropic_sample_is_sharper_than_the_isotropic_one_it_replaces() {
	// Vertical stripes on a 16 x 16 texture: constant down each column, alternating across.
	let mut level = Level::new(16, 16, 1);
	for y in 0..16 {
		for x in 0..16 {
			level.set(x, y, 0, red(if x % 2 == 0 { 0.0 } else { 1.0 }));
		}
	}
	let mut texture = Texture { id: 1, kind: Kind::Dim2, levels: vec![level], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: true };
	texture::generate_mips(&mut texture);

	// A footprint one texel wide and eight tall. The MINOR axis is x, so the level stays sharp and
	// the eight taps along y resolve the stretch; the isotropic rule takes the level from the major
	// axis and blurs the stripes away.
	let ddx = [1.0 / 16.0, 0.0];
	let ddy = [0.0, 8.0 / 16.0];
	let isotropic = Sampler { wrap_u: Wrap::Repeat, wrap_v: Wrap::Repeat, anisotropy: 1, ..Sampler::LINEAR };
	let anisotropic = Sampler { anisotropy: 8, ..isotropic };
	let coarse = texture::lambda(ddx, ddy, 16, 16, &isotropic, texture.levels());
	assert!(near(coarse, 3.0), "the isotropic rule picks level three, where the stripes are gone: {coarse}");

	// The centre of texel column 0, which is black.
	let point = [0.5 / 16.0, 0.5, 0.0];
	let sharp = texture::sample_anisotropic(&texture, &anisotropic, point, ddx, ddy, 16, 16);
	let blurred = texture::sample_anisotropic(&texture, &isotropic, point, ddx, ddy, 16, 16);
	assert!(sharp[0] < blurred[0] - 0.1, "the anisotropic sample kept the stripe: {} against {}", sharp[0], blurred[0]);
	assert!(blurred[0] > 0.4, "and the isotropic one averaged it away: {}", blurred[0]);

	// WITH `max_anisotropy` OF ONE THE RULE COLLAPSES TO THE ISOTROPIC ONE, which is what makes
	// anisotropy an addition rather than a second sampler.
	let plain = texture::sample(&texture, &isotropic, point, coarse, false);
	assert!(near(plain[0], blurred[0]), "{} against {}", plain[0], blurred[0]);
}

// ---------------------------------------------------------------------------------------------
// Render passes: load, store, the fragment path and the resolve.
// ---------------------------------------------------------------------------------------------

use crate::pass::{self, Colour, DepthStencil, Fragment, Operations, POISON};
use render3d::CompareOp;
use render3d::blend::{AttachmentBlend, BlendEquation, ColorWriteMask};
use render3d::depth::{DepthFormat, StencilFace, StencilOp};
use render3d::resource::{LoadOp, StoreOp};

fn opaque_blend() -> AttachmentBlend {
	AttachmentBlend { enabled: false, colour: BlendEquation::REPLACE, alpha: BlendEquation::REPLACE, write_mask: ColorWriteMask::ALL }
}

fn fragment<'a>(colour: Vec4, depth: f32, blend: &'a AttachmentBlend) -> Fragment<'a> {
	Fragment { x: 0, y: 0, coverage: 0b1111, depth, colour, blend, depth_compare: CompareOp::Less, depth_write: true, stencil: None, sample_mask: u32::MAX, alpha_to_coverage: false }
}

#[test]
// A DISCARDED ATTACHMENT IS POISONED AND NOT LEFT. A frame that reads what it discarded then looks
// wrong EVERYWHERE rather than looking right on the machine it was developed on - which is the
// difference between a defect found in an afternoon and one found by a user.
fn a_discarded_attachment_is_poisoned_at_both_ends_and_a_clear_has_a_value() {
	let mut attachment = Colour::new(2, 2, 1, false);
	attachment.fill(Vec4::new(0.25, 0.25, 0.25, 1.0));

	pass::load_colour(&mut attachment, &Operations { load: LoadOp::Load, store: StoreOp::Store, clear: Vec4::ZERO });
	assert!(near(attachment.at(0, 0, 0).x, 0.25), "a load keeps what is there");

	pass::load_colour(&mut attachment, &Operations { load: LoadOp::Clear, store: StoreOp::Store, clear: Vec4::new(0.5, 0.0, 0.0, 1.0) });
	assert!(near(attachment.at(1, 1, 0).x, 0.5), "a clear has a value");

	pass::load_colour(&mut attachment, &Operations { load: LoadOp::Discard, store: StoreOp::Store, clear: Vec4::ZERO });
	assert_eq!(attachment.at(0, 0, 0), POISON, "a discard has none, and says so loudly");

	let readable = pass::store_colour(&mut attachment, &Operations { load: LoadOp::Load, store: StoreOp::Store, clear: Vec4::ZERO });
	assert!(readable);
	let readable = pass::store_colour(&mut attachment, &Operations { load: LoadOp::Load, store: StoreOp::Discard, clear: Vec4::ZERO });
	assert!(!readable, "a discarded store says its contents may not be read");
	assert_eq!(attachment.at(0, 0, 0), POISON);
}

#[test]
// THE FRAGMENT PATH IS THE FROZEN ORDER. A nearer fragment replaces a further one and a further one
// is rejected, and the depth that was written is the one that passed.
fn a_depth_test_keeps_the_nearer_fragment_and_writes_its_depth() {
	let mut colour = Colour::new(1, 1, 1, false);
	let mut depth = DepthStencil::new(1, 1, 1, DepthFormat::Depth32F);
	depth.clear(1.0, 0);
	let state = opaque_blend();

	let written = pass::write_fragment(&mut colour, Some(&mut depth), &Fragment { coverage: 0b1, ..fragment(Vec4::new(1.0, 0.0, 0.0, 1.0), 0.5, &state) }).unwrap();
	assert_eq!(written, 1);
	assert!(near(colour.at(0, 0, 0).x, 1.0));

	// Further away: rejected, and the colour is untouched.
	let written = pass::write_fragment(&mut colour, Some(&mut depth), &Fragment { coverage: 0b1, ..fragment(Vec4::new(0.0, 1.0, 0.0, 1.0), 0.9, &state) }).unwrap();
	assert_eq!(written, 0);
	assert!(near(colour.at(0, 0, 0).x, 1.0) && near(colour.at(0, 0, 0).y, 0.0));

	// Nearer: accepted, and it replaces both the colour and the depth.
	let written = pass::write_fragment(&mut colour, Some(&mut depth), &Fragment { coverage: 0b1, ..fragment(Vec4::new(0.0, 0.0, 1.0, 1.0), 0.1, &state) }).unwrap();
	assert_eq!(written, 1);
	assert!(near(colour.at(0, 0, 0).z, 1.0));
	// And a pass with no depth attachment writes every fragment.
	let mut plain = Colour::new(1, 1, 1, false);
	assert_eq!(pass::write_fragment(&mut plain, None, &Fragment { coverage: 0b1, ..fragment(Vec4::new(1.0, 1.0, 1.0, 1.0), 0.9, &state) }).unwrap(), 1);
}

#[test]
// THE STENCIL TEST RUNS FIRST AND ITS OPERATION IS SELECTED BY WHICH TEST FAILED. An implementation
// that ran depth first would write a different stencil for every fragment the two disagree about,
// which is most of the fragments a stencil is used for.
fn the_stencil_test_runs_before_the_depth_test_and_selects_its_own_operation() {
	let mut colour = Colour::new(1, 1, 1, false);
	let mut depth = DepthStencil::new(1, 1, 1, DepthFormat::Depth24Stencil8);
	depth.clear(1.0, 0);
	let state = opaque_blend();
	// A stencil that never passes, with a distinct operation for each outcome.
	let face = StencilFace { compare: CompareOp::Never, read_mask: 0xFF, write_mask: 0xFF, reference: 1, on_fail: StencilOp::Replace, on_depth_fail: StencilOp::IncrementClamp, on_pass: StencilOp::DecrementClamp };
	let written = pass::write_fragment(&mut colour, Some(&mut depth), &Fragment { coverage: 0b1, stencil: Some((&face, true)), ..fragment(Vec4::new(1.0, 0.0, 0.0, 1.0), 0.1, &state) }).unwrap();
	assert_eq!(written, 0, "the stencil rejected it");
	assert_eq!(depth.stencil_at(0, 0, 0), 1, "and `on_fail` replaced the stencil with the reference");
	assert!(near(colour.at(0, 0, 0).x, 0.0), "nothing reached the colour attachment");

	// A stencil that always passes, with a depth that fails: `on_depth_fail`.
	let face = StencilFace { compare: CompareOp::Always, reference: 0, on_depth_fail: StencilOp::IncrementClamp, ..face };
	depth.clear(0.0, 5);
	let written = pass::write_fragment(&mut colour, Some(&mut depth), &Fragment { coverage: 0b1, stencil: Some((&face, true)), ..fragment(Vec4::new(1.0, 0.0, 0.0, 1.0), 0.9, &state) }).unwrap();
	assert_eq!(written, 0);
	assert_eq!(depth.stencil_at(0, 0, 0), 6, "the DEPTH-fail operation ran, not the stencil-fail one");
}

#[test]
// BLENDING THEN THE WRITE MASK, in that order: a masked channel keeps the DESTINATION's value rather
// than blending into it. Applying the mask first would blend a channel and then throw it away, which
// gives a different answer whenever the blend reads the destination.
fn blending_runs_before_the_write_mask_and_a_masked_channel_keeps_the_destination() {
	let mut colour = Colour::new(1, 1, 1, false);
	colour.set(0, 0, 0, Vec4::new(0.0, 0.0, 1.0, 1.0));
	// Premultiplied source-over, with the blue channel masked off.
	let state = AttachmentBlend { enabled: true, colour: BlendEquation::PREMULTIPLIED_OVER, alpha: BlendEquation::PREMULTIPLIED_OVER, write_mask: ColorWriteMask { red: true, green: true, blue: false, alpha: true } };
	pass::write_fragment(&mut colour, None, &Fragment { coverage: 0b1, ..fragment(Vec4::new(0.5, 0.0, 0.0, 0.5), 0.5, &state) }).unwrap();
	let result = colour.at(0, 0, 0);
	assert!(near(result.x, 0.5), "the red channel blended: {result:?}");
	assert!(near(result.z, 1.0), "and the blue one kept the destination whole rather than half of it");
}

#[test]
// AN INTEGER ATTACHMENT CANNOT BLEND, and the refusal is by name rather than a silently ignored
// state: an identity averaged with another identity is neither of them.
fn blending_an_integer_attachment_is_refused_rather_than_ignored() {
	let mut identity = Colour::new(1, 1, 1, true);
	let blending = AttachmentBlend { enabled: true, ..opaque_blend() };
	assert!(matches!(pass::write_fragment(&mut identity, None, &Fragment { coverage: 0b1, ..fragment(Vec4::new(7.0, 0.0, 0.0, 1.0), 0.5, &blending) }), Err(render3d::Error::UnsupportedFormat { .. })));
	// Without blending it writes the identity through unchanged.
	let plain = opaque_blend();
	pass::write_fragment(&mut identity, None, &Fragment { coverage: 0b1, ..fragment(Vec4::new(7.0, 0.0, 0.0, 1.0), 0.5, &plain) }).unwrap();
	assert!(near(identity.at(0, 0, 0).x, 7.0));
}

#[test]
// COLOUR AVERAGES AND AN INTEGER TAKES SAMPLE ZERO. Averaging an object identity produces one that
// belongs to no object, which makes a pick at a silhouette select something that is not there.
fn a_resolve_averages_colour_and_takes_the_first_sample_of_an_identity() {
	let mut colour = Colour::new(1, 1, 4, false);
	for sample in 0..4 {
		colour.set(0, 0, sample, Vec4::new(sample as f32, 0.0, 0.0, 1.0));
	}
	let resolved = colour.resolve().unwrap();
	assert!(near(resolved[0].x, 1.5), "the average of 0, 1, 2 and 3: {}", resolved[0].x);

	let mut identity = Colour::new(1, 1, 4, true);
	for sample in 0..4 {
		identity.set(0, 0, sample, Vec4::new(10.0 + sample as f32, 0.0, 0.0, 1.0));
	}
	let resolved = identity.resolve().unwrap();
	assert!(near(resolved[0].x, 10.0), "sample zero, not the average: {}", resolved[0].x);
}

#[test]
// THE SAMPLE MASK AND ALPHA-TO-COVERAGE BOTH NARROW COVERAGE BEFORE THE DEPTH TEST, so a sample they
// remove writes neither colour nor depth. Applying either afterwards would leave depth written for a
// sample that was not shaded.
fn coverage_is_narrowed_before_anything_is_written() {
	let state = opaque_blend();
	// A static sample mask of two bits out of four.
	let mut colour = Colour::new(1, 1, 4, false);
	let mut depth = DepthStencil::new(1, 1, 4, DepthFormat::Depth32F);
	depth.clear(1.0, 0);
	let written = pass::write_fragment(&mut colour, Some(&mut depth), &Fragment { sample_mask: 0b0011, ..fragment(Vec4::new(1.0, 0.0, 0.0, 1.0), 0.5, &state) }).unwrap();
	assert_eq!(written, 2);
	assert!(matches!(depth.depth_at(0, 0, 3), render3d::depth::Stored::Float(value) if near(value, 1.0)), "the masked samples kept their cleared depth");

	// Alpha-to-coverage: half an alpha narrows four samples to two.
	let mut colour = Colour::new(1, 1, 4, false);
	let written = pass::write_fragment(&mut colour, None, &Fragment { alpha_to_coverage: true, ..fragment(Vec4::new(1.0, 0.0, 0.0, 0.5), 0.5, &state) }).unwrap();
	assert_eq!(written, 2, "half an alpha covers half the samples");
	let written = pass::write_fragment(&mut colour, None, &Fragment { alpha_to_coverage: true, ..fragment(Vec4::new(1.0, 0.0, 0.0, 0.0), 0.5, &state) }).unwrap();
	assert_eq!(written, 0, "and a zero alpha covers none");
}

#[test]
// A LAYER OF AN ARRAY IS ADDRESSED BY AN INDEX AND IS NEVER FILTERED BETWEEN. Two layers are two
// unrelated images - an atlas page, a shadow cascade, a sprite frame - and blending between them
// produces a picture of neither. That is the whole difference between an ARRAY and a 3D texture,
// whose third axis IS filtered.
fn an_array_layer_is_selected_and_never_blended_with_its_neighbour() {
	let mut level = Level::new(2, 2, 3);
	for layer in 0..3_u32 {
		for y in 0..2 {
			for x in 0..2 {
				level.set(x, y, layer, red(layer as f32));
			}
		}
	}
	let array = Texture { id: 5, kind: Kind::Array, levels: vec![level.clone()], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: true };
	let sampler = Sampler { wrap_u: Wrap::ClampToEdge, wrap_v: Wrap::ClampToEdge, ..Sampler::LINEAR };
	for layer in 0..3_u32 {
		let texel = texture::sample_array(&array, &sampler, [0.5, 0.5], layer, 0.0).unwrap();
		assert!(near(texel[0], layer as f32), "layer {layer} answered {}", texel[0]);
	}
	// A layer the texture does not have is REFUSED rather than clamped to the last one, which would
	// draw the wrong page of an atlas and look like an authoring mistake.
	assert!(texture::sample_array(&array, &sampler, [0.5, 0.5], 3, 0.0).is_err());

	// The same storage as a 3D texture DOES filter its third axis, which is the distinction.
	let volume = Texture { id: 6, kind: Kind::Dim3, levels: vec![level], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: true };
	let between = texture::sample(&volume, &sampler, [0.5, 0.5, 0.5], 0.0, true)[0];
	assert!(near(between, 1.0), "halfway through three slices is the middle one: {between}");
	let quarter = texture::sample(&volume, &sampler, [0.5, 0.5, 1.0 / 3.0], 0.0, true)[0];
	assert!(quarter > 0.0 && quarter < 1.0, "and a point between two slices blends them: {quarter}");
}

// ---------------------------------------------------------------------------------------------
// The whole pipeline: prepare, execute, and what lands in the attachments.
// ---------------------------------------------------------------------------------------------

use crate::frame::{self, Attachments, Draw, Pipeline, Source};
use render3d::Render3DLimits;
use render3d::command::{Cull as CullMode, PipelineState};

/// Clip-space positions and colours, straight from tables.
struct Mesh {
	positions: Vec<[f32; 4]>,
	colours: Vec<[f32; 4]>,
	/// Added to every position, per instance, so an instanced draw is visibly several triangles.
	instance_offset: [f32; 4],
}

impl Source for Mesh {
	fn attribute(&self, location: u32, vertex: u32, instance: u32) -> Option<Val> {
		match location {
			0 => {
				let base = self.positions.get(vertex as usize)?;
				let shift = instance as f32;
				Some(Val::vector_f32(&[
					base[0] + self.instance_offset[0] * shift,
					base[1] + self.instance_offset[1] * shift,
					base[2] + self.instance_offset[2] * shift,
					base[3] + self.instance_offset[3] * shift,
				]))
			}
			1 => self.colours.get(vertex as usize).map(|colour| Val::vector_f32(colour)),
			_ => None,
		}
	}

	fn uniform(&self, _block: u32, _member: u32) -> Option<Val> {
		None
	}

	fn sample(&self, _texture: u32, _sampler: u32, _coordinate: &Val) -> Option<Val> {
		None
	}

	fn indices(&self) -> Indices<'_> {
		Indices::None
	}
}

/// A vertex stage that passes its position through and forwards a smooth colour.
fn passthrough_vertex() -> Module {
	let mut builder = Builder::new(Stage::Vertex, "passthrough");
	builder.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	let position = builder.load(Type::vec(4), Binding::Attribute { location: 0 });
	let colour = builder.load(Type::vec(4), Binding::Attribute { location: 1 });
	builder.store(Output::Position, position);
	builder.store(Output::Varying(0), colour);
	builder.finish()
}

/// A fragment stage that writes the interpolated colour.
fn colour_fragment() -> Module {
	let mut builder = Builder::new(Stage::Fragment, "colour");
	builder.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	let colour = builder.load(Type::vec(4), Binding::Varying { location: 0 });
	builder.store(Output::Colour(0), colour);
	builder.finish()
}

fn pipeline(topology: Topology) -> Pipeline {
	Pipeline { state: PipelineState { topology, cull: CullMode::None, depth_test: Some(CompareOp::Less), depth_write: true, samples: 1, per_sample_shading: false }, vertex: passthrough_vertex(), fragment: colour_fragment(), blend: vec![opaque_blend(), opaque_blend()], stencil: None, depth_compare: CompareOp::Less, depth_write: true, bias: (0.0, 0.0, 0.0), alpha_to_coverage: false, sample_mask: u32::MAX }
}

fn white() -> [f32; 4] {
	[1.0, 1.0, 1.0, 1.0]
}

#[test]
// THE WHOLE PIPELINE, END TO END: a clip-space triangle through the vertex stage, the clipper, the
// rasteriser and the fragment stage, landing in an attachment where a reader can check which pixels
// it covered by hand.
fn one_triangle_covers_exactly_the_pixels_its_edges_enclose() {
	// NDC (-1,-1), (1,-1), (-1,1): counter-clockwise, so front-facing. In a 16 x 16 viewport that is
	// window (0,16), (16,16), (0,0), whose interior is `y > x`.
	let mesh = Mesh { positions: vec![[-1.0, -1.0, 0.5, 1.0], [1.0, -1.0, 0.5, 1.0], [-1.0, 1.0, 0.5, 1.0]], colours: vec![white(), white(), white()], instance_offset: [0.0; 4] };
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![Draw { pipeline: 0, topology: Topology::TriangleList, count: 3, instances: 1, first_instance: 0, base_vertex: 0, restart: false }], 16, 16).unwrap();

	let mut colour = Colour::new(16, 16, 1, false);
	let mut depth = DepthStencil::new(16, 16, 1, DepthFormat::Depth32F);
	depth.clear(1.0, 0);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: Some(&mut depth), viewport: viewport(16.0, 16.0), scissor: None };
	let stats = frame::execute(&mut prepared, &mut attachments, &mesh).unwrap();

	assert_eq!(stats.primitives, 1);
	assert!(stats.fragments > 100, "about half the target: {}", stats.fragments);
	// Inside the triangle, where `y > x`.
	assert!(near(colour.at(2, 5, 0).x, 1.0), "(2, 5) is inside");
	assert!(near(colour.at(0, 15, 0).x, 1.0), "and so is the far corner");
	// Outside it, where `y < x`.
	assert!(near(colour.at(5, 2, 0).x, 0.0), "(5, 2) is on the other side of the diagonal");
	assert!(near(colour.at(15, 0, 0).x, 0.0));
	// The depth buffer holds the triangle's depth where it drew and its clear value where it did not.
	assert!(matches!(depth.depth_at(2, 5, 0), render3d::depth::Stored::Float(value) if near(value, 0.5)));
	assert!(matches!(depth.depth_at(5, 2, 0), render3d::depth::Stored::Float(value) if near(value, 1.0)));
}

#[test]
// A NEARER TRIANGLE WINS WHATEVER ORDER THE TWO ARE DRAWN IN, which is the whole point of a depth
// buffer and the thing a renderer that wrote depth after the colour would get wrong half the time.
fn the_nearer_of_two_overlapping_triangles_wins_in_either_order() {
	let far = [[-1.0_f32, -1.0, 0.8, 1.0], [1.0, -1.0, 0.8, 1.0], [-1.0, 1.0, 0.8, 1.0]];
	let near_one = [[-1.0_f32, -1.0, 0.2, 1.0], [1.0, -1.0, 0.2, 1.0], [-1.0, 1.0, 0.2, 1.0]];
	for (first, second, expected) in [(far, near_one, [0.0_f32, 1.0]), (near_one, far, [0.0, 1.0])] {
		let mesh = Mesh { positions: first.iter().chain(second.iter()).copied().collect(), colours: vec![[1.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]], instance_offset: [0.0; 4] };
		let _ = expected;
		let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![Draw { pipeline: 0, topology: Topology::TriangleList, count: 6, instances: 1, first_instance: 0, base_vertex: 0, restart: false }], 16, 16).unwrap();
		let mut colour = Colour::new(16, 16, 1, false);
		let mut depth = DepthStencil::new(16, 16, 1, DepthFormat::Depth32F);
		depth.clear(1.0, 0);
		let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: Some(&mut depth), viewport: viewport(16.0, 16.0), scissor: None };
		frame::execute(&mut prepared, &mut attachments, &mesh).unwrap();
		// Whichever order they were submitted in, the surviving depth is the nearer one's.
		assert!(matches!(depth.depth_at(2, 5, 0), render3d::depth::Stored::Float(value) if near(value, 0.2)), "the nearer triangle's depth survived");
	}
}

#[test]
// A SCISSOR REMOVES FRAGMENTS OUTSIDE A RECTANGLE AND CHANGES NOTHING ELSE. What makes it worth
// having is the COST: a fragment outside it is never shaded, so it cannot discard and cannot be
// counted - which is why this test asserts the fragment TOTAL as well as the picture. A scissor
// implemented at the write would draw the same frame in the same time, and the thing it exists for
// would be missing.
fn a_scissor_removes_the_fragments_outside_it_and_shades_none_of_them() {
	let quad = Mesh { positions: vec![[-1.0, -1.0, 0.5, 1.0], [1.0, -1.0, 0.5, 1.0], [1.0, 1.0, 0.5, 1.0], [-1.0, -1.0, 0.5, 1.0], [1.0, 1.0, 0.5, 1.0], [-1.0, 1.0, 0.5, 1.0]], colours: vec![white(); 6], instance_offset: [0.0; 4] };
	let draw = Draw { pipeline: 0, topology: Topology::TriangleList, count: 6, instances: 1, first_instance: 0, base_vertex: 0, restart: false };
	let covered = |colour: &Colour| (0..32).flat_map(|y| (0..32).map(move |x| (x, y))).filter(|(x, y)| colour.at(*x, *y, 0).x > 0.5).count();

	// The whole target first, so the scissored run has something to be a subset of.
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![draw], 32, 32).unwrap();
	let mut colour = Colour::new(32, 32, 1, false);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(32.0, 32.0), scissor: None };
	let whole = frame::execute(&mut prepared, &mut attachments, &quad).unwrap();
	assert_eq!(covered(&colour), 32 * 32, "the quad covers the whole target");

	// And the same draw under a scissor over the top-left quarter.
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![draw], 32, 32).unwrap();
	let mut colour = Colour::new(32, 32, 1, false);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(32.0, 32.0), scissor: Some(Scissor { x: 0, y: 0, width: 16, height: 16 }) };
	let clipped = frame::execute(&mut prepared, &mut attachments, &quad).unwrap();
	assert_eq!(covered(&colour), 16 * 16, "a scissor over a quarter of the target leaves a quarter drawn");
	assert!(colour.at(1, 1, 0).x > 0.5, "inside the rectangle the quad is drawn");
	assert!(colour.at(30, 30, 0).x < 0.5, "and outside it nothing is");
	assert_eq!(clipped.fragments, whole.fragments / 4, "and the fragments outside it were never shaded: {} against {}", clipped.fragments, whole.fragments);

	// A RECTANGLE REACHING PAST THE TARGET IS CLAMPED AND NOT AN OVERRUN. The recording boundary is
	// where a scissor outside the viewport is refused; by the time a plan runs, the only thing left to
	// do with one that reaches past the last pixel is to stop at it.
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![draw], 32, 32).unwrap();
	let mut colour = Colour::new(32, 32, 1, false);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(32.0, 32.0), scissor: Some(Scissor { x: -8, y: -8, width: 1000, height: 1000 }) };
	frame::execute(&mut prepared, &mut attachments, &quad).unwrap();
	assert_eq!(covered(&colour), 32 * 32, "a scissor larger than the target is the whole target");

	// AND AN EMPTY RECTANGLE DRAWS NOTHING, rather than being read as "no scissor".
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![draw], 32, 32).unwrap();
	let mut colour = Colour::new(32, 32, 1, false);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(32.0, 32.0), scissor: Some(Scissor { x: 4, y: 4, width: 0, height: 8 }) };
	let nothing = frame::execute(&mut prepared, &mut attachments, &quad).unwrap();
	assert_eq!(covered(&colour), 0, "a scissor with no width draws nothing");
	assert_eq!(nothing.fragments, 0, "and shades nothing");
}

#[test]
// INSTANCING IS ONE DRAW AND SEVERAL PRIMITIVES, with the instance index reaching the vertex stage
// so each instance can be somewhere else. A renderer that ran the vertex stage once for the whole
// draw would put every instance in the same place.
fn an_instanced_draw_runs_its_vertex_stage_once_per_instance() {
	// A small triangle in the lower-left, shifted right by a quarter of the volume per instance.
	let mesh = Mesh { positions: vec![[-0.9, -0.9, 0.5, 1.0], [-0.7, -0.9, 0.5, 1.0], [-0.9, -0.7, 0.5, 1.0]], colours: vec![white(), white(), white()], instance_offset: [0.5, 0.0, 0.0, 0.0] };
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![Draw { pipeline: 0, topology: Topology::TriangleList, count: 3, instances: 3, first_instance: 0, base_vertex: 0, restart: false }], 64, 64).unwrap();
	let mut colour = Colour::new(64, 64, 1, false);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(64.0, 64.0), scissor: None };
	let stats = frame::execute(&mut prepared, &mut attachments, &mesh).unwrap();
	assert_eq!(stats.primitives, 3, "one primitive per instance");

	// Three separate patches, each further right than the last.
	let column_lit = |x: u32| (0..64).any(|y| colour.at(x, y, 0).x > 0.5);
	let lit: Vec<u32> = (0..64).filter(|x| column_lit(*x)).collect();
	assert!(lit.len() >= 12, "three patches of about six columns each: {}", lit.len());
	assert!(lit.iter().any(|x| *x < 10) && lit.iter().any(|x| *x > 24), "and they are spread across the target: {lit:?}");

	// A draw of zero instances is refused at PREPARE, not silently skipped at execution.
	let bad = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![Draw { pipeline: 0, topology: Topology::TriangleList, count: 3, instances: 0, first_instance: 0, base_vertex: 0, restart: false }], 16, 16);
	assert!(bad.is_err());
}

#[test]
// MULTIPLE COLOUR ATTACHMENTS, EACH WITH ITS OWN BLEND STATE: a pass that writes colour to one and
// object identities to another must blend the first and not the second, and an attachment the shader
// did not write is LEFT ALONE rather than cleared - a pass that erased what it did not touch would
// make a two-attachment frame impossible to build up.
fn a_pass_writes_several_attachments_and_leaves_the_ones_its_shader_did_not() {
	let mut vertex = Builder::new(Stage::Vertex, "identity");
	vertex.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
	let colour = vertex.load(Type::vec(4), Binding::Attribute { location: 1 });
	vertex.store(Output::Position, position);
	vertex.store(Output::Varying(0), colour);

	let mut fragment = Builder::new(Stage::Fragment, "identity");
	fragment.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	let value = fragment.load(Type::vec(4), Binding::Varying { location: 0 });
	let identity = fragment.constant(Constant::U32(77));
	fragment.store(Output::Colour(0), value);
	fragment.store(Output::Integer(1), identity);

	let mut state = pipeline(Topology::TriangleList);
	state.vertex = vertex.finish();
	state.fragment = fragment.finish();

	let mesh = Mesh { positions: vec![[-1.0, -1.0, 0.5, 1.0], [1.0, -1.0, 0.5, 1.0], [-1.0, 1.0, 0.5, 1.0]], colours: vec![white(), white(), white()], instance_offset: [0.0; 4] };
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![state], vec![Draw { pipeline: 0, topology: Topology::TriangleList, count: 3, instances: 1, first_instance: 0, base_vertex: 0, restart: false }], 16, 16).unwrap();

	let mut targets = vec![Colour::new(16, 16, 1, false), Colour::new(16, 16, 1, true)];
	targets[1].fill(Vec4::new(9.0, 0.0, 0.0, 1.0));
	let mut attachments = Attachments { colour: &mut targets, depth_stencil: None, viewport: viewport(16.0, 16.0), scissor: None };
	frame::execute(&mut prepared, &mut attachments, &mesh).unwrap();
	assert!(near(targets[0].at(2, 5, 0).x, 1.0), "the colour attachment");
	assert!(near(targets[1].at(2, 5, 0).x, 77.0), "and the identity one");
	assert!(near(targets[1].at(15, 0, 0).x, 9.0), "outside the triangle the identity attachment is untouched");
}

#[test]
// EVERYTHING THAT CAN BE REFUSED IS REFUSED AT PREPARE, so `execute` walks a plan that has already
// been checked - which is what makes "allocates nothing in steady state" a property of the boundary
// rather than of the code happening not to allocate.
fn prepare_refuses_what_execute_must_never_meet() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	// A fragment stage reading a varying the vertex stage does not write: it would read whatever the
	// interpolator was left holding, which is a surface that is right until the draw before it
	// changes.
	let mut fragment = Builder::new(Stage::Fragment, "mismatched");
	fragment.varying(3, Type::vec(4), render_shader::Interpolation::Smooth);
	let value = fragment.load(Type::vec(4), Binding::Varying { location: 3 });
	fragment.store(Output::Colour(0), value);
	let mut mismatched = pipeline(Topology::TriangleList);
	mismatched.fragment = fragment.finish();
	assert!(matches!(frame::prepare(limits, vec![mismatched], vec![], 16, 16), Err(render3d::Error::InvalidShader { .. })));

	// The two stages the wrong way round.
	let mut swapped = pipeline(Topology::TriangleList);
	core::mem::swap(&mut swapped.vertex, &mut swapped.fragment);
	assert!(matches!(frame::prepare(limits, vec![swapped], vec![], 16, 16), Err(render3d::Error::InvalidShader { .. })));

	// A shading rate that disagrees with the fragment stage's own inputs.
	let mut per_sample = pipeline(Topology::TriangleList);
	per_sample.state.per_sample_shading = true;
	per_sample.state.samples = 1;
	assert!(matches!(frame::prepare(limits, vec![per_sample], vec![], 16, 16), Err(render3d::Error::IncompatiblePipeline { .. })));

	// A draw naming a pipeline the plan does not hold, and a target past the raster grid.
	let draw = Draw { pipeline: 4, topology: Topology::TriangleList, count: 3, instances: 1, first_instance: 0, base_vertex: 0, restart: false };
	assert!(frame::prepare(limits, vec![pipeline(Topology::TriangleList)], vec![draw], 16, 16).is_err());
	assert!(frame::prepare(limits, vec![pipeline(Topology::TriangleList)], vec![], crate::MAX_RASTER_EXTENT + 1, 16).is_err());
	assert!(frame::prepare(limits, vec![pipeline(Topology::TriangleList)], vec![], 0, 16).is_err());
}

#[test]
// A STEADY-STATE FRAME ALLOCATES NOTHING. The first frame sizes the scratch and every frame after it
// reuses the same memory, which is what the prepare/execute boundary exists to make true rather than
// to hope for.
fn a_second_frame_reuses_every_buffer_the_first_one_sized() {
	let mesh = Mesh { positions: vec![[-1.0, -1.0, 0.5, 1.0], [1.0, -1.0, 0.5, 1.0], [-1.0, 1.0, 0.5, 1.0]], colours: vec![white(), white(), white()], instance_offset: [0.0; 4] };
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![Draw { pipeline: 0, topology: Topology::TriangleList, count: 3, instances: 1, first_instance: 0, base_vertex: 0, restart: false }], 32, 32).unwrap();
	let mut colour = Colour::new(32, 32, 1, false);

	let once = |prepared: &mut crate::frame::Prepared, colour: &mut Colour| {
		let mut attachments = Attachments { colour: core::slice::from_mut(colour), depth_stencil: None, viewport: viewport(32.0, 32.0), scissor: None };
		frame::execute(prepared, &mut attachments, &mesh).unwrap()
	};
	let first = once(&mut prepared, &mut colour);
	let after_first = prepared.scratch_capacity();
	let second = once(&mut prepared, &mut colour);
	let after_second = prepared.scratch_capacity();
	let third = once(&mut prepared, &mut colour);
	let after_third = prepared.scratch_capacity();
	assert_eq!(after_first, after_second, "the second frame grew the scratch");
	assert_eq!(after_second, after_third, "and so did the third");
	assert_eq!(first, second, "and every frame did the same work");
	assert_eq!(second, third);
}

#[test]
// A LINE AND A POINT GO THROUGH THE SAME PLAN AS A TRIANGLE, with their own coverage rules and with
// BACK-FACE CULLING NOT APPLIED TO EITHER - a cull mode cannot remove something that has no winding,
// and a backend that applied the triangle rule would make a wireframe overlay vanish at half the
// angles.
fn a_line_and_a_point_draw_through_the_same_plan_and_are_never_culled() {
	// THROUGH THE PIXEL CENTRES AND NOT ALONG A PIXEL BOUNDARY. A horizontal line exactly on the
	// boundary between two rows is TANGENT to every diamond and enters none of them, so it lights
	// nothing - which is correct under the diamond-exit rule and is why a line meant to be seen is
	// offset by half a pixel. The second half of this fixture asserts that case rather than hiding
	// it.
	let through_centres = -0.031_25;
	let mesh = Mesh { positions: vec![[-0.9, through_centres, 0.5, 1.0], [0.9, through_centres, 0.5, 1.0]], colours: vec![white(), white()], instance_offset: [0.0; 4] };
	// A cull mode that would remove a front face, to show it does not reach a line.
	let mut state = pipeline(Topology::LineList);
	state.state.cull = CullMode::Front;
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![state], vec![Draw { pipeline: 0, topology: Topology::LineList, count: 2, instances: 1, first_instance: 0, base_vertex: 0, restart: false }], 32, 32).unwrap();
	let mut colour = Colour::new(32, 32, 1, false);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(32.0, 32.0), scissor: None };
	let stats = frame::execute(&mut prepared, &mut attachments, &mesh).unwrap();
	assert_eq!(stats.culled, 0, "a line has no winding for a cull mode to act on");
	let lit = (0..32).filter(|x| (0..32).any(|y| colour.at(*x, y, 0).x > 0.5)).count();
	assert!(lit > 20, "the line crossed most of the target: {lit}");

	// The same line ON the row boundary lights nothing, which is the diamond-exit rule being exact
	// rather than being approximately right.
	let on_the_boundary = Mesh { positions: vec![[-0.9, 0.0, 0.5, 1.0], [0.9, 0.0, 0.5, 1.0]], colours: vec![white(), white()], instance_offset: [0.0; 4] };
	let mut edge_case = Colour::new(32, 32, 1, false);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut edge_case), depth_stencil: None, viewport: viewport(32.0, 32.0), scissor: None };
	frame::execute(&mut prepared, &mut attachments, &on_the_boundary).unwrap();
	assert_eq!((0..32).filter(|x| (0..32).any(|y| edge_case.at(*x, y, 0).x > 0.5)).count(), 0);

	// A point, at the centre, with a size the vertex stage did not set - so it is one pixel.
	let mut point_state = pipeline(Topology::PointList);
	point_state.state.cull = CullMode::Back;
	let mesh = Mesh { positions: vec![[0.0, 0.0, 0.5, 1.0]], colours: vec![white()], instance_offset: [0.0; 4] };
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![point_state], vec![Draw { pipeline: 0, topology: Topology::PointList, count: 1, instances: 1, first_instance: 0, base_vertex: 0, restart: false }], 32, 32).unwrap();
	let mut colour = Colour::new(32, 32, 1, false);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(32.0, 32.0), scissor: None };
	frame::execute(&mut prepared, &mut attachments, &mesh).unwrap();
	let lit: Vec<(u32, u32)> = (0..32).flat_map(|y| (0..32).map(move |x| (x, y))).filter(|(x, y)| colour.at(*x, *y, 0).x > 0.5).collect();
	assert_eq!(lit.len(), 1, "one pixel: {lit:?}");
	// A ONE-PIXEL POINT CENTRED EXACTLY ON A PIXEL CORNER LANDS IN THE PIXEL BELOW AND LEFT OF IT.
	// Its square is `[15.5, 16.5)` on each axis, so the sample at `(15.5, 15.5)` is inside and the
	// one at `(16.5, 16.5)` is not - which is the SAME half-open tie-break the top-left rule makes
	// for a triangle, and is what lets two adjacent points tile without overlapping.
	assert_eq!(lit[0], (15, 15));
}

#[test]
// ALPHA-TO-COVERAGE IS A PIPELINE STATE AND REACHES THE FRAGMENT PATH. It is what makes alpha-tested
// foliage antialias without sorting, and the shader that produced the alpha has no way to know
// whether the pass wants it.
fn alpha_to_coverage_narrows_a_multisampled_draw_and_leaves_an_identity_alone() {
	let mut vertex = Builder::new(Stage::Vertex, "half");
	vertex.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
	let colour = vertex.load(Type::vec(4), Binding::Attribute { location: 1 });
	vertex.store(Output::Position, position);
	vertex.store(Output::Varying(0), colour);
	let mut state = pipeline(Topology::TriangleList);
	state.vertex = vertex.finish();
	state.state.samples = 4;
	state.alpha_to_coverage = true;
	state.sample_mask = u32::MAX;

	// A half-transparent triangle covering the whole target.
	let mesh = Mesh { positions: vec![[-1.0, -1.0, 0.5, 1.0], [3.0, -1.0, 0.5, 1.0], [-1.0, 3.0, 0.5, 1.0]], colours: vec![[1.0, 1.0, 1.0, 0.5]; 3], instance_offset: [0.0; 4] };
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![state], vec![Draw { pipeline: 0, topology: Topology::TriangleList, count: 3, instances: 1, first_instance: 0, base_vertex: 0, restart: false }], 8, 8).unwrap();
	let mut colour = Colour::new(8, 8, 4, false);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(8.0, 8.0), scissor: None };
	let stats = frame::execute(&mut prepared, &mut attachments, &mesh).unwrap();
	// Four samples per pixel over 64 pixels, narrowed to half by the alpha.
	assert_eq!(stats.samples_written, 128, "half of 256: {}", stats.samples_written);
	// And the resolve therefore gives half coverage rather than full.
	let resolved = colour.resolve().unwrap();
	assert!(near(resolved[0].x, 0.5), "{:?}", resolved[0]);
}

#[test]
// HIERARCHICAL DEPTH REJECTS A WHOLE TRIANGLE BEFORE A SAMPLE IS TESTED, and it is CONSERVATIVE:
// it rejects only what the per-sample test would reject anyway, so turning it off changes the
// frame's speed and not its picture.
fn hierarchical_depth_rejects_hidden_geometry_without_changing_the_picture() {
	let covering = [[-1.0_f32, -1.0, 0.2, 1.0], [3.0, -1.0, 0.2, 1.0], [-1.0, 3.0, 0.2, 1.0]];
	let hidden = [[-1.0_f32, -1.0, 0.9, 1.0], [3.0, -1.0, 0.9, 1.0], [-1.0, 3.0, 0.9, 1.0]];
	let mesh = Mesh { positions: covering.iter().chain(hidden.iter()).copied().collect(), colours: vec![[1.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]], instance_offset: [0.0; 4] };
	// TWO DRAWS, so the near one has finished filling the tiles before the far one is binned - which
	// is the case the bound exists for.
	let draw = Draw { pipeline: 0, topology: Topology::TriangleList, count: 3, instances: 1, first_instance: 0, base_vertex: 0, restart: false };
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![draw, Draw { base_vertex: 3, ..draw }], 32, 32).unwrap();
	let mut colour = Colour::new(32, 32, 1, false);
	let mut depth = DepthStencil::new(32, 32, 1, DepthFormat::Depth32F);
	depth.clear(1.0, 0);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: Some(&mut depth), viewport: viewport(32.0, 32.0), scissor: None };
	let stats = frame::execute(&mut prepared, &mut attachments, &mesh).unwrap();

	// The picture is the near triangle's, whatever the bound did.
	assert!(near(colour.at(16, 16, 0).x, 1.0) && near(colour.at(16, 16, 0).y, 0.0), "the near triangle won: {:?}", colour.at(16, 16, 0));
	// And the hidden triangle's fragments were never shaded: one target's worth, not two.
	assert!(stats.fragments <= 32 * 32 + 8, "the hidden draw was rejected before shading: {}", stats.fragments);
}

#[test]
// A COLOUR ATTACHMENT BECOMES A TEXTURE BY RESOLVING FIRST. Sampling one of several samples would
// make a render-to-texture pass aliased in a way nothing else in the frame is; averaging at fetch
// time would make every tap cost the sample count.
fn a_rendered_attachment_becomes_a_texture_that_is_already_linear() {
	let mut attachment = Colour::new(2, 2, 4, false);
	for sample in 0..4 {
		attachment.set(0, 0, sample, Vec4::new(sample as f32 * 0.25, 0.0, 0.0, 1.0));
	}
	let rendered = texture::from_attachment(&attachment, 9).unwrap();
	assert_eq!(rendered.transfer, Transfer::Linear, "a second decode would darken every step of a chain");
	assert!(rendered.premultiplied);
	assert!(near(rendered.levels[0].at(0, 0, 0)[0], 0.375), "the resolved average: {}", rendered.levels[0].at(0, 0, 0)[0]);

	// AN IDENTITY ATTACHMENT IS NEVER FILTERED AND NEVER CONVERTED, so a pass that samples one gets
	// the number it wrote.
	let mut identity = Colour::new(1, 1, 4, true);
	for sample in 0..4 {
		identity.set(0, 0, sample, Vec4::new(40.0 + sample as f32, 0.0, 0.0, 1.0));
	}
	let read_back = texture::from_attachment(&identity, 10).unwrap();
	assert_eq!(read_back.semantics, Semantics::Identity);
	assert!(near(read_back.levels[0].at(0, 0, 0)[0], 40.0), "sample zero, not the average");
}

#[test]
// A STEADY-STATE FRAME ALLOCATES NOTHING, MEASURED RATHER THAN CLAIMED. The first frame sizes every
// buffer; from then on the whole pipeline - assembly, the vertex stage, clipping, setup, binning,
// rasterisation and the fragment stage - runs without asking the allocator for anything.
//
// THE COUNTER COUNTS `realloc` TOO. A vector that grows is exactly what a steady-state frame must
// not do, and counting only `alloc` would miss every one of them.
fn a_warmed_frame_asks_the_allocator_for_nothing() {
	let mesh = Mesh {
		positions: vec![
			[-0.9, -0.9, 0.5, 1.0],
			[0.9, -0.9, 0.5, 1.0],
			[-0.9, 0.9, 0.5, 1.0],
			// A second triangle that straddles the right plane, so the CLIPPER runs too.
			[0.0, -0.5, 0.5, 1.0],
			[2.0, 0.0, 0.5, 1.0],
			[0.0, 0.5, 0.5, 1.0],
		],
		colours: vec![white(); 6],
		instance_offset: [0.0; 4],
	};
	let draw = Draw { pipeline: 0, topology: Topology::TriangleList, count: 6, instances: 2, first_instance: 0, base_vertex: 0, restart: false };
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![draw], 64, 64).unwrap();
	let mut colour = Colour::new(64, 64, 1, false);
	let mut depth = DepthStencil::new(64, 64, 1, DepthFormat::Depth32F);

	let once = |prepared: &mut crate::frame::Prepared, colour: &mut Colour, depth: &mut DepthStencil| {
		// A REAL FRAME CLEARS ITS DEPTH BUFFER, and so does this one - otherwise the second frame
		// tests against the first frame's depths and draws nothing, which would make the measurement
		// be about an empty frame.
		depth.clear(1.0, 0);
		let mut attachments = Attachments { colour: core::slice::from_mut(colour), depth_stencil: Some(depth), viewport: viewport(64.0, 64.0), scissor: None };
		frame::execute(prepared, &mut attachments, &mesh).unwrap()
	};

	// Two warm-up frames: the first sizes the scratch and the second proves it settled.
	let first = once(&mut prepared, &mut colour, &mut depth);
	once(&mut prepared, &mut colour, &mut depth);
	assert!(first.clipped > 0, "the clipper ran, so the measurement covers it: {first:?}");
	assert!(first.fragments > 500, "and so did the fragment stage: {first:?}");

	let before = crate::counted::count();
	let third = once(&mut prepared, &mut colour, &mut depth);
	let after = crate::counted::count();
	assert_eq!(after - before, 0, "a warmed frame allocated {} times", after - before);
	assert_eq!(third, first, "and did exactly the same work");
}

#[test]
// THE VARYING BOUND IS REFUSED AT PREPARE, where a caller can do something about it, rather than at
// the first draw. The bound is what keeps the clipper and the interpolator allocation-free, and a
// shader that exceeded it silently would be shaded from values the vertex stage did not write.
fn a_shader_with_more_varyings_than_the_backend_holds_is_refused_at_prepare() {
	let mut vertex = Builder::new(Stage::Vertex, "wide");
	let mut fragment = Builder::new(Stage::Fragment, "wide");
	// Nine `vec4`s of smooth varyings: thirty-six components, past the thirty-two the bound admits.
	for location in 0..9 {
		vertex.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
		fragment.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
	}
	let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
	vertex.store(Output::Position, position);
	for location in 0..9 {
		let value = vertex.load(Type::vec(4), Binding::Attribute { location: 1 });
		vertex.store(Output::Varying(location), value);
	}
	let colour = fragment.load(Type::vec(4), Binding::Varying { location: 0 });
	fragment.store(Output::Colour(0), colour);

	let mut wide = pipeline(Topology::TriangleList);
	wide.vertex = vertex.finish();
	wide.fragment = fragment.finish();
	let outcome = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![wide], vec![], 16, 16);
	assert!(matches!(outcome, Err(render3d::Error::LimitExceeded { limit: "varying components", .. })), "{:?}", outcome.err());

	// EIGHT `vec4`s FIT, which is the floor every graphics API of the last twenty years guarantees.
	let mut vertex = Builder::new(Stage::Vertex, "eight");
	let mut fragment = Builder::new(Stage::Fragment, "eight");
	for location in 0..8 {
		vertex.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
		fragment.varying(location, Type::vec(4), render_shader::Interpolation::Smooth);
	}
	let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
	vertex.store(Output::Position, position);
	for location in 0..8 {
		let value = vertex.load(Type::vec(4), Binding::Attribute { location: 1 });
		vertex.store(Output::Varying(location), value);
	}
	let colour = fragment.load(Type::vec(4), Binding::Varying { location: 0 });
	fragment.store(Output::Colour(0), colour);
	let mut narrow = pipeline(Topology::TriangleList);
	narrow.vertex = vertex.finish();
	narrow.fragment = fragment.finish();
	assert!(frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![narrow], vec![], 16, 16).is_ok());
}

#[test]
// THE TEXEL CACHE ANSWERS EXACTLY WHAT THE UNCACHED PATH DOES, and neighbouring pixels of a
// magnified surface hit it - which is the whole reason it exists: the four taps of pixel `n` and of
// pixel `n+1` overlap in two of them whenever a surface is magnified at all.
fn a_texel_cache_serves_the_same_values_and_is_hit_by_neighbouring_taps() {
	let mut level = Level::new(8, 8, 1);
	for y in 0..8 {
		for x in 0..8 {
			level.set(x, y, 0, red((x + y) as f32 / 14.0));
		}
	}
	let texture = Texture { id: 42, kind: Kind::Dim2, levels: vec![level], transfer: Transfer::Srgb, semantics: Semantics::Color, premultiplied: true };
	let sampler = Sampler { wrap_u: Wrap::ClampToEdge, wrap_v: Wrap::ClampToEdge, ..Sampler::LINEAR };
	let mut cache = texture::Cache::new();

	// A magnified walk across the surface: sixty-four samples over eight texels.
	for step in 0..64 {
		let u = (step as f32 + 0.5) / 64.0;
		let plain = texture::sample(&texture, &sampler, [u, 0.5, 0.0], 0.0, true);
		let cached = texture::sample_cached(&texture, &sampler, [u, 0.5, 0.0], 0.0, true, &mut Some(&mut cache));
		assert_eq!(plain, cached, "the cache changed an answer at step {step}");
	}
	assert!(cache.hits() > cache.misses(), "a magnified walk mostly hits: {} hits against {} misses", cache.hits(), cache.misses());
	// Only the texels the walk touched were ever fetched: sixteen taps' worth of distinct texels at
	// most, over eight texel columns and two rows.
	assert!(cache.misses() <= 32, "more misses than there are texels to miss: {}", cache.misses());

	// CLEARING IS NEEDED WHEN A TEXTURE'S CONTENTS CHANGE, because the key names an ADDRESS and not
	// a version - a render-to-texture pass that rewrote this texture would otherwise be sampled as
	// the previous frame.
	let before = cache.hits();
	cache.clear();
	texture::sample_cached(&texture, &sampler, [0.5, 0.5, 0.0], 0.0, true, &mut Some(&mut cache));
	assert_eq!(cache.hits(), before, "every tap after a clear is a miss");
}

#[test]
// THE LIBRARY REPORTS WHAT IT RESERVED AND THE PROCESS CHARGES IT. This crate is `no_std` and cannot
// reach a Domain; a renderer that tried would be one that could only run inside one process model.
// What it can do is say exactly what it owns, so the number charged is a measurement.
//
// AND IT COUNTS ONLY WHAT THE PLAN OWNS. The attachments are the caller's, so charging for them here
// would charge for them twice.
fn a_prepared_plan_reports_the_bytes_it_reserved() {
	let mesh = Mesh { positions: vec![[-0.9, -0.9, 0.5, 1.0], [0.9, -0.9, 0.5, 1.0], [-0.9, 0.9, 0.5, 1.0]], colours: vec![white(); 3], instance_offset: [0.0; 4] };
	let draw = Draw { pipeline: 0, topology: Topology::TriangleList, count: 3, instances: 1, first_instance: 0, base_vertex: 0, restart: false };
	let mut small = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![draw], 64, 64).unwrap();
	let mut large = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![draw], 1024, 1024).unwrap();
	// A bigger target reserves more, because the tile grid is the part that scales with it.
	assert!(large.reserved_bytes() > small.reserved_bytes(), "{} against {}", large.reserved_bytes(), small.reserved_bytes());
	assert!(small.reserved_bytes() > 0);

	// After a frame it has grown once - the scratch that frame sized - and then it settles.
	let mut colour = Colour::new(64, 64, 1, false);
	let mut once = |prepared: &mut crate::frame::Prepared| {
		let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(64.0, 64.0), scissor: None };
		frame::execute(prepared, &mut attachments, &mesh).unwrap();
	};
	once(&mut small);
	let after_first = small.reserved_bytes();
	once(&mut small);
	assert_eq!(small.reserved_bytes(), after_first, "a steady-state frame reserves nothing further");
	let _ = &mut large;
}

// ---------------------------------------------------------------------------------------------
// Clipping, plane by plane, and the analytic edge the profile names.
// ---------------------------------------------------------------------------------------------

#[test]
// EVERY PLANE, ONE AT A TIME. A clipper tested only against a triangle that leaves through one
// corner has been tested against one plane and credited with seven; each of these leaves through
// exactly one, so a plane whose inequality is written backwards fails its own case and nothing else.
fn a_triangle_is_cut_by_each_of_the_seven_planes_on_its_own() {
	// Each entry: a triangle that straddles ONE plane, and the predicate every surviving vertex
	// must satisfy - written from the plane's own inequality rather than from the implementation.
	let cases: [(&str, [Vec4; 3], fn(Vec4) -> bool); 6] = [
		("near, z >= 0", [Vec4::new(0.0, 0.0, -1.0, 1.0), Vec4::new(0.5, 0.0, 0.5, 1.0), Vec4::new(0.0, 0.5, 0.5, 1.0)], |p| p.z >= -1e-4),
		("far, z <= w", [Vec4::new(0.0, 0.0, 2.0, 1.0), Vec4::new(0.5, 0.0, 0.5, 1.0), Vec4::new(0.0, 0.5, 0.5, 1.0)], |p| p.z <= p.w + 1e-4),
		("left, x >= -w", [Vec4::new(-2.0, 0.0, 0.5, 1.0), Vec4::new(0.5, -0.5, 0.5, 1.0), Vec4::new(0.5, 0.5, 0.5, 1.0)], |p| p.x >= -p.w - 1e-4),
		("right, x <= w", [Vec4::new(2.0, 0.0, 0.5, 1.0), Vec4::new(-0.5, -0.5, 0.5, 1.0), Vec4::new(-0.5, 0.5, 0.5, 1.0)], |p| p.x <= p.w + 1e-4),
		("bottom, y >= -w", [Vec4::new(0.0, -2.0, 0.5, 1.0), Vec4::new(-0.5, 0.5, 0.5, 1.0), Vec4::new(0.5, 0.5, 0.5, 1.0)], |p| p.y >= -p.w - 1e-4),
		("top, y <= w", [Vec4::new(0.0, 2.0, 0.5, 1.0), Vec4::new(-0.5, -0.5, 0.5, 1.0), Vec4::new(0.5, -0.5, 0.5, 1.0)], |p| p.y <= p.w + 1e-4),
	];
	for (name, positions, inside) in cases {
		let triangle = [Vertex::new(positions[0]), Vertex::new(positions[1]), Vertex::new(positions[2])];
		let clipped = clip::clip_triangle(&triangle, Varyings::EMPTY).unwrap();
		assert!(!clipped.is_empty(), "{name}: the whole triangle was dropped");
		assert!(clipped.vertices().len() > 3, "{name}: nothing was cut, so the plane was not reached");
		for vertex in clipped.vertices() {
			assert!(inside(vertex.position), "{name}: a surviving vertex is outside its own plane: {:?}", vertex.position);
		}
	}

	// THE `w` PLANE IS CUT FIRST AND IS WHAT MAKES THE OTHERS SAFE: a vertex at or behind the eye
	// has no projection, and a clipper that cut the side planes first would compute a parameter from
	// a distance that is meaningless there.
	let straddling_the_eye = [Vertex::new(Vec4::new(0.0, 0.0, 0.5, -1.0)), Vertex::new(Vec4::new(0.2, 0.0, 0.5, 1.0)), Vertex::new(Vec4::new(0.0, 0.2, 0.5, 1.0))];
	let clipped = clip::clip_triangle(&straddling_the_eye, Varyings::EMPTY).unwrap();
	assert!(clipped.vertices().iter().all(|vertex| vertex.position.w > 0.0), "every surviving vertex is in front of the eye");
}

#[test]
// THE ANALYTIC EDGE THE PROFILE NAMES, with its numbers worked out by hand: `(x, w, a)` from
// `(-2, 1, 0)` to `(0, 2, 1)` cut at `x + w = 0` gives `t = 1/3`, a projected `x/w` of `-1`, a
// `smooth` value of `1/3` and a `noperspective` value of `1/2`.
//
// AND THE SURVIVING PRIMITIVE IS CHECKED, not only the new vertex: the interpolated field inside the
// clipped triangle must equal the unclipped triangle's field at the same point, because clipping is
// supposed to change the shape and not the surface.
fn the_analytic_unequal_w_edge_gives_the_parameters_the_profile_states() {
	let smooth = |value: f32| Varyings::from_slice(&[value]).unwrap();
	let triangle = [
		Vertex::new(Vec4::new(-2.0, 0.0, 0.5, 1.0)).with_smooth(smooth(0.0)).with_noperspective(smooth(0.0)),
		Vertex::new(Vec4::new(0.0, 0.0, 1.0, 2.0)).with_smooth(smooth(1.0)).with_noperspective(smooth(1.0)),
		Vertex::new(Vec4::new(0.0, 1.0, 0.5, 1.0)).with_smooth(smooth(0.5)).with_noperspective(smooth(0.5)),
	];
	let clipped = clip::clip_triangle(&triangle, Varyings::EMPTY).unwrap();
	// The vertex produced on `x + w = 0` along the edge from vertex 0 to vertex 1.
	let cut = clipped.vertices().iter().find(|vertex| (vertex.position.x + vertex.position.w).abs() < 1e-4 && vertex.position.y.abs() < 1e-4).expect("the edge was cut on the left plane");
	assert!(near(cut.position.x, -4.0 / 3.0), "x at t = 1/3: {}", cut.position.x);
	assert!(near(cut.position.w, 4.0 / 3.0), "w at t = 1/3: {}", cut.position.w);
	assert!(near(cut.position.x / cut.position.w, -1.0), "the projected x is exactly -1");
	assert!(near(cut.smooth.as_slice()[0], 1.0 / 3.0), "smooth follows the HOMOGENEOUS parameter: {}", cut.smooth.as_slice()[0]);
	assert!(near(cut.noperspective.as_slice()[0], 0.5), "noperspective follows the PROJECTED one: {}", cut.noperspective.as_slice()[0]);

	// THE SURVIVING PRIMITIVE'S FIELD, against the unclipped triangle's at the same point.
	//
	// EVALUATED IN FLOATING POINT AND NOT THROUGH THE RASTER GRID, because the unclipped triangle
	// reaches well outside the viewport - which is why it needed clipping - and the grid refuses a
	// coordinate that far out by design. The arithmetic below is the same one the rasteriser does:
	// project, take the screen-space barycentric weights, then correct by `1/w`.
	let project = |position: Vec4| -> (f32, f32, f32) { (position.x / position.w, position.y / position.w, 1.0 / position.w) };
	let field_at = |corners: [(f32, f32, f32); 3], values: [f32; 3], at: (f32, f32)| -> f32 {
		let area = (corners[1].0 - corners[0].0) * (corners[2].1 - corners[0].1) - (corners[2].0 - corners[0].0) * (corners[1].1 - corners[0].1);
		let edge = |a: (f32, f32, f32), b: (f32, f32, f32)| (b.0 - a.0) * (at.1 - a.1) - (at.0 - a.0) * (b.1 - a.1);
		let weights = [edge(corners[1], corners[2]) / area, edge(corners[2], corners[0]) / area, edge(corners[0], corners[1]) / area];
		let inverse_w = [corners[0].2, corners[1].2, corners[2].2];
		interp::smooth(weights, inverse_w, values)
	};

	let whole: [(f32, f32, f32); 3] = [project(triangle[0].position), project(triangle[1].position), project(triangle[2].position)];
	let whole_values = [triangle[0].smooth.as_slice()[0], triangle[1].smooth.as_slice()[0], triangle[2].smooth.as_slice()[0]];

	// A point inside the clipped piece: the centroid of its first fan triangle, in NDC.
	let piece = clipped.triangle(0).unwrap();
	let corners: [(f32, f32, f32); 3] = [project(clipped.vertices()[piece[0]].position), project(clipped.vertices()[piece[1]].position), project(clipped.vertices()[piece[2]].position)];
	let at = ((corners[0].0 + corners[1].0 + corners[2].0) / 3.0, (corners[0].1 + corners[1].1 + corners[2].1) / 3.0);
	let piece_values = [clipped.vertices()[piece[0]].smooth.as_slice()[0], clipped.vertices()[piece[1]].smooth.as_slice()[0], clipped.vertices()[piece[2]].smooth.as_slice()[0]];
	let from_piece = field_at(corners, piece_values, at);
	let from_whole = field_at(whole, whole_values, at);
	assert!((from_piece - from_whole).abs() < 1e-3, "clipping changed the surface: {from_piece} against {from_whole}");
}

#[test]
// REPEATED-PLANE CLIPPING AND `flat` PRESERVATION ACROSS SEVERAL FAN TRIANGLES. A triangle cut by
// four planes becomes a polygon of several triangles, and every one of them must carry the ORIGINAL
// provoking vertex's value - including when that vertex is the one the clipper removed first.
fn a_flat_value_survives_repeated_clipping_onto_every_fan_triangle() {
	let identity = Varyings::from_slice(&[1234.0, 5678.0]).unwrap();
	// A triangle whose first vertex - the provoking one for a list - is far outside, and whose other
	// two straddle two more planes each.
	// A triangle that CUTS THE CORNERS of the volume rather than swallowing it: each of its three
	// vertices is outside, but the volume's corners are outside the triangle too, so the cut produces
	// a polygon of several triangles rather than the four-cornered cross-section a containing
	// triangle gives.
	let triangle = [Vertex::new(Vec4::new(0.0, -1.5, 0.5, 1.0)), Vertex::new(Vec4::new(1.5, 1.2, 0.5, 1.0)), Vertex::new(Vec4::new(-1.5, 1.2, 0.5, 1.0))];
	let clipped = clip::clip_triangle(&triangle, identity).unwrap();
	assert!(clipped.triangle_count() >= 3, "the cut produced several fan triangles: {}", clipped.triangle_count());
	assert!(clipped.vertices().len() <= clip::MAX_CLIPPED_VERTICES);
	// The provoking vertex is gone.
	assert!(clipped.vertices().iter().all(|vertex| vertex.position.x >= -vertex.position.w - 1e-4));
	// And its value reached every triangle, because it belongs to the PRIMITIVE and not to a vertex.
	assert_eq!(clipped.flat.as_slice(), &[1234.0, 5678.0]);
	for index in 0..clipped.triangle_count() {
		assert!(clipped.triangle(index).is_some(), "fan triangle {index}");
	}
	assert!(clipped.triangle(clipped.triangle_count()).is_none(), "and there is no triangle past the last");
}

// ---------------------------------------------------------------------------------------------
// Every depth comparison, every stencil operation, every blend state.
// ---------------------------------------------------------------------------------------------

#[test]
// EVERY COMPARE OPERATION, against its own definition rather than against the one beside it. A
// renderer that had `Greater` and `GreaterOrEqual` the same way round passes any fixture that tests
// one of them.
fn every_depth_comparison_decides_the_way_its_name_says() {
	let state = opaque_blend();
	for operation in render3d::ALL_COMPARE_OPS {
		for (incoming, stored, expected) in [(0.25_f32, 0.5_f32, true), (0.75, 0.5, false), (0.5, 0.5, false)] {
			let _ = expected;
			let mut colour = Colour::new(1, 1, 1, false);
			let mut depth = DepthStencil::new(1, 1, 1, DepthFormat::Depth32F);
			depth.clear(stored, 0);
			let written = pass::write_fragment(&mut colour, Some(&mut depth), &Fragment { coverage: 0b1, depth_compare: *operation, ..fragment(Vec4::new(1.0, 0.0, 0.0, 1.0), incoming, &state) }).unwrap();
			// The rule, restated from the operation's name rather than from the implementation.
			let ought = match operation {
				render3d::CompareOp::Never => false,
				render3d::CompareOp::Less => incoming < stored,
				render3d::CompareOp::Equal => incoming == stored,
				render3d::CompareOp::LessOrEqual => incoming <= stored,
				render3d::CompareOp::Greater => incoming > stored,
				render3d::CompareOp::NotEqual => incoming != stored,
				render3d::CompareOp::GreaterOrEqual => incoming >= stored,
				render3d::CompareOp::Always => true,
			};
			assert_eq!(written == 1, ought, "{operation:?} with incoming {incoming} against stored {stored}");
		}
	}
}

#[test]
// EVERY STENCIL OPERATION, each through the renderer's own fragment path rather than through the
// arithmetic alone - which is what shows the operation is SELECTED correctly as well as applied.
fn every_stencil_operation_stores_what_its_name_says() {
	let state = opaque_blend();
	for operation in render3d::ALL_STENCIL_OPS {
		let mut colour = Colour::new(1, 1, 1, false);
		let mut depth = DepthStencil::new(1, 1, 1, DepthFormat::Depth24Stencil8);
		depth.clear(1.0, 200);
		// A stencil that always passes and a depth that always passes, so `on_pass` is selected.
		let face = StencilFace { compare: render3d::CompareOp::Always, read_mask: 0xFF, write_mask: 0xFF, reference: 7, on_fail: StencilOp::Zero, on_depth_fail: StencilOp::Zero, on_pass: *operation };
		pass::write_fragment(&mut colour, Some(&mut depth), &Fragment { coverage: 0b1, stencil: Some((&face, true)), depth_compare: render3d::CompareOp::Always, ..fragment(Vec4::new(1.0, 0.0, 0.0, 1.0), 0.5, &state) }).unwrap();
		let ought = match operation {
			StencilOp::Keep => 200,
			StencilOp::Zero => 0,
			StencilOp::Replace => 7,
			StencilOp::IncrementClamp => 201,
			StencilOp::DecrementClamp => 199,
			StencilOp::Invert => !200_u8,
			StencilOp::IncrementWrap => 201,
			StencilOp::DecrementWrap => 199,
		};
		assert_eq!(depth.stencil_at(0, 0, 0), ought, "{operation:?}");
	}

	// THE CLAMPING OPERATIONS SATURATE AND THE WRAPPING ONES DO NOT, which is the pair a fixture at
	// a middling value cannot tell apart.
	for (start, operation, ought) in [(255_u8, StencilOp::IncrementClamp, 255_u8), (255, StencilOp::IncrementWrap, 0), (0, StencilOp::DecrementClamp, 0), (0, StencilOp::DecrementWrap, 255)] {
		let mut colour = Colour::new(1, 1, 1, false);
		let mut depth = DepthStencil::new(1, 1, 1, DepthFormat::Depth24Stencil8);
		depth.clear(1.0, start);
		let face = StencilFace { compare: render3d::CompareOp::Always, read_mask: 0xFF, write_mask: 0xFF, reference: 0, on_fail: StencilOp::Keep, on_depth_fail: StencilOp::Keep, on_pass: operation };
		pass::write_fragment(&mut colour, Some(&mut depth), &Fragment { coverage: 0b1, stencil: Some((&face, true)), depth_compare: render3d::CompareOp::Always, ..fragment(Vec4::new(1.0, 0.0, 0.0, 1.0), 0.5, &state) }).unwrap();
		assert_eq!(depth.stencil_at(0, 0, 0), ought, "{operation:?} from {start}");
	}
}

#[test]
// EVERY BLEND STATE THROUGH THE RENDERER, held against `render3d`'s own arithmetic. The point is not
// to re-test the equations - that crate does - but to show the renderer applies them UNCHANGED, with
// the right source, the right destination and the write mask after rather than before.
fn every_blend_factor_and_operation_reaches_the_attachment_unchanged() {
	let source = Vec4::new(0.5, 0.25, 0.125, 0.5);
	let destination = Vec4::new(0.125, 0.5, 0.25, 0.75);
	let mut checked = 0;
	for factor in render3d::ALL_BLEND_FACTORS {
		for operation in render3d::ALL_BLEND_OPS {
			let equation = render3d::BlendEquation { source: *factor, destination: render3d::BlendFactor::One, operation: *operation };
			let state = render3d::AttachmentBlend { enabled: true, colour: equation, alpha: equation, write_mask: render3d::ColorWriteMask::ALL };
			let mut colour = Colour::new(1, 1, 1, false);
			colour.set(0, 0, 0, destination);
			pass::write_fragment(&mut colour, None, &Fragment { coverage: 0b1, ..fragment(source, 0.5, &state) }).unwrap();
			let expected = render3d::blend(&state, source, destination, Vec4::ZERO);
			let got = colour.at(0, 0, 0);
			assert!(near(got.x, expected.x) && near(got.y, expected.y) && near(got.z, expected.z) && near(got.w, expected.w), "{factor:?} {operation:?}: {got:?} against {expected:?}");
			checked += 1;
		}
	}
	assert!(checked >= 50, "only {checked} states were exercised");
}

#[test]
// READBACK: a colour attachment resolves and reads back, a depth attachment reads back through the
// format's own conversion, and a NORMALISED depth format round-trips within its own precision - which
// is what tells a caller whether a value it read is the one it wrote.
fn colour_and_depth_read_back_through_their_own_conversions() {
	let mut colour = Colour::new(2, 1, 2, false);
	colour.set(0, 0, 0, Vec4::new(0.0, 0.0, 0.0, 1.0));
	colour.set(0, 0, 1, Vec4::new(1.0, 1.0, 1.0, 1.0));
	colour.set(1, 0, 0, Vec4::new(0.25, 0.25, 0.25, 1.0));
	colour.set(1, 0, 1, Vec4::new(0.25, 0.25, 0.25, 1.0));
	let resolved = colour.resolve().unwrap();
	assert!(near(resolved[0].x, 0.5) && near(resolved[1].x, 0.25));

	for format in [DepthFormat::Depth16, DepthFormat::Depth24, DepthFormat::Depth32F, DepthFormat::Depth24Stencil8, DepthFormat::Depth32FStencil8] {
		let mut buffer = DepthStencil::new(1, 1, 1, format);
		buffer.clear(0.375, 0);
		let read = render3d::depth::readback(format, buffer.depth_at(0, 0, 0));
		// A NORMALISED FORMAT ROUND-TRIPS WITHIN ITS OWN PRECISION and a float one exactly; the
		// tolerance is the format's least significant bit rather than a number picked to pass.
		let precision = match format {
			DepthFormat::Depth16 => 1.0 / 65_535.0,
			DepthFormat::Depth24 | DepthFormat::Depth24Stencil8 => 1.0 / 16_777_215.0,
			_ => 0.0,
		};
		assert!((read - 0.375).abs() <= precision, "{format:?} read back {read}");
	}
}

#[test]
// AN ATTACHMENT THE FRAME DID NOT WRITE IS UNTOUCHED, AND NOTHING LANDS OUTSIDE THE EXTENT. The
// canary is a second attachment beside the target: a write that ran off the end of the first would
// land in it, and a write past the declared extent would be refused by the attachment's own bounds
// rather than reaching either.
fn a_canary_attachment_beside_the_target_is_never_touched() {
	// A triangle far larger than the target, so every edge of it runs off the attachment.
	let mesh = Mesh {
		// Large enough to CONTAIN the volume: its hypotenuse is `x + y = 4`, and the volume's
		// furthest corner sums to two.
		positions: vec![[-4.0, -4.0, 0.5, 1.0], [8.0, -4.0, 0.5, 1.0], [-4.0, 8.0, 0.5, 1.0]],
		colours: vec![white(); 3],
		instance_offset: [0.0; 4],
	};
	let draw = Draw { pipeline: 0, topology: Topology::TriangleList, count: 3, instances: 1, first_instance: 0, base_vertex: 0, restart: false };
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline(Topology::TriangleList)], vec![draw], 16, 16).unwrap();

	let mut targets = vec![Colour::new(16, 16, 1, false), Colour::new(16, 16, 1, false)];
	// The canary is filled with a value nothing in the frame produces.
	targets[1].fill(Vec4::new(-1.0, -1.0, -1.0, -1.0));
	let mut attachments = Attachments { colour: &mut targets, depth_stencil: None, viewport: viewport(16.0, 16.0), scissor: None };
	frame::execute(&mut prepared, &mut attachments, &mesh).unwrap();

	// The target's every pixel was written, including its corners - the triangle covers it whole.
	for (x, y) in [(0_u32, 0_u32), (15, 0), (0, 15), (15, 15), (8, 8)] {
		assert!(near(targets[0].at(x, y, 0).x, 1.0), "({x}, {y}) was not written");
	}
	// And the canary is exactly as it was left.
	for y in 0..16 {
		for x in 0..16 {
			assert_eq!(targets[1].at(x, y, 0), Vec4::new(-1.0, -1.0, -1.0, -1.0), "the canary was written at ({x}, {y})");
		}
	}
}

// ---------------------------------------------------------------------------------------------
// Property and fuzz: hostile input, under a strict allocation limit.
// ---------------------------------------------------------------------------------------------

/// A deterministic generator. NOT A RANDOM ONE: a fuzz fixture whose failing case cannot be
/// reproduced reports a defect nobody can find, so the sequence is fixed and a failure names the
/// iteration that produced it.
struct Noise(u64);

impl Noise {
	fn next(&mut self) -> u64 {
		let mut state = self.0;
		state ^= state >> 12;
		state ^= state << 25;
		state ^= state >> 27;
		self.0 = state;
		state.wrapping_mul(0x2545_F491_4F6C_DD1D)
	}

	fn below(&mut self, ceiling: u32) -> u32 {
		if ceiling == 0 { 0 } else { (self.next() % ceiling as u64) as u32 }
	}

	/// A coordinate drawn from the values that break things: the ordinary range, the clip planes
	/// exactly, far outside, zero, and the non-finite ones.
	fn coordinate(&mut self) -> f32 {
		match self.below(10) {
			0 => f32::NAN,
			1 => f32::INFINITY,
			2 => f32::NEG_INFINITY,
			3 => 0.0,
			4 => 1.0,
			5 => -1.0,
			6 => 1.0e30,
			7 => -1.0e30,
			8 => (self.next() % 2000) as f32 / 1000.0 - 1.0,
			_ => (self.next() % 200_000) as f32 / 1000.0 - 100.0,
		}
	}
}

/// A mesh whose attributes are whatever the generator produced.
struct Hostile {
	positions: Vec<[f32; 4]>,
	uvs: Vec<[f32; 4]>,
	indices: Vec<u32>,
}

impl Source for Hostile {
	fn attribute(&self, location: u32, vertex: u32, _instance: u32) -> Option<Val> {
		let table = if location == 0 { &self.positions } else { &self.uvs };
		table.get(vertex as usize % table.len().max(1)).map(|values| Val::vector_f32(values))
	}

	fn uniform(&self, _block: u32, _member: u32) -> Option<Val> {
		None
	}

	fn sample(&self, _texture: u32, _sampler: u32, _coordinate: &Val) -> Option<Val> {
		None
	}

	fn indices(&self) -> Indices<'_> {
		Indices::U32(&self.indices)
	}
}

#[test]
// HOSTILE GEOMETRY IS ALWAYS ANSWERED. Five hundred generated draws - non-finite positions, vertices
// exactly on the clip planes, coordinates a hundred volumes away, indices past the buffer, extents
// at the raster grid's edge - reach the whole pipeline, and every one comes back `Ok` or with a
// TYPED refusal. Nothing panics, nothing loops and nothing writes outside an attachment.
fn five_hundred_hostile_draws_are_each_answered_rather_than_crashing() {
	let mut noise = Noise(0xA11C_0DE0_1234_5678);
	let mut refusals = 0;
	let mut drawn = 0;
	for iteration in 0..500 {
		let vertices = (noise.below(9) + 1) as usize;
		let mesh = Hostile {
			positions: (0..vertices).map(|_| [noise.coordinate(), noise.coordinate(), noise.coordinate(), noise.coordinate()]).collect(),
			uvs: (0..vertices).map(|_| [noise.coordinate(), noise.coordinate(), 0.0, 1.0]).collect(),
			// Indices that reach past the vertex table on purpose.
			indices: (0..noise.below(12) as usize).map(|_| noise.below(20)).collect(),
		};
		let topology = match noise.below(6) {
			0 => Topology::TriangleList,
			1 => Topology::TriangleStrip,
			2 => Topology::TriangleFan,
			3 => Topology::LineList,
			4 => Topology::LineStrip,
			_ => Topology::PointList,
		};
		// An extent anywhere from one pixel to a large one, and a viewport that need not match it.
		let (width, height) = (noise.below(48) + 1, noise.below(48) + 1);
		let mut state = pipeline(topology);
		state.state.cull = match noise.below(3) {
			0 => CullMode::None,
			1 => CullMode::Front,
			_ => CullMode::Back,
		};
		let draw = Draw { pipeline: 0, topology, count: noise.below(12), instances: noise.below(3) + 1, first_instance: 0, base_vertex: (noise.below(6) as i32) - 3, restart: false };
		let Ok(mut prepared) = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![state], vec![draw], width, height) else {
			refusals += 1;
			continue;
		};
		let mut colour = Colour::new(width, height, 1, false);
		let mut depth = DepthStencil::new(width, height, 1, DepthFormat::Depth32F);
		depth.clear(1.0, 0);
		let view = Viewport::new(noise.coordinate(), noise.coordinate(), (noise.below(64) + 1) as f32, (noise.below(64) + 1) as f32);
		let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: Some(&mut depth), viewport: view, scissor: None };
		match frame::execute(&mut prepared, &mut attachments, &mesh) {
			Ok(stats) => {
				drawn += 1;
				// WHAT IS DRAWN MUST BE BOUNDED BY WHAT WAS ASKED FOR, which is the half a
				// refusal-only fixture never checks.
				assert!(stats.fragments <= width as u32 * height as u32 * 64, "iteration {iteration}: {stats:?} on a {width} x {height} target");
				assert!(stats.samples_written <= stats.fragments, "iteration {iteration}: more samples than fragments");
			}
			Err(render3d::Error::InvalidTransform | render3d::Error::NonFinite { .. } | render3d::Error::InvalidMesh { .. } | render3d::Error::InvalidShader { .. } | render3d::Error::InvalidRenderState { .. } | render3d::Error::LimitExceeded { .. } | render3d::Error::TargetMismatch { .. } | render3d::Error::IncompatiblePipeline { .. }) => refusals += 1,
			Err(other) => panic!("iteration {iteration}: a draw answered {other:?}, which is not a drawing refusal"),
		}
	}
	assert!(drawn > 50, "only {drawn} draws were accepted, so the generator is not reaching the drawing path");
	assert!(refusals > 50, "only {refusals} refusals, which is too few to be testing them");
}

#[test]
// A HOSTILE UV REACHES THE SAMPLER AND IS ANSWERED, for every wrap and filter combination. A texture
// coordinate is shader output: it can be a NaN, an infinity, or a hundred thousand, and a sampler
// that indexed an array with it would read whatever was there.
fn every_wrap_and_filter_answers_a_hostile_texture_coordinate() {
	let texture = strip(&[red(0.0), red(0.25), red(0.5), red(0.75)]);
	let mut noise = Noise(0xA11C_0DE0_8765_4321);
	let wraps = [Wrap::ClampToEdge, Wrap::Repeat, Wrap::MirroredRepeat, Wrap::ClampToBorder];
	let filters = [Filter::Nearest, Filter::Linear];
	for iteration in 0..2000 {
		let sampler = Sampler { wrap_u: wraps[noise.below(4) as usize], wrap_v: wraps[noise.below(4) as usize], wrap_w: wraps[noise.below(4) as usize], minify: filters[noise.below(2) as usize], magnify: filters[noise.below(2) as usize], mip: filters[noise.below(2) as usize], anisotropy: noise.below(8) + 1, ..Sampler::NEAREST };
		let coordinate = [noise.coordinate(), noise.coordinate(), noise.coordinate()];
		let level = noise.coordinate();
		let texel = texture::sample(&texture, &sampler, coordinate, level, noise.next() & 1 == 0);
		// EVERY CHANNEL IS A NUMBER THE TEXTURE OR THE BORDER HOLDS. A sampler that read outside its
		// levels would produce something that is neither.
		for channel in texel {
			assert!(channel.is_finite(), "iteration {iteration}: {sampler:?} at {coordinate:?} level {level} gave {texel:?}");
			assert!((-0.01..=1.01).contains(&channel), "iteration {iteration}: {texel:?} is outside what the texture holds");
		}
		// The anisotropic path answers the same way for the same hostile input.
		let ddx = [noise.coordinate(), noise.coordinate()];
		let ddy = [noise.coordinate(), noise.coordinate()];
		let stretched = texture::sample_anisotropic(&texture, &sampler, coordinate, ddx, ddy, 4, 1);
		for channel in stretched {
			assert!(channel.is_finite(), "iteration {iteration}: anisotropic gave {stretched:?}");
		}
	}
}

#[test]
// ALL THREE INTERPOLATION QUALIFIERS THROUGH THE WHOLE FRAME, at a pixel whose value each rule gives
// a different answer for. A renderer that applied one rule to all three passes every fixture that
// uses only one of them.
fn a_frame_interpolates_each_qualifier_by_its_own_rule() {
	let mut vertex = Builder::new(Stage::Vertex, "qualifiers");
	vertex.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	vertex.varying(1, Type::vec(4), render_shader::Interpolation::NoPerspective);
	vertex.varying(2, Type::vec(4), render_shader::Interpolation::Flat);
	let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
	let value = vertex.load(Type::vec(4), Binding::Attribute { location: 1 });
	vertex.store(Output::Position, position);
	vertex.store(Output::Varying(0), value);
	vertex.store(Output::Varying(1), value);
	vertex.store(Output::Varying(2), value);

	// The fragment stage writes each qualifier into its own channel, so one pixel shows all three.
	let mut fragment = Builder::new(Stage::Fragment, "qualifiers");
	fragment.varying(0, Type::vec(4), render_shader::Interpolation::Smooth);
	fragment.varying(1, Type::vec(4), render_shader::Interpolation::NoPerspective);
	fragment.varying(2, Type::vec(4), render_shader::Interpolation::Flat);
	let smooth = fragment.load(Type::vec(4), Binding::Varying { location: 0 });
	let screen = fragment.load(Type::vec(4), Binding::Varying { location: 1 });
	let flat = fragment.load(Type::vec(4), Binding::Varying { location: 2 });
	let red_part = fragment.assign(Type::f32(), Op::Extract(smooth, 0));
	let green_part = fragment.assign(Type::f32(), Op::Extract(screen, 0));
	let blue_part = fragment.assign(Type::f32(), Op::Extract(flat, 0));
	let one = fragment.constant(Constant::F32(1.0));
	let colour = fragment.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![red_part, green_part, blue_part, one]));
	fragment.store(Output::Colour(0), colour);

	let mut state = pipeline(Topology::TriangleList);
	state.vertex = vertex.finish();
	state.fragment = fragment.finish();

	// A FORESHORTENED TRIANGLE, so the three rules disagree: two vertices four times as far away as
	// the third. The attribute is zero at the near vertex and one at both far ones.
	let mesh = Mesh { positions: vec![[-1.0, -1.0, 0.5, 1.0], [4.0, -4.0, 2.0, 4.0], [-4.0, 4.0, 2.0, 4.0]], colours: vec![[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0], [1.0, 1.0, 1.0, 1.0]], instance_offset: [0.0; 4] };
	let draw = Draw { pipeline: 0, topology: Topology::TriangleList, count: 3, instances: 1, first_instance: 0, base_vertex: 0, restart: false };
	let mut prepared = frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![state], vec![draw], 32, 32).unwrap();
	let mut colour = Colour::new(32, 32, 1, false);
	let mut attachments = Attachments { colour: core::slice::from_mut(&mut colour), depth_stencil: None, viewport: viewport(32.0, 32.0), scissor: None };
	frame::execute(&mut prepared, &mut attachments, &mesh).unwrap();

	// A pixel well inside the triangle, away from every edge.
	let lit: Vec<(u32, u32)> = (4..28).flat_map(|y| (4..28).map(move |x| (x, y))).filter(|(x, y)| colour.at(*x, *y, 0).w > 0.5).collect();
	assert!(!lit.is_empty(), "the triangle covered something");
	let (x, y) = lit[lit.len() / 2];
	let pixel = colour.at(x, y, 0);
	// THE PERSPECTIVE-CORRECT VALUE IS NEARER THE NEAR VERTEX'S than the screen-linear one, because
	// the far vertices' contribution is weighted down by their `1/w`.
	assert!(pixel.x < pixel.y - 0.02, "smooth should be below noperspective at ({x}, {y}): {pixel:?}");
	// AND THE FLAT ONE IS THE PROVOKING VERTEX'S VALUE EXACTLY, whatever the rest of the triangle is.
	assert!(near(pixel.z, 0.0), "flat takes the first vertex of a list, which is zero: {pixel:?}");
}

#[test]
// HOSTILE SHADER IR REACHES `prepare` AND IS ANSWERED THERE. A module the IR refuses, a stage pair
// the wrong way round, a fragment stage reading a varying nothing writes - each is a typed refusal
// before a frame runs, which is what the prepare boundary is for.
fn hostile_shader_modules_are_refused_before_a_frame_runs() {
	let limits = Render3DLimits::PROFILE_MINIMUM;
	let mut refused = 0;
	let mut accepted = 0;
	for iteration in 0..200 {
		let mut state = pipeline(Topology::TriangleList);
		let mut vertex = Builder::new(Stage::Vertex, "hostile");
		let mut fragment = Builder::new(Stage::Fragment, "hostile");
		let choice = iteration % 8;
		match choice {
			0 => {
				// A vertex stage that writes no position.
				let _ = vertex.constant(Constant::F32(1.0));
			}
			1 => {
				// A fragment stage that writes nothing.
				let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
				vertex.store(Output::Position, position);
			}
			2 => {
				// A position computed from a transcendental, which the strict-float rule refuses.
				let source = vertex.load(Type::f32(), Binding::Attribute { location: 0 });
				let turned = vertex.assign(Type::f32(), Op::Transcendental(render_shader::Transcendental::Sin, source, None));
				let position = vertex.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![turned, turned, turned, turned]));
				vertex.store(Output::Position, position);
				let colour = fragment.constant(Constant::F32(1.0));
				let out = fragment.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![colour, colour, colour, colour]));
				fragment.store(Output::Colour(0), out);
			}
			3 => {
				// A loop with no trips.
				let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
				vertex.store(Output::Position, position);
				vertex.loop_bounded(0);
				vertex.end_loop(0);
				let colour = fragment.constant(Constant::F32(1.0));
				let out = fragment.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![colour, colour, colour, colour]));
				fragment.store(Output::Colour(0), out);
			}
			4 => {
				// A fragment stage reading a varying the vertex stage does not write.
				let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
				vertex.store(Output::Position, position);
				fragment.varying(5, Type::vec(4), render_shader::Interpolation::Smooth);
				let value = fragment.load(Type::vec(4), Binding::Varying { location: 5 });
				fragment.store(Output::Colour(0), value);
			}
			5 => {
				// A `discard` in a VERTEX stage.
				let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
				vertex.store(Output::Position, position);
				vertex.discard();
				let colour = fragment.constant(Constant::F32(1.0));
				let out = fragment.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![colour, colour, colour, colour]));
				fragment.store(Output::Colour(0), out);
			}
			6 => {
				// A fragment stage writing a POSITION.
				let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
				vertex.store(Output::Position, position);
				let value = fragment.load(Type::vec(4), Binding::Attribute { location: 0 });
				fragment.store(Output::Position, value);
			}
			_ => {
				// The well-formed one, so the fixture is not testing that everything is refused.
				let position = vertex.load(Type::vec(4), Binding::Attribute { location: 0 });
				vertex.store(Output::Position, position);
				let colour = fragment.constant(Constant::F32(1.0));
				let out = fragment.assign(Type::vec(4), Op::Compose(Type::vec(4), vec![colour, colour, colour, colour]));
				fragment.store(Output::Colour(0), out);
			}
		}
		state.vertex = vertex.finish();
		state.fragment = fragment.finish();
		match frame::prepare(limits, vec![state], vec![], 16, 16) {
			Ok(prepared) => {
				accepted += 1;
				// UNDER A STRICT ALLOCATION LIMIT: an accepted plan for an empty draw list reserves
				// the tile grid and nothing else of consequence.
				assert!(prepared.reserved_bytes() < 64 * 1024, "iteration {iteration}: reserved {} bytes", prepared.reserved_bytes());
			}
			Err(render3d::Error::InvalidShader { .. } | render3d::Error::IncompatiblePipeline { .. } | render3d::Error::LimitExceeded { .. }) => refused += 1,
			Err(other) => panic!("iteration {iteration}: a module answered {other:?}, which is not a shader refusal"),
		}
	}
	assert!(refused >= 150, "only {refused} modules were refused");
	assert!(accepted >= 20, "and {accepted} accepted, so the fixture is not refusing everything");
}

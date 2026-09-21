//! Level of detail: the metric, the ladder, the hysteresis and what happens at the ends.

use render_math::Vec3;
use scene3d::bounds::Sphere;
use scene3d::detail::{self, Detail, Ladder};

use crate::Outcome;
use crate::extended::{claimed, exact};

fn ladder(levels: &[u32]) -> Result<Ladder, crate::Trouble> {
	Ok(Ladder::with_default_thresholds(levels, &claimed())?)
}

pub fn screen_coverage_lod() -> Outcome {
	// THE PROJECTED RADIUS OVER HALF THE VIEWPORT HEIGHT, so a sphere exactly filling the frame
	// vertically covers one. At a 90 degree field of view `tan(fov/2)` is 1, and a unit sphere ten
	// units away covers 0.1.
	let eye = Vec3::new(0.0, 0.0, 0.0);
	for (distance, expected) in [(10.0f32, 0.1f32), (5.0, 0.2), (1.0, 1.0)] {
		let sphere = Sphere::new(Vec3::new(0.0, 0.0, -distance), 1.0);
		let coverage = detail::coverage_perspective(&sphere, eye, 1.0)?;
		require!(exact(coverage, expected), "a unit sphere at {distance} covers {expected}, got {coverage}");
	}
	// AND IT IS NOT DISTANCE: zooming in raises the coverage of the same object at the same place,
	// which a distance-based ladder would miss entirely.
	let sphere = Sphere::new(Vec3::new(0.0, 0.0, -10.0), 1.0);
	let wide = detail::coverage_perspective(&sphere, eye, 1.0)?;
	let narrow = detail::coverage_perspective(&sphere, eye, 0.5)?;
	require!(narrow > wide, "zooming in raises coverage: {narrow} against {wide}");
	require!(exact(narrow, 0.2), "halving tan(fov/2) doubles it, got {narrow}");
	// AN ORTHOGRAPHIC CAMERA HAS NO DISTANCE TERM AT ALL, which is the same formula and not a
	// special case.
	for depth in [-1.0f32, -1000.0] {
		let flat = Sphere::new(Vec3::new(0.0, 0.0, depth), 2.0);
		require!(exact(detail::coverage_orthographic(&flat, 8.0)?, 0.25), "a radius of 2 in a half-height of 8 covers 0.25 at every depth");
	}
	// THE EYE AT THE CENTRE IS THE FINEST LEVEL AND NOT A DIVISION BY ZERO - a camera standing
	// inside a drawable is what walking into a room is.
	let around = Sphere::new(eye, 2.0);
	require!(detail::coverage_perspective(&around, eye, 1.0)? == f32::INFINITY, "the eye at the centre covers everything");
	require!(detail::coverage_perspective(&around, eye, 0.0).is_err(), "and a degenerate camera is refused rather than answered");
	Ok(())
}

pub fn lod_threshold_ladder() -> Outcome {
	// MOST DETAILED FIRST, each level taking over AT OR BELOW its threshold, and the defaults halve:
	// 0.5, 0.25, 0.125, 0.0625, so a level draws roughly a quarter of the pixels of the one above.
	let rungs = ladder(&[10, 11, 12, 13])?;
	require!(rungs.levels() == 4, "four meshes are four levels, got {}", rungs.levels());
	require!(rungs.threshold(0).is_none(), "the finest level has no threshold");
	require!(rungs.threshold(1) == Some(0.5), "the second takes over at 0.5");
	require!(rungs.threshold(3) == Some(0.125), "and the fourth at 0.125");
	for (coverage, level, mesh) in [(0.9f32, 0u32, 10u32), (0.5, 1, 11), (0.25, 2, 12), (0.125, 3, 13)] {
		let chosen = rungs.select(coverage, None);
		require!(chosen == Detail::Level(level), "coverage {coverage} is level {level}, got {chosen:?}");
		require!(rungs.mesh_of(chosen) == Some(mesh), "level {level} draws mesh {mesh}");
	}
	// A LADDER THAT DOES NOT STRICTLY DESCEND IS REFUSED AT LOAD AND NOT SORTED: sorting draws a
	// scene the author did not write and hides the error for ever.
	let rising = alloc::vec![detail::Level { mesh: 1, threshold: 0.25 }, detail::Level { mesh: 2, threshold: 0.5 }];
	require!(Ladder::new(0, rising, &claimed()).is_err(), "a ladder that rises is refused");
	let flat = alloc::vec![detail::Level { mesh: 1, threshold: 0.25 }, detail::Level { mesh: 2, threshold: 0.25 }];
	require!(Ladder::new(0, flat, &claimed()).is_err(), "and so is one that repeats a threshold");
	// A MESH WITH ONE LEVEL NEVER CONSULTS A THRESHOLD, which is what makes the feature free for the
	// meshes that do not use it.
	let single = Ladder::single(7);
	for coverage in [1000.0f32, 0.001] {
		require!(single.select(coverage, None) == Detail::Level(0), "one level is always that level");
	}
	Ok(())
}

pub fn lod_hysteresis() -> Outcome {
	// A TENTH OF THE THRESHOLD, APPLIED TO THE LEVEL HELD LAST FRAME. Without it a drawable sitting
	// on a boundary swaps mesh every frame, which reads as a fault rather than as detail.
	//
	// Against the ladder 0.5 / 0.25: holding level 0 the widened boundary is 0.45, so 0.48 stays and
	// 0.44 moves; holding level 1 it is 0.55, so 0.52 stays and 0.58 comes back.
	let rungs = ladder(&[0, 1, 2])?;
	require!(rungs.select(0.48, Some(Detail::Level(0))) == Detail::Level(0), "0.48 is inside the widened band");
	require!(rungs.select(0.44, Some(Detail::Level(0))) == Detail::Level(1), "0.44 is past it");
	require!(rungs.select(0.52, Some(Detail::Level(1))) == Detail::Level(1), "0.52 is inside it from the other side");
	require!(rungs.select(0.58, Some(Detail::Level(1))) == Detail::Level(0), "0.58 is past it");
	require!(exact(detail::HYSTERESIS, 0.1), "the band is a tenth, got {}", detail::HYSTERESIS);
	// THE FIRST FRAME HAS NO BAND AT ALL: a drawable that appears already small starts small rather
	// than starting detailed and stepping down in view.
	require!(rungs.select(0.48, None) == Detail::Level(1), "with no previous frame the ladder is read directly");
	// AND A BIG JUMP LANDS WHERE THE LADDER SAYS: hysteresis decides WHETHER the level is left, not
	// where it goes, so an object that moves far in one frame does not step down one level a frame.
	require!(rungs.select(0.01, Some(Detail::Level(0))) == Detail::Level(2), "leaving level 0 lands at the level the coverage names");
	Ok(())
}

pub fn last_lod_beyond_ladder() -> Outcome {
	// BELOW THE LAST THRESHOLD THE LAST LEVEL KEEPS BEING DRAWN. A ladder that ran out and drew
	// nothing would delete distant geometry for a reason the author never wrote.
	let rungs = ladder(&[0, 1])?;
	for coverage in [0.4f32, 0.01, 1e-6, 0.0] {
		require!(rungs.select(coverage, None) == Detail::Level(1), "coverage {coverage} still draws the coarsest level");
	}
	Ok(())
}

pub fn lod_cull_below_coverage() -> Outcome {
	// A MESH MAY DECLARE A COVERAGE IT VANISHES AT, which is a decision a scene makes about its own
	// content rather than one the ladder makes for it. VANISHED IS NOT CULLED: the drawable is on
	// screen and the scene chose not to draw it, and the two are told apart by the answer.
	let rungs = ladder(&[0, 1])?.vanishing_below(0.01)?;
	require!(rungs.select(0.02, None) == Detail::Level(1), "above the coverage it is drawn");
	require!(rungs.select(0.005, None) == Detail::Vanished, "and below it, it is not");
	require!(rungs.mesh_of(Detail::Vanished).is_none(), "a vanished drawable draws no mesh");
	// THE SAME TENTH GUARDS IT, because the vanishing coverage is a threshold like any other and a
	// drawable sitting on it would otherwise blink.
	require!(rungs.select(0.0095, Some(Detail::Level(1))) == Detail::Level(1), "a drawn drawable holds inside the band");
	require!(rungs.select(0.0105, Some(Detail::Vanished)) == Detail::Vanished, "and a vanished one stays away inside it");
	// AND A DEFAULT LADDER HAS NONE, so nothing vanishes unless a mesh asked for it.
	require!(ladder(&[0, 1])?.select(1e-9, None) == Detail::Level(1), "without a declared coverage nothing vanishes");
	Ok(())
}

pub fn dynamic_bounds_after_deformation() -> Outcome {
	// THE COVERAGE COMES FROM THE BOUNDS THE DRAWABLE HAS NOW. A character that raises an arm GROWS
	// its bounding sphere, and a coverage taken from the rest pose would step that arm down a level
	// while it is still on screen.
	//
	// A rest pose of radius 2.4 at ten units under a 90 degree field of view covers 0.24, just under
	// the 0.25 threshold and therefore level 2; the same character with an arm up has a radius of
	// 2.6, covers 0.26, and is level 1.
	let rungs = ladder(&[0, 1, 2])?;
	let eye = Vec3::new(0.0, 0.0, 0.0);
	let at = Vec3::new(0.0, 0.0, -10.0);
	let rest = detail::coverage_perspective(&Sphere::new(at, 2.4), eye, 1.0)?;
	let posed = detail::coverage_perspective(&Sphere::new(at, 2.6), eye, 1.0)?;
	require!(rest < 0.25 && posed > 0.25, "the fixture straddles the threshold: {rest} and {posed}");
	require!(rungs.select(rest, None) == Detail::Level(2), "the rest pose is the coarser level");
	require!(rungs.select(posed, None) == Detail::Level(1), "and the pose the drawable is actually in is the finer one");
	Ok(())
}

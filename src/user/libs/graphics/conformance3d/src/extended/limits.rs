//! The extended limits, and that a scene which never claimed the part cannot reach any of them.

use scene3d::Limits;

use crate::Outcome;
use crate::extended::claimed;

pub fn extended_limits() -> Outcome {
	// EVERY LIMIT THE PROFILE NAMES IS ONE THIS LAYER ENFORCES, BY NAME, at the profile's own floor.
	// A limit the document names and the layer does not enforce is a promise nothing keeps.
	let limits = claimed();
	for entry in graphics_profile::scene3d_extended::SCENE3D_EXTENDED_1_MIN_LIMITS {
		let Some(held) = limits.by_name(entry.name) else {
			return Err(crate::Trouble::Failed(alloc::format!("the Extended profile names `{}` and this layer has no such limit", entry.name)));
		};
		require!(held == entry.minimum, "`{}` must be the Extended profile's floor of {}, got {held}", entry.name, entry.minimum);
	}
	// AND EXTENDED IS ADDITIVE: claiming it moves no core limit at all. A part that quietly raised
	// or lowered one would be a second profile wearing the first one's name.
	let core = Limits::PROFILE_MINIMUM;
	for entry in graphics_profile::scene3d::SCENE3D_PROFILE_1_MIN_LIMITS {
		require!(limits.by_name(entry.name) == core.by_name(entry.name), "`{}` is a core limit and Extended does not move it", entry.name);
	}
	Ok(())
}

pub fn extended_limit_refusal() -> Outcome {
	// ENTIRELY OR NOT AT ALL IS THE PART'S OWN RULE, and a scene carrying some of the limits and not
	// the others is exactly what it refuses: an application would find shadows and no skinning, with
	// nothing anywhere saying which half it had.
	require!(claimed().claims_extended(), "a scene at the Extended floor claims the part");
	require!(!Limits::PROFILE_MINIMUM.claims_extended(), "and a core one does not");
	let half = Limits { max_skeleton_joints: 0, ..claimed() };
	require!(!half.claims_extended(), "a scene carrying some of the limits and not the others claims nothing");
	// AND A CORE SCENE REACHES NO EXTENDED OPERATION AT ALL, because every one of them is bounded by
	// a limit that is zero there. Checked through the cheapest of them.
	require!(scene3d::shadow::split_distances(1.0, 100.0, 1, 0.5, &Limits::PROFILE_MINIMUM).is_err(), "a core scene has no cascades");
	require!(scene3d::Ladder::with_default_thresholds(&[0, 1], &Limits::PROFILE_MINIMUM).is_err(), "and no level-of-detail ladder");
	Ok(())
}

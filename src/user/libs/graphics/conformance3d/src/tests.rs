//! THE SUITE RUN AS A HOST TEST, which is what makes it runnable at all while it is being written.
//!
//! WHAT THIS ASSERTS IS THE SUITE'S OWN CLAIM: every scene passes, and every feature the profile has
//! is covered by one. The guest run makes the same claim on each target; this one makes it on the
//! machine that builds the tree, in a second.

use super::{Verdict, run};

#[test]
fn every_scene_passes_and_every_feature_has_one() {
	let mut trouble = alloc::vec::Vec::new();
	let summary = run(|name, group, verdict| {
		if !matches!(verdict, Verdict::Pass) {
			trouble.push(alloc::format!("{group}/{name}: {verdict:?}"));
		}
	});
	assert!(trouble.is_empty(), "scenes that did not pass:\n{}", trouble.join("\n"));
	assert!(summary.render3d.untested.is_empty(), "Render3D features with no scene: {:?}", summary.render3d.untested);
	assert!(summary.scene3d.untested.is_empty(), "Scene3D features with no scene: {:?}", summary.scene3d.untested);
	assert!(summary.complete(), "{summary:?}");

	// AND THE EXTENDED PROFILE, WHICH THIS LAYER CLAIMS. It is optional as a whole - `complete()`
	// above deliberately leaves it out, because an implementation conforms to the two core profiles
	// while supporting none of it - so the claim that this one carries it is made here, separately
	// and on purpose.
	assert!(summary.extended.untested.is_empty(), "Extended features with no scene: {:?}", summary.extended.untested);
	assert!(summary.complete_with_extended(), "this layer claims Scene3D Extended Profile 1: {summary:?}");
	// ENTIRELY OR NOT AT ALL: one case per entry, neither more nor fewer. A case for a feature the
	// profile does not have would be a scene measuring an extension while reporting Profile 1.
	assert_eq!(super::EXTENDED_CASES.len(), graphics_profile::SCENE3D_EXTENDED_PROFILE_1.len(), "one scene per entry: {} cases against {} features", super::EXTENDED_CASES.len(), graphics_profile::SCENE3D_EXTENDED_PROFILE_1.len());
}

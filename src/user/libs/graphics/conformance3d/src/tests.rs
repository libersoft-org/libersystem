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
}

use super::*;

#[test]
// THE SUITE RUNS ON THE HOST TOO, which is what makes a broken scene a compile-and-test failure here
// rather than a serial log from a guest an hour later. The guest run is what proves the arithmetic
// agrees on the target; this is what proves the scenes are right.
fn every_scene_passes_and_every_feature_has_one() {
	let mut trouble = Vec::new();
	let summary = run(|name, group, verdict| {
		if !matches!(verdict, Verdict::Pass) {
			trouble.push(alloc::format!("{group}/{name}: {verdict:?}"));
		}
	});
	assert!(trouble.is_empty(), "{trouble:#?}");
	assert!(summary.untested.is_empty(), "profile features with no scene: {:?}", summary.untested);
	assert_eq!(summary.passed, RENDER2D_CORE_PROFILE_1.len() + OUTPUT_CASES.len(), "every feature is covered exactly once, and the output path with it");
	assert!(summary.complete());
}

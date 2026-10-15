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

#[test]
// THE THRESHOLDS THE SCENES APPLY ARE THE FROZEN ONES. `graphics-profile` freezes them as text and the scenes
// apply them as numbers: this holds the two to each other, so neither moves alone.
fn the_thresholds_the_scenes_apply_are_the_frozen_ones() {
	use crate::harness::threshold;
	let frozen = |what: &str| graphics_profile::thresholds::THRESHOLDS.iter().find(|entry| entry.profile == "render2d" && entry.what == what).unwrap_or_else(|| panic!("the frozen `{what}` threshold")).tolerance;
	assert!(frozen("antialiasing coverage").starts_with("<= 2/255 absolute per pixel against the analytic area, and <= 1/255 mean over the covered region"));
	assert_eq!((threshold::COVERAGE_PER_PIXEL, threshold::COVERAGE_MEAN), (2.0 / 255.0, 1.0 / 255.0));
	assert!(frozen("a filter kernel's output").starts_with("<= 1e-3 relative against the kernel evaluated at f64"));
	assert_eq!(threshold::FILTER_RELATIVE, 1e-3);
	assert!(frozen("the edge policy").contains("compared EXACTLY"));
	assert!(frozen("the image comparison mask").starts_with("EVERY pixel of the target is compared"));
}

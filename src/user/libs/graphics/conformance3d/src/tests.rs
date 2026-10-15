//! Each mandatory Core profile and the optional Extended profile has its own host result.
//! The default combined API remains covered, with exactly the same original scenes.

use super::{Verdict, run, run_core, run_extended};

#[test]
fn core_scenes_pass_without_running_or_claiming_extended() {
	let mut reports = alloc::vec::Vec::new();
	let mut summary = run_core(|name, group, verdict| {
		assert!(matches!(verdict, Verdict::Pass), "{group}/{name}: {verdict:?}");
		reports.push((name, group));
	});
	let mut expected: alloc::vec::Vec<_> = graphics_profile::RENDER3D_CORE_PROFILE_1.iter().map(|entry| (entry.name, entry.group)).chain(graphics_profile::SCENE3D_CORE_PROFILE_1.iter().map(|entry| (entry.name, entry.group))).collect();
	reports.sort_unstable();
	expected.sort_unstable();
	assert_eq!(reports, expected, "Core must report exactly its two registries, with no Extended scene");
	assert!(summary.complete(), "{summary:?}");
	assert_eq!(summary.extended.total(), 0, "Extended was not executed");
	assert!(!summary.complete_with_extended(), "unperformed Extended is not conformance");
	// A separately failed/unsupported/missing Extended profile never changes Core's claim.
	summary.extended.failed = 1;
	summary.extended.unsupported = 1;
	summary.extended.untested.push("not performed");
	assert!(summary.complete());
	assert!(!summary.complete_with_extended());
}

#[test]
fn extended_scenes_pass_as_their_own_profile() {
	let mut reports = alloc::vec::Vec::new();
	let summary = run_extended(|name, group, verdict| {
		assert!(matches!(verdict, Verdict::Pass), "{group}/{name}: {verdict:?}");
		reports.push((name, group));
	});
	let mut expected: alloc::vec::Vec<_> = graphics_profile::SCENE3D_EXTENDED_PROFILE_1.iter().map(|entry| (entry.name, entry.group)).collect();
	reports.sort_unstable();
	expected.sort_unstable();
	assert_eq!(reports, expected, "Extended must report only its own registry");
	assert!(summary.complete(), "{summary:?}");
	assert_eq!(super::EXTENDED_CASES.len(), graphics_profile::SCENE3D_EXTENDED_PROFILE_1.len());
	assert_eq!(summary.passed, expected.len());
}

#[test]
fn combined_entry_point_preserves_all_three_profile_results() {
	let mut reports = 0;
	let summary = run(|name, group, verdict| {
		assert!(matches!(verdict, Verdict::Pass), "{group}/{name}: {verdict:?}");
		reports += 1;
	});
	assert!(summary.complete_with_extended(), "{summary:?}");
	assert_eq!(reports, graphics_profile::RENDER3D_CORE_PROFILE_1.len() + graphics_profile::SCENE3D_CORE_PROFILE_1.len() + graphics_profile::SCENE3D_EXTENDED_PROFILE_1.len());
	assert_eq!(summary.passed(), reports);
}

#[test]
// THE DEPTH THRESHOLD THE SCENES APPLY IS THE FROZEN ONE. `graphics-profile` freezes it as text, and the
// scenes apply it as a number: this holds the two to each other, so neither moves alone.
fn the_depth_threshold_the_scenes_apply_is_the_frozen_one() {
	let frozen = graphics_profile::thresholds::THRESHOLDS.iter().find(|threshold| threshold.profile == "render3d" && threshold.what == "a depth value").expect("the frozen depth threshold");
	assert!(frozen.tolerance.starts_with("<= 1 ULP for a floating-point format and EXACT for a normalised one"), "{}", frozen.tolerance);
	assert_eq!(crate::harness::DEPTH_FLOAT_ULPS, 1);
	// AND ONE ULP IS ONE STEP AND NOT TWO.
	let half = 0.5f32;
	assert_eq!(crate::harness::ulps_apart(half, f32::from_bits(half.to_bits() + 1)), 1);
	assert_eq!(crate::harness::ulps_apart(half, f32::from_bits(half.to_bits() - 2)), 2);
	assert_eq!(crate::harness::ulps_apart(0.25, 0.25), 0);
}

use super::*;
use crate::render2d::{entry_by_name, in_profile_1};

// THE CLOSED-LIST CHECK, WRITTEN ONCE FOR BOTH PROFILES. Two copies of it is the second place a
// rule can be weakened, and the 3D list is the one nothing else reads yet.
fn a_closed_list<F: Copy + PartialEq + core::fmt::Debug>(what: &str, profile: &[ProfileEntry<F>], groups: &[&str]) {
	for (index, entry) in profile.iter().enumerate() {
		assert!(groups.contains(&entry.group), "{what}: {} is documented under an unknown group {}", entry.name, entry.group);
		for other in &profile[index + 1..] {
			assert_ne!(entry.name, other.name, "{what}: the profile names {} twice", entry.name);
			assert_ne!(entry.feature, other.feature, "{what}: the profile carries {:?} twice", entry.feature);
		}
	}
	// AND EVERY GROUP HAS AT LEAST ONE FEATURE. A group nobody is in is a heading in a generated
	// table with nothing under it, which reads as a feature area that was forgotten rather than one
	// that is empty on purpose.
	for group in groups {
		assert!(profile.iter().any(|entry| entry.group == *group), "{what}: the group {group} has no features");
	}
	// AND SOMETHING IS BACKEND-OWNED. If nothing were, the handler check would range over an empty
	// set and pass for a backend that implements nothing.
	assert!(profile.iter().any(|entry| entry.owner == FeatureOwner::Backend), "{what}: the profile must name what a backend has to implement");
}

#[test]
// THE PROFILE IS CLOSED, AND CLOSED MEANS NO NAME APPEARS TWICE AND EVERY GROUP IS ONE THE PROFILE
// DOCUMENTS. A duplicate entry is a conformance matrix with two rows for one feature - which reports
// a backend as covering something twice and another thing not at all.
fn both_profiles_are_closed_lists_with_unique_names_and_known_groups() {
	a_closed_list("render2d", RENDER2D_CORE_PROFILE_1, RENDER2D_GROUPS);
	a_closed_list("render3d", RENDER3D_CORE_PROFILE_1, RENDER3D_GROUPS);
}

#[test]
// THE COUNT AND THE LIST HAVE TO AGREE, which the plan says in as many words about Porter-Duff:
// twelve operators plus additive `Plus`, which is not one of them. A reader who trusts the sentence
// and a matrix built from the list would otherwise disagree by one.
fn compositing_carries_twelve_porter_duff_operators_plus_additive_plus() {
	let porter_duff = [
		Render2DFeature::CompositeClear,
		Render2DFeature::CompositeSource,
		Render2DFeature::CompositeDestination,
		Render2DFeature::CompositeSourceOver,
		Render2DFeature::CompositeDestinationOver,
		Render2DFeature::CompositeSourceIn,
		Render2DFeature::CompositeDestinationIn,
		Render2DFeature::CompositeSourceOut,
		Render2DFeature::CompositeDestinationOut,
		Render2DFeature::CompositeSourceAtop,
		Render2DFeature::CompositeDestinationAtop,
		Render2DFeature::CompositeXor,
	];
	assert_eq!(porter_duff.len(), 12);
	for operator in porter_duff {
		assert!(in_profile_1(operator), "{operator:?} is a Porter-Duff operator and must be in the profile");
	}
	assert!(in_profile_1(Render2DFeature::CompositePlus), "additive Plus is in the profile and is not a Porter-Duff operator");
	// The separable eleven and the non-separable four, counted the same way.
	let separable = [
		"BlendMultiply",
		"BlendScreen",
		"BlendOverlay",
		"BlendDarken",
		"BlendLighten",
		"BlendColorDodge",
		"BlendColorBurn",
		"BlendHardLight",
		"BlendSoftLight",
		"BlendDifference",
		"BlendExclusion",
	];
	assert_eq!(separable.len(), 11);
	let non_separable = ["BlendHue", "BlendSaturation", "BlendColor", "BlendLuminosity"];
	assert_eq!(non_separable.len(), 4);
	for name in separable.iter().chain(non_separable.iter()) {
		assert!(entry_by_name(name).is_some(), "{name} is a blend mode Profile 1 requires");
	}
}

#[test]
// THE QUERIES ARE NOT THE BACKEND'S, and this is the whole reason a feature carries an owner. A
// handler check that ranged over these would force a rasteriser to stub `QueryPathLength` for no
// reason but to satisfy a gate.
fn geometry_queries_are_owned_by_render2d_and_never_by_a_backend() {
	for name in ["QueryPathBoolean", "QueryHitTest", "QueryTightBounds", "QueryStrokeBounds", "QueryPathLength", "QueryPointAtDistance", "QueryTangentAtDistance"] {
		let entry = entry_by_name(name).expect("the query is in the profile");
		assert_eq!(entry.owner, FeatureOwner::Render2D, "{name} is a geometry-core function a rasteriser never sees");
	}
}

#[test]
// THE 3D PROFILE SAYS WHAT IT SAYS, counted rather than read. Each of these is a sentence in the
// plan whose list form is what a backend is measured against - and a depth format or a sample count
// missing from the enumeration is one nothing would ever ask a backend for.
fn the_3d_profile_carries_every_depth_format_sample_count_and_readback_the_plan_names() {
	for name in ["Depth16", "Depth24", "Depth32F", "Depth24Stencil8", "Depth32FStencil8"] {
		assert!(render3d::entry_by_name(name).is_some(), "{name} is a depth format Profile 1 requires");
	}
	for name in ["Msaa1x", "Msaa2x", "Msaa4x", "SampleMask", "AlphaToCoverage", "MsaaResolve"] {
		assert!(render3d::entry_by_name(name).is_some(), "{name} is part of the multisampling the profile requires");
	}
	for name in ["ReadbackColor", "ReadbackDepth", "ReadbackObjectId"] {
		assert!(render3d::entry_by_name(name).is_some(), "{name} is a readback Profile 1 requires");
	}
	assert!(render3d::in_profile_1(Render3DFeature::Anisotropy8), "anisotropic filtering to max_anisotropy 8 is in the profile");
	// AND THE COLOUR FORMATS ARE THE IMAGE MODEL'S, not a second sRGB storage format invented here:
	// the encoding is `ColorSpace`'s, which is why these are owned by `GraphicsCore`.
	for name in ["FormatR8", "FormatRG8", "FormatRGBA8", "FormatRGB10A2", "FormatRGBA16F", "FormatRGBA32F", "FormatR32Uint"] {
		let entry = render3d::entry_by_name(name).expect("the format is in the profile");
		assert_eq!(entry.owner, FeatureOwner::GraphicsCore, "{name} is the shared image model's format");
	}
}

#[test]
fn a_declaration_below_the_guaranteed_minima_is_refused_and_names_the_field() {
	assert_eq!(RENDER2D_PROFILE_1_MINIMA.meets_profile_1(), Ok(()), "the minima meet themselves");
	// RAISING IS ALLOWED AND LOWERING IS NOT, which is what "guaranteed minimum" means.
	let generous = Render2DLimits { max_commands: RENDER2D_PROFILE_1_MINIMA.max_commands * 2, ..RENDER2D_PROFILE_1_MINIMA };
	assert_eq!(generous.meets_profile_1(), Ok(()));
	// EVERY FIELD IS CHECKED, one at a time, and the refusal names the one that is short - which is
	// the difference between a reader raising the right number and comparing fifteen pairs by hand.
	let short = Render2DLimits { max_path_points: 10, ..RENDER2D_PROFILE_1_MINIMA };
	assert_eq!(short.meets_profile_1(), Err("max_path_points"), "a profile that accepted ten path points is what the minima exist to refuse");
	let shallow = Render2DLimits { max_clip_depth: 1, ..RENDER2D_PROFILE_1_MINIMA };
	assert_eq!(shallow.meets_profile_1(), Err("max_clip_depth"));
	let small_layer = Render2DLimits { max_layer_pixels: 1, ..RENDER2D_PROFILE_1_MINIMA };
	assert_eq!(small_layer.meets_profile_1(), Err("max_layer_pixels"));
}

#[test]
// THE TWO NAME SETS ARE DISJOINT, and something depends on that rather than assuming it. A claim in
// the source names a feature and not a profile - `@covers: FillNonZero` - so a name carried by both
// lists would be a test counted twice, or counted for the wrong profile. Today they share nothing;
// the day somebody adds `Viewport` to the 2D list, this says so.
fn the_two_profiles_share_no_feature_name() {
	for entry in RENDER2D_CORE_PROFILE_1 {
		assert!(render3d::entry_by_name(entry.name).is_none(), "{} is in both profiles, and a claim naming it would belong to neither", entry.name);
	}
}

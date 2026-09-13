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
	assert_eq!(RENDER2D_PROFILE_1_MIN_LIMITS.meets_profile_1(), Ok(()), "the minima meet themselves");
	// RAISING IS ALLOWED AND LOWERING IS NOT, which is what "guaranteed minimum" means.
	let generous = Render2DLimits { max_commands: RENDER2D_PROFILE_1_MIN_LIMITS.max_commands * 2, ..RENDER2D_PROFILE_1_MIN_LIMITS };
	assert_eq!(generous.meets_profile_1(), Ok(()));
	// EVERY FIELD IS CHECKED, one at a time, and the refusal names the one that is short - which is
	// the difference between a reader raising the right number and comparing fifteen pairs by hand.
	let short = Render2DLimits { max_path_points: 10, ..RENDER2D_PROFILE_1_MIN_LIMITS };
	assert_eq!(short.meets_profile_1(), Err("max_path_points"), "a profile that accepted ten path points is what the minima exist to refuse");
	let shallow = Render2DLimits { max_clip_depth: 1, ..RENDER2D_PROFILE_1_MIN_LIMITS };
	assert_eq!(shallow.meets_profile_1(), Err("max_clip_depth"));
	let small_layer = Render2DLimits { max_layer_pixels: 1, ..RENDER2D_PROFILE_1_MIN_LIMITS };
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

#[test]
// A FORMAT SET CHOSEN FOR A SCANOUT IS THE WRONG SET. Eight-bit RGB is what a display takes; it is
// not what a coverage mask, a glyph cache, a filter intermediate, a wide-gamut composite or an HDR
// target is made of, and adding those formats after the compositing code exists means rewriting the
// compositing code. Every one of those uses has a format here.
fn the_format_set_covers_every_use_and_not_only_a_scanout() {
	use crate::image::{FORMATS, format_named};
	for required in ["A8_UNORM", "B8G8R8A8_UNORM", "R10G10B10A2_UNORM", "R16G16B16A16_FLOAT", "R32_UINT", "R32G32B32A32_FLOAT"] {
		assert!(format_named(required).is_some(), "{required} is required by the profile and is not in the list");
	}
	// A format nobody declared is not a format, and answering a default for an unknown name would let
	// a backend accept storage the profile never froze.
	assert!(format_named("R5G6B5_UNORM").is_none());
	assert!(format_named("").is_none());

	// EVERY ENTRY SAYS WHAT IT IS FOR. A format list with no reason beside each entry is a list
	// somebody extends, and the extension is what a closed profile exists to refuse.
	for format in FORMATS {
		assert!(!format.purpose.is_empty(), "{} publishes no reason", format.name);
		assert!(!format.bits.is_empty(), "{} publishes no channel widths", format.name);
		// The bits and the byte count must agree, which is the arithmetic a packed format gets wrong.
		let total: u32 = format.bits.iter().map(|bits| *bits as u32).sum();
		assert_eq!(total, format.bytes_per_pixel as u32 * 8, "{}'s channel widths and its byte count disagree", format.name);
		assert_eq!(format.channels.len(), format.bits.len(), "{} names a different number of channels than it has widths", format.name);
	}
	// And no name appears twice, which a hand-maintained list does eventually.
	for (index, format) in FORMATS.iter().enumerate() {
		assert!(!FORMATS[index + 1..].iter().any(|other| other.name == format.name), "{} appears twice", format.name);
	}
}

#[test]
// `Straight` ON A FORMAT WITH NO ALPHA IS A TYPE ERROR AND NOT AN UNSUPPORTED CASE - it is a
// combination that cannot mean anything, and saying so here is what keeps it out of every backend's
// match arms. And an ALPHA-ONLY format is a third case: premultiplication is a relation between
// colour and alpha, and with no colour there is nothing to have been multiplied.
fn the_alpha_modes_a_format_admits_are_decided_by_what_channels_it_has() {
	use crate::image::{AlphaMode, alpha_modes, format_named};
	let modes = |name: &str| alpha_modes(format_named(name).expect("a format the profile carries"));
	assert_eq!(modes("B8G8R8A8_UNORM"), &[AlphaMode::Opaque, AlphaMode::Straight, AlphaMode::Premultiplied]);
	assert_eq!(modes("B8G8R8X8_UNORM"), &[AlphaMode::Opaque], "an X8 format is opaque only");
	assert_eq!(modes("R32_UINT"), &[AlphaMode::Opaque], "a data format has no alpha to be straight about");
	assert_eq!(modes("A8_UNORM"), &[AlphaMode::Straight], "an alpha-only format has no colour to premultiply");
}

#[test]
// A NORMAL MAP, A ROUGHNESS MAP, A COVERAGE MASK AND A DEPTH BUFFER ARE ALL JUST BYTES, and running
// any of them through an sRGB decode corrupts them silently - the artefact looks like a lighting bug,
// which is where the days go. Exactly one kind of image admits a transfer function.
fn a_transfer_function_applies_to_colour_and_to_nothing_else() {
	use crate::image::Semantics;
	assert!(Semantics::Color.is_colour());
	for other in [Semantics::Mask, Semantics::Normal, Semantics::Data, Semantics::Depth, Semantics::Identity] {
		assert!(!other.is_colour(), "{} must not admit a transfer function", other.name());
	}
}

#[test]
// "SUPPORTS COLOUR MANAGEMENT" IS NOT A CONTRACT. Every constant two implementations would otherwise
// choose differently is a value here, and this holds them to being the values the standards state
// rather than the values somebody remembered.
fn every_colour_constant_is_the_one_its_standard_states() {
	use crate::image::{bradford, hlg, pq, srgb};
	// sRGB is NOT a pure power law, and the linear segment is the part that gets dropped.
	assert_eq!(srgb::ENCODED_THRESHOLD, 0.04045);
	assert_eq!(srgb::LINEAR_THRESHOLD, 0.0031308);
	assert_eq!(srgb::SLOPE, 12.92);
	assert_eq!(srgb::OFFSET, 0.055);
	assert_eq!(srgb::EXPONENT, 2.4);
	// THE TWO SEGMENTS MUST MEET. A threshold pair that does not is a discontinuity at the darkest
	// values, which is exactly where banding is visible - and it is the check that catches a
	// transcription error in either number.
	let joined = srgb::LINEAR_THRESHOLD * srgb::SLOPE;
	assert!((joined - srgb::ENCODED_THRESHOLD).abs() < 1e-5, "the linear and the power segments must meet: {joined} against {}", srgb::ENCODED_THRESHOLD);

	// PQ's constants are twelve-bit FRACTIONS, and holding them to their fractional form is what
	// catches a decimal rounding somebody pasted from elsewhere.
	assert_eq!(pq::M1, 2610.0 / 16384.0);
	assert_eq!(pq::M2, 2523.0 / 4096.0 * 128.0);
	assert_eq!(pq::C1, 3424.0 / 4096.0);
	assert_eq!(pq::C2, 2413.0 / 4096.0 * 32.0);
	assert_eq!(pq::C3, 2392.0 / 4096.0 * 32.0);
	// `c1 = c3 - c2 + 1` is the standard's own identity, and it is what a mistyped constant breaks.
	assert!((pq::C1 - (pq::C3 - pq::C2 + 1.0)).abs() < 1e-9);
	assert_eq!(pq::PEAK_NITS, 10_000.0);

	// HLG's `b` and `c` are DERIVED from `a`, and the derivation is the check.
	assert_eq!(hlg::A, 0.178_832_77);
	assert!((hlg::B - (1.0 - 4.0 * hlg::A)).abs() < 1e-7, "b must be 1 - 4a");
	assert!((hlg::C - (0.5 - hlg::A * (4.0 * hlg::A).ln())).abs() < 1e-7, "c must be 0.5 - a ln(4a)");

	// BRADFORD, and its inverse must actually be one: a transcribed matrix pair that is not is a
	// round trip that loses colour, which looks like a gamut problem rather than a typo.
	for row in 0..3 {
		for column in 0..3 {
			let mut sum = 0.0;
			for inner in 0..3 {
				sum += bradford::FORWARD[row][inner] * bradford::INVERSE[inner][column];
			}
			let expected = if row == column { 1.0 } else { 0.0 };
			assert!((sum - expected) < 1e-6 && (expected - sum) < 1e-6, "Bradford forward times inverse is not the identity at {row},{column}: {sum}");
		}
	}
}

#[test]
// A DITHER MATRIX THAT IS NOT A PERMUTATION IS NOT A DITHER MATRIX: the whole point is that the
// sixty-four offsets are each used once, so the pattern is uniform. And the phase is anchored to the
// TARGET's origin, because a tile-relative phase makes the pattern restart at every tile boundary -
// the artefact that looks like a seam.
fn the_dither_matrix_is_a_permutation_of_its_own_range() {
	use crate::image::dither;
	let mut seen = [false; 64];
	for row in dither::MATRIX {
		for value in row {
			assert!(!seen[value as usize], "{value} appears twice in the dither matrix");
			seen[value as usize] = true;
		}
	}
	assert!(seen.iter().all(|seen| *seen), "the dither matrix does not use every value in its range");
	assert_eq!(dither::PHASE, "the target's origin");
}

#[test]
// GETTING THE YUV RANGE WRONG IS THE SINGLE COMMONEST CAUSE OF WASHED-OUT OR CRUSHED VIDEO, and
// getting the ORDER of operations wrong decodes the transfer function of a signal that is not yet a
// colour. Both are values here rather than a recipe a reader reconstructs.
fn the_yuv_rules_are_values_rather_than_a_recipe() {
	use crate::image::{YUV_LAYOUTS, YUV_MATRICES, yuv};
	// Every layout says how many planes it has and where a sample's bits sit - and P010's bits are in
	// the HIGH ten of its word, which is the one a reader assumes is the low ten.
	for layout in YUV_LAYOUTS {
		assert!(layout.planes >= 2, "{} states fewer planes than a YUV layout has", layout.name);
		assert!(!layout.bit_placement.is_empty(), "{} does not say where its bits sit", layout.name);
	}
	let p010 = YUV_LAYOUTS.iter().find(|layout| layout.name == "P010").expect("P010 is required");
	assert_eq!(p010.bits, 10);
	assert!(p010.bit_placement.contains("HIGH"), "P010's ten bits are the HIGH ten of a sixteen-bit word");

	// The three matrices, each stated as the two coefficients every other one is derived from - and
	// the third, `Kg`, must be what they leave.
	for matrix in YUV_MATRICES {
		let green = 1.0 - matrix.kr - matrix.kb;
		assert!(green > 0.5 && green < 0.8, "{}'s implied green coefficient {green} is not a luminance coefficient", matrix.name);
	}
	assert_eq!(YUV_MATRICES.len(), 3);

	// Limited range is NOT the full byte, and the two ranges differ for luma and chroma.
	assert_eq!(yuv::LIMITED_LUMA_8, (16, 235));
	assert_eq!(yuv::LIMITED_CHROMA_8, (16, 240));
	assert_eq!(yuv::FULL_8, (0, 255));
	// The ten-bit ranges are the eight-bit ones scaled by four, which is the derivation a
	// re-derivation gets wrong.
	assert_eq!(yuv::LIMITED_LUMA_10, (yuv::LIMITED_LUMA_8.0 * 4, yuv::LIMITED_LUMA_8.1 * 4));
	assert_eq!(yuv::LIMITED_CHROMA_10, (yuv::LIMITED_CHROMA_8.0 * 4, yuv::LIMITED_CHROMA_8.1 * 4));

	// THE ORDER IS THE PART A RECIPE WRITTEN FOR RGB GETS WRONG.
	assert!(yuv::ORDER.contains("encoded RGB"), "the matrix produces ENCODED RGB, before any transfer decoding");
	assert!(yuv::ORDER.find("matrix").unwrap() < yuv::ORDER.find("transfer").unwrap(), "the matrix is applied BEFORE the transfer function is decoded");
}

#[test]
// A COLOUR-MANAGED PIPELINE THAT CANNOT TELL A COLOUR FROM A MEASUREMENT WILL TRANSFORM THE
// MEASUREMENT. Each refusal here is a defect somebody has shipped: a filtered identity image averages
// two object ids into a third that names a different object, and the bug that follows is a click
// landing on the wrong thing.
fn the_operations_an_image_admits_follow_from_what_it_means() {
	use crate::image::{Operation, Semantics, operations};
	let allows = |semantics: Semantics, operation: Operation| operations(semantics).contains(&operation);

	// Only colour is transferred, converted, premultiplied or dithered.
	for operation in [Operation::Transfer, Operation::ConvertPrimaries, Operation::Premultiply, Operation::Dither] {
		assert!(allows(Semantics::Color, operation), "colour must admit {}", operation.name());
		for other in [Semantics::Mask, Semantics::Normal, Semantics::Data, Semantics::Depth, Semantics::Identity] {
			assert!(!allows(other, operation), "{} must not admit {}", other.name(), operation.name());
		}
	}
	// An identity is never filtered, and neither is depth.
	assert!(!allows(Semantics::Identity, Operation::Filter));
	assert!(!allows(Semantics::Depth, Operation::Filter));
	// A mask filters and blends - that is what coverage is for.
	assert!(allows(Semantics::Mask, Operation::Filter));
	assert!(allows(Semantics::Mask, Operation::Blend));
	// EVERYTHING CAN BE SAMPLED. An image nothing may read is not an image.
	for semantics in [Semantics::Color, Semantics::Mask, Semantics::Normal, Semantics::Data, Semantics::Depth, Semantics::Identity] {
		assert!(allows(semantics, Operation::Sample), "{} must be readable", semantics.name());
	}
}

#[test]
// PADDING IS PART OF THE IMAGE'S BYTES AND NOT PART OF THE IMAGE, and the three byte spans are three
// different numbers. Confusing them is how a buffer is accepted that cannot hold the image - or how
// one that is exactly big enough is refused.
fn the_allocation_rules_distinguish_the_three_byte_spans() {
	use crate::image::{allocation, spans};
	assert_eq!(allocation::EXPORTED_PADDING, "zero", "two exports of one image must be the same bytes");
	assert!(allocation::ROW_ALIGNMENT_BYTES.is_power_of_two());
	// THE THREE SPANS ARE THREE DIFFERENT QUESTIONS, and an implementation that stores one number
	// answers all three with it and is wrong about two.
	assert!(spans::MINIMUM_VISIBLE_BYTES.contains("height - 1"), "the last row's padding is not part of what a CPU view needs");
	assert!(spans::BACKEND_ACCESS_SPAN.contains("padding"), "a scanout engine fetching whole rows DOES touch the final row's padding, which is why this is a separate span");
	assert!(spans::ALLOCATION_LEN.contains("at least"), "the allocation is a fact about memory and is not a second answer to how big the image is");
	for (left, right) in [
		(spans::MINIMUM_VISIBLE_BYTES, spans::BACKEND_ACCESS_SPAN),
		(spans::BACKEND_ACCESS_SPAN, spans::ALLOCATION_LEN),
		(spans::MINIMUM_VISIBLE_BYTES, spans::ALLOCATION_LEN),
	] {
		assert_ne!(left, right, "two of the three spans are stated as the same thing, which is how one number comes to answer all three");
	}
	assert_ne!(crate::image::rows::MINIMUM_ROW_BYTES, crate::image::rows::PITCH, "the row a format needs and the row a layout gives are different numbers");
}

#[test]
// WITHOUT METADATA A PQ IMAGE IS UNTONE-MAPPABLE. PQ is absolute, so a display dimmer than the
// content has to know what the content's peak actually was - and assuming the format's ten thousand
// tone-maps every image as though it were the brightest ever made.
fn an_absolute_transfer_function_states_what_metadata_it_needs() {
	use crate::image::{COLOR_SPACES, Transfer, hdr_metadata, reference};
	// EXACTLY ONE COLOUR SPACE USES THE ABSOLUTE TRANSFER FUNCTION, and the profile says which. A
	// second one would mean two spaces whose 1.0 is a quantity of light, and a pipeline would have to
	// ask which quantity.
	let absolute: std::vec::Vec<&str> = COLOR_SPACES.iter().filter(|space| space.transfer == Transfer::Pq).map(|space| space.name).collect();
	assert_eq!(absolute, std::vec!["rec2020-pq"]);
	assert!(hdr_metadata::PQ.contains("refusal"), "absent PQ metadata is a refusal to tone-map rather than an assumed peak");
	assert!(hdr_metadata::HLG.contains("nominal peak"), "HLG is relative and needs the peak its signal is referred to");
	assert!(!hdr_metadata::SDR.is_empty());
	assert_eq!(reference::DIFFUSE_WHITE_NITS, 203.0, "a relative 1.0 in an SDR space is this much light, which is what makes SDR and HDR composable");
}

#[test]
// A WORD THAT MEANS FIVE THINGS IS A WORD EVERY INTERFACE EVENTUALLY GETS WRONG, and the way it gets
// wrong is that somebody passes one of the five where another was meant - which type-checks whenever
// both are a pointer and a length. Each of the five has its own name, and no name is reused.
fn the_five_things_framebuffer_meant_each_have_their_own_name() {
	use crate::layers::{LAYERS, layer};
	assert_eq!(LAYERS.len(), 5, "there were five meanings and there are five names");
	for required in ["FRAMEBUFFER", "SCANOUT", "IMAGE", "SURFACE", "RENDER TARGET"] {
		let entry = layer(required).unwrap_or_else(|| panic!("{required} is one of the five and is not named"));
		assert!(!entry.what.is_empty(), "{required} has no definition");
		assert!(!entry.owner.is_empty(), "{required} has no owner, which is the question 'whose bug is it' reduced");
	}
	// No name twice, and no two definitions the same - either would put the ambiguity back.
	for (index, entry) in LAYERS.iter().enumerate() {
		for other in &LAYERS[index + 1..] {
			assert_ne!(entry.name, other.name, "{} is named twice", entry.name);
			assert_ne!(entry.what, other.what, "{} and {} are defined as the same thing", entry.name, other.name);
		}
	}
	// AND `FRAMEBUFFER` IS THE NARROW ONE. It was the word that meant everything; it now means the
	// legacy boot surface and nothing else, which is the whole point of the exercise.
	let framebuffer = layer("FRAMEBUFFER").expect("named above");
	assert!(framebuffer.what.contains("LEGACY"), "framebuffer must be narrowed to the boot surface rather than left broad");
}

#[test]
// NOTHING BELOW DEPENDS ON ANYTHING ABOVE IT, and the edges are an enumeration rather than a diagram
// because a diagram is checked by whoever reads it. A cycle here is the failure that turns a stack
// into a knot, and it is the one a picture never shows.
fn the_ownership_graph_is_acyclic_and_every_edge_says_what_crosses_it() {
	use crate::layers::OWNERSHIP;
	for edge in OWNERSHIP {
		assert!(!edge.carries.is_empty(), "{} -> {} states no payload, which is a dependency nobody has thought about", edge.above, edge.below);
		assert_ne!(edge.above, edge.below, "{} depends on itself", edge.above);
	}
	// No edge and its reverse, which is the smallest cycle and the one a refactor introduces.
	for edge in OWNERSHIP {
		assert!(!OWNERSHIP.iter().any(|other| other.above == edge.below && other.below == edge.above), "{} and {} depend on each other", edge.above, edge.below);
	}
	// And no cycle at all, by walking every edge to a fixed depth: a stack ten layers deep cannot
	// have a path longer than its edge count without revisiting something.
	for start in OWNERSHIP {
		let mut here = start.below;
		for _ in 0..OWNERSHIP.len() {
			match OWNERSHIP.iter().find(|edge| edge.above == here) {
				Some(next) => {
					assert_ne!(next.below, start.above, "following {} downward arrives back at it", start.above);
					here = next.below;
				}
				None => break,
			}
		}
	}
	// The stack reaches the device, which is what makes it a stack rather than a set of layers.
	assert!(OWNERSHIP.iter().any(|edge| edge.below == "display driver"), "the bottom of the stack is the device transport");
}

#[test]
// A PROVISIONAL `gl*` OR `vk*` API INTRODUCED TO DRAW A DEMO IS THE API THE TREE THEN HAS. Naming the
// routes before any of them exists is what makes "properly later" a plan rather than a hope - and
// each route names what is REFUSED, because the absence is the decision.
fn every_integration_route_names_what_it_refuses() {
	use crate::layers::ROUTES;
	for route in ROUTES {
		assert!(!route.through.is_empty(), "{} has no route", route.api);
		assert!(route.refused.len() > 20, "{} does not say what it refuses, and the absence is the decision", route.api);
	}
	// THE TWO REFUSALS THE PLAN NAMES BY NAME: no common GL/Vulkan command language invented here,
	// and no virtqueue descriptors reaching applications.
	let refusals: std::string::String = ROUTES.iter().map(|route| route.refused).collect::<std::vec::Vec<&str>>().join(" ");
	assert!(refusals.contains("common GL/Vulkan command language"), "a third API neither upstream tests is refused by name");
	assert!(refusals.contains("virtqueue descriptors"), "device descriptors reaching applications is refused by name");
}

#[test]
// THE POINT OF NAMING A BOUNDARY IS THAT THE LAYER BELOW MAY THEN ASSUME. A stack where every layer
// re-checks is one where the check that matters is the one nobody wrote because everybody assumed
// somebody else had; a stack where none does is the other failure.
fn every_untrusted_input_has_a_boundary_that_validates_it() {
	use crate::layers::BOUNDARIES;
	for boundary in BOUNDARIES {
		assert!(!boundary.untrusted.is_empty(), "{} names no untrusted input", boundary.at);
		assert!(boundary.checks.len() > 20, "{} does not say what it checks", boundary.at);
	}
	// THE THREE KINDS THE PLAN NAMES: a length, an opcode or command, and a resource reference.
	let checks: std::string::String = BOUNDARIES.iter().map(|boundary| boundary.checks).collect::<std::vec::Vec<&str>>().join(" ");
	assert!(checks.contains("length"), "every untrusted LENGTH is validated somewhere named");
	assert!(checks.contains("resource index") || checks.contains("resource table"), "every untrusted RESOURCE REFERENCE is validated somewhere named");
	// AND THE DEVICE IS NOT TRUSTED EITHER, which is the boundary a driver is most likely to skip:
	// a reply is input, and it arrives from something that may be emulated, buggy or hostile.
	assert!(BOUNDARIES.iter().any(|boundary| boundary.at.contains("display driver")), "what a device writes back is untrusted input too");
}

#[test]
// A PROFILE WITH NO MINIMA PROMISES NOTHING. "Supports large images" is satisfied by an
// implementation that refuses at 513 pixels, so an application written against the profile discovers
// the real limit by being refused - in front of a user.
fn the_guaranteed_minima_are_numbers_an_application_may_assume() {
	use crate::image::{FORMATS, minima};
	// A COMPILE-TIME CHECK, because it is a relation between two constants: a runtime assertion over
	// two literals is a test that cannot fail at a moment when it could still matter.
	const { assert!(minima::IMAGE_EXTENT >= 8192, "a guaranteed extent below eight thousand is a promise no application can build on") };
	assert_eq!(minima::PLANES, 3, "the widest layout the profile carries is three-plane I420");
	// THE PITCH MINIMUM MUST ACTUALLY REACH THE WIDEST IMAGE AT THE WIDEST FORMAT, or the two minima
	// contradict each other and an application obeying both still gets refused.
	let widest = FORMATS.iter().map(|format| format.bytes_per_pixel as u32).max().unwrap_or(0);
	assert!(minima::PITCH_BYTES >= minima::IMAGE_EXTENT * widest, "the pitch minimum does not reach the extent minimum at {widest} bytes per pixel");
}

#[test]
// "PASSES CONFORMANCE" MUST HAVE A BOUNDARY RATHER THAN A JUDGEMENT. An exact comparison is the wrong
// test for a transcendental, and a judgement is not a test at all.
fn every_compared_result_has_a_stated_tolerance() {
	use crate::image::tolerance;
	for (name, value) in [
		("transfer round trip", tolerance::TRANSFER_ROUND_TRIP),
		("primary round trip", tolerance::PRIMARY_ROUND_TRIP),
		("tone map", tolerance::TONE_MAP),
		("yuv code values", tolerance::YUV_CODE_VALUES),
		("filtered sample", tolerance::FILTERED_SAMPLE),
	] {
		assert!(value > 0.0, "{name} has a tolerance of zero, which is an exact comparison of an approximation");
	}
	// THE DITHER IS THE ONE EXACT COMPARISON, because its matrix and its phase are both stated: two
	// implementations that disagree by anything disagree about the rule rather than about arithmetic.
	assert_eq!(tolerance::DITHER, 0.0);
	// And the transfer round trip is the tightest, because both directions are stated exactly while
	// a primary conversion goes through an adaptation matrix.
	const { assert!(tolerance::TRANSFER_ROUND_TRIP < tolerance::PRIMARY_ROUND_TRIP) };
}

#[test]
// THE UNINITIALISED CASE IS A DISCLOSURE. A presentable image whose bytes were never written shows
// whatever the allocator handed over, which in a system with a shared page pool is somebody else's
// pixels - and the bug reads as a flicker rather than as a leak, so it is not reported.
fn an_unwritten_surface_is_refused_rather_than_presented() {
	use crate::image::initialisation;
	assert!(initialisation::BEFORE_FIRST_PRESENT.contains("refused"), "presenting an unwritten surface must be a refusal and not advice");
	assert!(initialisation::NEW_IMAGE.contains("undefined"), "a consumer that assumed zero would be right on most allocators and wrong on the one that matters");
	assert!(initialisation::PADDING.contains("zeroed on export"), "two exports of one image must be the same bytes");
}

#[test]
// THE FACTORS ARE THE OPERATOR, which is what makes thirteen of them checkable against one another
// rather than thirteen separate paragraphs. Every pair must be distinct: two operators with the same
// factors are the same operator under two names, and a consumer picking between them is choosing
// nothing.
fn every_compositing_operator_is_a_distinct_pair_of_factors() {
	use crate::compositing::{COMPOSITING_EQUATION, OPERATORS};
	assert!(COMPOSITING_EQUATION.contains("premultiplied"), "the equation is defined on premultiplied colour and says so");
	for (index, operator) in OPERATORS.iter().enumerate() {
		for other in &OPERATORS[index + 1..] {
			assert_ne!(operator.name, other.name, "{} appears twice", operator.name);
			assert!(!(operator.source_factor == other.source_factor && operator.backdrop_factor == other.backdrop_factor), "{} and {} are the same operator under two names", operator.name, other.name);
		}
	}
	// THE FOUR THAT DEFINE THE REST: `Clear` keeps nothing, `Src` keeps only the source, `Dst` only
	// the backdrop, and `SrcOver` is what every ordinary draw is.
	let factors = |name: &str| OPERATORS.iter().find(|operator| operator.name == name).map(|operator| (operator.source_factor, operator.backdrop_factor));
	assert_eq!(factors("Clear"), Some(("0", "0")));
	assert_eq!(factors("Src"), Some(("1", "0")));
	assert_eq!(factors("Dst"), Some(("0", "1")));
	assert_eq!(factors("SrcOver"), Some(("1", "1 - as")));
	// AND `Plus` IS THE ONE THAT IS NOT PORTER-DUFF: both factors one, which no coverage pair gives.
	assert_eq!(factors("Plus"), Some(("1", "1")));
}

#[test]
// "SUPPORTS THE STANDARD BLEND MODES" IS NOT A CONTRACT. Two implementations satisfy that sentence
// and disagree on `ColorBurn` at zero, on `SoftLight` below a quarter, and on all four non-separable
// modes - so every equation is here, and the ones with a division state their endpoints.
fn every_blend_mode_states_its_equation_including_its_endpoints() {
	use crate::compositing::{BLENDS, NON_SEPARABLE_BLENDS};
	assert_eq!(BLENDS.len(), 12, "the twelve separable modes of the compositing specification");
	assert_eq!(NON_SEPARABLE_BLENDS.len(), 4);
	for blend in BLENDS {
		assert!(!blend.equation.is_empty(), "{} has no equation", blend.name);
	}
	let equation = |name: &str| BLENDS.iter().find(|blend| blend.name == name).map(|blend| blend.equation).unwrap_or("");
	// THE TWO WITH A DIVISION must state what happens at the values that make the denominator zero,
	// which is where two implementations silently differ.
	assert!(equation("ColorDodge").contains("Cs == 1"), "ColorDodge must state its endpoint at Cs == 1");
	assert!(equation("ColorBurn").contains("Cs == 0"), "ColorBurn must state its endpoint at Cs == 0");
	// SOFTLIGHT'S THRESHOLD IS ON THE BACKDROP, which is the part that is got wrong.
	assert!(equation("SoftLight").contains("D(Cb)") && equation("SoftLight").contains("Cb <= 0.25"), "SoftLight's piecewise function is of the BACKDROP");
	// And the trivial one is trivial: a blend mode list whose `Normal` does something is a list with
	// a bug in the case every draw takes.
	assert_eq!(equation("Normal"), "Cs");
}

#[test]
// THE FOUR NON-SEPARABLE MODES NEED A COLOUR MODEL THE MODE'S NAME DOES NOT IMPLY - a luminance
// model, a saturation model and a GAMUT CLIP. The clip is the step every implementation that omits it
// gets wrong on saturated colours, and it preserves luminance rather than clamping each channel.
fn the_non_separable_modes_carry_their_whole_colour_model() {
	use crate::compositing::model;
	let (red, green, blue) = model::LUMINANCE_COEFFICIENTS;
	// The compositing specification's own fixed triple, which sums to one and is NOT the colour
	// space's luminance - a mode defined on the numbers rather than on the light.
	assert!((red + green + blue - 1.0).abs() < 1e-9, "the luminance coefficients must sum to one");
	assert_eq!((red, green, blue), (0.30, 0.59, 0.11));
	assert!(model::CLIP_COLOR.contains("Luminance is preserved"), "the clip preserves luminance and reduces chroma, rather than clamping each channel");
	assert!(model::SET_SATURATION.contains("Cmax == Cmin"), "a colour with no chroma has no direction to spread in, and the case is stated");
}

#[test]
// ONE TOLERANCE, SHARED. If fills, strokes, boolean operations, hit tests and bounds each flattened to
// their own, a point would be inside a path for a hit test and outside it for the fill that drew it.
fn the_geometry_numbers_are_numbers_and_the_singular_case_is_answered() {
	use crate::geometry::{AT_MAX_DEPTH, COINCIDENCE_EPSILON_PIXELS, FLATTENING_TOLERANCE_PIXELS, HORIZON_RULE, MAX_SUBDIVISION_DEPTH, PROJECTIVE_W_EPSILON};
	const { assert!(FLATTENING_TOLERANCE_PIXELS > 0.0 && FLATTENING_TOLERANCE_PIXELS <= 0.5, "a tolerance above half a pixel is visible as facets") };
	const { assert!(MAX_SUBDIVISION_DEPTH >= 8) };
	// REACHING THE LIMIT HAS A STATED OUTCOME rather than an implementation's guess, and it is not a
	// refusal: a legal drawing must not fail for a reason nobody can act on.
	assert!(AT_MAX_DEPTH.contains("not refused"), "the depth limit emits a line rather than refusing the primitive");
	// THE COINCIDENCE EPSILON IS SEPARATE AND SMALLER. Using the flattening tolerance for both merges
	// vertices a quarter of a pixel apart, which collapses thin features that were meant to be there.
	const { assert!(COINCIDENCE_EPSILON_PIXELS < FLATTENING_TOLERANCE_PIXELS) };
	// AND THE HORIZON IS CLIPPED BEFORE THE DIVIDE. Dividing first produces a vertex at ten million
	// pixels and a rasteriser that spends a second on one triangle.
	const { assert!(PROJECTIVE_W_EPSILON > 0.0) };
	assert!(HORIZON_RULE.contains("before dividing"), "the clip happens in homogeneous space, before the perspective divide");
}

#[test]
// TWO BACKENDS PRODUCE DIFFERENT UNIONS IF ANY OF THE EIGHT QUESTIONS IS LEFT OPEN, and the difference
// is not subtle: it is a hole that is there in one and not the other.
fn every_boolean_question_the_plan_asks_has_an_answer() {
	use crate::geometry::BOOLEAN_RULES;
	for required in ["self-intersections", "OPEN subpath", "different fill rules", "preserves curves", "tolerance", "ordering and winding", "degenerate", "determinism"] {
		assert!(BOOLEAN_RULES.iter().any(|rule| rule.question.contains(required)), "the question about {required} has no answer");
	}
	for rule in BOOLEAN_RULES {
		assert!(rule.answer.len() > 40, "'{}' is answered too briefly to be an answer", rule.question);
	}
}

#[test]
// A PREPARED LIST IS A CACHE, and a cache whose validity conditions are not enumerated eventually
// replays a drawing that is not the one recorded. Every dependency the plan names is here, each with
// the reason a change to it invalidates - so a test can change one ALONE and see the refusal.
fn every_prepared_list_dependency_is_named_with_its_reason() {
	use crate::contracts::{CONTENT_REFRESH, ON_MISMATCH, PREPARED_DEPENDENCIES, builder, filter};
	for required in ["profile version", "backend identity", "target format", "colour space", "extent and scale", "resource identity", "glyph-cache", "filter parameters"] {
		assert!(PREPARED_DEPENDENCIES.iter().any(|dependency| dependency.name.contains(required)), "{required} is not among the dependencies a prepared list is bound to");
	}
	for dependency in PREPARED_DEPENDENCIES {
		assert!(dependency.why.len() > 40, "{} states no reason", dependency.name);
	}
	// A MISMATCH IS TYPED AND NEVER A SILENT RE-PREPARE: a caller getting a full preparation sixty
	// times a second has a performance bug it cannot see.
	assert!(ON_MISMATCH.contains("typed"), "a mismatch is a typed requirement rather than a silent re-prepare");
	// CONTENT IS NOT STRUCTURE.
	assert!(CONTENT_REFRESH.contains("not re-flattened"), "a new video frame must not re-flatten unchanged paths");
	assert_eq!(builder::WITHIN_RESERVATION, "no allocation");
	assert!(builder::BEYOND_RESERVATION.contains("before any replay"), "a list that half-drew and then refused has already put pixels on the screen");
	// AND THE FILTER GRAPH IS A DAG WITH A BOUNDS MAP, without which a blur over a small dirty region
	// costs a full-screen blur.
	assert!(filter::SHAPE.contains("acyclic"));
	assert!(filter::BOUNDS_MAP.contains("output rectangle"));
	assert!(filter::SCRATCH.contains("refused up front"), "a frame that cannot fit says so before it starts drawing");
}

#[test]
// A STATE MACHINE WITH A STATE NOTHING LEAVES, OR AN EDGE TO A STATE THAT DOES NOT EXIST, IS NOT A
// MACHINE. The present queue's is the part of a window system an implementation gets wrong after
// shipping - a resized window that leaks an image per resize, an acquired image with no way back
// that is not a present - so the closure is checked here rather than discovered there.
fn the_present_queue_state_machine_is_closed_and_has_its_awkward_edges() {
	use wsi::{IMAGE_STATES, TRANSITIONS};
	let names: std::vec::Vec<&str> = IMAGE_STATES.iter().map(|state| state.name).collect();
	assert_eq!(names.len(), 4);
	for transition in TRANSITIONS {
		assert!(names.contains(&transition.from), "a transition leaves a state that does not exist: {}", transition.from);
		assert!(names.contains(&transition.to), "a transition arrives at a state that does not exist: {}", transition.to);
		assert!(!transition.why.is_empty());
	}
	// EVERY STATE IS ENTERED AND LEFT. A state nothing enters is dead; one nothing leaves is a leak.
	for state in IMAGE_STATES {
		assert!(TRANSITIONS.iter().any(|transition| transition.to == state.name), "nothing enters {}", state.name);
		assert!(TRANSITIONS.iter().any(|transition| transition.from == state.name), "nothing leaves {}", state.name);
	}
	// THE EDGE AN IMPLEMENTATION FORGETS: acquire, then decide not to draw.
	assert!(TRANSITIONS.iter().any(|transition| transition.from == "Acquired" && transition.event == "abandon" && transition.to == "Available"), "a client that acquired and did not draw must have a way back that is not a present");
	// AND THE THREE RESIZE TRANSITIONS, one per state a generation change can catch an image in.
	for from in ["Available", "Acquired", "PendingPresent"] {
		assert!(TRANSITIONS.iter().any(|transition| transition.from == from && transition.event == "generation changed" && transition.to == "Stale"), "a generation change must have an answer for an image that is {from}");
	}
}

#[test]
// EVERY NUMBER IN A WINDOW-SYSTEM CONTRACT IS ONE TWO SIDES ROUND DIFFERENTLY IF IT IS NOT STATED,
// and every list is one an implementation will otherwise extend quietly. These are the ones a
// conformance suite is measured against.
fn the_window_system_numbers_and_lists_are_frozen() {
	use wsi::{COMPLETION_FACTS, COMPLETION_PAIRS, CONFIGURATION, DAMAGE_RULES, EVENTS, MAX_DAMAGE_RECTS, MAX_IMAGES, MIN_IMAGES, PRESENT_MODES, PRESENT_OUTCOMES, RESERVED_PRESENT_MODES, SCALE_REPRESENTATION, TIMESTAMP_EVIDENCE};
	// THE SCALE IS A RATIO AND NEVER A FLOAT on the wire, which is the whole of the one-pixel seam.
	assert!(SCALE_REPRESENTATION.contains("numerator") && SCALE_REPRESENTATION.contains("denominator") && SCALE_REPRESENTATION.contains("never a float"));

	// NEGOTIATED STAYS NEGOTIATED, but a service free to answer one turns the double-or-triple
	// buffering gate into a test of nothing.
	assert!(MIN_IMAGES >= 2 && MAX_IMAGES >= MIN_IMAGES, "{MIN_IMAGES}..={MAX_IMAGES}");
	assert_eq!(MAX_DAMAGE_RECTS, 16, "a small cap is not an ABI");

	// NO NAME APPEARS TWICE in any of the lists, which is what makes each of them an enumeration.
	let unique = |names: std::vec::Vec<&str>| {
		let mut sorted = names.clone();
		sorted.sort_unstable();
		sorted.dedup();
		assert_eq!(sorted.len(), names.len(), "a name appears twice: {names:?}");
	};
	unique(CONFIGURATION.iter().map(|field| field.name).collect());
	unique(EVENTS.iter().map(|event| event.name).collect());
	unique(PRESENT_OUTCOMES.iter().map(|outcome| outcome.name).collect());
	unique(DAMAGE_RULES.iter().map(|rule| rule.question).collect());
	unique(COMPLETION_PAIRS.iter().map(|pair| pair.name).collect());

	// WHAT INVALIDATES AN IMAGE GENERATION IS EXACTLY WHAT THE LIFECYCLE SAYS: extent, scale,
	// orientation and presentable format. A field that quietly joined them would throw away every
	// image on a change that does not need to.
	let invalidating: std::vec::Vec<&str> = CONFIGURATION.iter().filter(|field| field.invalidates_images).map(|field| field.name).collect();
	assert_eq!(invalidating, std::vec!["logical extent", "physical extent", "scale", "transform", "presentable pixel format"]);

	// `Fifo` IS DEFINED AND THE OTHER TWO HAVE A PLACE rather than a meaning.
	assert_eq!(PRESENT_MODES, &["Fifo"]);
	assert_eq!(RESERVED_PRESENT_MODES, &["Mailbox", "Immediate"]);

	// A PRESENT HAS FOUR FATES, and a bare completion cannot express them: "every present is
	// displayed" cannot hold for a background client that must also not be blocked forever.
	assert_eq!(PRESENT_OUTCOMES.len(), 4);
	assert!(PRESENT_OUTCOMES.iter().any(|outcome| outcome.name == "DiscardedOccluded"));

	// AND "DISPLAYED" IS THREE FACTS, of which the current backend can observe two. This pins the
	// honest answer: when a vblank capability arrives, the third becomes observable in a DIFF.
	assert_eq!(COMPLETION_FACTS.len(), 3);
	let physical = COMPLETION_FACTS.iter().find(|fact| fact.name == "PhysicallyDisplayed").expect("the third fact");
	assert!(!physical.observable_today, "reporting a command acknowledgement as physical presentation is what this flag exists to prevent");
	assert_eq!(TIMESTAMP_EVIDENCE, &["Unavailable", "Estimated(t)", "Measured(t)"]);

	// THE COMPLETION PAIRS SPLIT AUTHORITY, which is why they are two pairs and not one duplex
	// channel: the client may signal readiness and may not signal completion.
	let producer = COMPLETION_PAIRS.iter().find(|pair| pair.name == "PRODUCER_READY").expect("the producer pair");
	let done = COMPLETION_PAIRS.iter().find(|pair| pair.name == "PRESENT_DONE").expect("the completion pair");
	assert_eq!(producer.client_rights, "SEND");
	assert_eq!(done.service_rights, "SEND");
	assert!(done.client_rights.contains("RECEIVE") && !done.client_rights.contains("SEND"));
}

// ---------------------------------------------------------------------------------------------
// `Render3D Profile 1`: the numbers and the rules.
//
// A SPECIFICATION REGISTRY IS ONLY WORTH WHAT ITS SELF-CONSISTENCY IS. These do not check that the
// answers are the right ones - nothing can, they are decisions - but they check that the registry
// cannot answer one question twice, cannot declare a capability that contradicts another, and cannot
// lose an entry the feature list requires it to have.
// ---------------------------------------------------------------------------------------------

#[test]
fn no_question_in_the_render3d_registry_has_two_answers() {
	use crate::render3d_spec::{CLIP_COORD_Q, DEPTH_RULES, HAZARD_RULES, MSAA_RULES, PROVOKING_VERTEX, SAMPLER_RULES, SUBMISSION_RULES, VERTEX_NORMALISATION};
	// A registry with the same question twice is a registry whose reader gets whichever answer the
	// iteration reached first, and the two need not agree.
	for (name, rules) in [
		("clip-coord-q", CLIP_COORD_Q),
		("provoking-vertex", PROVOKING_VERTEX),
		("vertex-normalisation", VERTEX_NORMALISATION),
		("msaa", MSAA_RULES),
		("depth", DEPTH_RULES),
		("sampler", SAMPLER_RULES),
		("hazard", HAZARD_RULES),
		("submission", SUBMISSION_RULES),
	] {
		for (index, rule) in rules.iter().enumerate() {
			assert!(!rule.question.is_empty(), "{name} has an empty question");
			assert!(!rule.answer.is_empty(), "{name}: `{}` has no answer", rule.question);
			assert!(rules[..index].iter().all(|earlier| earlier.question != rule.question), "{name} answers `{}` twice", rule.question);
		}
	}
}

#[test]
fn every_multisample_position_is_inside_its_pixel_and_distinct() {
	use crate::render3d_spec::{MSAA_2X, MSAA_4X};
	for (count, samples) in [(2usize, MSAA_2X), (4usize, MSAA_4X)] {
		assert_eq!(samples.len(), count, "the {count}x table must have {count} positions");
		for (index, sample) in samples.iter().enumerate() {
			// THE INDEX IS THE SAMPLE MASK'S BIT NUMBER, so a table whose indices are not 0..n is a
			// table where a mask names the wrong sample.
			assert_eq!(sample.index as usize, index);
			assert!(sample.x > 0.0 && sample.x < 1.0, "sample {index} of {count}x is outside its pixel in x");
			assert!(sample.y > 0.0 && sample.y < 1.0, "sample {index} of {count}x is outside its pixel in y");
			// Two samples at one position is a coverage level that can never be produced.
			for earlier in &samples[..index] {
				assert!(earlier.x != sample.x || earlier.y != sample.y, "samples {} and {index} of {count}x are at the same point", earlier.index);
			}
		}
	}
	// AND THE 4x GRID IS ROTATED, not axis-aligned: no two samples share a row or a column, which is
	// the whole reason a rotated grid gives a near-horizontal edge more than two coverage levels.
	for (index, sample) in MSAA_4X.iter().enumerate() {
		for earlier in &MSAA_4X[..index] {
			assert!(earlier.x != sample.x, "two 4x samples share a column, which is an axis-aligned grid");
			assert!(earlier.y != sample.y, "two 4x samples share a row, which is an axis-aligned grid");
		}
	}
}

#[test]
fn a_format_capability_row_cannot_contradict_itself() {
	use crate::render3d_spec::COLOUR_FORMATS;
	for format in COLOUR_FORMATS {
		// Filtering is a way of sampling, blending is a way of writing an attachment, and an
		// attachment is a thing that is rendered to. A row that claimed the second without the first
		// would be a row no backend could implement as written.
		assert!(!format.filterable || format.sampled, "{}: filterable without sampled", format.name);
		assert!(!format.blendable || format.renderable, "{}: blendable without renderable", format.name);
		assert!(!format.attachment || format.renderable, "{}: an attachment that is not renderable", format.name);
		assert!(!format.msaa || format.renderable, "{}: multisampled without being renderable", format.name);
		assert!(format.bits_per_texel > 0 && format.bits_per_texel % 8 == 0, "{}: a texel that is not a whole number of bytes", format.name);
		// THE FULL NAME IS THE POINT OF THE COLUMN. `RGBA8` says nothing about whether it is
		// normalised or sRGB-encoded, and the table exists to stop that being a guess.
		assert!(format.full_name.len() > format.name.len(), "{}: the full name is not fuller than the short one", format.name);
	}
	// The profile's own feature list names seven colour formats, and this table is the same seven.
	assert_eq!(COLOUR_FORMATS.len(), 7);
}

#[test]
fn the_clip_plane_order_starts_at_the_horizon_and_the_epsilon_is_the_two_d_one() {
	use crate::geometry::PROJECTIVE_W_EPSILON;
	use crate::render3d_spec::{CLIP_PLANE_ORDER, CLIP_W_EPSILON};
	// The w plane is FIRST or a vertex just behind the eye is divided before it is clipped, which is
	// the coordinate at ten million pixels this order exists to prevent.
	assert!(CLIP_PLANE_ORDER[0].starts_with("w ="), "the horizon plane must be clipped first");
	assert_eq!(CLIP_PLANE_ORDER.len(), 7, "six volume planes and the horizon");
	// ONE EPSILON FOR ONE QUESTION. A projective singularity does not become a different singularity
	// because the geometry has three dimensions, and a reader who learned the 2D value has learned
	// this one.
	assert_eq!(CLIP_W_EPSILON, PROJECTIVE_W_EPSILON);
}

#[test]
fn every_topology_in_the_feature_list_has_a_provoking_vertex_rule() {
	use crate::render3d_spec::PROVOKING_VERTEX;
	// A `flat` attribute without a rule for a topology is a colour that depends on the backend, and
	// the feature list has six topologies plus primitive restart.
	for topology in ["triangle list", "triangle strip", "triangle fan", "line list", "line strip", "point list", "after primitive restart", "after fan triangulation"] {
		assert!(PROVOKING_VERTEX.iter().any(|rule| rule.question == topology), "no provoking-vertex rule for `{topology}`");
	}
}

#[test]
fn every_qualifier_says_what_a_clip_does_to_it() {
	use crate::render3d_spec::QUALIFIERS;
	// The five the shader model has. A qualifier whose clip behaviour is unstated is the kink at a
	// clip edge that this whole section exists to remove.
	assert_eq!(QUALIFIERS.len(), 5);
	for name in ["smooth", "noperspective", "flat", "centroid", "sample"] {
		let qualifier = QUALIFIERS.iter().find(|qualifier| qualifier.name == name).unwrap_or_else(|| panic!("no rule for `{name}`"));
		assert!(!qualifier.at_a_clip_intersection.is_empty());
		assert!(!qualifier.why.is_empty(), "`{name}` states a rule and not a reason");
	}
	// AND THE THREE BASE QUALIFIERS DIFFER FROM EACH OTHER AT A CLIP, which is the property the
	// section is about: homogeneous for `smooth`, projected for `noperspective`, unchanged for
	// `flat`. A registry where two of them said the same thing would have lost the distinction.
	let smooth = QUALIFIERS.iter().find(|qualifier| qualifier.name == "smooth").expect("smooth");
	let noperspective = QUALIFIERS.iter().find(|qualifier| qualifier.name == "noperspective").expect("noperspective");
	let flat = QUALIFIERS.iter().find(|qualifier| qualifier.name == "flat").expect("flat");
	assert_ne!(smooth.at_a_clip_intersection, noperspective.at_a_clip_intersection);
	assert_ne!(smooth.at_a_clip_intersection, flat.at_a_clip_intersection);
}

#[test]
fn every_minimum_limit_is_a_floor_somebody_could_fail() {
	use crate::render3d_spec::RENDER3D_PROFILE_1_MIN_LIMITS;
	for limit in RENDER3D_PROFILE_1_MIN_LIMITS {
		// A minimum of zero is not a floor: it is the absence of one written down.
		assert!(limit.minimum > 0, "{} has no floor", limit.name);
		assert!(!limit.why.is_empty(), "{} states a number and not a reason", limit.name);
	}
	// The ones an implementation is most tempted to under-provide, pinned by value so a later edit
	// that lowered them is a visible change rather than a quiet one.
	let by_name = |name: &str| RENDER3D_PROFILE_1_MIN_LIMITS.iter().find(|limit| limit.name == name).unwrap_or_else(|| panic!("no minimum for `{name}`")).minimum;
	assert_eq!(by_name("max_texture_extent_2d"), 4096);
	assert_eq!(by_name("max_colour_attachments"), 4);
	assert_eq!(by_name("max_draws_per_pass"), 65536);
	assert_eq!(by_name("max_shader_instructions"), 4096);
}

#[test]
fn the_depth32f_question_is_answered_rather_than_described() {
	use crate::render3d_spec::{DEPTH_FORMATS, DEPTH32F_ANSWER};
	// THE PLAN NAMES THIS ONE BY NAME as the question the freeze has to settle, and an answer that
	// listed the alternatives would be the open question with more words.
	assert!(DEPTH32F_ANSWER.contains("NOT QUANTISED"), "the answer must say which of the two behaviours this profile has");
	assert!(DEPTH32F_ANSWER.contains("LessOrEqual"), "the answer must state the consequence a caller has to act on");
	assert_eq!(DEPTH_FORMATS.len(), 5);
	let depth32f = DEPTH_FORMATS.iter().find(|format| format.name == "Depth32F").expect("Depth32F");
	assert!(depth32f.comparison.contains("STORED FLOATS"));
}

// ---------------------------------------------------------------------------------------------
// `Shader IR 1`: what a shader means.
// ---------------------------------------------------------------------------------------------

#[test]
fn no_question_in_the_shader_ir_registry_has_two_answers() {
	use crate::shader_ir::{CONVERSION_RULES, ENCODING_RULES, MATRIX_LAYOUT, NUMERIC_RULES, STAGE_RULES, STRICT_F32_RULES, UNIFORM_LAYOUT};
	for (name, rules) in [
		("numeric", NUMERIC_RULES),
		("conversion", CONVERSION_RULES),
		("uniform-layout", UNIFORM_LAYOUT),
		("matrix-layout", MATRIX_LAYOUT),
		("stage", STAGE_RULES),
		("strict-f32", STRICT_F32_RULES),
		("encoding", ENCODING_RULES),
	] {
		for (index, rule) in rules.iter().enumerate() {
			assert!(!rule.answer.is_empty(), "{name}: `{}` has no answer", rule.question);
			assert!(rules[..index].iter().all(|earlier| earlier.question != rule.question), "{name} answers `{}` twice", rule.question);
		}
	}
}

#[test]
fn every_undefined_behaviour_a_shading_language_usually_has_is_defined_here() {
	use crate::shader_ir::NUMERIC_RULES;
	// THE LIST IS THE POINT. Each of these is undefined in at least one shipping shading language,
	// and each has produced a bug that behaves differently on two machines. A registry that lost one
	// of them would be a specification with a hole exactly where implementations differ.
	for question in [
		"signed integer overflow",
		"integer division by zero",
		"integer modulo by zero",
		"float division by zero",
		"NaN comparison",
		"signed zero",
		"subnormals",
		"uninitialised values",
		"out-of-bounds array read",
		"out-of-bounds array write",
	] {
		let rule = NUMERIC_RULES.iter().find(|rule| rule.question == question).unwrap_or_else(|| panic!("`{question}` is not answered"));
		// "IT IS UNDEFINED" AS AN ANSWER WOULD BE THE QUESTION RESTATED. The word itself is allowed
		// and is used deliberately - several of these answers say what they are NOT, and "not
		// undefined" is the most useful thing to say about a behaviour every other system leaves
		// open. What is refused is the answer that declares it.
		assert!(!rule.answer.to_ascii_lowercase().contains("is undefined"), "`{question}` is answered with `undefined`");
	}
}

#[test]
fn an_out_of_bounds_read_answers_zero_rather_than_a_plausible_neighbour() {
	use crate::shader_ir::NUMERIC_RULES;
	let read = NUMERIC_RULES.iter().find(|rule| rule.question == "out-of-bounds array read").expect("the read rule");
	assert!(read.answer.contains("ZERO"));
	// Clamping to the last element is the alternative, and it is the one that hides the bug: the
	// shader reads a real value from the wrong place and the picture looks almost right.
	assert!(read.answer.contains("not a clamp"), "the rule must say what it is NOT, because the alternative is the plausible one");
	let write = NUMERIC_RULES.iter().find(|rule| rule.question == "out-of-bounds array write").expect("the write rule");
	assert!(write.answer.contains("DISCARDED"));
}

#[test]
fn every_accuracy_bound_is_a_number_and_sqrt_is_exact() {
	use crate::shader_ir::TRANSCENDENTAL_ACCURACY;
	for accuracy in TRANSCENDENTAL_ACCURACY {
		assert!(!accuracy.domain.is_empty(), "{} states a bound over no domain", accuracy.operation);
		// A bound above 16 ULP is not a bound: it admits an implementation a conformance suite
		// cannot distinguish from a wrong one.
		assert!(accuracy.max_ulp <= 16, "{} has a bound that admits anything", accuracy.operation);
	}
	let sqrt = TRANSCENDENTAL_ACCURACY.iter().find(|accuracy| accuracy.operation == "sqrt").expect("sqrt");
	// It is an IEEE 754 operation, so 0 ULP is not a demand - it is what every implementation
	// already does, and accepting less would be admitting a backend that reimplemented it worse.
	assert_eq!(sqrt.max_ulp, 0);
}

#[test]
fn the_strict_path_refuses_what_it_cannot_reproduce_and_says_where() {
	use crate::shader_ir::STRICT_F32_RULES;
	let transcendental = STRICT_F32_RULES.iter().find(|rule| rule.question == "a transcendental on a position path").expect("the transcendental rule");
	// `sqrt` is allowed and `sin` is not, and the distinction is the accuracy bound - which is the
	// only principled line between them.
	assert!(transcendental.answer.contains("sqrt") && transcendental.answer.contains("sin"));
	let refusal = STRICT_F32_RULES.iter().find(|rule| rule.question == "what happens on a refusal").expect("the refusal rule");
	// AT LOAD AND NOT AT DRAW. A shader that compiles and then refuses to draw is a failure nobody
	// can attribute to the line that caused it.
	assert!(refusal.answer.contains("Not at draw time"));
	let fragment = STRICT_F32_RULES.iter().find(|rule| rule.question == "fragment arithmetic").expect("the fragment rule");
	assert!(fragment.answer.contains("does NOT acquire strict requirements"));
}

#[test]
fn a_discarded_lane_keeps_running_so_its_neighbours_derivatives_are_real() {
	use crate::shader_ir::STAGE_RULES;
	let discard = STAGE_RULES.iter().find(|rule| rule.question == "a derivative after `discard`").expect("the discard rule");
	assert!(discard.answer.contains("HELPER LANE"));
	let helper = STAGE_RULES.iter().find(|rule| rule.question == "helper lanes").expect("the helper rule");
	// It runs AND writes nothing: either half alone is a different and wrong contract.
	assert!(helper.answer.contains("RUNS") && helper.answer.contains("WRITES NOTHING"));
	// And implicit LOD is fragment-only, because no other stage has a quad to take a derivative over.
	let implicit = STAGE_RULES.iter().find(|rule| rule.question == "implicit LOD").expect("the LOD rule");
	assert!(implicit.answer.contains("FRAGMENT stage only"));
}

#[test]
fn the_encoding_refuses_an_unknown_instruction_rather_than_skipping_it() {
	use crate::shader_ir::{ENCODING_RULES, SHADER_IR_VERSION};
	assert_eq!(SHADER_IR_VERSION, 1);
	let forward = ENCODING_RULES.iter().find(|rule| rule.question == "forward compatibility").expect("the compatibility rule");
	// Skipping produces a program that RUNS and computes something else, which is the failure mode a
	// refusal exists to prevent.
	assert!(forward.answer.contains("REFUSAL"));
	let identity = ENCODING_RULES.iter().find(|rule| rule.question == "instruction identity").expect("the identity rule");
	assert!(identity.answer.contains("retired rather than reused"));
	let constants = ENCODING_RULES.iter().find(|rule| rule.question == "canonical constants").expect("the constant rule");
	// `-0.0` normalised to `0.0` changes the sign of a division, which is a real picture difference.
	assert!(constants.answer.contains("-0.0"));
}

#[test]
fn a_three_component_vector_is_aligned_as_four_and_a_matrix_is_column_major() {
	use crate::shader_ir::{MATRIX_LAYOUT, UNIFORM_LAYOUT};
	let vec3 = UNIFORM_LAYOUT.iter().find(|rule| rule.question.contains("three- or four-component")).expect("the vec3 rule");
	// THE ONE EVERYBODY GETS WRONG ONCE, and the host side writes bytes against it.
	assert!(vec3.answer.contains("FOUR times"));
	let order = MATRIX_LAYOUT.iter().find(|rule| rule.question == "the order").expect("the matrix order");
	assert!(order.answer.contains("COLUMN-MAJOR"));
	let multiply = MATRIX_LAYOUT.iter().find(|rule| rule.question == "multiplication order").expect("the multiplication order");
	// A profile that stated the storage order and not the multiplication order would still let two
	// implementations transpose each other.
	assert!(multiply.answer.contains("COLUMN"));
}

// ---------------------------------------------------------------------------------------------
// `Scene3D Core Profile 1`: the retained layer's contract.
// ---------------------------------------------------------------------------------------------

#[test]
fn no_question_in_the_scene3d_registry_has_two_answers() {
	use crate::scene3d::{CAMERA_RULES, CULLING_RULES, HIERARCHY_RULES, INSTANCING_RULES, LIGHTING_RULES, MATERIAL_RULES, PICKING_RULES, QUEUE_RULES};
	for (name, rules) in [
		("hierarchy", HIERARCHY_RULES),
		("camera", CAMERA_RULES),
		("queue", QUEUE_RULES),
		("culling", CULLING_RULES),
		("instancing", INSTANCING_RULES),
		("material", MATERIAL_RULES),
		("lighting", LIGHTING_RULES),
		("picking", PICKING_RULES),
	] {
		for (index, rule) in rules.iter().enumerate() {
			assert!(!rule.answer.is_empty(), "{name}: `{}` has no answer", rule.question);
			assert!(rules[..index].iter().all(|earlier| earlier.question != rule.question), "{name} answers `{}` twice", rule.question);
		}
	}
}

#[test]
fn the_three_queues_order_and_write_depth_the_way_transparency_needs() {
	use crate::scene3d::QUEUES;
	assert_eq!(QUEUES.len(), 3);
	let opaque = &QUEUES[0];
	let mask = &QUEUES[1];
	let transparent = &QUEUES[2];
	assert_eq!((opaque.name, mask.name, transparent.name), ("Opaque", "AlphaMask", "Transparent"));
	// FRONT TO BACK for the two that write depth, so the depth test rejects the most fragments;
	// BACK TO FRONT for the one that blends, because a blend is order-dependent.
	assert!(opaque.order.contains("FRONT TO BACK"));
	assert!(transparent.order.contains("BACK TO FRONT"));
	// AND THE TRANSPARENT QUEUE DOES NOT WRITE DEPTH. Writing it makes a transparent surface hide
	// the one behind it, which is the commonest transparency bug there is.
	assert!(opaque.depth_write && mask.depth_write);
	assert!(!transparent.depth_write);
	// The sort must be stable, or two coincident objects flicker between frames.
	assert!(transparent.order.contains("STABLE"));
}

#[test]
fn the_transform_order_and_the_normal_matrix_are_both_stated() {
	use crate::scene3d::HIERARCHY_RULES;
	let compose = HIERARCHY_RULES.iter().find(|rule| rule.question == "transform composition order").expect("the composition rule");
	// The three do not commute, and a scene authored under one order looks wrong under the other.
	assert!(compose.answer.contains("TRANSLATION * ROTATION * SCALE"));
	let normals = HIERARCHY_RULES.iter().find(|rule| rule.question == "non-uniform scale and normals").expect("the normal rule");
	assert!(normals.answer.contains("INVERSE TRANSPOSE"));
}

#[test]
fn the_light_accumulation_order_is_fixed_because_addition_is_not_associative() {
	use crate::scene3d::LIGHTING_RULES;
	let which = LIGHTING_RULES.iter().find(|rule| rule.question == "which lights affect a drawable").expect("the selection rule");
	// TWO IMPLEMENTATIONS THAT ACCUMULATE IN DIFFERENT ORDERS PRODUCE DIFFERENT COLOURS, which a
	// conformance comparison sees - so the order is part of the profile rather than an optimisation.
	assert!(which.answer.contains("DESCENDING contribution"));
	assert!(which.answer.contains("not associative"));
	let accumulate = LIGHTING_RULES.iter().find(|rule| rule.question == "accumulation").expect("the accumulation rule");
	assert!(accumulate.answer.contains("linear space"));
	// Shadows are Extended, and the core profile says so rather than leaving it to be discovered.
	assert!(accumulate.answer.contains("Unshadowed"));
}

#[test]
fn blinn_phong_states_its_equation_rather_than_its_name() {
	use crate::scene3d::MATERIALS;
	assert_eq!(MATERIALS.len(), 4);
	for name in ["Unlit", "VertexColor", "Lambert", "BlinnPhong"] {
		assert!(MATERIALS.iter().any(|material| material.name == name), "`{name}` is not in the core material set");
	}
	let blinn = MATERIALS.iter().find(|material| material.name == "BlinnPhong").expect("BlinnPhong");
	// THE HALF VECTOR IS WHAT MAKES IT BLINN-PHONG. A material named for an equation and specified
	// without one is a material two implementations write differently.
	assert!(blinn.equation.contains("normalize(L + V)"));
	assert!(blinn.equation.contains("shininess"));
	// And every material's equation is an equation, not a description.
	for material in MATERIALS {
		assert!(material.equation.contains('='), "{} has no equation", material.name);
	}
}

#[test]
fn a_bounding_sphere_does_not_grow_as_its_object_rotates() {
	use crate::scene3d::CULLING_RULES;
	let derive = CULLING_RULES.iter().find(|rule| rule.question == "how the world sphere is derived").expect("the derivation rule");
	// Recomputing a tight box per frame is the bounds that inflate until everything is visible.
	assert!(derive.answer.contains("LARGEST of the three axis scale factors"));
	let unbounded = CULLING_RULES.iter().find(|rule| rule.question == "a drawable with no bounds").expect("the unbounded rule");
	assert!(unbounded.answer.contains("NEVER culled"));
	let observable = CULLING_RULES.iter().find(|rule| rule.question == "whether culling is observable").expect("the observability rule");
	assert!(observable.answer.contains("only in performance"));
}

#[test]
fn picking_answers_what_rather_than_where_and_reserves_zero() {
	use crate::scene3d::PICKING_RULES;
	let read = PICKING_RULES.iter().find(|rule| rule.question == "what is read").expect("the read rule");
	assert!(read.answer.contains("integer attachment"));
	let id = PICKING_RULES.iter().find(|rule| rule.question == "the id").expect("the id rule");
	// Zero reserved is what makes a pick on the background unambiguous rather than a valid id.
	assert!(id.answer.contains("Zero is reserved"));
}

#[test]
fn every_scene_minimum_is_a_floor_with_a_reason() {
	use crate::scene3d::SCENE3D_PROFILE_1_MIN_LIMITS;
	for limit in SCENE3D_PROFILE_1_MIN_LIMITS {
		assert!(limit.minimum > 0, "{} has no floor", limit.name);
		assert!(!limit.why.is_empty(), "{} states a number and not a reason", limit.name);
	}
	let by_name = |name: &str| SCENE3D_PROFILE_1_MIN_LIMITS.iter().find(|limit| limit.name == name).unwrap_or_else(|| panic!("no minimum for `{name}`")).minimum;
	// The depth floor is the one a recursive traversal depends on, and it is bounded on BOTH sides
	// by the reason: deep enough for an articulated model, shallow enough not to overflow a stack.
	assert_eq!(by_name("max_hierarchy_depth"), 64);
	assert_eq!(by_name("max_lights_per_drawable"), 8);
	assert_eq!(by_name("max_instances_per_drawable"), 4096);
}

// ---------------------------------------------------------------------------------------------
// `Scene3D Extended 1`: the equations.
// ---------------------------------------------------------------------------------------------

#[test]
fn every_pbr_term_is_an_equation_and_says_why_this_one() {
	use crate::scene3d_extended::PBR_TERMS;
	assert_eq!(PBR_TERMS.len(), 5);
	for term in PBR_TERMS {
		assert!(term.equation.contains('='), "{} names a function without giving it", term.name);
		// WHY THIS ONE is the column that matters: every published renderer picks slightly different
		// terms, and a profile that listed its choices without the reason cannot be argued with.
		assert!(!term.why.is_empty(), "{} states an equation and not a reason", term.name);
	}
	let visibility = PBR_TERMS.iter().find(|term| term.name.contains("visibility")).expect("the visibility term");
	// THE FOUR-TIMES-TOO-BRIGHT BUG. The Smith visibility form used here absorbs the specular
	// denominator, and an implementation that divided again would halve every highlight.
	assert!(visibility.why.contains("denominator"));
	let distribution = PBR_TERMS.iter().find(|term| term.name.contains("normal distribution")).expect("the NDF");
	assert!(distribution.equation.contains("roughness^2"));
}

#[test]
fn the_roughness_floor_exists_because_zero_is_a_delta_function() {
	use crate::scene3d_extended::PBR_CONSTANTS;
	let minimum = PBR_CONSTANTS.iter().find(|rule| rule.question == "the minimum roughness").expect("the roughness floor");
	assert!(minimum.answer.contains("0.045"));
	// A roughness of zero is an infinite highlight at one pixel and a NaN in a filtered environment
	// lookup, which is why the floor is a value rather than a suggestion.
	assert!(minimum.answer.contains("NaN"));
	let remap = PBR_CONSTANTS.iter().find(|rule| rule.question == "the roughness remapping").expect("the remapping");
	assert!(remap.answer.contains("PERCEPTUAL"));
}

#[test]
fn the_split_sum_states_both_halves_because_one_is_useless_alone() {
	use crate::scene3d_extended::ENVIRONMENT_RULES;
	let split = ENVIRONMENT_RULES.iter().find(|rule| rule.question == "the split-sum approximation").expect("the split sum");
	assert!(split.answer.contains("prefiltered") && split.answer.contains("brdf"));
	let table = ENVIRONMENT_RULES.iter().find(|rule| rule.question == "the BRDF lookup table").expect("the table");
	// ITS GENERATION IS PART OF THE PROFILE, or two implementations build two different tables and
	// the prefilter that matches one is wrong with the other.
	assert!(table.answer.contains("generated by the same GGX and Smith terms"));
}

#[test]
fn the_shadow_bias_is_named_rather_than_left_to_be_tuned() {
	use crate::scene3d_extended::SHADOW_RULES;
	let bias = SHADOW_RULES.iter().find(|rule| rule.question == "the bias").expect("the bias rule");
	// A scene tuned against an unstated bias acnes on the next implementation.
	assert!(bias.answer.contains("0.0015") && bias.answer.contains("2.0"));
	let outside = SHADOW_RULES.iter().find(|rule| rule.question == "what is outside the last cascade").expect("the outside rule");
	assert!(outside.answer.contains("UNSHADOWED"));
	let format = SHADOW_RULES.iter().find(|rule| rule.question == "the map format").expect("the format rule");
	// It names the 3D profile's comparison rule rather than restating it, so the two cannot drift.
	assert!(format.answer.contains("not quantised"));
}

#[test]
fn tone_mapping_is_the_two_d_operator_and_comes_last() {
	use crate::scene3d_extended::POSTPROCESS_RULES;
	let tone = POSTPROCESS_RULES.iter().find(|rule| rule.question == "tone mapping").expect("the tone rule");
	// TWO OPERATORS IN ONE SYSTEM IS TWO SYSTEMS: a 3D frame composited with a 2D one would have two
	// different highlights at the same radiance.
	assert!(tone.answer.contains("SAME operator") && tone.answer.contains("WHITE = 4.0"));
	let order = POSTPROCESS_RULES.iter().find(|rule| rule.question == "the order").expect("the order rule");
	assert!(order.answer.contains("LAST"));
	let luminance = POSTPROCESS_RULES.iter().find(|rule| rule.question == "the luminance used").expect("the luminance rule");
	assert!(luminance.answer.contains("0.2126"));
}

#[test]
fn root_motion_is_chosen_rather_than_required_and_rotation_slerps() {
	use crate::scene3d_extended::ANIMATION_RULES;
	let root = ANIMATION_RULES.iter().find(|rule| rule.question == "the root-motion policy").expect("the root-motion rule");
	// The plan asks for a policy CHOSEN rather than required, and a default that does not surprise:
	// an application that never asked for root motion should not have its character drift.
	assert!(root.answer.contains("CHOSEN RATHER THAN REQUIRED"));
	assert!(root.answer.contains("EXTRACTED by default"));
	let interpolation = ANIMATION_RULES.iter().find(|rule| rule.question == "interpolation").expect("the interpolation rule");
	assert!(interpolation.answer.contains("SPHERICAL LINEAR"));
	let weights = ANIMATION_RULES.iter().find(|rule| rule.question == "the skinning model").expect("the skinning rule");
	// Normalised at LOAD, so an authoring error is paid for once rather than every frame.
	assert!(weights.answer.contains("at load"));
}

#[test]
fn the_extended_profile_is_separate_from_the_core_one() {
	use crate::scene3d::SCENE3D_PROFILE_1_MIN_LIMITS;
	use crate::scene3d_extended::SCENE3D_EXTENDED_1_MIN_LIMITS;
	// NO LIMIT IS IN BOTH. A limit in both profiles is a limit an implementation could satisfy in
	// one and not the other, and a conformance claim that means two things.
	for extended in SCENE3D_EXTENDED_1_MIN_LIMITS {
		assert!(SCENE3D_PROFILE_1_MIN_LIMITS.iter().all(|core| core.name != extended.name), "`{}` is in both profiles", extended.name);
		assert!(extended.minimum > 0);
		assert!(!extended.why.is_empty());
	}
}

// ---------------------------------------------------------------------------------------------
// Conformance thresholds.
// ---------------------------------------------------------------------------------------------

#[test]
fn every_compared_profile_publishes_thresholds_and_none_is_written_twice() {
	use crate::thresholds::THRESHOLDS;
	// A PROFILE WITH NO THRESHOLD IS ONE WHOSE SUITE EITHER DEMANDS BIT-EXACTNESS OR DEMANDS
	// NOTHING, and both are ways of not checking.
	for profile in ["image-colour", "render2d", "render3d", "scene3d", "scene3d-extended"] {
		assert!(THRESHOLDS.iter().any(|threshold| threshold.profile == profile), "`{profile}` publishes no threshold");
	}
	for (index, threshold) in THRESHOLDS.iter().enumerate() {
		assert!(!threshold.tolerance.is_empty(), "{}/{}: no tolerance", threshold.profile, threshold.what);
		assert!(!threshold.why.is_empty(), "{}/{}: a number with no reason", threshold.profile, threshold.what);
		assert!(THRESHOLDS[..index].iter().all(|earlier| earlier.profile != threshold.profile || earlier.what != threshold.what), "{}/{} is written twice", threshold.profile, threshold.what);
	}
}

#[test]
fn what_has_no_tolerance_says_none_rather_than_being_absent() {
	use crate::thresholds::{THRESHOLD_RULES, THRESHOLDS};
	// THE LIST HAS TO READ AS DELIBERATE. A classification that simply had no row would be
	// indistinguishable from one somebody forgot, so the ones with no tolerance are IN the table and
	// say so.
	for (profile, what) in [("render3d", "a clip-space position"), ("render3d", "sample ownership"), ("scene3d", "a pick result")] {
		let threshold = THRESHOLDS.iter().find(|threshold| threshold.profile == profile && threshold.what == what).unwrap_or_else(|| panic!("`{profile}`/`{what}` is missing"));
		assert!(threshold.tolerance.starts_with("NONE"), "`{what}` must state that it has no tolerance");
	}
	let none = THRESHOLD_RULES.iter().find(|rule| rule.question == "what has no tolerance at all").expect("the no-tolerance rule");
	assert!(none.answer.contains("clip-space positions") && none.answer.contains("pick ids"));
}

#[test]
fn a_tolerance_may_not_be_loosened_per_architecture() {
	use crate::thresholds::THRESHOLD_RULES;
	let per_arch = THRESHOLD_RULES.iter().find(|rule| rule.question == "per-architecture loosening").expect("the architecture rule");
	// A tolerance that widened on the slow target would stop checking exactly where the second
	// implementation is, which is the only place the comparison is worth making.
	assert!(per_arch.answer.contains("REFUSED"));
	let space = THRESHOLD_RULES.iter().find(|rule| rule.question == "where a tolerance is compared").expect("the space rule");
	assert!(space.answer.contains("NAMED comparison space"));
	let reference = THRESHOLD_RULES.iter().find(|rule| rule.question == "the reference").expect("the reference rule");
	// Comparing two f32 implementations against each other alone passes two that are wrong the same
	// way, which is why the reference is f64 wherever an expression exists.
	assert!(reference.answer.contains("f64"));
	let incomplete = THRESHOLD_RULES.iter().find(|rule| rule.question == "a freeze with no thresholds").expect("the completeness rule");
	assert!(incomplete.answer.contains("NOT COMPLETE"));
}

#[test]
fn the_two_d_coverage_bound_is_bounded_per_pixel_and_in_the_mean() {
	use crate::thresholds::THRESHOLDS;
	let coverage = THRESHOLDS.iter().find(|threshold| threshold.what == "antialiasing coverage").expect("the coverage threshold");
	// THE MEAN BOUND IS THE ONE THAT CATCHES A SYSTEMATIC BIAS. A per-pixel bound alone passes an
	// implementation whose every pixel is one step light.
	assert!(coverage.tolerance.contains("2/255") && coverage.tolerance.contains("mean"));
	let edge = THRESHOLDS.iter().find(|threshold| threshold.what == "the edge policy").expect("the edge threshold");
	// A fully covered pixel that differs at all is a colour error wearing an antialiasing tolerance.
	assert!(edge.tolerance.contains("EXACTLY"));
	let mask = THRESHOLDS.iter().find(|threshold| threshold.what == "the image comparison mask").expect("the mask threshold");
	assert!(mask.tolerance.contains("EVERY pixel"));
}

// ---------------------------------------------------------------------------------------------
// How a profile is hashed.
// ---------------------------------------------------------------------------------------------

#[test]
fn the_canonical_encoding_rule_excludes_the_hash_and_the_prose() {
	use crate::hashing::CANONICAL_ENCODING;
	for (index, rule) in CANONICAL_ENCODING.iter().enumerate() {
		assert!(!rule.answer.is_empty(), "`{}` has no answer", rule.question);
		assert!(CANONICAL_ENCODING[..index].iter().all(|earlier| earlier.question != rule.question), "`{}` is answered twice", rule.question);
	}
	let hash_field = CANONICAL_ENCODING.iter().find(|rule| rule.question == "the hash field").expect("the hash-field rule");
	// A HASH THAT COVERED ITSELF COULD NOT BE COMPUTED. Saying so is what stops somebody adding it
	// to the canonical form later.
	assert!(hash_field.answer.contains("EXCLUDED"));
	let document = CANONICAL_ENCODING.iter().find(|rule| rule.question == "what the document contributes").expect("the document rule");
	// Hashing the Markdown would make rewrapping a paragraph a profile change AND would let a
	// corrected number pass unnoticed if the prose around it was rewritten at the same time.
	assert!(document.answer.contains("NOTHING"));
}

#[test]
fn the_ordering_rule_does_not_sort_lists_whose_order_is_the_profile() {
	use crate::hashing::CANONICAL_ENCODING;
	let within = CANONICAL_ENCODING.iter().find(|rule| rule.question == "within a registry").expect("the ordering rule");
	// SORTING WOULD DESTROY MEANING. The clip planes, the render queues and the light accumulation
	// are all ordered on purpose, and a canonical form that sorted them would hash two different
	// profiles the same.
	assert!(within.answer.contains("DECLARATION ORDER"));
	assert!(within.answer.contains("clip planes"));
	let inventory = CANONICAL_ENCODING.iter().find(|rule| rule.question == "the input inventory").expect("the inventory rule");
	// A directory listing or a hash map differs between runs and between machines, which is a hash
	// that changes with nothing.
	assert!(inventory.answer.contains("FIXED order"));
}

#[test]
fn a_version_changes_with_the_semantics_and_not_with_the_prose() {
	use crate::hashing::CANONICAL_ENCODING;
	let when = CANONICAL_ENCODING.iter().find(|rule| rule.question == "when the hash may change").expect("the version rule");
	assert!(when.answer.contains("VERSION changes with it"));
	let gate = CANONICAL_ENCODING.iter().find(|rule| rule.question == "what the gate checks").expect("the gate rule");
	// The three things that have to agree, named - so a gate that checked one of them would be
	// visibly short of what the rule says.
	assert!(gate.answer.contains("document") && gate.answer.contains("manifest") && gate.answer.contains("coverage table"));
}

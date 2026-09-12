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

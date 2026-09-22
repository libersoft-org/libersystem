//! `Render3D Core Profile 1` AND `Scene3D Core Profile 1`, WALKED ENTRY BY ENTRY.
//!
//! WHAT A CONFORMANCE SUITE IS FOR, and it is not "does it draw". A profile is a closed list, and
//! "supports Profile 1" is a claim about EVERY entry in it - so the suite is driven by the
//! machine-readable registry rather than by a list of scenes somebody remembered to write. A feature
//! added to a profile with no scene here is a failure of this suite, which is what makes the two stay
//! in step.
//!
//! ONE SCENE PER FEATURE, AND EACH SCENE STATES ITS OWN PASS CONDITION. A single frame compared
//! against a stored one fails for every reason at once and says nothing about which feature broke;
//! these fail one at a time and name the feature. Nothing here compares against a recorded baseline:
//! a baseline captured from this backend agrees with this backend by construction, which makes it a
//! regression test rather than a conformance one.
//!
//! AND IT IS A LIBRARY AND NOT A PROGRAM, so that the same scenes run as host tests on the machine
//! that builds the tree and inside a booted guest on each architecture. The program is a few lines
//! around `run`; what it proves is that the arithmetic agrees on the target and not only on the host.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use graphics_profile::{ExtendedFeature, RENDER3D_CORE_PROFILE_1, Render3DFeature, SCENE3D_CORE_PROFILE_1, SCENE3D_EXTENDED_PROFILE_1, Scene3DFeature};

#[macro_use]
mod harness;
mod depth;
pub mod extended;
mod formats;
mod geometry;
mod msaa;
mod passes;
mod pipeline;
mod readback;
mod resources;
mod sampling;
mod scene;

pub use harness::Trouble;

/// What a scene answers.
pub type Outcome = Result<(), Trouble>;

/// One feature's scene.
///
/// THE FEATURE IS THE PROFILE'S OWN ENUM and the name is read from the registry rather than written
/// here, so a scene cannot claim to cover a feature under a name the profile spells differently.
pub struct Case {
	pub feature: Render3DFeature,
	pub scene: fn() -> Outcome,
}

/// The same, for the retained layer's profile.
///
/// TWO CASE TYPES AND NOT ONE WITH A TAG, because the two profiles are two closed lists with two
/// hashes: an implementation may conform to one and not the other, and a suite that mixed them could
/// not say which.
pub struct SceneCase {
	pub feature: Scene3DFeature,
	pub scene: fn() -> Outcome,
}

/// The same, for `Scene3D Extended Profile 1`.
///
/// A THIRD CASE TYPE FOR A THIRD CLOSED LIST WITH A THIRD HASH. Extended is OPTIONAL as a whole: an
/// implementation conforms to the two core profiles while supporting none of it, so its tally is
/// reported separately and a run that does not claim it is not failed for the absence.
pub struct ExtendedCase {
	pub feature: ExtendedFeature,
	pub scene: fn() -> Outcome,
}

/// How one case came out.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
	Pass,
	/// The scene ran and the frame was wrong.
	Fail(String),
	/// The backend REFUSED something the profile requires. In Profile 1 this is not a gap, it is a
	/// backend that does not conform - `Unsupported` is reserved for extensions added after it.
	Unsupported(String),
}

/// What one profile's scenes came to.
#[derive(Clone, Default, Debug)]
pub struct Tally {
	pub passed: usize,
	pub failed: usize,
	pub unsupported: usize,
	/// Profile entries with no scene at all. THE CHECK THAT KEEPS THE SUITE HONEST: without it, a
	/// feature added to the profile is a feature nobody tests and every run still says "all passed".
	pub untested: Vec<&'static str>,
}

impl Tally {
	/// A profile conforms only when every feature of it has a scene and every scene passes.
	pub fn complete(&self) -> bool {
		self.failed == 0 && self.unsupported == 0 && self.untested.is_empty()
	}

	pub fn total(&self) -> usize {
		self.passed + self.failed + self.unsupported
	}
}

/// What a whole run came to.
///
/// TWO TALLIES AND NOT ONE, because the two profiles are checked INDEPENDENTLY: an implementation may
/// carry `Render3D Core Profile 1` and not the retained layer above it, and one number that mixed
/// them could not say so. A run conforms when both do.
#[derive(Clone, Default, Debug)]
pub struct Summary {
	pub render3d: Tally,
	pub scene3d: Tally,
	/// `Scene3D Extended Profile 1`, which is OPTIONAL as a whole - see `complete`.
	pub extended: Tally,
}

impl Summary {
	/// A run passes only when every feature of both CORE profiles has a scene and every scene
	/// passes.
	///
	/// EXTENDED IS NOT IN THIS ANSWER, because the profile is optional as a whole: a conforming
	/// implementation may support none of it, and folding it in here would make "conforms" mean
	/// something the profile does not say. `complete_with_extended` is the claim for a layer that
	/// says it carries the part.
	pub fn complete(&self) -> bool {
		self.render3d.complete() && self.scene3d.complete()
	}

	/// The same claim for a layer that says it carries `Scene3D Extended Profile 1` as well.
	///
	/// ENTIRELY OR NOT AT ALL, which is the part's own rule: a layer claiming Extended claims every
	/// entry of it, so a single untested or refused feature is a claim that does not hold.
	pub fn complete_with_extended(&self) -> bool {
		self.complete() && self.extended.complete()
	}

	pub fn passed(&self) -> usize {
		self.render3d.passed + self.scene3d.passed + self.extended.passed
	}

	pub fn failed(&self) -> usize {
		self.render3d.failed + self.scene3d.failed + self.extended.failed
	}

	pub fn unsupported(&self) -> usize {
		self.render3d.unsupported + self.scene3d.unsupported + self.extended.unsupported
	}

	pub fn untested(&self) -> usize {
		self.render3d.untested.len() + self.scene3d.untested.len() + self.extended.untested.len()
	}
}

/// The profile entry a feature belongs to, which is where a scene's name and group come from.
pub fn entry(feature: Render3DFeature) -> Option<&'static graphics_profile::ProfileEntry<Render3DFeature>> {
	RENDER3D_CORE_PROFILE_1.iter().find(|entry| entry.feature == feature)
}

/// The same, in the retained layer's profile.
pub fn scene_entry(feature: Scene3DFeature) -> Option<&'static graphics_profile::ProfileEntry<Scene3DFeature>> {
	SCENE3D_CORE_PROFILE_1.iter().find(|entry| entry.feature == feature)
}

/// The same, in the extended profile.
pub fn extended_entry(feature: ExtendedFeature) -> Option<&'static graphics_profile::ProfileEntry<ExtendedFeature>> {
	SCENE3D_EXTENDED_PROFILE_1.iter().find(|entry| entry.feature == feature)
}

/// Run every scene of both profiles, reporting each as it finishes.
///
/// REPORTED AS THEY FINISH rather than collected and printed at the end, because this runs inside a
/// guest whose output is a serial log: a suite that prints at the end prints nothing at all when one
/// scene hangs, and which scene it was is the whole of what a reader needs.
///
/// THE TWO PROFILES ARE WALKED SEPARATELY AND TALLIED SEPARATELY, so "the backend conforms and the
/// scene layer does not" is a sentence this run can say.
pub fn run(mut report: impl FnMut(&'static str, &'static str, &Verdict)) -> Summary {
	let mut summary = Summary::default();
	for case in CASES {
		let Some(entry) = entry(case.feature) else {
			// A CASE FOR A FEATURE THE PROFILE DOES NOT HAVE is a scene measuring an extension while
			// reporting Profile 1.
			summary.render3d.failed += 1;
			continue;
		};
		let verdict = tally(&mut summary.render3d, case.scene);
		report(entry.name, entry.group, &verdict);
	}
	for entry in RENDER3D_CORE_PROFILE_1 {
		if !CASES.iter().any(|case| case.feature == entry.feature) {
			summary.render3d.untested.push(entry.name);
		}
	}
	for case in SCENE_CASES {
		let Some(entry) = scene_entry(case.feature) else {
			summary.scene3d.failed += 1;
			continue;
		};
		let verdict = tally(&mut summary.scene3d, case.scene);
		report(entry.name, entry.group, &verdict);
	}
	for entry in SCENE3D_CORE_PROFILE_1 {
		if !SCENE_CASES.iter().any(|case| case.feature == entry.feature) {
			summary.scene3d.untested.push(entry.name);
		}
	}
	for case in EXTENDED_CASES {
		let Some(entry) = extended_entry(case.feature) else {
			summary.extended.failed += 1;
			continue;
		};
		let verdict = tally(&mut summary.extended, case.scene);
		report(entry.name, entry.group, &verdict);
	}
	for entry in SCENE3D_EXTENDED_PROFILE_1 {
		if !EXTENDED_CASES.iter().any(|case| case.feature == entry.feature) {
			summary.extended.untested.push(entry.name);
		}
	}
	summary
}

/// Run one scene and count it.
fn tally(into: &mut Tally, scene: fn() -> Outcome) -> Verdict {
	let verdict = match scene() {
		Ok(()) => Verdict::Pass,
		Err(Trouble::Failed(why)) => Verdict::Fail(why),
		Err(Trouble::Unsupported(why)) => Verdict::Unsupported(why),
	};
	match verdict {
		Verdict::Pass => into.passed += 1,
		Verdict::Fail(_) => into.failed += 1,
		Verdict::Unsupported(_) => into.unsupported += 1,
	}
	verdict
}

/// Every scene, by the feature it covers.
pub const CASES: &[Case] = &[
	Case { feature: Render3DFeature::IndexU16, scene: geometry::index_u16 },
	Case { feature: Render3DFeature::IndexU32, scene: geometry::index_u32 },
	Case { feature: Render3DFeature::MultipleVertexStreams, scene: geometry::multiple_vertex_streams },
	Case { feature: Render3DFeature::ConfigurableAttributes, scene: geometry::configurable_attributes },
	Case { feature: Render3DFeature::TriangleList, scene: geometry::triangle_list },
	Case { feature: Render3DFeature::TriangleStrip, scene: geometry::triangle_strip },
	Case { feature: Render3DFeature::TriangleFan, scene: geometry::triangle_fan },
	Case { feature: Render3DFeature::LineList, scene: geometry::line_list },
	Case { feature: Render3DFeature::LineStrip, scene: geometry::line_strip },
	Case { feature: Render3DFeature::PointList, scene: geometry::point_list },
	Case { feature: Render3DFeature::PrimitiveRestart, scene: geometry::primitive_restart },
	Case { feature: Render3DFeature::IndexedDraw, scene: geometry::indexed_draw },
	Case { feature: Render3DFeature::NonIndexedDraw, scene: geometry::non_indexed_draw },
	Case { feature: Render3DFeature::Instancing, scene: geometry::instancing },
	Case { feature: Render3DFeature::BaseVertex, scene: geometry::base_vertex },
	Case { feature: Render3DFeature::BaseInstance, scene: geometry::base_instance },
	Case { feature: Render3DFeature::Depth16, scene: depth::depth16 },
	Case { feature: Render3DFeature::Depth24, scene: depth::depth24 },
	Case { feature: Render3DFeature::Depth32F, scene: depth::depth32f },
	Case { feature: Render3DFeature::Depth24Stencil8, scene: depth::depth24_stencil8 },
	Case { feature: Render3DFeature::Depth32FStencil8, scene: depth::depth32f_stencil8 },
	Case { feature: Render3DFeature::DepthCompareOps, scene: depth::depth_compare_ops },
	Case { feature: Render3DFeature::DepthWrite, scene: depth::depth_write },
	Case { feature: Render3DFeature::DepthBias, scene: depth::depth_bias },
	Case { feature: Render3DFeature::StencilCompare, scene: depth::stencil_compare },
	Case { feature: Render3DFeature::StencilReadMask, scene: depth::stencil_read_mask },
	Case { feature: Render3DFeature::StencilWriteMask, scene: depth::stencil_write_mask },
	Case { feature: Render3DFeature::StencilFailOp, scene: depth::stencil_fail_op },
	Case { feature: Render3DFeature::StencilDepthFailOp, scene: depth::stencil_depth_fail_op },
	Case { feature: Render3DFeature::StencilPassOp, scene: depth::stencil_pass_op },
	Case { feature: Render3DFeature::StencilSeparateFrontBack, scene: depth::stencil_separate_front_back },
	Case { feature: Render3DFeature::Msaa1x, scene: msaa::msaa_1x },
	Case { feature: Render3DFeature::Msaa2x, scene: msaa::msaa_2x },
	Case { feature: Render3DFeature::Msaa4x, scene: msaa::msaa_4x },
	Case { feature: Render3DFeature::SampleMask, scene: msaa::sample_mask },
	Case { feature: Render3DFeature::AlphaToCoverage, scene: msaa::alpha_to_coverage },
	Case { feature: Render3DFeature::MsaaResolve, scene: msaa::msaa_resolve },
	Case { feature: Render3DFeature::MinNearest, scene: sampling::min_nearest },
	Case { feature: Render3DFeature::MinLinear, scene: sampling::min_linear },
	Case { feature: Render3DFeature::MagNearest, scene: sampling::mag_nearest },
	Case { feature: Render3DFeature::MagLinear, scene: sampling::mag_linear },
	Case { feature: Render3DFeature::MipNearest, scene: sampling::mip_nearest },
	Case { feature: Render3DFeature::MipLinear, scene: sampling::mip_linear },
	Case { feature: Render3DFeature::Trilinear, scene: sampling::trilinear },
	Case { feature: Render3DFeature::SamplerWrapClamp, scene: sampling::sampler_wrap_clamp },
	Case { feature: Render3DFeature::SamplerWrapRepeat, scene: sampling::sampler_wrap_repeat },
	Case { feature: Render3DFeature::SamplerWrapMirror, scene: sampling::sampler_wrap_mirror },
	Case { feature: Render3DFeature::SamplerWrapBorder, scene: sampling::sampler_wrap_border },
	Case { feature: Render3DFeature::LodBias, scene: sampling::lod_bias },
	Case { feature: Render3DFeature::LodClamp, scene: sampling::lod_clamp },
	Case { feature: Render3DFeature::DepthCompareSampling, scene: sampling::depth_compare_sampling },
	Case { feature: Render3DFeature::Anisotropy8, scene: sampling::anisotropy8 },
	Case { feature: Render3DFeature::Buffer, scene: resources::buffer },
	Case { feature: Render3DFeature::Texture2D, scene: resources::texture_2d },
	Case { feature: Render3DFeature::TextureCube, scene: resources::texture_cube },
	Case { feature: Render3DFeature::Texture2DArray, scene: resources::texture_2d_array },
	Case { feature: Render3DFeature::Texture3D, scene: resources::texture_3d },
	Case { feature: Render3DFeature::Sampler, scene: resources::sampler },
	Case { feature: Render3DFeature::Mipmaps, scene: resources::mipmaps },
	Case { feature: Render3DFeature::RenderTarget, scene: resources::render_target },
	Case { feature: Render3DFeature::ProgrammableVertexStage, scene: pipeline::programmable_vertex_stage },
	Case { feature: Render3DFeature::ProgrammableFragmentStage, scene: pipeline::programmable_fragment_stage },
	Case { feature: Render3DFeature::VertexLayout, scene: pipeline::vertex_layout },
	Case { feature: Render3DFeature::PrimitiveTopology, scene: pipeline::primitive_topology },
	Case { feature: Render3DFeature::RasteriserState, scene: pipeline::rasteriser_state },
	Case { feature: Render3DFeature::DepthStencilState, scene: pipeline::depth_stencil_state },
	Case { feature: Render3DFeature::BlendStatePerAttachment, scene: pipeline::blend_state_per_attachment },
	Case { feature: Render3DFeature::ColorWriteMask, scene: pipeline::color_write_mask },
	Case { feature: Render3DFeature::Viewport, scene: pipeline::viewport },
	Case { feature: Render3DFeature::Scissor, scene: pipeline::scissor },
	Case { feature: Render3DFeature::LoadClear, scene: passes::load_clear },
	Case { feature: Render3DFeature::LoadLoad, scene: passes::load_load },
	Case { feature: Render3DFeature::LoadDiscard, scene: passes::load_discard },
	Case { feature: Render3DFeature::StoreStore, scene: passes::store_store },
	Case { feature: Render3DFeature::StoreDiscard, scene: passes::store_discard },
	Case { feature: Render3DFeature::MultipleColorAttachments, scene: passes::multiple_color_attachments },
	Case { feature: Render3DFeature::DepthStencilAttachment, scene: passes::depth_stencil_attachment },
	Case { feature: Render3DFeature::OffscreenRenderTarget, scene: passes::offscreen_render_target },
	Case { feature: Render3DFeature::RenderToTexture, scene: passes::render_to_texture },
	Case { feature: Render3DFeature::FormatR8, scene: formats::format_r8 },
	Case { feature: Render3DFeature::FormatRG8, scene: formats::format_rg8 },
	Case { feature: Render3DFeature::FormatRGBA8, scene: formats::format_rgba8 },
	Case { feature: Render3DFeature::FormatRGB10A2, scene: formats::format_rgb10a2 },
	Case { feature: Render3DFeature::FormatRGBA16F, scene: formats::format_rgba16f },
	Case { feature: Render3DFeature::FormatRGBA32F, scene: formats::format_rgba32f },
	Case { feature: Render3DFeature::FormatR32Uint, scene: formats::format_r32_uint },
	Case { feature: Render3DFeature::ReadbackColor, scene: readback::readback_color },
	Case { feature: Render3DFeature::ReadbackDepth, scene: readback::readback_depth },
	Case { feature: Render3DFeature::ReadbackObjectId, scene: readback::readback_object_id },
];

/// Every scene of the retained layer's profile, by the feature it covers.
pub const SCENE_CASES: &[SceneCase] = &[
	SceneCase { feature: Scene3DFeature::Node, scene: scene::hierarchy::node },
	SceneCase { feature: Scene3DFeature::ParentChildTransform, scene: scene::hierarchy::parent_child_transform },
	SceneCase { feature: Scene3DFeature::LocalTransformOrder, scene: scene::hierarchy::local_transform_order },
	SceneCase { feature: Scene3DFeature::QuaternionRotation, scene: scene::hierarchy::quaternion_rotation },
	SceneCase { feature: Scene3DFeature::NormalMatrix, scene: scene::hierarchy::normal_matrix_feature },
	SceneCase { feature: Scene3DFeature::LazyWorldTransforms, scene: scene::hierarchy::lazy_world_transforms },
	SceneCase { feature: Scene3DFeature::HierarchyCycleRefusal, scene: scene::hierarchy::hierarchy_cycle_refusal },
	SceneCase { feature: Scene3DFeature::HierarchyDepthLimit, scene: scene::hierarchy::hierarchy_depth_limit },
	SceneCase { feature: Scene3DFeature::VisibilityMask, scene: scene::hierarchy::visibility_mask },
	SceneCase { feature: Scene3DFeature::NodeEnable, scene: scene::hierarchy::node_enable },
	SceneCase { feature: Scene3DFeature::PerspectiveCamera, scene: scene::camera::perspective_camera },
	SceneCase { feature: Scene3DFeature::OrthographicCamera, scene: scene::camera::orthographic_camera },
	SceneCase { feature: Scene3DFeature::InfiniteFarPlane, scene: scene::camera::infinite_far_plane },
	SceneCase { feature: Scene3DFeature::ViewFromNodeTransform, scene: scene::camera::view_from_node_transform },
	SceneCase { feature: Scene3DFeature::CustomProjection, scene: scene::camera::custom_projection },
	SceneCase { feature: Scene3DFeature::DegenerateProjectionRefusal, scene: scene::camera::degenerate_projection_refusal },
	SceneCase { feature: Scene3DFeature::OpaqueQueue, scene: scene::queues::opaque_queue },
	SceneCase { feature: Scene3DFeature::AlphaMaskQueue, scene: scene::queues::alpha_mask_queue },
	SceneCase { feature: Scene3DFeature::TransparentQueue, scene: scene::queues::transparent_queue },
	SceneCase { feature: Scene3DFeature::DistanceSortKey, scene: scene::queues::distance_sort_key },
	SceneCase { feature: Scene3DFeature::StableTiebreak, scene: scene::queues::stable_tiebreak },
	SceneCase { feature: Scene3DFeature::FixedQueueOrder, scene: scene::queues::fixed_queue_order },
	SceneCase { feature: Scene3DFeature::TransparentDepthNoWrite, scene: scene::queues::transparent_depth_no_write },
	SceneCase { feature: Scene3DFeature::LocalBoundingBox, scene: scene::culling::local_bounding_box },
	SceneCase { feature: Scene3DFeature::WorldBoundingSphere, scene: scene::culling::world_bounding_sphere },
	SceneCase { feature: Scene3DFeature::FrustumPlaneExtraction, scene: scene::culling::frustum_plane_extraction },
	SceneCase { feature: Scene3DFeature::SphereFrustumTest, scene: scene::culling::sphere_frustum_test },
	SceneCase { feature: Scene3DFeature::BoxFrustumTest, scene: scene::culling::box_frustum_test },
	SceneCase { feature: Scene3DFeature::FixedPlaneTestOrder, scene: scene::culling::fixed_plane_test_order },
	SceneCase { feature: Scene3DFeature::UnboundedNeverCulled, scene: scene::culling::unbounded_never_culled },
	SceneCase { feature: Scene3DFeature::InstanceStream, scene: scene::instancing::instance_stream },
	SceneCase { feature: Scene3DFeature::PerInstanceCulling, scene: scene::instancing::per_instance_culling },
	SceneCase { feature: Scene3DFeature::InstanceCompaction, scene: scene::instancing::instance_compaction },
	SceneCase { feature: Scene3DFeature::SingleSortPerInstanceSet, scene: scene::instancing::single_sort_per_instance_set },
	SceneCase { feature: Scene3DFeature::TransparentInstancingApproximate, scene: scene::instancing::transparent_instancing_approximate },
	SceneCase { feature: Scene3DFeature::MaterialUnlit, scene: scene::materials::material_unlit },
	SceneCase { feature: Scene3DFeature::MaterialVertexColor, scene: scene::materials::material_vertex_color },
	SceneCase { feature: Scene3DFeature::MaterialLambert, scene: scene::materials::material_lambert },
	SceneCase { feature: Scene3DFeature::MaterialBlinnPhong, scene: scene::materials::material_blinn_phong },
	SceneCase { feature: Scene3DFeature::LinearColourSpace, scene: scene::materials::linear_colour_space },
	SceneCase { feature: Scene3DFeature::NormalRenormalisation, scene: scene::materials::normal_renormalisation },
	SceneCase { feature: Scene3DFeature::TwoSidedNormalFlip, scene: scene::materials::two_sided_normal_flip },
	SceneCase { feature: Scene3DFeature::AlphaThresholdDiscard, scene: scene::materials::alpha_threshold_discard },
	SceneCase { feature: Scene3DFeature::QueueFromBlending, scene: scene::materials::queue_from_blending },
	SceneCase { feature: Scene3DFeature::PerDrawBlendState, scene: scene::materials::per_draw_blend_state },
	SceneCase { feature: Scene3DFeature::PerDrawColorWriteMask, scene: scene::materials::per_draw_color_write_mask },
	SceneCase { feature: Scene3DFeature::LightAmbient, scene: scene::lighting::light_ambient },
	SceneCase { feature: Scene3DFeature::LightDirectional, scene: scene::lighting::light_directional },
	SceneCase { feature: Scene3DFeature::LightPoint, scene: scene::lighting::light_point },
	SceneCase { feature: Scene3DFeature::LightSpot, scene: scene::lighting::light_spot },
	SceneCase { feature: Scene3DFeature::LightSelectionOrder, scene: scene::lighting::light_selection_order },
	SceneCase { feature: Scene3DFeature::LightsPerDrawableLimit, scene: scene::lighting::lights_per_drawable_limit },
	SceneCase { feature: Scene3DFeature::PointAttenuation, scene: scene::lighting::point_attenuation },
	SceneCase { feature: Scene3DFeature::SpotConeAttenuation, scene: scene::lighting::spot_cone_attenuation },
	SceneCase { feature: Scene3DFeature::DirectionalNoAttenuation, scene: scene::lighting::directional_no_attenuation },
	SceneCase { feature: Scene3DFeature::ObjectIdAttachment, scene: scene::picking::object_id_attachment },
	SceneCase { feature: Scene3DFeature::ReservedZeroIdentity, scene: scene::picking::reserved_zero_identity },
	SceneCase { feature: Scene3DFeature::ApplicationAssignedIdentity, scene: scene::picking::application_assigned_identity },
	SceneCase { feature: Scene3DFeature::SelectionPass, scene: scene::picking::selection_pass },
	SceneCase { feature: Scene3DFeature::TransparentWritesNoIdentity, scene: scene::picking::transparent_writes_no_identity },
	SceneCase { feature: Scene3DFeature::IdentityReadback, scene: scene::picking::identity_readback },
	SceneCase { feature: Scene3DFeature::DepthReadback, scene: scene::picking::depth_readback },
	SceneCase { feature: Scene3DFeature::ColourReadback, scene: scene::picking::colour_readback },
	SceneCase { feature: Scene3DFeature::AsynchronousReadback, scene: scene::picking::asynchronous_readback },
	SceneCase { feature: Scene3DFeature::PickOutsideAttachmentRefusal, scene: scene::picking::pick_outside_attachment_refusal },
	SceneCase { feature: Scene3DFeature::RenderPassGraph, scene: scene::graph::render_pass_graph },
	SceneCase { feature: Scene3DFeature::DerivedPassOrder, scene: scene::graph::derived_pass_order },
	SceneCase { feature: Scene3DFeature::PassCycleRefusal, scene: scene::graph::pass_cycle_refusal },
	SceneCase { feature: Scene3DFeature::OffscreenTarget, scene: scene::graph::offscreen_target },
	SceneCase { feature: Scene3DFeature::SceneLimits, scene: scene::graph::scene_limits },
	SceneCase { feature: Scene3DFeature::LimitRefusal, scene: scene::graph::limit_refusal },
];

#[cfg(test)]
mod tests;

/// Every scene of the extended profile, by the feature it covers.
///
/// SIXTY ENTRIES FOR SIXTY FEATURES, and the walk above reports any the list misses. A feature added
/// to the profile with no scene here is a failure of this suite, which is what keeps the two in step.
pub const EXTENDED_CASES: &[ExtendedCase] = &[
	ExtendedCase { feature: ExtendedFeature::MaterialPbrMetallicRoughness, scene: extended::material::material_pbr_metallic_roughness },
	ExtendedCase { feature: ExtendedFeature::NormalDistributionGgx, scene: extended::material::normal_distribution_ggx },
	ExtendedCase { feature: ExtendedFeature::VisibilitySmithHeightCorrelated, scene: extended::material::visibility_smith_height_correlated },
	ExtendedCase { feature: ExtendedFeature::FresnelSchlick, scene: extended::material::fresnel_schlick },
	ExtendedCase { feature: ExtendedFeature::DiffuseLambert, scene: extended::material::diffuse_lambert },
	ExtendedCase { feature: ExtendedFeature::EnergyConservingDiffuse, scene: extended::material::energy_conserving_diffuse },
	ExtendedCase { feature: ExtendedFeature::PerceptualRoughnessRemap, scene: extended::material::perceptual_roughness_remap },
	ExtendedCase { feature: ExtendedFeature::MinimumRoughnessClamp, scene: extended::material::minimum_roughness_clamp },
	ExtendedCase { feature: ExtendedFeature::DielectricF0Constant, scene: extended::material::dielectric_f0_constant },
	ExtendedCase { feature: ExtendedFeature::ClampedDotProducts, scene: extended::material::clamped_dot_products },
	ExtendedCase { feature: ExtendedFeature::DirectTermComposition, scene: extended::material::direct_term_composition },
	ExtendedCase { feature: ExtendedFeature::SplitSumApproximation, scene: extended::environment::split_sum_approximation },
	ExtendedCase { feature: ExtendedFeature::GgxPrefilteredEnvironment, scene: extended::environment::ggx_prefiltered_environment },
	ExtendedCase { feature: ExtendedFeature::PrefilterLevelRule, scene: extended::environment::prefilter_level_rule },
	ExtendedCase { feature: ExtendedFeature::BrdfIntegrationTable, scene: extended::environment::brdf_integration_table },
	ExtendedCase { feature: ExtendedFeature::IrradianceTerm, scene: extended::environment::irradiance_term },
	ExtendedCase { feature: ExtendedFeature::ShadowMapDepth32F, scene: extended::shadows::shadow_map_depth32_f },
	ExtendedCase { feature: ExtendedFeature::ShadowDepthBias, scene: extended::shadows::shadow_depth_bias },
	ExtendedCase { feature: ExtendedFeature::PercentageCloserFilter3x3, scene: extended::shadows::percentage_closer_filter_3x3 },
	ExtendedCase { feature: ExtendedFeature::CascadedShadowMaps, scene: extended::shadows::cascaded_shadow_maps },
	ExtendedCase { feature: ExtendedFeature::CascadeSplitBlend, scene: extended::shadows::cascade_split_blend },
	ExtendedCase { feature: ExtendedFeature::PerFragmentCascadeSelection, scene: extended::shadows::per_fragment_cascade_selection },
	ExtendedCase { feature: ExtendedFeature::CascadeTransitionBlend, scene: extended::shadows::cascade_transition_blend },
	ExtendedCase { feature: ExtendedFeature::UnshadowedBeyondLastCascade, scene: extended::shadows::unshadowed_beyond_last_cascade },
	ExtendedCase { feature: ExtendedFeature::CascadeCountRefusal, scene: extended::shadows::cascade_count_refusal },
	ExtendedCase { feature: ExtendedFeature::PointLightCubeShadow, scene: extended::shadows::point_light_cube_shadow },
	ExtendedCase { feature: ExtendedFeature::ShadowProjectionFit, scene: extended::shadows::shadow_projection_fit },
	ExtendedCase { feature: ExtendedFeature::HdrTargetFormat, scene: extended::postprocess::hdr_target_format },
	ExtendedCase { feature: ExtendedFeature::BloomSoftKnee, scene: extended::postprocess::bloom_soft_knee },
	ExtendedCase { feature: ExtendedFeature::BloomPyramid, scene: extended::postprocess::bloom_pyramid },
	ExtendedCase { feature: ExtendedFeature::Rec709Luminance, scene: extended::postprocess::rec709_luminance },
	ExtendedCase { feature: ExtendedFeature::ToneMapExtendedReinhard, scene: extended::postprocess::tone_map_extended_reinhard },
	ExtendedCase { feature: ExtendedFeature::FogExponentialSquared, scene: extended::postprocess::fog_exponential_squared },
	ExtendedCase { feature: ExtendedFeature::FixedPostprocessOrder, scene: extended::postprocess::fixed_postprocess_order },
	ExtendedCase { feature: ExtendedFeature::LinearBlendSkinning, scene: extended::animation::linear_blend_skinning },
	ExtendedCase { feature: ExtendedFeature::FourInfluencesPerVertex, scene: extended::animation::four_influences_per_vertex },
	ExtendedCase { feature: ExtendedFeature::WeightNormalisationAtLoad, scene: extended::animation::weight_normalisation_at_load },
	ExtendedCase { feature: ExtendedFeature::JointInverseBindComposition, scene: extended::animation::joint_inverse_bind_composition },
	ExtendedCase { feature: ExtendedFeature::MorphTargets, scene: extended::animation::morph_targets },
	ExtendedCase { feature: ExtendedFeature::MorphBeforeSkinning, scene: extended::animation::morph_before_skinning },
	ExtendedCase { feature: ExtendedFeature::LinearTranslationScaleKeys, scene: extended::animation::linear_translation_scale_keys },
	ExtendedCase { feature: ExtendedFeature::SphericalLinearRotationKeys, scene: extended::animation::spherical_linear_rotation_keys },
	ExtendedCase { feature: ExtendedFeature::ShorterArcRotation, scene: extended::animation::shorter_arc_rotation },
	ExtendedCase { feature: ExtendedFeature::RootMotionExtraction, scene: extended::animation::root_motion_extraction },
	ExtendedCase { feature: ExtendedFeature::LoopSeamRefusal, scene: extended::animation::loop_seam_refusal },
	ExtendedCase { feature: ExtendedFeature::StepInterpolation, scene: extended::animation::step_interpolation },
	ExtendedCase { feature: ExtendedFeature::CubicHermiteInterpolation, scene: extended::animation::cubic_hermite_interpolation },
	ExtendedCase { feature: ExtendedFeature::CubicTangentsPerSecond, scene: extended::animation::cubic_tangents_per_second },
	ExtendedCase { feature: ExtendedFeature::ClampEnding, scene: extended::animation::clamp_ending },
	ExtendedCase { feature: ExtendedFeature::PingPongEnding, scene: extended::animation::ping_pong_ending },
	ExtendedCase { feature: ExtendedFeature::PoseBlending, scene: extended::animation::pose_blending },
	ExtendedCase { feature: ExtendedFeature::BlendKeepsUndrivenTargets, scene: extended::animation::blend_keeps_undriven_targets },
	ExtendedCase { feature: ExtendedFeature::BlendedRootMotion, scene: extended::animation::blended_root_motion },
	ExtendedCase { feature: ExtendedFeature::MorphWeightTracks, scene: extended::animation::morph_weight_tracks },
	ExtendedCase { feature: ExtendedFeature::ScreenCoverageLod, scene: extended::detail::screen_coverage_lod },
	ExtendedCase { feature: ExtendedFeature::LodThresholdLadder, scene: extended::detail::lod_threshold_ladder },
	ExtendedCase { feature: ExtendedFeature::LodHysteresis, scene: extended::detail::lod_hysteresis },
	ExtendedCase { feature: ExtendedFeature::LastLodBeyondLadder, scene: extended::detail::last_lod_beyond_ladder },
	ExtendedCase { feature: ExtendedFeature::LodCullBelowCoverage, scene: extended::detail::lod_cull_below_coverage },
	ExtendedCase { feature: ExtendedFeature::DynamicBoundsAfterDeformation, scene: extended::detail::dynamic_bounds_after_deformation },
	ExtendedCase { feature: ExtendedFeature::ExtendedLimits, scene: extended::limits::extended_limits },
	ExtendedCase { feature: ExtendedFeature::ExtendedLimitRefusal, scene: extended::limits::extended_limit_refusal },
];

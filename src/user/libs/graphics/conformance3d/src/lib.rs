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
use graphics_profile::{RENDER3D_CORE_PROFILE_1, Render3DFeature, SCENE3D_CORE_PROFILE_1, Scene3DFeature};

#[macro_use]
mod harness;
mod depth;
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
}

impl Summary {
	/// A run passes only when every feature of BOTH profiles has a scene and every scene passes.
	pub fn complete(&self) -> bool {
		self.render3d.complete() && self.scene3d.complete()
	}

	pub fn passed(&self) -> usize {
		self.render3d.passed + self.scene3d.passed
	}

	pub fn failed(&self) -> usize {
		self.render3d.failed + self.scene3d.failed
	}

	pub fn unsupported(&self) -> usize {
		self.render3d.unsupported + self.scene3d.unsupported
	}

	pub fn untested(&self) -> usize {
		self.render3d.untested.len() + self.scene3d.untested.len()
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

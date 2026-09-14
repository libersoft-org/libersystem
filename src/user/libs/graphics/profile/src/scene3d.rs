//! `Scene3D Core Profile 1`: THE RETAINED LAYER'S OWN CONTRACT.
//!
//! `render3d` draws what it is told. A scene layer decides WHAT to tell it, and every decision it
//! makes is one an application can see: the order two transparent objects are drawn in, which of two
//! lights is applied first, whether a bounding sphere is in world space or local space, and what a
//! pick at a pixel answers when two objects meet there. A scene layer whose answers are not stated
//! is a scene layer whose picture changes when its implementation is optimised.
//!
//! WHAT IS CORE AND WHAT IS EXTENDED. Core is what an ordinary application needs to draw a lit
//! scene: a hierarchy, a camera, four materials, unshadowed lights, culling, instancing and
//! picking. Physically based materials, shadows, skinning, animation and post-processing are
//! `Scene3D Extended 1` - a separate profile with a separate hash, so an implementation can conform
//! to the core without claiming the rest.

use crate::render3d_spec::Rule;
use crate::{FeatureOwner, ProfileEntry};

/// The node and hierarchy model.
pub const HIERARCHY_RULES: &[Rule] = &[
	Rule { question: "what a node is", answer: "a local transform, an optional mesh-and-material pair, an optional camera or light, and an ordered list of children. A node with no drawable is a transform and is drawn as nothing rather than being a different kind of object" },
	Rule { question: "transform composition order", answer: "world = parent_world * local, with the local transform composed as TRANSLATION * ROTATION * SCALE. The order is stated because the three do not commute: scaling after rotating shears, and a scene authored under one order looks wrong under the other" },
	Rule { question: "the rotation's representation", answer: "a unit quaternion, and it is NORMALISED on composition. Euler angles are ambiguous in their order and a matrix drifts away from orthonormal as it is composed, which shows up as a slowly shearing child" },
	Rule { question: "non-uniform scale and normals", answer: "a normal is transformed by the INVERSE TRANSPOSE of the upper 3x3 and renormalised. Using the matrix itself is correct only for uniform scale, and a scene with one squashed object is where it stops being correct" },
	Rule { question: "when a world transform is computed", answer: "lazily, once per frame per dirty subtree, in a single traversal. A parent's change dirties its whole subtree, and the traversal is depth-first in child order so the result does not depend on when the dirty flag was set" },
	Rule { question: "a cycle in the hierarchy", answer: "REFUSED when the parent is set, not discovered during traversal. A traversal that discovered it would already be in an infinite loop" },
	Rule { question: "the maximum depth", answer: "`max_hierarchy_depth`, checked when the parent is set, so a traversal needs no depth counter and cannot overflow a stack" },
];

/// The camera, and the conventions that decide where things end up.
pub const CAMERA_RULES: &[Rule] = &[
	Rule { question: "the view transform", answer: "the INVERSE of the camera node's world transform. A camera is a node like any other, so moving its parent moves it" },
	Rule { question: "the projection", answer: "right-handed VIEW space with -z forward, mapping to the clip volume `Render3D Profile 1` fixes: z in [0, w], y down in NDC" },
	Rule { question: "perspective parameters", answer: "vertical field of view in radians, aspect ratio as width over height, near and far. Vertical rather than horizontal because a window that changes width then keeps the same amount of vertical content, which is what a person expects" },
	Rule { question: "an infinite far plane", answer: "permitted and expressed as a far of infinity, which with a [0, w] depth range is well conditioned - the depth precision is spent near the camera either way" },
	Rule { question: "orthographic parameters", answer: "half-width, half-height, near and far, centred on the view axis" },
	Rule { question: "a degenerate projection", answer: "REFUSED at the call that sets it: a zero or negative near, a far at or below near, a non-finite field of view, or an aspect of zero. A degenerate projection produces a matrix of infinities and every vertex after it is a NaN" },
];

/// The render queues, which is where drawing order is decided.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Queue {
	pub name: &'static str,
	pub contents: &'static str,
	pub order: &'static str,
	pub depth_write: bool,
}

pub const QUEUES: &[Queue] = &[
	Queue { name: "Opaque", contents: "every drawable whose material is fully opaque", order: "FRONT TO BACK by the distance from the camera to the drawable's bounding-sphere centre, so the depth test rejects the most fragments", depth_write: true },
	Queue { name: "AlphaMask", contents: "drawables whose material discards fragments by an alpha threshold", order: "front to back, after `Opaque`, because a discard defeats early depth rejection and running it later means fewer of them", depth_write: true },
	Queue { name: "Transparent", contents: "drawables whose material blends", order: "BACK TO FRONT by the same distance, and the sort is STABLE so two drawables at the same distance keep their submission order", depth_write: false },
];

pub const QUEUE_RULES: &[Rule] = &[
	Rule { question: "how a drawable is assigned", answer: "by its MATERIAL's declared blending, not by a flag on the node. A node's queue therefore changes when its material does, and the two can never disagree" },
	Rule { question: "the sort key", answer: "the distance from the camera POSITION to the drawable's world-space bounding-sphere CENTRE. Not the nearest point of the bounds, which makes a large object sort ahead of a small one it contains" },
	Rule { question: "ties", answer: "broken by submission order, and the sort is stable - so a frame with two coincident objects draws them the same way every frame rather than flickering" },
	Rule { question: "transparent depth", answer: "tested against the depth written by the two opaque queues and NOT written. Writing it would make a transparent surface hide the one behind it, which is the commonest transparency bug" },
	Rule { question: "the order of the queues themselves", answer: "Opaque, then AlphaMask, then Transparent, and a scene may not reorder them" },
];

/// Bounding volumes and culling.
pub const CULLING_RULES: &[Rule] = &[
	Rule { question: "the bounding volume", answer: "an axis-aligned box in LOCAL space per mesh, and a SPHERE in world space per drawable derived from it. The box is what a mesh can state exactly; the sphere is what survives an arbitrary transform without growing every frame" },
	Rule { question: "how the world sphere is derived", answer: "the local box's centre transformed by the world matrix, with the radius scaled by the LARGEST of the three axis scale factors. Recomputing a tight box per frame turns a rotation into a growing box - the classic bounds that inflate until everything is visible" },
	Rule { question: "the frustum test", answer: "the sphere against six planes, in the order near, far, left, right, bottom, top, rejecting on the first plane the sphere is entirely outside. Near first because it rejects the most in an ordinary scene" },
	Rule { question: "the plane form", answer: "each plane normalised and pointing INWARD, so `dot(plane.xyz, centre) + plane.w < -radius` is the rejection. Normalised because the comparison is against a radius in world units" },
	Rule { question: "a drawable with no bounds", answer: "is NEVER culled. An unbounded drawable is one the scene cannot reason about, and dropping it would make it disappear for a reason nobody can see" },
	Rule { question: "whether culling is observable", answer: "only in performance. A culled drawable must not change the picture, so the test is conservative: a sphere partly inside is kept" },
];

/// The instancing model.
pub const INSTANCING_RULES: &[Rule] = &[
	Rule { question: "what an instance is", answer: "one world transform and one colour, in a per-instance vertex stream. Not a node: a node has a hierarchy and an identity, and a million of them is a million traversals" },
	Rule { question: "what instances share", answer: "the mesh, the material and the queue. Two instances that need different materials are two drawables" },
	Rule { question: "culling", answer: "per INSTANCE against the frustum, with the surviving instances compacted into the stream in their original order. Culling the whole set would draw a city because one building is visible" },
	Rule { question: "sorting", answer: "an instanced drawable sorts ONCE, by the bounding sphere of the whole set. Sorting instances against each other would break the single draw that instancing exists to produce" },
	Rule { question: "transparent instancing", answer: "PERMITTED AND DOCUMENTED AS APPROXIMATE: instances within one set are not sorted against each other, so overlapping transparent instances may composite in the wrong order. Refusing it would make grass and particles impossible; hiding the limitation would make the wrong picture a mystery" },
];

/// One core material, and what it computes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Material {
	pub name: &'static str,
	pub inputs: &'static str,
	pub equation: &'static str,
}

pub const MATERIALS: &[Material] = &[
	Material { name: "Unlit", inputs: "base colour, optional base-colour texture, optional alpha threshold", equation: "colour = base_colour * texture(uv); no light is applied and no normal is needed" },
	Material { name: "VertexColor", inputs: "the vertex colour attribute, optional base-colour texture", equation: "colour = vertex_colour * texture(uv); unlit, and the vertex colour is interpolated with the `smooth` qualifier" },
	Material { name: "Lambert", inputs: "base colour, optional base-colour texture, normal", equation: "colour = base_colour * texture(uv) * (ambient + sum over lights of light_colour * max(dot(N, L), 0) * attenuation)" },
	Material { name: "BlinnPhong", inputs: "base colour, optional base-colour texture, normal, specular colour, shininess", equation: "colour = base * (ambient + sum of light_colour * max(dot(N, L), 0) * attenuation) + specular * sum of light_colour * pow(max(dot(N, H), 0), shininess) * attenuation, where H = normalize(L + V), N is the normalised interpolated normal, L points from the surface to the light and V from the surface to the camera. The half vector rather than the reflection vector, which is what makes it Blinn-Phong rather than Phong and what keeps the highlight from vanishing at grazing angles" },
];

pub const MATERIAL_RULES: &[Rule] = &[
	Rule { question: "the colour space the equation is evaluated in", answer: "LINEAR. A texture declared as sRGB is decoded before it is multiplied, and the result is encoded once at the end by the image profile's rule. Lighting in an encoded space is the classic too-dark shadow" },
	Rule { question: "the normal", answer: "interpolated with `smooth`, then RENORMALISED in the fragment stage. Interpolating unit vectors does not produce unit vectors, and the error is largest in the middle of a large triangle" },
	Rule { question: "a two-sided surface", answer: "the normal is flipped for a back-facing fragment. Without it the lit side of a leaf is the side away from the light" },
	Rule { question: "the alpha threshold", answer: "a fragment with alpha strictly below the threshold is discarded, so a threshold of 0 discards nothing" },
];

/// Lighting, and the accumulation rule that decides whether two implementations agree.
pub const LIGHTING_RULES: &[Rule] = &[
	Rule { question: "the light kinds", answer: "ambient, directional, point and spot. A light is a node, so it moves with its parent" },
	Rule { question: "how many", answer: "no fixed count: up to `max_lights_per_drawable` affect one drawable, and the scene may hold `max_lights`" },
	Rule { question: "which lights affect a drawable", answer: "the ones whose volume intersects its bounding sphere, ordered by DESCENDING contribution - a directional light first, then point and spot lights by irradiance at the sphere's centre - and truncated at the per-drawable maximum. The order is stated because floating-point addition is not associative: two implementations that accumulate in different orders produce different colours" },
	Rule { question: "accumulation", answer: "summed in linear space in that order, starting from the ambient term. Unshadowed in the core profile: shadows are Extended" },
	Rule { question: "point attenuation", answer: "`1 / (1 + d^2 / r^2)` with a smooth cut-off to zero at the light's range, where d is the distance and r the light's radius. The cut-off is a multiplicative `saturate(1 - (d/range)^4)^2` so the light reaches exactly zero at its range rather than leaving a visible edge" },
	Rule { question: "spot attenuation", answer: "the point attenuation multiplied by `smoothstep(cos(outer), cos(inner), dot(-L, spot_direction))`, so the cone edge is soft and an inner angle equal to the outer gives a hard edge rather than a division by zero" },
	Rule { question: "directional attenuation", answer: "none: a directional light is infinitely far away" },
];

/// Picking and readback, which an editor, a CAD viewport and any application with a cursor needs.
pub const PICKING_RULES: &[Rule] = &[
	Rule { question: "what is read", answer: "an integer attachment written by the same pass that wrote the colour, holding a per-drawable id. Reading the depth and unprojecting instead answers WHERE and not WHAT" },
	Rule { question: "the id", answer: "a 32-bit unsigned value the scene assigns and the application may set. Zero is reserved for NOTHING, so a pick on the background is unambiguous" },
	Rule { question: "what a pick answers when two drawables meet at a pixel", answer: "the one that PASSED the depth test, which is the nearest opaque one. A transparent drawable does not write the id attachment, so a pick through glass answers what is behind it" },
	Rule { question: "when the result is available", answer: "with the frame's completion, as a typed readback. Not synchronously: a synchronous pick stalls the pipeline for a cursor" },
	Rule { question: "a pick outside the attachment", answer: "REFUSED rather than clamped, because a clamped pick answers about a pixel the caller did not ask about" },
];

/// One guaranteed minimum for the scene layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SceneLimit {
	pub name: &'static str,
	pub minimum: u32,
	pub why: &'static str,
}

pub const SCENE3D_PROFILE_1_MIN_LIMITS: &[SceneLimit] = &[
	SceneLimit { name: "max_nodes", minimum: 65536, why: "a room-scale scene with props, and one order of magnitude above what a demo needs" },
	SceneLimit { name: "max_hierarchy_depth", minimum: 64, why: "deep enough for an articulated model and shallow enough that a recursive traversal cannot overflow a stack" },
	SceneLimit { name: "max_drawables", minimum: 16384, why: "a quarter of the node budget being drawable is an ordinary ratio" },
	SceneLimit { name: "max_instances_per_drawable", minimum: 4096, why: "grass, foliage and crowd instancing at a useful density" },
	SceneLimit { name: "max_lights", minimum: 256, why: "a lit interior with one light per fixture" },
	SceneLimit { name: "max_lights_per_drawable", minimum: 8, why: "what a forward material can accumulate without the shader-instruction floor being the binding constraint" },
	SceneLimit { name: "max_materials", minimum: 4096, why: "one per distinct surface in a detailed scene" },
	SceneLimit { name: "max_cameras", minimum: 8, why: "a main view, a picture-in-picture, a reflection and room to spare" },
];

/// One feature of `Scene3D Core Profile 1`.
///
/// THE GRANULARITY IS "SOMETHING AN IMPLEMENTATION CAN FAIL TO DO", as in the two profiles below it.
/// The three queues are separate variants because a layer can carry the opaque one and get the
/// transparent one's direction wrong, and a matrix that could only say "queues" would call that
/// conforming. The same reasoning splits the four light kinds, the four materials and the three
/// readbacks.
///
/// WHAT IS DELIBERATELY NOT HERE. Everything in `Scene3D Extended 1` - physically based materials,
/// shadows, skinning, animation, post-processing - is a separate closed list with a separate hash,
/// so an implementation conforms to the core without claiming the rest. And the command model below
/// this layer is `Render3D Core Profile 1`: a scene feature is a DECISION about what to draw, and a
/// backend feature is an ability to draw it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scene3DFeature {
	// The hierarchy.
	Node,
	ParentChildTransform,
	LocalTransformOrder,
	QuaternionRotation,
	NormalMatrix,
	LazyWorldTransforms,
	HierarchyCycleRefusal,
	HierarchyDepthLimit,
	VisibilityMask,
	NodeEnable,
	// The camera.
	PerspectiveCamera,
	OrthographicCamera,
	InfiniteFarPlane,
	ViewFromNodeTransform,
	CustomProjection,
	DegenerateProjectionRefusal,
	// The queues.
	OpaqueQueue,
	AlphaMaskQueue,
	TransparentQueue,
	DistanceSortKey,
	StableTiebreak,
	FixedQueueOrder,
	TransparentDepthNoWrite,
	// Bounds and culling.
	LocalBoundingBox,
	WorldBoundingSphere,
	FrustumPlaneExtraction,
	SphereFrustumTest,
	BoxFrustumTest,
	FixedPlaneTestOrder,
	UnboundedNeverCulled,
	// Instancing.
	InstanceStream,
	PerInstanceCulling,
	InstanceCompaction,
	SingleSortPerInstanceSet,
	TransparentInstancingApproximate,
	// Materials.
	MaterialUnlit,
	MaterialVertexColor,
	MaterialLambert,
	MaterialBlinnPhong,
	LinearColourSpace,
	NormalRenormalisation,
	TwoSidedNormalFlip,
	AlphaThresholdDiscard,
	QueueFromBlending,
	PerDrawBlendState,
	PerDrawColorWriteMask,
	// Lighting.
	LightAmbient,
	LightDirectional,
	LightPoint,
	LightSpot,
	LightSelectionOrder,
	LightsPerDrawableLimit,
	PointAttenuation,
	SpotConeAttenuation,
	DirectionalNoAttenuation,
	// Picking and readback.
	ObjectIdAttachment,
	ReservedZeroIdentity,
	ApplicationAssignedIdentity,
	SelectionPass,
	TransparentWritesNoIdentity,
	IdentityReadback,
	DepthReadback,
	ColourReadback,
	AsynchronousReadback,
	PickOutsideAttachmentRefusal,
	// Passes and limits.
	RenderPassGraph,
	DerivedPassOrder,
	PassCycleRefusal,
	OffscreenTarget,
	SceneLimits,
	LimitRefusal,
}

profile! {
	/// `Scene3D Core Profile 1`, closed and enumerated.
	SCENE3D_CORE_PROFILE_1: Scene3DFeature;
	"hierarchy", Scene3D, Node;
	"hierarchy", Scene3D, ParentChildTransform;
	"hierarchy", Scene3D, LocalTransformOrder;
	"hierarchy", Scene3D, QuaternionRotation;
	"hierarchy", Scene3D, NormalMatrix;
	"hierarchy", Scene3D, LazyWorldTransforms;
	"hierarchy", Scene3D, HierarchyCycleRefusal;
	"hierarchy", Scene3D, HierarchyDepthLimit;
	"hierarchy", Scene3D, VisibilityMask;
	"hierarchy", Scene3D, NodeEnable;
	"camera", Scene3D, PerspectiveCamera;
	"camera", Scene3D, OrthographicCamera;
	"camera", Scene3D, InfiniteFarPlane;
	"camera", Scene3D, ViewFromNodeTransform;
	"camera", Scene3D, CustomProjection;
	"camera", Scene3D, DegenerateProjectionRefusal;
	"queues", Scene3D, OpaqueQueue;
	"queues", Scene3D, AlphaMaskQueue;
	"queues", Scene3D, TransparentQueue;
	"queues", Scene3D, DistanceSortKey;
	"queues", Scene3D, StableTiebreak;
	"queues", Scene3D, FixedQueueOrder;
	"queues", Scene3D, TransparentDepthNoWrite;
	"culling", Scene3D, LocalBoundingBox;
	"culling", Scene3D, WorldBoundingSphere;
	"culling", Scene3D, FrustumPlaneExtraction;
	"culling", Scene3D, SphereFrustumTest;
	"culling", Scene3D, BoxFrustumTest;
	"culling", Scene3D, FixedPlaneTestOrder;
	"culling", Scene3D, UnboundedNeverCulled;
	"instancing", Scene3D, InstanceStream;
	"instancing", Scene3D, PerInstanceCulling;
	"instancing", Scene3D, InstanceCompaction;
	"instancing", Scene3D, SingleSortPerInstanceSet;
	"instancing", Scene3D, TransparentInstancingApproximate;
	"materials", Scene3D, MaterialUnlit;
	"materials", Scene3D, MaterialVertexColor;
	"materials", Scene3D, MaterialLambert;
	"materials", Scene3D, MaterialBlinnPhong;
	"materials", Scene3D, LinearColourSpace;
	"materials", Scene3D, NormalRenormalisation;
	"materials", Scene3D, TwoSidedNormalFlip;
	"materials", Scene3D, AlphaThresholdDiscard;
	"materials", Scene3D, QueueFromBlending;
	"materials", Scene3D, PerDrawBlendState;
	"materials", Scene3D, PerDrawColorWriteMask;
	"lighting", Scene3D, LightAmbient;
	"lighting", Scene3D, LightDirectional;
	"lighting", Scene3D, LightPoint;
	"lighting", Scene3D, LightSpot;
	"lighting", Scene3D, LightSelectionOrder;
	"lighting", Scene3D, LightsPerDrawableLimit;
	"lighting", Scene3D, PointAttenuation;
	"lighting", Scene3D, SpotConeAttenuation;
	"lighting", Scene3D, DirectionalNoAttenuation;
	"picking", Scene3D, ObjectIdAttachment;
	"picking", Scene3D, ReservedZeroIdentity;
	"picking", Scene3D, ApplicationAssignedIdentity;
	"picking", Scene3D, SelectionPass;
	"picking", Scene3D, TransparentWritesNoIdentity;
	"picking", Backend, IdentityReadback;
	"picking", Backend, DepthReadback;
	"picking", Backend, ColourReadback;
	"picking", Backend, AsynchronousReadback;
	"picking", Scene3D, PickOutsideAttachmentRefusal;
	"passes", Scene3D, RenderPassGraph;
	"passes", Scene3D, DerivedPassOrder;
	"passes", Scene3D, PassCycleRefusal;
	"passes", Backend, OffscreenTarget;
	"limits", Scene3D, SceneLimits;
	"limits", Scene3D, LimitRefusal;
}

/// The groups, in the order the profile documents them.
pub const SCENE3D_GROUPS: &[&str] = &["hierarchy", "camera", "queues", "culling", "instancing", "materials", "lighting", "picking", "passes", "limits"];

/// Is this feature in Profile 1?
pub fn in_profile_1(feature: Scene3DFeature) -> bool {
	SCENE3D_CORE_PROFILE_1.iter().any(|entry| entry.feature == feature)
}

/// The entry for a feature named as the profile spells it.
pub fn entry_by_name(name: &str) -> Option<&'static ProfileEntry<Scene3DFeature>> {
	SCENE3D_CORE_PROFILE_1.iter().find(|entry| entry.name == name)
}

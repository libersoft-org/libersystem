//! `Scene3D Extended 1`: THE EQUATIONS THAT MAKE TWO RENDERERS AGREE ABOUT A SURFACE.
//!
//! Everything here is optional to conform to and exact to implement. A physically based material is
//! four or five functions multiplied together, and every published renderer chooses slightly
//! different ones - a different normal distribution, a different visibility term, a different
//! roughness remapping - so two implementations of "PBR" produce visibly different metal. The
//! difference is not a quality setting; it is a different material.
//!
//! IT IS A SEPARATE PROFILE WITH A SEPARATE HASH, so an implementation conforms to `Scene3D Core
//! Profile 1` without claiming any of this, and a scene that uses it says so.

use crate::render3d_spec::Rule;
use crate::{FeatureOwner, ProfileEntry};

/// One term of the physically based material, written as the equation rather than named.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Term {
	pub name: &'static str,
	pub equation: &'static str,
	pub why: &'static str,
}

pub const PBR_TERMS: &[Term] = &[
	Term { name: "normal distribution (GGX / Trowbridge-Reitz)", equation: "D(h) = a^2 / (pi * ((dot(N,H)^2 * (a^2 - 1) + 1))^2), with a = roughness^2", why: "GGX rather than Beckmann because its tail is longer, which is what makes a rough metal look rough rather than plastic; `a = roughness^2` is the remapping that makes the perceptual slider linear" },
	Term { name: "visibility (Smith height-correlated)", equation: "V = 0.5 / (dot(N,L) * sqrt(dot(N,V)^2 * (1 - a^2) + a^2) + dot(N,V) * sqrt(dot(N,L)^2 * (1 - a^2) + a^2))", why: "the height-correlated form, and it INCLUDES the 1/(4 dot(N,L) dot(N,V)) denominator of the specular BRDF - which is why the whole specular term is D * V * F and not D * G * F / (4 ...). Stating which convention the term carries is the difference between a correct highlight and one four times too bright" },
	Term { name: "Fresnel (Schlick)", equation: "F = F0 + (1 - F0) * (1 - dot(V,H))^5, with F0 = mix(vec3(0.04), base_colour, metallic)", why: "the 0.04 is the reflectance of an ordinary dielectric at normal incidence; a metal's F0 IS its base colour, which is what `metallic` selects between" },
	Term { name: "diffuse (Lambert)", equation: "diffuse = (1 - metallic) * base_colour / pi, multiplied by (1 - F) so energy is not created", why: "Lambert rather than Oren-Nayar or Disney: it is the one every reference implementation shares, and the others are a different material" },
	Term { name: "the whole direct term", equation: "colour = (diffuse + D * V * F) * light_colour * dot(N,L) * attenuation", why: "written out because the order and the placement of `dot(N,L)` is exactly what implementations differ about" },
];

pub const PBR_CONSTANTS: &[Rule] = &[
	Rule { question: "the minimum roughness", answer: "0.045, clamped before `a = roughness^2` is taken. A roughness of zero makes `D` a delta function, which is an infinite highlight at one pixel and a NaN in a filtered environment lookup" },
	Rule { question: "the roughness remapping", answer: "`a = roughness^2`, and the material's roughness input is PERCEPTUAL. A renderer that fed the perceptual value straight into `D` produces a material that goes from mirror to matte in the first quarter of the slider" },
	Rule { question: "the dielectric F0", answer: "0.04 for every dielectric. A per-material specular level is Extended-of-Extended; one constant covers every non-metal a scene actually has" },
	Rule { question: "clamping", answer: "every dot product in the equations above is clamped to [0, 1] before use, and `dot(N,V)` is clamped to at least 1e-4 so the visibility term cannot divide by zero at a grazing angle" },
	Rule { question: "the colour space", answer: "LINEAR throughout, as in the core profile" },
];

/// The material's INPUTS, as distinct from the equations that consume them.
///
/// THE EQUATIONS ABOVE ARE HALF OF A MATERIAL. The other half is what is fed into them, and it is
/// where two implementations that agree about GGX still disagree about the picture: one decodes the
/// roughness map through a transfer function and the other does not, one applies occlusion to a
/// light the scene placed and the other does not, one reads a normal map written for the other
/// handedness. None of those is a quality setting either.
///
/// THE DATA MODEL IS glTF's, DELIBERATELY. A material model that cannot consume the assets the world
/// already has is a material model for one demo, so the maps are the five glTF names and the alpha
/// modes are its three.
pub const PBR_MATERIAL_RULES: &[Rule] = &[
	Rule { question: "the base colour under `metallic`", answer: "ONE input with two meanings, mixed by `metallic`: at 0 it is the diffuse albedo and the surface's F0 is the dielectric 0.04; at 1 it IS the metal's F0 and there is no diffuse term at all. That is what `(1 - metallic)` on the diffuse and `mix(vec3(0.04), base_colour, metallic)` on F0 say between them, and stating it here is what stops an implementation keeping a dim diffuse term on a metal" },
	Rule { question: "occlusion", answer: "applied as `mix(1, sampled, strength)` to the AMBIENT and ENVIRONMENT terms ONLY, and never to direct light. Ambient occlusion is a statement about how much of the sky a point can see; applying it to a light the scene placed would darken a surface that light demonstrably reaches, which is a shadow the scene did not ask for and cannot remove" },
	Rule { question: "emissive", answer: "LINEAR RADIANCE, added after the direct and environment terms, unattenuated and untouched by occlusion or by shadow. An emissive surface is a SOURCE and not a receiver, so nothing that describes how much light reaches it applies" },
	Rule { question: "the normal map convention", answer: "TANGENT SPACE WITH +Y UP, the OpenGL convention, sampled as `2 * texel - 1`. The tangent frame is right-handed and the bitangent is `cross(normal, tangent) * tangent.w`, with `w` either +1 or -1 as glTF stores it. Stated because the other convention inverts every crevice into a bump: the surface still looks lit, and it looks lit from the wrong side" },
	Rule { question: "a missing tangent", answer: "the normal map is NOT APPLIED, and the material draws with its interpolated normal. Deriving a tangent from screen-space derivatives is the common alternative and produces a frame that depends on the rasteriser's derivative rule, which is a different picture per backend for a mesh whose author simply did not export tangents" },
	Rule { question: "the colour space of each map", answer: "base colour and emissive are `Color` and ARE transfer-decoded to linear; metallic-roughness, normal and occlusion are `Data` and are NEVER decoded. This is the one rule that costs nothing to get wrong and changes every pixel: decoding a normal map bends every normal toward the surface, and decoding a roughness map makes a whole material glossier" },
	Rule { question: "the metallic-roughness packing", answer: "glTF's: ROUGHNESS in the green channel and METALLIC in the blue, with red and alpha ignored. Two channels of one texture rather than two textures, and naming which is which because a renderer that swapped them would produce a rough metal wherever the author wrote a smooth dielectric" },
	Rule { question: "the alpha modes", answer: "`Opaque`, `Mask` and `Blend`, which are the core profile's three under glTF's names. `Mask` discards STRICTLY BELOW its threshold, so a threshold of zero discards nothing; the threshold is the material's own and is not a global" },
	Rule { question: "a double-sided surface", answer: "flips the interpolated normal on a back-facing fragment BEFORE anything else reads it - the normal map, the lighting and the environment lookup all see the flipped one. Flipping after shading would light the leaf's underside as though it were its top" },
];

/// Environment lighting: the prefilter and the lookup table, which are two halves of one
/// approximation and are useless if the two ends disagree.
pub const ENVIRONMENT_RULES: &[Rule] = &[
	Rule { question: "the split-sum approximation", answer: "the environment term is `prefiltered(R, roughness) * (F0 * brdf.x + brdf.y)`, where `brdf` is looked up by `(dot(N,V), roughness)`. Both halves are stated because a prefilter built for one lookup table is wrong with another" },
	Rule { question: "the prefilter", answer: "a cube map whose mip level i is the GGX-importance-sampled radiance at `roughness = i / (levels - 1)`, with 1024 samples per texel and the source sampled at a mip chosen by the sample's own solid angle - which is what removes the fireflies a naive prefilter has" },
	Rule { question: "the number of levels", answer: "as many as the cube's size allows down to 8x8; below that the filter is wider than the face and the result is the average of the whole environment anyway" },
	Rule { question: "the BRDF lookup table", answer: "a 2-channel 16-bit table, 256 by 256, `x` = dot(N,V) and `y` = roughness, generated by the same GGX and Smith terms above. Its generation is part of this profile, so two implementations produce the same table" },
	Rule { question: "the irradiance term", answer: "a separate cube map holding the cosine-convolved irradiance, sampled by `N`. Nine spherical-harmonic coefficients are the permitted alternative and produce the same result to within the conformance threshold" },
];

/// Shadows.
pub const SHADOW_RULES: &[Rule] = &[
	Rule { question: "the map format", answer: "`Depth32F`, with the comparison rule the 3D profile fixes: the stored floats are compared and the incoming depth is not quantised" },
	Rule { question: "the bias", answer: "constant 0.0015 and slope 2.0, applied through the 3D profile's depth-bias equation, with a clamp of 0.01. Named values rather than 'tune it': a scene tuned against an unstated bias acnes on the next implementation" },
	Rule { question: "the filter", answer: "a 3x3 percentage-closer kernel at the map's texel size with EQUAL weights, using the compare-then-filter rule the sampler contract fixes. Equal weights rather than a Gaussian because the sample positions are already a box" },
	Rule { question: "the cascade decision", answer: "by VIEW-SPACE DEPTH against fixed split distances, computed as a blend of the uniform and logarithmic splits with lambda 0.5, and the cascade index is chosen by the fragment's own depth rather than by the drawable's - a large object spans cascades" },
	Rule { question: "the cascade count", answer: "up to `max_shadow_cascades`, and a scene that asks for more is REFUSED rather than silently given fewer" },
	Rule { question: "the transition between cascades", answer: "a blend over the last 10 per cent of each cascade's range, so the seam is a gradient rather than a line" },
	Rule { question: "what is outside the last cascade", answer: "UNSHADOWED. Extending the last cascade to infinity makes its texels useless everywhere; saying so lets a scene choose its far distance" },
	Rule { question: "a point light's shadow", answer: "a cube map with the same bias and filter, its face chosen by the major axis of the light-to-fragment vector as the 3D profile's cube orientation defines" },
];

/// Post-processing.
pub const POSTPROCESS_RULES: &[Rule] = &[
	Rule { question: "the bloom threshold", answer: "a soft knee at luminance 1.0 with a knee width of 0.5: below 0.5 nothing, above 1.5 the whole excess, and a quadratic between. A hard threshold makes a moving highlight pop in and out" },
	Rule { question: "the luminance used", answer: "`dot(colour, vec3(0.2126, 0.7152, 0.0722))` in linear Rec. 709 primaries, which is the same luminance the image-colour profile uses" },
	Rule { question: "the bloom kernel", answer: "a progressive downsample with a 13-tap filter and an upsample with a 9-tap tent, six levels, combined with weight 0.04 by default. Stated because a bloom built from a different pyramid has a different shape at the same 'intensity'" },
	Rule { question: "tone mapping", answer: "the SAME operator the 2D image-colour profile fixes - extended Reinhard with WHITE = 4.0 - applied in linear space before encoding. Two operators in one system is two systems" },
	Rule { question: "the fog equation", answer: "exponential-squared: `f = exp(-(density * distance)^2)`, with distance the view-space depth and the result mixing toward the fog colour. Squared rather than linear because it has no visible start plane" },
	Rule { question: "the order", answer: "tone mapping is LAST, after bloom and fog, because both are defined on linear radiance and a tone-mapped input would compress the highlights they exist to spread" },
];

/// Animation and skinning.
pub const ANIMATION_RULES: &[Rule] = &[
	Rule { question: "the skinning model", answer: "linear blend skinning with up to four influences per vertex, weights normalised to sum to one at load - not at draw, because a shader that normalised every frame would be paying for an authoring error every frame" },
	Rule { question: "the joint transform", answer: "`joint_world * inverse_bind`, composed on the host once per frame per skeleton; the vertex stage multiplies and adds" },
	Rule { question: "morph targets", answer: "additive displacement, up to `max_morph_targets` per mesh, applied BEFORE skinning - a morph that moved a vertex after skinning would move it out of the pose" },
	Rule { question: "interpolation", answer: "linear for translation and scale, SPHERICAL LINEAR for rotation, along the shorter arc. Interpolating quaternions linearly and normalising is the cheaper alternative and produces a different, visibly uneven, rotation speed" },
	Rule { question: "the root-motion policy", answer: "CHOSEN RATHER THAN REQUIRED: root motion is EXTRACTED by default - the root joint's translation is removed from the pose and handed to the application as a delta - and a clip may declare that it keeps it. Extraction by default because an application that does not ask for root motion should not have its character drift" },
	Rule { question: "looping", answer: "a clip declares whether it loops, and a looping clip's last keyframe must equal its first within the conformance threshold or the clip is REFUSED at load rather than popping once a cycle" },
];

/// PLAYING a clip, as distinct from the equations that interpolate one.
///
/// THE RULES ABOVE SAY WHAT TWO KEYFRAMES MEAN. These say what a CLIP does: which curve joins its
/// keys, what happens at its ends, and what two clips playing at once produce. Each is a thing an
/// implementation must otherwise decide for itself, and each is visible - a cubic with a different
/// tangent convention overshoots where the author did not ask it to, a ping-pong that repeats its
/// turning frame stutters once a cycle, and a cross-fade that blends root motion wrongly makes a
/// character accelerate when it should change gait.
pub const ANIMATION_PLAYBACK_RULES: &[Rule] = &[
	Rule { question: "the interpolation modes", answer: "STEP, LINEAR and CUBIC, declared PER TRACK. A clip mixes them freely: a visibility flag wants step, a slide wants linear, and a bounce wants cubic, and forcing one mode on a whole clip is what makes an author fake the others with extra keys" },
	Rule { question: "what STEP does", answer: "holds the PREVIOUS key's value until the next key's time is REACHED, and the value AT a key is that key's own. So a step track changes exactly at its keyframes and nowhere else, which is what a discrete channel means" },
	Rule { question: "the cubic form", answer: "HERMITE, with an IN and an OUT tangent stored per key: `p(t) = (2t^3 - 3t^2 + 1) * v_k + (t^3 - 2t^2 + t) * dt * out_k + (-2t^3 + 3t^2) * v_(k+1) + (t^3 - t^2) * dt * in_(k+1)`, where `t` is the position within the span and `dt` is the span in seconds. Hermite rather than Catmull-Rom or Bezier because the tangents are AUTHORED rather than derived - a derived tangent changes when a neighbouring key moves, which makes editing one key alter a curve three keys away" },
	Rule { question: "the units of a cubic tangent", answer: "the value's units PER SECOND, and SCALED BY THE SPAN inside the equation. What that buys is INVARIANCE TO KEY DENSITY: a slope of one unit per second means the same thing however far apart the neighbouring keys are, so INSERTING a key between two existing ones - at the value and slope the curve already has there - reproduces the same curve. Stored per span instead, every stored tangent either side of the new key would have to be rewritten to keep the shape, which makes adding a key an edit to the whole track" },
	Rule { question: "a cubic ROTATION track", answer: "interpolated on the quaternion's four components and then NORMALISED - NOT slerped. This is not a contradiction of the linear rule above, which refuses normalised-linear: a cubic through four control points has no spherical form at all, so the choice there is between a component-wise cubic and no cubic. Stated because a reader who has just read the linear rule would expect the same refusal" },
	Rule { question: "the end behaviour", answer: "CLAMP, LOOP or PING-PONG, declared per clip. Clamp holds the first key before the clip and the last after it; loop wraps and its seam must close, as the rule above requires; ping-pong plays forward then backward with a period of TWICE the duration" },
	Rule { question: "ping-pong's turning frames", answer: "VISITED ONCE PER PERIOD AND NOT TWICE. The turn happens AT the end key rather than after it, so the last frame is not held for two frames while the direction changes - which is a visible stutter at both ends, once a cycle, for the life of the clip. A ping-pong clip needs no closed seam, because its ends meet themselves" },
	Rule { question: "blending two clips", answer: "a WEIGHTED BLEND OF THEIR SAMPLED POSES, per target, with the weights summing to one: translation and scale interpolate linearly, rotation SPHERICALLY along the shorter arc, exactly as within one clip. A cross-fade is this with the weight moving from one clip to the other over time, so there is one rule rather than two" },
	Rule { question: "a target only one clip drives", answer: "taken from that clip UNCHANGED, at full value. The other clip says NOTHING about that joint, which is not the same as saying it should be at rest - scaling it by its clip's weight would pull the joint toward the origin as the blend moves away, which is a limb collapsing rather than a blend" },
	Rule { question: "root motion under a blend", answer: "the same weighted sum of the two deltas. A cross-fade from a walk to a run then produces a speed BETWEEN them, where adding the two deltas would produce a character briefly moving faster than either clip ever asks for" },
	Rule { question: "morph-weight tracks", answer: "a track may drive ONE morph target's weight as a scalar, under the same three interpolation modes. The weights are NOT normalised across targets, because morph targets are ADDITIVE - normalising them would make a second expression undo half of the first, which is the same reason the displacement rule gives" },
];

/// Level of detail: WHICH MESH IS DRAWN, which is the one extended decision that changes the
/// picture on purpose.
///
/// CULLING IS INVISIBLE AND THIS IS NOT. The core profile can say a culled drawable "must not change
/// the picture", because a drawable outside the frustum contributes nothing either way. A level of
/// detail is a different mesh: it is chosen to be cheaper and it looks different, and the whole
/// question is WHEN. An unstated threshold means two implementations swap levels at different
/// moments on the same scene, which is a visible difference neither can be said to have got wrong.
///
/// THESE ANSWERS ARE CHOSEN RATHER THAN REQUIRED, on the same terms as the root-motion rule above:
/// nothing outside this profile fixes them, and what matters is that they are fixed HERE rather than
/// in each implementation. The part that owns this profile was activated with a closed feature list
/// as its first deliverable, and a feature with no stated rule cannot be in a closed list.
pub const LEVEL_OF_DETAIL_RULES: &[Rule] = &[
	Rule { question: "the selection metric", answer: "SCREEN COVERAGE: the drawable's world bounding sphere's projected radius as a fraction of HALF THE VIEWPORT HEIGHT - `coverage = radius / (distance * tan(fov_y / 2))` under perspective, and `coverage = radius / half_height` under an orthographic camera. Not raw distance, which selects a different level at the same apparent size when the field of view or the viewport changes; coverage is the thing a person actually sees" },
	Rule { question: "the ladder", answer: "a mesh declares its levels most detailed first, each with the coverage AT OR BELOW WHICH it takes over, and the list must be strictly DESCENDING or the mesh is REFUSED at load. The first level has no threshold: it is what is drawn above the second's" },
	Rule { question: "the default ladder", answer: "0.5, 0.25, 0.125 and 0.0625 for a mesh that declares levels without thresholds - each step a halving of coverage, so a level draws roughly a quarter of the pixels of the one before it" },
	Rule { question: "hysteresis", answer: "10 per cent of the threshold, applied to the level held LAST FRAME: a drawable already at level i leaves it only once coverage passes the boundary by more than a tenth of it. Without it a drawable sitting exactly on a threshold swaps mesh every frame, which reads as flicker rather than as detail" },
	Rule { question: "the first frame", answer: "no previous level, so the ladder is read directly with no hysteresis. A drawable that appears already small starts small rather than starting detailed and stepping down in view" },
	Rule { question: "below the last threshold", answer: "the LAST level keeps being drawn. A mesh may additionally declare a coverage below which it is not drawn at all, and the default is that there is none - vanishing is a decision a scene makes, not one a threshold ladder makes for it" },
	Rule { question: "a mesh with one level", answer: "always that level, and its thresholds are not consulted. This is what makes the feature free for the meshes that do not use it" },
	Rule { question: "which bounds the coverage uses", answer: "the drawable's CURRENT world bounding sphere - recomputed after morphing and skinning, not the rest pose's. A character that raises an arm grows its bounds, and a coverage taken from the rest pose would step down a level while the arm is still on screen" },
	Rule { question: "whether the level is observable", answer: "YES, and this is the difference from culling: the chosen level is readable per drawable, so an application and a conformance scene can assert which mesh was drawn rather than inferring it from pixels" },
];

/// One guaranteed minimum for the extended profile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExtendedLimit {
	pub name: &'static str,
	pub minimum: u32,
	pub why: &'static str,
}

pub const SCENE3D_EXTENDED_1_MIN_LIMITS: &[ExtendedLimit] = &[
	ExtendedLimit { name: "max_shadow_cascades", minimum: 4, why: "four is what covers a walkable view distance at a usable texel density" },
	ExtendedLimit { name: "max_shadow_maps", minimum: 8, why: "one cascaded directional light and a few local ones" },
	ExtendedLimit { name: "max_skeleton_joints", minimum: 128, why: "a humanoid with fingers" },
	ExtendedLimit { name: "max_morph_targets", minimum: 32, why: "a facial rig's expressions" },
	ExtendedLimit { name: "max_animation_tracks", minimum: 256, why: "two joints' worth of curves per joint on the skeleton above" },
	ExtendedLimit { name: "environment_prefilter_levels", minimum: 6, why: "a 256-texel face down to 8, which is where the filter stops meaning anything" },
	ExtendedLimit { name: "max_lod_levels", minimum: 4, why: "the default ladder is four halvings of coverage, from full size down to a sixteenth of the pixels" },
];

/// `Scene3D Extended 1`, as a CLOSED ENUMERATED LIST.
///
/// WHAT THIS ADDS TO THE EQUATIONS ABOVE. The rules say what an implementation must compute; the
/// list says what an implementation must HAVE. They are different questions and the second one is
/// the one a conformance suite, a capability report and a backend checklist are built over: a rule
/// is prose a reader checks, a feature is a name a generator can range over. Every other part of
/// this profile refers to "a test per feature", and until this list existed there was nothing for
/// that phrase to range over.
///
/// EVERY VARIANT HERE COMES FROM A RULE ABOVE, and the profile crate's tests hold that both ways -
/// no feature without a rule, no rule group without features. That is what stops this list becoming
/// a second, drifting statement of the same profile.
///
/// IT IS STILL OPTIONAL. Nothing in the core profile's Done depends on any of it, and an
/// implementation conforms to `Scene3D Core Profile 1` while supporting none of these.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExtendedFeature {
	// The physically based material.
	MaterialPbrMetallicRoughness,
	NormalDistributionGgx,
	VisibilitySmithHeightCorrelated,
	FresnelSchlick,
	DiffuseLambert,
	EnergyConservingDiffuse,
	PerceptualRoughnessRemap,
	MinimumRoughnessClamp,
	DielectricF0Constant,
	ClampedDotProducts,
	DirectTermComposition,
	// Environment lighting. `IrradianceTerm` IS ONE ENTRY OVER TWO PERMITTED FORMS - a cosine-
	// convolved cube map or nine spherical-harmonic coefficients - because the profile admits either
	// and requires only that they agree within its threshold. Two entries would have demanded BOTH,
	// which is a stricter claim than the profile makes and one no implementation would satisfy.
	SplitSumApproximation,
	GgxPrefilteredEnvironment,
	PrefilterLevelRule,
	BrdfIntegrationTable,
	IrradianceTerm,
	// Shadows.
	ShadowMapDepth32F,
	ShadowDepthBias,
	PercentageCloserFilter3x3,
	CascadedShadowMaps,
	CascadeSplitBlend,
	PerFragmentCascadeSelection,
	CascadeTransitionBlend,
	UnshadowedBeyondLastCascade,
	CascadeCountRefusal,
	PointLightCubeShadow,
	// Post-processing.
	BloomSoftKnee,
	BloomPyramid,
	Rec709Luminance,
	ToneMapExtendedReinhard,
	FogExponentialSquared,
	FixedPostprocessOrder,
	// Skinning, morphing and animation.
	LinearBlendSkinning,
	FourInfluencesPerVertex,
	WeightNormalisationAtLoad,
	JointInverseBindComposition,
	MorphTargets,
	MorphBeforeSkinning,
	LinearTranslationScaleKeys,
	SphericalLinearRotationKeys,
	ShorterArcRotation,
	RootMotionExtraction,
	LoopSeamRefusal,
	StepInterpolation,
	CubicHermiteInterpolation,
	CubicTangentsPerSecond,
	ClampEnding,
	PingPongEnding,
	PoseBlending,
	BlendKeepsUndrivenTargets,
	BlendedRootMotion,
	MorphWeightTracks,
	// Level of detail.
	ScreenCoverageLod,
	LodThresholdLadder,
	LodHysteresis,
	LastLodBeyondLadder,
	LodCullBelowCoverage,
	DynamicBoundsAfterDeformation,
	// Limits.
	ExtendedLimits,
	ExtendedLimitRefusal,
}

profile! {
	/// `Scene3D Extended Profile 1`, closed and enumerated.
	SCENE3D_EXTENDED_PROFILE_1: ExtendedFeature;
	"material", Scene3D, MaterialPbrMetallicRoughness;
	"material", Scene3D, NormalDistributionGgx;
	"material", Scene3D, VisibilitySmithHeightCorrelated;
	"material", Scene3D, FresnelSchlick;
	"material", Scene3D, DiffuseLambert;
	"material", Scene3D, EnergyConservingDiffuse;
	"material", Scene3D, PerceptualRoughnessRemap;
	"material", Scene3D, MinimumRoughnessClamp;
	"material", Scene3D, DielectricF0Constant;
	"material", Scene3D, ClampedDotProducts;
	"material", Scene3D, DirectTermComposition;
	"environment", Scene3D, SplitSumApproximation;
	"environment", Scene3D, GgxPrefilteredEnvironment;
	"environment", Scene3D, PrefilterLevelRule;
	"environment", Scene3D, BrdfIntegrationTable;
	"environment", Scene3D, IrradianceTerm;
	"shadows", Scene3D, ShadowMapDepth32F;
	"shadows", Scene3D, ShadowDepthBias;
	"shadows", Scene3D, PercentageCloserFilter3x3;
	"shadows", Scene3D, CascadedShadowMaps;
	"shadows", Scene3D, CascadeSplitBlend;
	"shadows", Scene3D, PerFragmentCascadeSelection;
	"shadows", Scene3D, CascadeTransitionBlend;
	"shadows", Scene3D, UnshadowedBeyondLastCascade;
	"shadows", Scene3D, CascadeCountRefusal;
	"shadows", Scene3D, PointLightCubeShadow;
	"postprocess", Scene3D, BloomSoftKnee;
	"postprocess", Scene3D, BloomPyramid;
	"postprocess", Scene3D, Rec709Luminance;
	"postprocess", Scene3D, ToneMapExtendedReinhard;
	"postprocess", Scene3D, FogExponentialSquared;
	"postprocess", Scene3D, FixedPostprocessOrder;
	"animation", Scene3D, LinearBlendSkinning;
	"animation", Scene3D, FourInfluencesPerVertex;
	"animation", Scene3D, WeightNormalisationAtLoad;
	"animation", Scene3D, JointInverseBindComposition;
	"animation", Scene3D, MorphTargets;
	"animation", Scene3D, MorphBeforeSkinning;
	"animation", Scene3D, LinearTranslationScaleKeys;
	"animation", Scene3D, SphericalLinearRotationKeys;
	"animation", Scene3D, ShorterArcRotation;
	"animation", Scene3D, RootMotionExtraction;
	"animation", Scene3D, LoopSeamRefusal;
	"animation", Scene3D, StepInterpolation;
	"animation", Scene3D, CubicHermiteInterpolation;
	"animation", Scene3D, CubicTangentsPerSecond;
	"animation", Scene3D, ClampEnding;
	"animation", Scene3D, PingPongEnding;
	"animation", Scene3D, PoseBlending;
	"animation", Scene3D, BlendKeepsUndrivenTargets;
	"animation", Scene3D, BlendedRootMotion;
	"animation", Scene3D, MorphWeightTracks;
	"detail", Scene3D, ScreenCoverageLod;
	"detail", Scene3D, LodThresholdLadder;
	"detail", Scene3D, LodHysteresis;
	"detail", Scene3D, LastLodBeyondLadder;
	"detail", Scene3D, LodCullBelowCoverage;
	"detail", Scene3D, DynamicBoundsAfterDeformation;
	"limits", Scene3D, ExtendedLimits;
	"limits", Scene3D, ExtendedLimitRefusal;
}

/// The groups, in the order the profile documents them.
pub const SCENE3D_EXTENDED_GROUPS: &[&str] = &["material", "environment", "shadows", "postprocess", "animation", "detail", "limits"];

/// Is this feature in the extended profile?
pub fn in_extended_profile_1(feature: ExtendedFeature) -> bool {
	SCENE3D_EXTENDED_PROFILE_1.iter().any(|entry| entry.feature == feature)
}

/// The entry for a feature named as the profile spells it.
pub fn entry_by_name(name: &str) -> Option<&'static ProfileEntry<ExtendedFeature>> {
	SCENE3D_EXTENDED_PROFILE_1.iter().find(|entry| entry.name == name)
}

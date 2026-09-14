//! `Render3D Core Profile 1`: THE NUMBERS AND THE RULES, frozen before a backend exists.
//!
//! `render3d.rs` is the closed list of things a backend can fail to do. This is the other half: the
//! values two backends round differently and the questions an implementation discovers it had to
//! answer only after somebody's picture was wrong. A feature list without these is a conformance
//! matrix every implementation passes while producing different images.
//!
//! WRITTEN BEFORE `render3d` AND `soft3d` EXIST, ON PURPOSE. A specification produced after an
//! implementation is a description of what was built. Every answer below was chosen here, and where
//! a choice is arbitrary the reason it was made that way is written beside it rather than left for a
//! reader to reconstruct from the code.
//!
//! THE CONVENTIONS THIS PROFILE FIXES AT THE TOP, because everything else is stated in them:
//!
//!   - CLIP SPACE is `x, y` in `[-w, w]`, `z` in `[0, w]`. The zero-to-w depth range rather than
//!     minus-w-to-w: it spends the whole floating-point mantissa on the near half of the range,
//!     which is where depth precision is actually needed, and it removes one clip plane.
//!   - NDC has `y` DOWN, matching the 2D profile's device space and the scanout's row order. A 3D
//!     stack whose y points the other way from the 2D stack it composites with is two coordinate
//!     systems in one frame, and every surface handed between them needs a flip somebody forgets.
//!   - THE VIEWPORT maps NDC to pixels with pixel CENTRES at half-integers, which is the same rule
//!     the 2D rasteriser uses.
//!   - WINDING: counter-clockwise in NDC is FRONT-facing by default, and NDC has y down, so a
//!     triangle wound counter-clockwise on screen as a reader sees it is front-facing.

/// One question with one answer, where the question is what an implementation would otherwise guess.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rule {
	pub question: &'static str,
	pub answer: &'static str,
}

// ---------------------------------------------------------------------------------------------
// Clip space, and the one type every position flows through.
// ---------------------------------------------------------------------------------------------

/// The clip-space volume, stated as the inequalities a clipper tests.
pub const CLIP_VOLUME: &str = "-w <= x <= w, -w <= y <= w, 0 <= z <= w, with w > 0 for a visible point";

/// `ClipCoordQ`: the type a vertex position has after the vertex stage and before the divide.
///
/// DEFINED COMPLETELY HERE, because "a vec4" is not a definition: what an implementation needs to
/// know is which values are legal, what happens at the ones that are not, and in what arithmetic the
/// clip is performed.
pub const CLIP_COORD_Q: &[Rule] = &[
	Rule { question: "what it is", answer: "four `f32` components x, y, z, w in the clip volume above, produced by the vertex stage and consumed by the clipper before any divide" },
	Rule { question: "the arithmetic it is clipped in", answer: "HOMOGENEOUS and at `f32`, before the perspective divide. Dividing first and clipping after produces coordinates of ten million pixels for a vertex just behind the eye, and a rasteriser that spends a frame on one triangle" },
	Rule { question: "a non-finite component", answer: "the PRIMITIVE is refused, not the vertex. A NaN or an infinity in any component makes every inequality above false, so a clipper cannot decide the primitive at all - and silently dropping it would make a shader bug look like a culling rule" },
	Rule { question: "w <= 0 on every vertex", answer: "the primitive is entirely behind the eye and is dropped. This is a CULL and not a refusal: it is the ordinary state of geometry behind the camera" },
	Rule { question: "w <= 0 on some vertices", answer: "the primitive is clipped against the `w = CLIP_W_EPSILON` plane FIRST, and the remaining planes afterwards. Clipping against `w = 0` exactly produces a vertex at infinity after the divide" },
	Rule { question: "the epsilon", answer: "`CLIP_W_EPSILON`, and it is a value rather than an implementation's guess" },
	Rule { question: "whether a backend may clip in a different space", answer: "NO. A backend that clips after the divide, or in fixed point, produces different vertices at the same input, and the conformance suite compares vertices" },
];

/// The `w` below which a vertex is on the horizon rather than in front of the eye.
///
/// THE SAME VALUE THE 2D PROFILE USES for the same question, because the two are the same question:
/// a projective transform's singularity does not become a different singularity because the geometry
/// has three dimensions. A reader who has learned one has learned both.
pub const CLIP_W_EPSILON: f64 = 1.0 / 1_048_576.0;

/// The clip planes, in the order a clipper applies them.
///
/// THE ORDER IS PART OF THE PROFILE. Clipping is not associative in floating point: the vertex where
/// a triangle crosses two planes depends on which plane cut it first, and two backends that disagree
/// about the order disagree about that vertex by an amount a conformance comparison can see.
pub const CLIP_PLANE_ORDER: &[&str] = &["w = CLIP_W_EPSILON", "z >= 0", "z <= w", "x >= -w", "x <= w", "y >= -w", "y <= w"];

// ---------------------------------------------------------------------------------------------
// Interpolation, and what clipping does to it.
// ---------------------------------------------------------------------------------------------

/// How one interpolation qualifier behaves, at the edge intersection and across the primitive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Qualifier {
	pub name: &'static str,
	pub across_the_primitive: &'static str,
	pub at_a_clip_intersection: &'static str,
	pub why: &'static str,
}

/// The qualifiers, and the equations that define them.
///
/// EVERY BACKEND THAT CHOOSES ITS OWN RULE HERE PRODUCES A DIFFERENT IMAGE, and the difference is
/// largest exactly where it is most visible: a texture coordinate interpolated without perspective
/// correction on a floor plane is the classic wrong picture, and a `flat` value taken from the wrong
/// vertex changes a whole triangle's colour.
pub const QUALIFIERS: &[Qualifier] = &[
	Qualifier { name: "smooth", across_the_primitive: "perspective-correct: the value is interpolated as `v/w` against `1/w` and divided back at the sample, so the result is the value at the point the sample corresponds to on the surface", at_a_clip_intersection: "HOMOGENEOUS edge interpolation. For endpoints P0 and P1 cut at parameter `t` computed from the homogeneous plane equation, every component INCLUDING `w` is interpolated linearly at that `t`, and the attribute follows the same `t`", why: "the `t` that makes the position correct is the `t` that makes a perspective-correct attribute correct, because both are linear in homogeneous space" },
	Qualifier { name: "noperspective", across_the_primitive: "linear in SCREEN space: the value is interpolated against the barycentric coordinates of the projected triangle, with no `1/w` weighting", at_a_clip_intersection: "PROJECTED edge interpolation. The parameter is recomputed from the PROJECTED endpoints - `t_screen` - and the attribute follows that, not the homogeneous `t`", why: "an attribute declared to be linear on the screen must stay linear on the screen after a cut; using the homogeneous `t` would put a kink at the clip edge, which is visible as a bent line in exactly the wrong place" },
	Qualifier { name: "flat", across_the_primitive: "constant, taken from the PROVOKING vertex of the primitive as assembled, before any clipping", at_a_clip_intersection: "UNCHANGED. The value of the ORIGINAL primitive's provoking vertex is carried onto every fragment, INCLUDING when that vertex was clipped away entirely", why: "a flat attribute identifies the primitive - a material index, an object id - and a clipped triangle is the same triangle. Taking the value from a vertex the clipper invented would make an id depend on where the camera is" },
	Qualifier { name: "centroid", across_the_primitive: "as its base qualifier, evaluated at a point INSIDE the covered part of the pixel rather than at the pixel centre", at_a_clip_intersection: "as its base qualifier. `centroid` chooses an evaluation LOCATION and does not change the interpolation rule", why: "at an edge the pixel centre can lie outside the primitive, and extrapolating a texture coordinate there samples outside the surface - which is the shimmer along silhouettes that multisampling is supposed to remove" },
	Qualifier { name: "sample", across_the_primitive: "as its base qualifier, evaluated once per COVERED SAMPLE rather than once per pixel", at_a_clip_intersection: "as its base qualifier, for the same reason as `centroid`", why: "it is what makes per-sample shading mean anything: a fragment shader run once per pixel cannot produce different values at two samples of it" },
];

/// LINE AND POINT RASTERISATION, which a profile that mandates six topologies and specifies one
/// would leave to every implementation to invent.
///
/// THE TRIANGLE RULES DO NOT COVER THESE. A line has no interior and no winding; a point has neither
/// and no edges either. Every question the triangle rules answer - which samples are covered, which
/// side is the front, what the fill rule decides at a tie - has to be answered again here or it is
/// answered differently by every backend.
pub const LINE_POINT_RULES: &[Rule] = &[
	Rule { question: "line coverage", answer: "the DIAMOND-EXIT rule. A line covers a pixel when the segment, travelled from its first endpoint to its second, EXITS the diamond inscribed in that pixel - the set of points whose Manhattan distance from the pixel centre is below half a pixel. The first endpoint's own pixel is covered only if the segment leaves its diamond, and the second endpoint's pixel is covered only if the segment enters and leaves that diamond" },
	Rule { question: "why the diamond-exit rule and not a midpoint walk", answer: "two segments that meet end to end must together cover each pixel EXACTLY once, the same requirement the top-left rule meets for triangles. A midpoint or Bresenham walk covers the shared endpoint's pixel from both segments, which is visible the moment a polyline is drawn with any blending at all" },
	Rule { question: "line width", answer: "exactly 1.0, which the rasteriser state already fixes. A wide line is ten different rules across implementations and belongs to the 2D profile's stroker, which has defined joins and caps" },
	Rule { question: "line interpolation parameter", answer: "the distance along the segment in WINDOW space, so `noperspective` varyings are linear along the drawn line; `smooth` varyings apply the same `1/w` correction they do on a triangle, with the two endpoint weights being `1 - s` and `s`" },
	Rule { question: "point size", answer: "the `PointSize` output of the vertex stage, in PIXELS, clamped to `[1.0, 64.0]` and rounded to the nearest integer with ties away from zero. A size of zero draws nothing rather than one pixel, because a program that computes a size from a distance expects it to vanish" },
	Rule { question: "point coverage", answer: "the axis-aligned SQUARE of that size centred on the point's window position, with the same sample test and the same top-left tie rule a triangle's edges get - so two adjacent points of the same size tile without overlap" },
	Rule { question: "point interpolation", answer: "NONE. Every varying takes the point's own vertex value, whatever its qualifier, because a point has one vertex and there is nothing to interpolate between" },
	Rule { question: "culling", answer: "BACK-FACE CULLING APPLIES TO TRIANGLES ONLY. A line and a point have no winding, so a cull mode cannot remove them; a backend that applied the triangle rule to them would make a wireframe overlay disappear at half the angles" },
	Rule { question: "clipping", answer: "against the SAME six planes and the same `w` plane, in the same order. A line is clipped as a two-vertex polygon and keeps two vertices or none; a point is IN or OUT with no partial case, and a point whose centre is outside the volume is removed whole even when its square would have covered visible pixels - the alternative is a point that is clipped to a rectangle, which is not a point" },
	Rule { question: "depth for lines and points", answer: "interpolated along the line and constant across a point, in both cases in WINDOW space without the perspective correction, exactly as for a triangle" },
];

/// Which vertex of a primitive a `flat` attribute comes from, per topology.
///
/// STATED PER TOPOLOGY AND NOT AS ONE SENTENCE, because a strip and a fan number their vertices
/// differently and "the first vertex" means a different thing in each.
pub const PROVOKING_VERTEX: &[Rule] = &[
	Rule { question: "triangle list", answer: "the FIRST vertex of each triangle: 3i" },
	Rule { question: "triangle strip", answer: "the FIRST vertex of each triangle in emission order: vertex i of the strip for triangle i, whatever the winding flip does to the other two" },
	Rule { question: "triangle fan", answer: "the SECOND vertex of each triangle - the one that is not the shared hub. The hub is in every triangle, so choosing it would give every triangle of a fan the same flat value" },
	Rule { question: "line list", answer: "the FIRST vertex of each line: 2i" },
	Rule { question: "line strip", answer: "the FIRST vertex of each segment: vertex i for segment i" },
	Rule { question: "point list", answer: "the point itself" },
	Rule { question: "after fan triangulation", answer: "a clipped polygon is triangulated as a FAN from its first vertex, and every resulting triangle carries the ORIGINAL primitive's provoking value - the triangulation is an implementation detail of clipping and must not be visible in a flat attribute" },
	Rule { question: "after primitive restart", answer: "the strip or fan begins again, so the next primitive's provoking vertex is the first of the NEW run" },
];

/// What an intersection's numbers are rounded to.
pub const CLIP_ROUNDING: &str = "the intersection parameter and every interpolated attribute are computed at `f32` and kept at `f32`; no backend may compute them at a wider precision and round, because the conformance suite compares positions bit-exactly under StrictF32";

// ---------------------------------------------------------------------------------------------
// Formats.
// ---------------------------------------------------------------------------------------------

/// What a backend must be able to do with one colour format.
///
/// EIGHT COLUMNS, BECAUSE EACH IS SEPARATELY FAILABLE. An implementation can sample a format and be
/// unable to filter it, or render to it and be unable to blend - and a table that said "supported"
/// would call all of those conforming.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FormatCapability {
	pub name: &'static str,
	pub full_name: &'static str,
	pub bits_per_texel: u32,
	pub sampled: bool,
	pub filterable: bool,
	pub renderable: bool,
	pub blendable: bool,
	pub msaa: bool,
	pub vertex: bool,
	pub readback: bool,
	pub attachment: bool,
}

/// The colour formats of `Render3D Core Profile 1`, with FULL names rather than shorthand.
///
/// `RGBA8` names four channels and says nothing about whether they are normalised, signed or
/// sRGB-encoded - three different formats a backend can implement one of and call itself conforming.
pub const COLOUR_FORMATS: &[FormatCapability] = &[
	FormatCapability { name: "R8", full_name: "R8_UNORM: one 8-bit channel, unsigned normalised to [0, 1]", bits_per_texel: 8, sampled: true, filterable: true, renderable: true, blendable: true, msaa: true, vertex: false, readback: true, attachment: true },
	FormatCapability { name: "RG8", full_name: "R8G8_UNORM: two 8-bit channels, unsigned normalised to [0, 1]", bits_per_texel: 16, sampled: true, filterable: true, renderable: true, blendable: true, msaa: true, vertex: false, readback: true, attachment: true },
	FormatCapability { name: "RGBA8", full_name: "R8G8B8A8_UNORM: four 8-bit channels, unsigned normalised to [0, 1], NOT sRGB-encoded", bits_per_texel: 32, sampled: true, filterable: true, renderable: true, blendable: true, msaa: true, vertex: false, readback: true, attachment: true },
	FormatCapability { name: "RGB10A2", full_name: "A2B10G10R10_UNORM_PACK32: three 10-bit channels and two bits of alpha, unsigned normalised", bits_per_texel: 32, sampled: true, filterable: true, renderable: true, blendable: true, msaa: true, vertex: false, readback: true, attachment: true },
	FormatCapability { name: "RGBA16F", full_name: "R16G16B16A16_SFLOAT: four IEEE 754 binary16 channels", bits_per_texel: 64, sampled: true, filterable: true, renderable: true, blendable: true, msaa: true, vertex: false, readback: true, attachment: true },
	FormatCapability { name: "RGBA32F", full_name: "R32G32B32A32_SFLOAT: four IEEE 754 binary32 channels", bits_per_texel: 128, sampled: true, filterable: false, renderable: true, blendable: false, msaa: false, vertex: false, readback: true, attachment: true },
	FormatCapability { name: "R32Uint", full_name: "R32_UINT: one 32-bit unsigned integer channel, not normalised", bits_per_texel: 32, sampled: true, filterable: false, renderable: true, blendable: false, msaa: true, vertex: false, readback: true, attachment: true },
];

/// Why the three `false` cells above are false, so a reader does not read them as an omission.
pub const FORMAT_EXCEPTIONS: &[Rule] = &[
	Rule { question: "RGBA32F is not filterable", answer: "a 128-bit filtered fetch is four 32-bit lerps per tap and eight taps for trilinear, which is a cost no software backend can meet at a frame rate and no hardware backend guarantees. `MinNearest`/`MagNearest` are the conforming way to sample it" },
	Rule { question: "RGBA32F is not blendable and not multisampled", answer: "for the same reason: a blend is a read-modify-write of 128 bits per sample, and multisampling multiplies it. A pass that needs full float precision needs one sample and no blending, and saying so is better than a backend discovering it" },
	Rule { question: "an integer format is not filterable or blendable", answer: "there is no correct answer to what the average of two object ids is, and blending them produces an id that identifies nothing. It IS multisampled, because coverage still applies - the resolve rule for it is stated with the MSAA rules" },
];

/// One vertex attribute format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VertexFormat {
	pub name: &'static str,
	pub full_name: &'static str,
	pub bytes: u32,
	pub shader_type: &'static str,
}

/// The vertex formats, which are a DIFFERENT set from the texture formats and are listed separately.
///
/// A format table that mixed them would let a backend claim a vertex format because it could sample
/// the texture format with the same name, which is a different piece of hardware.
pub const VERTEX_FORMATS: &[VertexFormat] = &[
	VertexFormat { name: "Float32", full_name: "R32_SFLOAT", bytes: 4, shader_type: "f32" },
	VertexFormat { name: "Float32x2", full_name: "R32G32_SFLOAT", bytes: 8, shader_type: "vec2" },
	VertexFormat { name: "Float32x3", full_name: "R32G32B32_SFLOAT", bytes: 12, shader_type: "vec3" },
	VertexFormat { name: "Float32x4", full_name: "R32G32B32A32_SFLOAT", bytes: 16, shader_type: "vec4" },
	VertexFormat { name: "Unorm8x4", full_name: "R8G8B8A8_UNORM", bytes: 4, shader_type: "vec4 in [0, 1]" },
	VertexFormat { name: "Snorm8x4", full_name: "R8G8B8A8_SNORM", bytes: 4, shader_type: "vec4 in [-1, 1]" },
	VertexFormat { name: "Unorm16x2", full_name: "R16G16_UNORM", bytes: 4, shader_type: "vec2 in [0, 1]" },
	VertexFormat { name: "Unorm16x4", full_name: "R16G16B16A16_UNORM", bytes: 8, shader_type: "vec4 in [0, 1]" },
	VertexFormat { name: "Uint16x2", full_name: "R16G16_UINT", bytes: 4, shader_type: "uvec2" },
	VertexFormat { name: "Uint32", full_name: "R32_UINT", bytes: 4, shader_type: "u32" },
];

/// How a normalised integer vertex attribute becomes a float, which is the one conversion two
/// implementations get different answers for.
pub const VERTEX_NORMALISATION: &[Rule] = &[
	Rule { question: "unsigned normalised", answer: "`value / MAX`, so an 8-bit 255 is exactly 1.0 and 0 is exactly 0.0" },
	Rule { question: "signed normalised", answer: "`max(value / MAX_POSITIVE, -1.0)`, so an 8-bit 127 is exactly 1.0, -127 is exactly -1.0, and -128 CLAMPS to -1.0 rather than producing -1.0079. The other convention - dividing by 128 - makes 127 fall short of 1.0, which shows up as a normal that is not unit length" },
	Rule { question: "alignment", answer: "an attribute's offset within its stream must be a multiple of the size of its largest component; an unaligned layout is REFUSED at pipeline creation rather than working on one backend" },
];

// ---------------------------------------------------------------------------------------------
// Rasteriser state.
// ---------------------------------------------------------------------------------------------

/// One piece of rasteriser state, its closed set of values, and its default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StateField {
	pub name: &'static str,
	pub values: &'static str,
	pub default: &'static str,
	pub why: &'static str,
}

pub const RASTERISER_STATE: &[StateField] = &[
	StateField { name: "fill mode", values: "solid", default: "solid", why: "wireframe is a DIFFERENT primitive topology in this profile, not a rasteriser mode: a line-list draw produces lines a backend can define exactly, where wireframe fill leaves every implementation to invent its own edge rule" },
	StateField { name: "cull mode", values: "none | front | back", default: "back", why: "back by default because the commonest mistake is forgetting to set it and seeing interior faces, which is a wrong picture rather than a slow one" },
	StateField { name: "front face", values: "counter-clockwise | clockwise", default: "counter-clockwise", why: "counter-clockwise in NDC, and NDC has y down - so what a reader sees on the screen as counter-clockwise is front-facing" },
	StateField { name: "depth clamp", values: "off", default: "off", why: "clamping instead of clipping at the near plane changes what a depth value means, and a profile that offered both would have two depth semantics" },
	StateField { name: "depth bias", values: "constant factor, slope factor, clamp; each an f32", default: "0, 0, 0", why: "shadow-map acne is not solvable without it, and the exact equation is stated below rather than left to a backend" },
	StateField { name: "line width", values: "1.0", default: "1.0", why: "a fixed width of one pixel. Wide lines are ten different rasterisation rules across implementations, and the 2D profile is where a stroked line with a defined join belongs" },
	StateField { name: "scissor", values: "one rectangle in pixels, clamped to the attachment", default: "the whole attachment", why: "one and not an array: a per-viewport scissor array needs a viewport array, which this profile does not have" },
	StateField { name: "viewport", values: "x, y, width, height, min depth, max depth", default: "the whole attachment, depth 0 to 1", why: "the depth range is part of the viewport because that is where NDC z becomes a depth value, and stating it anywhere else splits one transform in two" },
];

/// The depth-bias equation, written out.
///
/// A BIAS WHOSE EQUATION IS NOT STATED IS A DIFFERENT BIAS ON EVERY BACKEND, and shadow bias is
/// tuned by hand against whatever the equation happens to be - so a scene tuned on one backend
/// acnes on another.
pub const DEPTH_BIAS_EQUATION: &str = "offset = constant_factor * r + slope_factor * m, where m is the maximum of |dz/dx| and |dz/dy| over the primitive and r is the smallest representable difference at the depth format's precision near the fragment's depth; for a floating-point depth format r is 2^(exponent(z) - 23). The offset is added AFTER the depth value is computed and BEFORE the depth test and the depth write. A non-zero clamp bounds |offset|";

// ---------------------------------------------------------------------------------------------
// Multisampling.
// ---------------------------------------------------------------------------------------------

/// One sample position within a pixel, in the pixel's own [0, 1] space with the origin at its
/// top-left corner.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SamplePosition {
	pub index: u32,
	pub x: f32,
	pub y: f32,
}

/// The 2x sample positions: the standard diagonal pair.
///
/// EXACT AND NOT "IMPLEMENTATION-DEFINED". Coverage is what the positions decide, so an edge falls
/// on a different side of a sample on two backends that chose differently - and the conformance
/// suite compares coverage.
pub const MSAA_2X: &[SamplePosition] = &[SamplePosition { index: 0, x: 0.75, y: 0.75 }, SamplePosition { index: 1, x: 0.25, y: 0.25 }];

/// The 4x sample positions: the standard rotated grid, which is what makes a near-horizontal and a
/// near-vertical edge get the same number of coverage levels.
pub const MSAA_4X: &[SamplePosition] = &[
	SamplePosition { index: 0, x: 0.375, y: 0.125 },
	SamplePosition { index: 1, x: 0.875, y: 0.375 },
	SamplePosition { index: 2, x: 0.125, y: 0.625 },
	SamplePosition { index: 3, x: 0.625, y: 0.875 },
];

pub const MSAA_RULES: &[Rule] = &[
	Rule { question: "shading frequency", answer: "ONCE PER PIXEL by default, with the result written to every covered sample. Per-sample shading happens only where an input is declared `sample`, and then the fragment stage runs once per covered sample" },
	Rule { question: "the centroid rule", answer: "a `centroid` input is evaluated at the CENTRE OF MASS of the covered sample positions. Where no sample is covered - which cannot happen for a fragment that exists - it is the pixel centre. It is NOT a covered sample's position: an average is stable as coverage changes, where 'the first covered sample' jumps between pixels along an edge" },
	Rule { question: "sample mask order", answer: "bit i of a sample mask is sample i as indexed above, least significant bit first. A mask is ANDed with coverage BEFORE the depth test, so a masked-out sample is not tested and not written" },
	Rule { question: "alpha to coverage", answer: "coverage is ANDed with the first `round(alpha * sample_count)` sample bits IN INDEX ORDER, where alpha is attachment 0's alpha after the fragment stage and before blending. In index order rather than by a dither pattern, because a pattern makes the result depend on the pixel's position and two adjacent pixels with the same alpha get different coverage" },
	Rule { question: "colour resolve", answer: "the ARITHMETIC MEAN of the samples, computed in the attachment's own numeric space and rounded once at the end. Averaging in a different space - sRGB-encoded values, say - is the classic resolve that darkens edges" },
	Rule { question: "integer resolve", answer: "sample 0 is taken and the rest are discarded. There is no average of two object ids that identifies anything, and a resolve that produced one would invent an id" },
	Rule { question: "depth resolve", answer: "sample 0 is taken. A depth buffer's resolved value is used for reconstruction, and the MINIMUM of the samples belongs to whichever surface is nearest rather than to the surface the colour resolve is mostly made of" },
	Rule { question: "stencil resolve", answer: "sample 0 is taken, for the same reason as the integer case" },
	Rule { question: "a resolve to a different format", answer: "REFUSED. A resolve is an average and not a conversion; a pass that wants both states both" },
];

// ---------------------------------------------------------------------------------------------
// Depth and stencil.
// ---------------------------------------------------------------------------------------------

/// One depth or depth-stencil format, and what its numbers MEAN.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DepthFormat {
	pub name: &'static str,
	pub full_name: &'static str,
	pub storage: &'static str,
	pub stored_range: &'static str,
	pub comparison: &'static str,
}

pub const DEPTH_FORMATS: &[DepthFormat] = &[
	DepthFormat { name: "Depth16", full_name: "D16_UNORM", storage: "16-bit unsigned normalised", stored_range: "[0, 1] mapped to [0, 65535]", comparison: "the stored integers are compared" },
	DepthFormat { name: "Depth24", full_name: "X8_D24_UNORM_PACK32", storage: "24-bit unsigned normalised in the low bits of a 32-bit word", stored_range: "[0, 1] mapped to [0, 16777215]", comparison: "the stored integers are compared" },
	DepthFormat { name: "Depth32F", full_name: "D32_SFLOAT", storage: "IEEE 754 binary32", stored_range: "[0, 1] as floating point, with the whole mantissa spent near 0", comparison: "THE STORED FLOATS ARE COMPARED - see `DEPTH32F_ANSWER`" },
	DepthFormat { name: "Depth24Stencil8", full_name: "D24_UNORM_S8_UINT", storage: "24-bit unsigned normalised depth and an 8-bit unsigned stencil", stored_range: "depth [0, 1] mapped to [0, 16777215]; stencil [0, 255]", comparison: "the stored integers are compared" },
	DepthFormat { name: "Depth32FStencil8", full_name: "D32_SFLOAT_S8_UINT", storage: "IEEE 754 binary32 depth and an 8-bit unsigned stencil, separately addressed", stored_range: "depth [0, 1] as floating point; stencil [0, 255]", comparison: "the stored floats are compared" },
];

/// THE `Depth32F` QUESTION, ANSWERED.
///
/// The question the plan names is whether a floating-point depth buffer stores and compares the
/// value as written, or whether the incoming fragment depth is quantised to the format's precision
/// before the comparison the way a normalised format's is. Both exist in shipping systems, and they
/// differ: with quantisation, drawing the same geometry twice with `LessOrEqual` is guaranteed to
/// pass on the second pass; without it, a fragment whose depth was computed at a different precision
/// can fail against itself, which is the classic decal that flickers.
pub const DEPTH32F_ANSWER: &str = "THE STORED FLOATS ARE COMPARED, AND THE INCOMING DEPTH IS NOT QUANTISED. A fragment's depth is computed at f32, clamped to the viewport's depth range, and compared against the stored f32 exactly. Quantisation is what a normalised format does because its storage cannot hold anything else; imposing it on a float format would throw away the precision the format exists for. The consequence is stated so nobody has to discover it: a second pass over the same geometry must use `LessOrEqual` AND must compute depth by the same path, which under this profile's StrictF32 rules it does - the vertex stage is bit-exact, so the same vertices produce the same depth";

pub const DEPTH_RULES: &[Rule] = &[
	Rule { question: "the compare operations", answer: "Never, Less, Equal, LessOrEqual, Greater, NotEqual, GreaterOrEqual, Always - applied as `incoming OP stored`" },
	Rule { question: "when the depth write happens", answer: "after the depth test passes and after the stencil test passes, and never when either fails. A fragment discarded by the fragment stage writes neither" },
	Rule { question: "clamping", answer: "the incoming depth is clamped to the viewport's [min depth, max depth] AFTER the bias and BEFORE the test, so a bias cannot push a fragment outside the buffer's range" },
	Rule { question: "clear", answer: "a depth clear takes an f32 in [0, 1] and is converted to the format's storage by the same rule a fragment's depth is; a stencil clear takes a u32 and its low 8 bits are stored" },
	Rule { question: "stencil test order", answer: "stencil test first, then depth test. The stencil op selected depends on BOTH: `fail` when the stencil test fails, `depth-fail` when the stencil passed and the depth failed, `pass` when both passed" },
	Rule { question: "readback", answer: "a depth readback answers the STORED values converted to f32 in [0, 1], not the raw integers, so a caller does not have to know the storage" },
];

// ---------------------------------------------------------------------------------------------
// Sampling.
// ---------------------------------------------------------------------------------------------

/// The LOD formula, written out.
pub const LOD_FORMULA: &str = "lambda = log2(rho) + bias, clamped to [min_lod, max_lod] and then to [0, level_count - 1], where rho is the MAXIMUM of the lengths of the two texture-coordinate derivative vectors in texels: rho = max(length(ddx(uv) * size), length(ddy(uv) * size)). The maximum rather than a geometric mean, because the mean under-filters exactly where an anisotropic footprint is worst";

pub const SAMPLER_RULES: &[Rule] = &[
	Rule { question: "tap count, nearest", answer: "one tap at the texel whose centre is nearest the sample point; a tie goes to the LOWER index in each dimension, so the rule is total and two backends round the same way" },
	Rule { question: "tap count, linear within a level", answer: "four taps in 2D, eight in 3D, weighted by the fractional position of the sample point relative to texel CENTRES, computed at f32" },
	Rule { question: "tap count, trilinear", answer: "two levels linearly filtered and blended by frac(lambda), which is eight taps in 2D. When lambda is at or above the last level, the last level alone is used and no blend happens" },
	Rule { question: "the footprint", answer: "the derivatives of the texture coordinate with respect to the FRAGMENT's x and y in the attachment's pixel space, evaluated at the sample location. In a fragment stage they come from a 2x2 quad; where no quad exists - a vertex-stage fetch - the explicit-LOD form is the only legal one" },
	Rule { question: "anisotropic sampling", answer: "up to 8 taps along the major axis of the footprint, each a full filtered sample at the level chosen from the MINOR axis, averaged with equal weights. The tap count is `min(ceil(major/minor), max_anisotropy)`; with max_anisotropy 1 the rule collapses to the isotropic one above" },
	Rule { question: "cube-face orientation", answer: "the six faces are +X, -X, +Y, -Y, +Z, -Z in that index order, with the same major-axis selection and the same s/t sign conventions the standard cube-map table defines - so a cube built for any other system loads without a flip" },
	Rule { question: "the cube seam rule", answer: "a linear filter whose footprint crosses a face edge TAKES THE TAPS FROM THE NEIGHBOURING FACE, not from a clamped edge texel. Clamping is what makes the seam visible, and a profile that left it open would make every cube map look different on two backends" },
	Rule { question: "the mip-generation kernel", answer: "a 2x2 BOX filter of the level above, averaged in the texture's own numeric space - linear for a float format, and for a normalised format the four integers are summed and divided with round-half-up. An odd dimension halves by `max(1, floor(n/2))` and the box takes the three or two texels that exist; there is no weighting to invent" },
	Rule { question: "depth-compare sampling", answer: "each tap is compared against the reference FIRST, producing 0.0 or 1.0, and the taps are then filtered - so a linear depth-compare sample is the FRACTION of taps that passed, which is what makes percentage-closer filtering work. Filtering the depths and comparing once produces a hard edge and is the wrong answer" },
	Rule { question: "the border colour", answer: "one of three values and not an arbitrary colour: transparent black (0,0,0,0), opaque black (0,0,0,1), opaque white (1,1,1,1). An arbitrary border needs a whole colour to travel with the sampler, and the three above cover every use a border wrap has" },
	Rule { question: "wrap modes", answer: "clamp-to-edge, repeat, mirrored-repeat and clamp-to-border, applied per axis before the texel fetch and AFTER the LOD is chosen - so a wrapped coordinate does not change the derivative the level came from" },
	Rule { question: "unnormalised coordinates", answer: "NOT IN THIS PROFILE. Every sample takes normalised coordinates; a fetch by integer texel is a different operation with its own rules and no filtering" },
];

// ---------------------------------------------------------------------------------------------
// Hazards.
// ---------------------------------------------------------------------------------------------

/// The resource hazard policy, as RULES rather than as alternatives an implementation picks from.
///
/// THE ALTERNATIVE IS WHAT MAKES A RENDERER NON-PORTABLE. "The application must synchronise" and
/// "the implementation inserts barriers" are both defensible and they are not the same contract, and
/// a renderer written against one corrupts frames under the other.
pub const HAZARD_RULES: &[Rule] = &[
	Rule { question: "who is responsible", answer: "THE IMPLEMENTATION. A submitted command list observes every write made by every command list submitted before it, and every write made earlier within itself. An application never inserts a barrier, because a profile that required one would need to enumerate every barrier kind and every implementation would need the same ones" },
	Rule { question: "a texture read while it is an attachment", answer: "REFUSED at submission. The read has no defined value - it depends on tile order - and refusing it is the only answer that does not make the picture depend on the backend" },
	Rule { question: "a buffer written by the host while it is in flight", answer: "REFUSED: a resource handed to a submission is owned by that submission until its completion is observable. The refusal is at the WRITE, not at the submission, so the report names the thing that is wrong" },
	Rule { question: "read-after-write across passes", answer: "ordered by submission order. The result of a pass that writes a texture is visible to every later pass that samples it, with no action by the caller" },
	Rule { question: "write-after-read within one pass", answer: "REFUSED for the same reason as the attachment case: within a pass there is no order between fragments" },
	Rule { question: "two passes writing one attachment", answer: "ordered by submission order, and the load operation of the second decides what the first's output is worth - `Load` keeps it, `Clear` discards it, `Discard` says nobody may read it" },
	Rule { question: "whether the ordering costs a flush", answer: "that is the implementation's business. What the profile fixes is the OBSERVABLE order; a backend that can prove two passes do not touch the same memory may run them in either order" },
];

// ---------------------------------------------------------------------------------------------
// Submission and readback.
// ---------------------------------------------------------------------------------------------

pub const SUBMISSION_RULES: &[Rule] = &[
	Rule { question: "what a submission is", answer: "one command list, submitted whole. A partially submitted list is not a state this profile has" },
	Rule { question: "ordering", answer: "submissions complete in the order they were submitted. A backend may execute them out of order only where nothing can observe it" },
	Rule { question: "how completion is observed", answer: "by a typed completion the submission answers with, not by a sleep and not by a query loop. The 2D profile's presentation completion has the same shape for the same reason" },
	Rule { question: "readback", answer: "a readback is a command IN a list and completes with it. A readback issued outside a list would need its own ordering rules against the lists around it" },
	Rule { question: "what a readback of an untouched attachment answers", answer: "the clear value where the pass cleared, and a REFUSAL where the pass discarded. Reading discarded content is how one backend's uninitialised memory becomes another's black" },
	Rule { question: "object-id readback", answer: "an integer attachment read back at a pixel, with the integer resolve rule above. It is in the profile because picking is not optional in an application, and every implementation that leaves it out grows a worse version of it" },
];

/// The guaranteed minimum limits: the floor a conforming backend may not go below.
///
/// WITHOUT A FLOOR EVERY OTHER SENTENCE IS SATISFIABLE BY A USELESS IMPLEMENTATION. A backend with a
/// maximum texture extent of 16 and a maximum draw count of 1 passes every rule above.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MinimumLimit {
	pub name: &'static str,
	pub minimum: u32,
	pub why: &'static str,
}

pub const RENDER3D_PROFILE_1_MIN_LIMITS: &[MinimumLimit] = &[
	MinimumLimit { name: "max_texture_extent_2d", minimum: 4096, why: "a 4K colour attachment is the size of one screen, and a backend that cannot hold one cannot render a frame" },
	MinimumLimit { name: "max_texture_extent_3d", minimum: 256, why: "a 256-cubed volume is the smallest useful one and fits in 64 MB at RGBA8" },
	MinimumLimit { name: "max_texture_layers", minimum: 256, why: "one layer per shadow cascade, per material atlas page and per instance set" },
	MinimumLimit { name: "max_colour_attachments", minimum: 4, why: "a deferred pass needs colour, normal, material and an object id, which is four" },
	MinimumLimit { name: "max_vertex_streams", minimum: 4, why: "position, normal, texture coordinates and one instance stream" },
	MinimumLimit { name: "max_vertex_attributes", minimum: 16, why: "four streams of four attributes" },
	MinimumLimit { name: "max_draws_per_pass", minimum: 65536, why: "a scene of a few thousand objects with a few materials each, which is an ordinary scene and not a stress test" },
	MinimumLimit { name: "max_shader_instructions", minimum: 4096, why: "enough for a physically based material with four lights, which is the standard material this profile's scene layer ships" },
	MinimumLimit { name: "max_uniform_bytes_per_stage", minimum: 16384, why: "a 4x4 matrix per bone for 64 bones, plus the per-frame block" },
	MinimumLimit { name: "max_samplers_per_stage", minimum: 16, why: "a PBR material is five textures, and a pass has a shadow map and an environment map on top" },
	MinimumLimit { name: "max_anisotropy", minimum: 8, why: "the value the feature list names; below it the anisotropic rule above collapses to something the profile already has" },
];

//! THE NUMBERS GEOMETRY NEEDS, and the answers to the questions a boolean operation asks.
//!
//! A TOLERANCE THAT IS NOT ONE NUMBER IS NOT A TOLERANCE. Fills, strokes, boolean operations, hit
//! tests and bounds all consult geometry, and if each flattens to its own tolerance then a point is
//! inside a path for a hit test and outside it for the fill that drew it. One number, shared.
//!
//! AND A PROJECTIVE TRANSFORM GIVES IT A SINGULARITY TO ANSWER FOR. A control point at `w == 0`
//! projects to infinity and has no useful flattening, so the epsilon and what happens at it are
//! values here rather than an implementation's guess.

/// The one flattening tolerance, in PHYSICAL PIXELS.
///
/// PHYSICAL AND NOT LOGICAL, because a curve is flattened for a device: flattening in logical pixels
/// makes a curve on a two-times display twice as coarse as the same curve on a one-times display,
/// which is visible as facets on exactly the screens that show them best.
pub const FLATTENING_TOLERANCE_PIXELS: f64 = 0.25;

/// How far subdivision may go before it stops.
pub const MAX_SUBDIVISION_DEPTH: u32 = 16;

/// WHAT HAPPENS AT THE DEPTH LIMIT, which is the part an implementation guesses.
///
/// THE SEGMENT IS EMITTED AS A LINE rather than the primitive being refused. A curve that needed a
/// seventeenth subdivision is a curve whose remaining error is below anything a reader can see at the
/// tolerance above; refusing the whole path over it would make a legal drawing fail for a reason
/// nobody can act on.
pub const AT_MAX_DEPTH: &str = "the remaining segment is emitted as a line; the primitive is not refused";

/// The projective `w` below which a point is on the horizon.
pub const PROJECTIVE_W_EPSILON: f64 = 1.0 / 1_048_576.0;

/// THE HORIZON RULE. A curve whose control point projects to infinity has no useful flattening.
///
/// THE SEGMENT IS CLIPPED against the `w = epsilon` plane in homogeneous space BEFORE the divide, and
/// only a segment with no part on the near side of it is dropped. Dividing first and clipping after
/// is the version that produces a vertex at ten million pixels and a rasteriser that spends a second
/// on one triangle.
pub const HORIZON_RULE: &str = "clip the segment against w = epsilon in homogeneous space before dividing; a segment entirely beyond it is dropped, and the primitive is refused only if nothing survives";

/// One question a boolean operation asks, and the answer this profile gives.
///
/// TWO BACKENDS PRODUCE DIFFERENT UNIONS IF ANY OF THESE IS LEFT OPEN, and the difference is not
/// subtle: it is a hole that is there in one and not the other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BooleanRule {
	pub question: &'static str,
	pub answer: &'static str,
}

/// The boolean-operation answers, rather than the boolean-operation questions.
pub const BOOLEAN_RULES: &[BooleanRule] = &[
	BooleanRule { question: "how self-intersections are resolved", answer: "each operand is rewritten FIRST into non-self-intersecting contours under its own fill rule; operating on a self-intersecting operand makes the result depend on the algorithm's traversal order" },
	BooleanRule { question: "what an OPEN subpath means as an operand", answer: "it is closed with a straight segment from its last point to its first. A boolean operation is defined on REGIONS and an open subpath is not one - and refusing it would fail the commonest use, the union of two stroke outlines" },
	BooleanRule { question: "how two operands with different fill rules combine", answer: "each is resolved to regions under ITS OWN rule first, so the operation is on regions and the two rules never have to agree. The result is non-zero" },
	BooleanRule { question: "whether the result preserves curves or is polygonised", answer: "POLYGONISED at the flattening tolerance, and the profile says so: preserving curves needs exact curve-curve intersection, whose answer is approximate anyway - so the honest form is the polygon, at a tolerance the caller knows" },
	BooleanRule { question: "the tolerance used", answer: "the one flattening tolerance, with points within a coincidence epsilon of each other treated as one" },
	BooleanRule { question: "the canonical ordering and winding of the output contours", answer: "contours sorted by their bounding box's minimum y then minimum x then their first point; an outer contour is wound so its signed area is POSITIVE in the device's y-down space and a hole negative" },
	BooleanRule { question: "what happens to degenerate and zero-length edges", answer: "dropped before the operation. A contour left with fewer than three distinct points contributes nothing, and a result with no contours is an EMPTY path rather than a refusal" },
	BooleanRule { question: "determinism", answer: "the output is a function of the input bytes and the tolerance alone: no reduction whose order depends on threading, and no iteration over a container whose order is not stated" },
];

/// How close two points must be to be the same point.
///
/// A SEPARATE AND SMALLER NUMBER THAN THE FLATTENING TOLERANCE, because they answer different
/// questions: the tolerance is how far a curve may deviate from its flattening, and this is when two
/// vertices are one. Using the tolerance for both merges vertices a quarter of a pixel apart, which
/// collapses thin features that were meant to be there.
pub const COINCIDENCE_EPSILON_PIXELS: f64 = 1.0 / 256.0;

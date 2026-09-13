//! THE NUMBERS A CONFORMANCE COMPARISON USES, and the ones it must not.
//!
//! A conformance suite without stated tolerances is a suite that either demands bit-exactness - which
//! no two implementations of a sine achieve - or demands nothing, which passes everything. Both are
//! ways of not checking. These are the numbers, per profile, with the classifications that have NO
//! tolerance named separately so a reader can see that the list is deliberate rather than incomplete.
//!
//! NO TOLERANCE MAY BE LOOSENED PER ARCHITECTURE. The whole point of a threshold is that one
//! implementation's output is compared against another's; a threshold that widened on the slow target
//! would be a suite that stopped checking exactly where the second implementation is.

use crate::render3d_spec::Rule;

/// One numerical tolerance, and where it applies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Threshold {
	pub profile: &'static str,
	pub what: &'static str,
	/// The tolerance, written as the expression a comparison evaluates.
	pub tolerance: &'static str,
	pub why: &'static str,
}

/// The thresholds, per profile.
pub const THRESHOLDS: &[Threshold] = &[
	// ---- shared, which is where colour lives ----
	Threshold { profile: "image-colour", what: "a colour conversion's round trip", tolerance: "<= 1 code value at 8 bits, <= 4 at 10 bits, and <= 1e-4 relative in a floating-point space", why: "one code value is the quantisation itself; anything larger is the conversion being wrong rather than the storage being coarse" },
	Threshold { profile: "image-colour", what: "the tone-map operator's output", tolerance: "<= 2 ULP of f32 against the operator's own expression", why: "it is a closed-form expression with no iteration, so the only error is rounding order" },
	Threshold { profile: "image-colour", what: "the comparison space for a final colour", tolerance: "NAMED: linear Rec. 709 premultiplied, at f32. A comparison in an encoded space hides a large linear error in the dark half and inflates a small one in the bright half", why: "two suites comparing in different spaces disagree about the same pair of images" },
	// ---- 2D ----
	Threshold { profile: "render2d", what: "antialiasing coverage", tolerance: "<= 2/255 absolute per pixel against the analytic area, and <= 1/255 mean over the covered region", why: "the per-pixel bound admits one rounding step of an 8-bit coverage value; the mean bound is what stops a systematic bias passing because every pixel is within one step of it" },
	Threshold { profile: "render2d", what: "a filter kernel's output", tolerance: "<= 1e-3 relative against the kernel evaluated at f64, per channel in linear space", why: "a separable blur is a sum of hundreds of taps, and f32 accumulation over that many terms is worth a thousandth" },
	Threshold { profile: "render2d", what: "the image comparison mask", tolerance: "EVERY pixel of the target is compared. There is no border exclusion and no mask, and a suite that needs one has found a defect rather than an edge case", why: "an excluded border is where clipping, coverage and the edge rule all fail at once" },
	Threshold { profile: "render2d", what: "the edge policy", tolerance: "a pixel whose analytic coverage is exactly 0 or exactly 1 is compared EXACTLY; only partially covered pixels carry the coverage tolerance above", why: "a fully covered pixel that differs at all is a colour error wearing an antialiasing tolerance" },
	// ---- 3D ----
	Threshold { profile: "render3d", what: "a clip-space position", tolerance: "NONE. Bit-exact under StrictF32", why: "it is what makes two backends place a triangle in the same place, and a tolerance here is a tolerance on WHERE a picture is" },
	Threshold { profile: "render3d", what: "sample ownership", tolerance: "NONE. A sample is covered or it is not, and the two backends must agree on every sample", why: "coverage is decided by the frozen sample positions, so there is nothing left to round" },
	Threshold { profile: "render3d", what: "an interpolated attribute", tolerance: "<= 4 ULP of f32 against the barycentric expression evaluated at f64", why: "the interpolation is three multiplies and two adds whose order the profile does not fix, which is worth a few ULP and nothing more" },
	Threshold { profile: "render3d", what: "a depth value", tolerance: "<= 1 ULP for a floating-point format and EXACT for a normalised one", why: "a normalised depth is an integer after the conversion, and an integer that differs is a different depth test" },
	Threshold { profile: "render3d", what: "a filtered texture sample", tolerance: "<= 2/255 per channel for a normalised format and <= 1e-4 relative for a floating-point one", why: "the taps and their weights are fixed by the sampler contract, so the remaining error is the weight arithmetic" },
	Threshold { profile: "render3d", what: "a transcendental in a shader", tolerance: "the ULP bound `Shader IR 1` states for that operation, and no other", why: "restating it here would be a second number to keep in step with the first" },
	// ---- scene, core ----
	Threshold { profile: "scene3d", what: "a world transform", tolerance: "<= 4 ULP of f32 per component against the composition evaluated at f64", why: "a chain of matrix multiplications whose association the profile does fix, leaving only rounding" },
	Threshold { profile: "scene3d", what: "a culling decision", tolerance: "NONE for a drawable wholly inside or wholly outside the frustum; a drawable within 1e-4 of a plane may be classified either way", why: "the boundary case is decided by a subtraction of nearly equal numbers, and demanding agreement there would be demanding bit-exact arithmetic for a decision that cannot change the picture" },
	Threshold { profile: "scene3d", what: "a lit colour", tolerance: "<= 2/255 per channel in the named comparison space", why: "the accumulation order is fixed by the profile, so what is left is the rounding of a handful of terms" },
	Threshold { profile: "scene3d", what: "a pick result", tolerance: "NONE. The id is an integer and the two implementations must answer the same one", why: "a tolerance on an identity is not a tolerance" },
	// ---- scene, extended ----
	Threshold { profile: "scene3d-extended", what: "a physically based direct term", tolerance: "<= 4/255 per channel in the named comparison space", why: "wider than the core material because the term is a product of four functions each with its own rounding, and narrower than anything a different NDF would produce - which is what the tolerance has to distinguish" },
	Threshold { profile: "scene3d-extended", what: "an environment lookup", tolerance: "<= 8/255 per channel, with the prefilter and the BRDF table generated by this profile's own rules", why: "the prefilter is an importance-sampled integral, and its residual noise is the largest term in the comparison" },
	Threshold { profile: "scene3d-extended", what: "a shadow test", tolerance: "<= 1 of the 9 percentage-closer taps may differ at a fragment, and <= 0.1 per cent of the fragments in a frame may differ at all", why: "the tap positions are fixed but the depth comparison is at the edge of the bias, so a single tap can flip; a frame where many flip is a different bias rather than a rounding difference" },
	Threshold { profile: "scene3d-extended", what: "a tone-mapped and bloomed frame", tolerance: "<= 2/255 per channel after the shared tone-map operator, with bloom compared before encoding", why: "the operator is the shared one, so the tolerance is the shared one; the bloom pyramid's own error is bounded by the filter tolerance above" },
];

/// What the thresholds do NOT admit, stated so the list reads as deliberate.
pub const THRESHOLD_RULES: &[Rule] = &[
	Rule { question: "per-architecture loosening", answer: "REFUSED. A tolerance that widened on the slow target would be a suite that stopped checking exactly where the second implementation is, which is the only place a comparison is worth making" },
	Rule { question: "what has no tolerance at all", answer: "clip-space positions, sample ownership, bit-exact classifications, normalised depth values and pick ids. Each is either an integer or is bit-exact by the StrictF32 rules, and a tolerance on one would be a tolerance on identity" },
	Rule { question: "where a tolerance is compared", answer: "in the NAMED comparison space - linear Rec. 709 premultiplied at f32 - and never in an encoded one, which hides a large error in the dark half and inflates a small one in the bright half" },
	Rule { question: "the reference", answer: "the expression evaluated at f64 where one exists, and the other implementation where it does not. A suite that compared two f32 implementations against each other alone would pass two implementations that are wrong in the same way" },
	Rule { question: "when a threshold may change", answer: "with a version change of the profile it belongs to, because the threshold is part of what conforming means" },
	Rule { question: "a freeze with no thresholds", answer: "is NOT COMPLETE, and neither is a backend measured against one. Writing these numbers is specification work and not a testing detail" },
];

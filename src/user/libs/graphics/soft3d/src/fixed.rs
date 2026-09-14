//! THE FIXED-POINT RASTER GRID, AND THE PROOF THAT ITS ARITHMETIC CANNOT OVERFLOW.
//!
//! FIXED-POINT EDGE EQUATIONS AND NOT FLOATING-POINT COMPARISONS. Two triangles that share an edge
//! must produce exactly complementary coverage along it: every sample belongs to one or the other,
//! never to both and never to neither. In floating point the same edge evaluated from two different
//! vertex orders gives two different values, so a shared edge either double-fills - which is visible
//! the moment anything is blended - or leaves a seam of background pixels. In fixed point the edge
//! function is an integer, the two evaluations are the same integer, and the top-left rule decides
//! the tie by a rule rather than by a rounding mode.
//!
//! AND IT IS WHAT MAKES THREE ARCHITECTURES AGREE. Integer arithmetic is exact everywhere; a float
//! comparison is not, which is why coverage is on the BIT-EXACT side of this stack's split and
//! shading is not.
//!
//! THE BOUND IS A PROOF AND NOT A HOPE, and it is written out here because "use i64" without the
//! numbers is half a contract:
//!
//! - `MAX_RASTER_EXTENT` is `16_384` pixels, so a clipped viewport coordinate lies in
//!   `0 ..= 16_384` pixels.
//! - `SUBPIXEL_BITS` is `8`, so in subpixel units that range is `0 ..= 16_384 * 256`, which is
//!   `0 ..= 2^22`.
//! - A coordinate DIFFERENCE is therefore at most `2^22` in magnitude.
//! - An edge function is `(x - x0) * (y1 - y0) - (y - y0) * (x1 - x0)`: each product is at most
//!   `2^22 * 2^22 = 2^44`, and the difference of two such products is bounded by `2^45`.
//! - `i64` holds `2^63`, so there are eighteen bits of headroom. The products are formed in `i64`
//!   and never in `i32`, where `2^44` would wrap silently.
//!
//! THE COORDINATE BOUND IS VALIDATED BEFORE ANY EDGE IS EVALUATED, because the proof is about
//! coordinates that satisfy it. A coordinate EQUAL to `2^22` is admitted and one past it is refused:
//! the arithmetic above is inclusive of `2^22`, and a check written as "fits in 22 bits" would
//! wrongly refuse the largest legal value.

/// Fractional bits in a raster coordinate. Eight gives 1/256 of a pixel, which is finer than any
/// coverage difference a person can see and coarse enough to keep the products small.
pub const SUBPIXEL_BITS: u32 = 8;

/// One pixel, in subpixel units.
pub const SUBPIXEL_ONE: i64 = 1 << SUBPIXEL_BITS;

/// Half a pixel, which is where a sample sits at 1x.
pub const SUBPIXEL_HALF: i64 = SUBPIXEL_ONE / 2;

/// The largest raster target this backend admits, in pixels, on either axis.
pub const MAX_RASTER_EXTENT: u32 = 16_384;

/// The largest legal coordinate in subpixel units: `MAX_RASTER_EXTENT` pixels. INCLUSIVE.
pub const MAX_SUBPIXEL_COORD: i64 = MAX_RASTER_EXTENT as i64 * SUBPIXEL_ONE;

/// Why a coordinate was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FixedFault {
	/// A coordinate outside `0 ..= MAX_SUBPIXEL_COORD`, which the edge-function bound above is
	/// stated for. A primitive that reached here with one has not been clipped.
	OutOfRange { subpixel: i64 },
	/// A non-finite coordinate, which has no fixed-point representation at all.
	NotFinite,
}

/// A viewport coordinate in subpixel units.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Subpixel(pub i64);

impl Subpixel {
	/// The whole pixel this coordinate is in, rounding towards negative infinity.
	pub const fn pixel(self) -> i64 {
		self.0 >> SUBPIXEL_BITS
	}
}

/// Quantise a float viewport coordinate, ROUND-HALF-AWAY-FROM-ZERO.
///
/// AWAY FROM ZERO AND NOT TO EVEN. The rule has to be stated because the three architectures this
/// runs on have different default rounding in their float-to-int paths, and a coordinate that
/// rounded differently on one of them would put a sample on the other side of an edge - which is a
/// pixel of difference in a comparison that is meant to be exact. Away-from-zero is chosen over
/// to-even because it is the same rule at every magnitude, so a reader can apply it by hand.
///
/// REFUSES rather than saturating. A coordinate outside the range is a primitive that was not
/// clipped, and clamping it would rasterise a triangle that is not the one that was submitted.
pub fn quantise(value: f32) -> Result<Subpixel, FixedFault> {
	if !value.is_finite() {
		return Err(FixedFault::NotFinite);
	}
	let scaled = value as f64 * SUBPIXEL_ONE as f64;
	// `round` is not in `core`; half-away-from-zero is `trunc(x + 0.5 * sign(x))`.
	let rounded = if scaled >= 0.0 { (scaled + 0.5) as i64 } else { (scaled - 0.5) as i64 };
	admit(rounded)
}

/// Admit a subpixel coordinate the caller already has, applying the same bound.
pub fn admit(subpixel: i64) -> Result<Subpixel, FixedFault> {
	if !(0..=MAX_SUBPIXEL_COORD).contains(&subpixel) {
		return Err(FixedFault::OutOfRange { subpixel });
	}
	Ok(Subpixel(subpixel))
}

/// The edge function of the directed edge `from -> to`, evaluated at `at`.
///
/// POSITIVE IS TO THE RIGHT OF THE EDGE in a left-handed screen space where `y` grows downward,
/// which after the viewport's Y inversion makes a counter-clockwise triangle in NDC produce three
/// positive edges. The sign convention is stated because `winding::facing` decides the front face
/// BEFORE the inversion, and a rasteriser that took the other sign here would cull the wrong side
/// while satisfying every other rule in the stack.
///
/// `i64` THROUGHOUT. The products reach `2^44`, which `i32` cannot hold.
pub const fn edge(from: (Subpixel, Subpixel), to: (Subpixel, Subpixel), at: (Subpixel, Subpixel)) -> i64 {
	let (x0, y0) = (from.0.0, from.1.0);
	let (x1, y1) = (to.0.0, to.1.0);
	let (x, y) = (at.0.0, at.1.0);
	(x - x0) * (y1 - y0) - (y - y0) * (x1 - x0)
}

/// Whether an edge is a TOP or a LEFT edge, which is what the fill rule needs.
///
/// THE TOP-LEFT RULE IS WHAT MAKES A SHARED EDGE FILL EXACTLY ONCE. A sample exactly on an edge
/// belongs to the triangle for which that edge is a top or a left one; for the triangle on the other
/// side the same edge is a bottom or a right one, so it does not. Without the rule a sample on a
/// shared edge is either in both triangles - visible wherever anything is blended - or in neither,
/// which is a seam of background.
///
/// In a downward-`y` screen space: a TOP edge is exactly horizontal and goes right to left; a LEFT
/// edge is one that goes downward.
pub const fn is_top_or_left(from: (Subpixel, Subpixel), to: (Subpixel, Subpixel)) -> bool {
	let (x0, y0) = (from.0.0, from.1.0);
	let (x1, y1) = (to.0.0, to.1.0);
	let horizontal = y0 == y1;
	if horizontal { x1 < x0 } else { y1 > y0 }
}

/// Whether a sample is covered by an edge, applying the fill rule to the tie.
pub const fn covered_by(value: i64, top_or_left: bool) -> bool {
	if value > 0 {
		true
	} else if value < 0 {
		false
	} else {
		// EXACTLY ON THE EDGE: the fill rule decides, and it decides the same way from both sides.
		top_or_left
	}
}

/// Twice the signed area of a triangle in subpixel units. ZERO MEANS DEGENERATE.
pub const fn double_area(a: (Subpixel, Subpixel), b: (Subpixel, Subpixel), c: (Subpixel, Subpixel)) -> i64 {
	edge(a, b, c)
}

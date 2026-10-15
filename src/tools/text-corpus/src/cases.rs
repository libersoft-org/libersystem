//! THE VARIATION CASES: what the variable face measures at one instance, read from the face rather
//! than from the shaper.
//!
//! THE SHAPING AND PARAGRAPH CASES ARE NOT HERE. They live in `src/user/apps/tools/src/textconf/corpus.rs`,
//! which this gate includes and the guest conformance run compiles, so the two read one set of
//! expectations. These stay with the gate because nothing but the gate reads them: the variable face
//! is installed in no guest.
//!
//! EVERY NUMBER HERE COMES FROM THE FACE'S DESIGN and not from a run of the stack, exactly as the
//! shaping cases' do.

const VARIABLE: &str = "conformance-variable.ttf";

/// One instance of the variable face, and what it measures there.
pub struct Instance {
	pub name: &'static str,
	pub face: &'static str,
	pub glyph: &'static str,
	/// The user coordinate on the one axis, in design units.
	pub coordinate: i32,
	pub advance: i32,
	pub point: usize,
	pub x: i16,
}

/// THE CASE THE PLAN NAMES: metrics that vary INDEPENDENTLY of outlines.
///
/// The face gives the two their own halves of the axis. Above the default the advance moves through
/// `HVAR` and the outline does not; below it the outline moves through `gvar` and the advance does
/// not. A stack that took the advance from the outline's phantom points, or the outline from the
/// advance, gets one of the four rows below wrong - and each row is a number the face declares
/// rather than a number a previous run produced.
pub static INSTANCES: &[Instance] = &[
	Instance { name: "the default instance is the face as drawn", face: VARIABLE, glyph: "bar", coordinate: 400, advance: 600, point: 2, x: 400 },
	Instance { name: "at the top of the axis the advance moves and the outline does not", face: VARIABLE, glyph: "bar", coordinate: 900, advance: 780, point: 2, x: 400 },
	Instance { name: "half way up, half the advance delta", face: VARIABLE, glyph: "bar", coordinate: 650, advance: 690, point: 2, x: 400 },
	Instance { name: "at the bottom the outline moves and the advance does not", face: VARIABLE, glyph: "bar", coordinate: 100, advance: 600, point: 2, x: 550 },
	Instance { name: "half way down, half the outline delta", face: VARIABLE, glyph: "bar", coordinate: 250, advance: 600, point: 2, x: 475 },
];

//! THE NUMERIC CEILINGS, FROZEN WITH THE PROFILE RATHER THAN AFTER IT.
//!
//! CHECKED OFFSETS DO NOT BOUND WORK. Every length and every index in this parser is checked against
//! the table it is in, which is what stops a crafted font reading somebody else's memory - and it
//! stops none of the other thing a font can do. A STRUCTURALLY VALID face can ask for a composite
//! glyph nested inside itself five hundred deep, a contextual rule whose output is a thousand times
//! its input, or a paint graph with a million nodes, and every offset in it is in range. What that
//! exhausts is time, stack and the caller's memory, and none of it is a parse error at any single
//! read.
//!
//! SO THEY ARE VALUES AND NOT CATEGORIES. An earlier form of this list named the things to decide -
//! "a limit on composite depth", "a limit on expansion" - which is a list of decisions and not a
//! bound: a gate asking "is every numeric limit tested" had no number to test at, and two
//! implementations could pick different ceilings after the work began. They are part of the CLOSED
//! PROFILE, they are hashed with it, and raising one afterwards is a profile change with its
//! conformance consequences.
//!
//! THE ABSOLUTE ONES ARE WHAT MAKE THE REST BOUND ANYTHING. Most of the rows below are either
//! font-INTERNAL - a property of the face, which a larger document does not change - or
//! PROPORTIONAL. `64x the input run` is the clearest case: it caps the MULTIPLIER and not the
//! product, so an arbitrarily large source still demands arbitrarily large work. A proportional rule
//! with no absolute ceiling under it is a ratio, not a bound; the four input and output ceilings are
//! that floor, and the proportional rule continues to apply INSIDE them rather than being replaced
//! by them.
//!
//! BOTH DIRECTIONS, BECAUSE EITHER ALONE LEAVES A HOLE. An input cap without an output cap still
//! admits a run a pathological face expands sixty-four fold; an output cap without an input cap still
//! admits an unbounded source whose refusal is only discovered after it has been read. They are
//! independent and both refuse, whichever binds first.
//!
//! EXCEEDING ONE IS A REFUSAL AND NEVER A TRUNCATION. Truncating is a document silently rendered
//! wrong, which is the failure that is never reported because it does not look like one.

/// The largest font file this profile opens.
pub const FONT_BYTES: u32 = 16 * 1024 * 1024;
/// The largest single table within it.
pub const TABLE_BYTES: u32 = 4 * 1024 * 1024;
/// How deeply a composite glyph may nest components.
pub const COMPOSITE_DEPTH: u32 = 5;
/// How many points one glyph may have after every component is expanded.
pub const COMPOSITE_POINTS: u32 = 10_000;
/// How deeply a `CFF`/`CFF2` charstring may call subroutines.
pub const CHARSTRING_DEPTH: u32 = 10;
/// How many values a charstring's operand stack holds.
pub const CHARSTRING_STACK: u32 = 48;
/// How deeply a `COLR` v1 paint graph may nest.
pub const PAINT_DEPTH: u32 = 64;
/// How many paint nodes one glyph's graph may have.
pub const PAINT_NODES: u32 = 8192;
/// How deeply a contextual `GSUB`/`GPOS` rule may recurse into another lookup.
pub const CONTEXT_DEPTH: u32 = 64;
/// How much larger than its input a shaping run's output may become.
pub const OUTPUT_EXPANSION: u32 = 64;
/// How many variation axes a face may declare.
pub const VARIATION_AXES: u32 = 64;
/// How many regions one item variation store may hold.
pub const VARIATION_REGIONS: u32 = 4096;
/// How many features one shaping run may select.
pub const FEATURES: u32 = 256;
/// How deeply bidi controls may nest - the algorithm's own maximum depth.
pub const BIDI_DEPTH: u32 = 125;
/// How many fallback faces may be tried for one cluster.
pub const FALLBACK_FACES: u32 = 16;
/// How many times one run may be re-shaped.
pub const SHAPING_RETRIES: u32 = 4;
/// How many layout passes one line may take.
pub const LINE_PASSES: u32 = 8;
/// How many layout passes one paragraph may take.
pub const PARAGRAPH_PASSES: u32 = 2;
/// THE ABSOLUTE INPUT CEILING for one shaping run, in code points.
pub const RUN_INPUT: u32 = 4096;
/// THE ABSOLUTE INPUT CEILING for one paragraph, in code points.
pub const PARAGRAPH_INPUT: u32 = 65_536;
/// THE ABSOLUTE OUTPUT CEILING for one shaping run, in glyphs.
pub const RUN_OUTPUT: u32 = 16_384;
/// THE ABSOLUTE OUTPUT CEILING for one paragraph, in glyphs.
pub const PARAGRAPH_OUTPUT: u32 = 262_144;

/// Which of the three kinds a ceiling is, because it decides what the ceiling can promise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	/// A property of the FACE. A larger document does not change it.
	Internal,
	/// A ratio against an input. It caps the multiplier and not the product.
	Proportional,
	/// An ABSOLUTE ceiling on a document, which is what makes the proportional ones bound anything.
	Absolute,
}

impl Kind {
	pub const fn name(self) -> &'static str {
		match self {
			Kind::Internal => "internal",
			Kind::Proportional => "proportional",
			Kind::Absolute => "absolute",
		}
	}
}

/// One ceiling, with the value a call site uses and the reason a reader needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limit {
	/// The name a refusal carries, which is how a report says WHICH ceiling was met.
	pub name: &'static str,
	pub value: u32,
	pub unit: &'static str,
	pub kind: Kind,
	/// What a font or a document could do without it.
	pub why: &'static str,
}

/// Every ceiling `OpenType Profile 1` freezes.
///
/// THE TABLE AND THE CONSTANTS ARE THE SAME VALUES, and a fixture proves it: a call site uses the
/// constant, a document is written from the table, and a table that drifted from the constants would
/// publish a ceiling nothing enforces.
pub const LIMITS: &[Limit] = &[
	Limit { name: "font bytes", value: FONT_BYTES, unit: "bytes per face", kind: Kind::Internal, why: "a face larger than any real one is a document trying to exhaust memory before it is parsed" },
	Limit { name: "table bytes", value: TABLE_BYTES, unit: "bytes per table", kind: Kind::Internal, why: "one table claiming most of a file is a length nobody drew" },
	Limit { name: "composite depth", value: COMPOSITE_DEPTH, unit: "nested components", kind: Kind::Internal, why: "a composite glyph is the one place recursion enters a font parser" },
	Limit { name: "composite points", value: COMPOSITE_POINTS, unit: "points after expansion", kind: Kind::Internal, why: "five levels of nesting multiply, and depth alone bounds the stack rather than the work" },
	Limit { name: "charstring depth", value: CHARSTRING_DEPTH, unit: "nested subroutine calls", kind: Kind::Internal, why: "a subroutine that calls itself is a charstring that never returns" },
	Limit { name: "charstring stack", value: CHARSTRING_STACK, unit: "operands", kind: Kind::Internal, why: "the interpreter's own working set, which a font must not choose the size of" },
	Limit { name: "paint depth", value: PAINT_DEPTH, unit: "nested paints", kind: Kind::Internal, why: "a paint graph is a graph, and a cycle in it is a glyph that never finishes" },
	Limit { name: "paint nodes", value: PAINT_NODES, unit: "nodes per glyph", kind: Kind::Internal, why: "a bounded depth over an unbounded breadth is still unbounded work" },
	Limit { name: "context depth", value: CONTEXT_DEPTH, unit: "nested lookups", kind: Kind::Internal, why: "a contextual rule invokes another lookup, which may invoke it again" },
	Limit { name: "output expansion", value: OUTPUT_EXPANSION, unit: "x the input run", kind: Kind::Proportional, why: "a one-to-many substitution applied repeatedly turns a word into a page" },
	Limit { name: "variation axes", value: VARIATION_AXES, unit: "axes per face", kind: Kind::Internal, why: "every axis multiplies the regions a delta is scaled over" },
	Limit { name: "variation regions", value: VARIATION_REGIONS, unit: "regions per store", kind: Kind::Internal, why: "each region is read for every delta, so the store's width is work per glyph" },
	Limit { name: "features", value: FEATURES, unit: "features per run", kind: Kind::Internal, why: "each selected feature contributes lookups, and each lookup walks the buffer" },
	Limit { name: "bidi depth", value: BIDI_DEPTH, unit: "nested controls", kind: Kind::Absolute, why: "the algorithm's own maximum depth, which is not this profile's choice to make" },
	Limit { name: "fallback faces", value: FALLBACK_FACES, unit: "faces per cluster", kind: Kind::Proportional, why: "a cluster no face covers would otherwise try every face in the catalogue" },
	Limit { name: "shaping retries", value: SHAPING_RETRIES, unit: "retries per run", kind: Kind::Proportional, why: "a retry re-shapes, so an unbounded count multiplies the whole run's cost" },
	Limit { name: "line passes", value: LINE_PASSES, unit: "passes per line", kind: Kind::Proportional, why: "justification iterates, and an iteration that does not converge must still stop" },
	Limit { name: "paragraph passes", value: PARAGRAPH_PASSES, unit: "passes per paragraph", kind: Kind::Proportional, why: "the same, one level up, where each pass costs every line" },
	Limit { name: "run input", value: RUN_INPUT, unit: "code points per run", kind: Kind::Absolute, why: "WITHOUT THIS THE PROPORTIONAL RULES BOUND NOTHING: a ratio against an unbounded input is not a bound" },
	Limit { name: "paragraph input", value: PARAGRAPH_INPUT, unit: "code points per paragraph", kind: Kind::Absolute, why: "the same at the layer that reads a whole document's text" },
	Limit { name: "run output", value: RUN_OUTPUT, unit: "glyphs per run", kind: Kind::Absolute, why: "an input cap alone still admits a run a pathological face expands sixty-four fold" },
	Limit { name: "paragraph output", value: PARAGRAPH_OUTPUT, unit: "glyphs per paragraph", kind: Kind::Absolute, why: "an output cap alone still admits a source whose refusal is discovered only after it is read" },
];

/// The ceiling by name, for a report and a gate. A name this profile does not carry is `None` rather
/// than a default: a limit nobody declared is not a limit.
pub fn limit(name: &str) -> Option<&'static Limit> {
	LIMITS.iter().find(|entry| entry.name == name)
}

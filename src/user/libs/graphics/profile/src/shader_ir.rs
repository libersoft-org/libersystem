//! `Shader IR 1`: WHAT A SHADER MEANS, which is not the same as what it looks like.
//!
//! An intermediate representation is a list of operations, and a list of operations is not a
//! specification: two implementations that agree about every opcode still disagree about what
//! happens when an integer overflows, what a division by zero produces, whether a NaN compares
//! equal to itself, what an out-of-bounds array read returns, and where a derivative comes from in a
//! lane that was discarded. Every one of those has been a shipping graphics bug, and every one of
//! them is answered here rather than left to a backend.
//!
//! THE PROFILE HAS TWO ARITHMETICS AND SAYS SO. Anything a vertex POSITION depends on is StrictF32:
//! IEEE 754 binary32, round-to-nearest-even, no contraction, no reassociation, no fast reciprocal.
//! Fragment arithmetic that reaches only a colour is relaxed within stated bounds. The two are not a
//! quality setting - the first is what makes two backends agree about where a triangle is, and the
//! second is what makes a shader affordable. An operation with no deterministic strict definition is
//! REFUSED on a position dependency path and permitted off it.

use crate::render3d_spec::Rule;

/// The version this IR is frozen at. A shader carries it, and a consumer that does not know it
/// refuses the module rather than guessing which meaning the bytes have.
pub const SHADER_IR_VERSION: u32 = 1;

// ---------------------------------------------------------------------------------------------
// Numbers.
// ---------------------------------------------------------------------------------------------

/// What the arithmetic does at the points where implementations differ.
pub const NUMERIC_RULES: &[Rule] = &[
	Rule { question: "signed integer overflow", answer: "WRAPS, two's complement. Not undefined and not saturating: undefined is what lets one backend delete the code around it, and saturation makes a hash function produce different values on two machines" },
	Rule { question: "unsigned integer overflow", answer: "wraps, which is the only behaviour the type has" },
	Rule { question: "integer division by zero", answer: "produces the dividend's type's MAXIMUM for a positive dividend, its MINIMUM for a negative one, and zero for a zero dividend. A defined value rather than a trap: a shader cannot handle a trap, and undefined lets the optimiser assume the divisor is non-zero and delete the guard the author wrote" },
	Rule { question: "integer modulo by zero", answer: "produces zero, for the same reason" },
	Rule { question: "INT_MIN divided by -1", answer: "produces INT_MIN, which is the wrapping result and the only representable one" },
	Rule { question: "float division by zero", answer: "IEEE 754: a signed infinity, or a NaN for zero over zero. No flush, no trap" },
	Rule { question: "NaN comparison", answer: "IEEE 754: every ordered comparison with a NaN is false, and `!=` with a NaN is true. `min` and `max` return the NON-NaN operand when exactly one is a NaN, which is the behaviour a clamp needs" },
	Rule { question: "signed zero", answer: "PRESERVED. `-0.0` is not `0.0` bitwise, `1.0 / -0.0` is negative infinity, and a backend that normalised zeros would change the sign of a reflected ray" },
	Rule { question: "subnormals", answer: "PRESERVED on a StrictF32 path and MAY be flushed to zero off it, with the flush stated as a permission rather than assumed. Flushing on a position path makes two backends place a vertex differently by one unit in the last place, which the conformance comparison sees" },
	Rule { question: "contraction", answer: "REFUSED on a StrictF32 path: `a * b + c` is a multiply and an add, each rounded. A fused multiply-add is a DIFFERENT operation with its own opcode, and an implementation that contracted silently would produce a result no other implementation can reproduce" },
	Rule { question: "reassociation", answer: "REFUSED on a StrictF32 path. Floating-point addition is not associative, so `(a + b) + c` and `a + (b + c)` are two different numbers" },
	Rule { question: "uninitialised values", answer: "there are none: every variable is zero-initialised at its declaration, and every output not written by a path that reaches the end of the stage is zero. Reading uninitialised memory is the class of bug that behaves differently on every machine and cannot be reproduced" },
	Rule { question: "out-of-bounds array read", answer: "returns the ZERO value of the element type. Not undefined, not a clamp to the last element, not a trap - zero is the only answer that is both defined and obviously wrong, so a shader reading past its array produces a visible black rather than a plausible neighbour" },
	Rule { question: "out-of-bounds array write", answer: "is DISCARDED. Clamping would corrupt the last element, which is worse than losing the write: the corruption is attributed to whatever wrote the last element legitimately" },
	Rule { question: "a dynamic index into a matrix or vector", answer: "the same rules: out-of-bounds reads zero and out-of-bounds writes are discarded" },
];

/// How a float becomes an integer, which is the conversion two languages define differently.
pub const CONVERSION_RULES: &[Rule] = &[
	Rule { question: "float to signed integer", answer: "TRUNCATES toward zero, and a value outside the destination's range CLAMPS to its nearest bound. Truncation rather than rounding because it is what every shading language has always done; clamping rather than wrapping because a wrapped out-of-range index is a wrong array element and a clamped one is an edge" },
	Rule { question: "a NaN converted to an integer", answer: "produces zero. Every other choice is a different arbitrary value, and zero is the one a reader recognises as an error" },
	Rule { question: "integer to float", answer: "round-to-nearest-even, which can lose precision above 2^24 and does so identically everywhere" },
	Rule { question: "float to half", answer: "round-to-nearest-even, with a magnitude above the half's range producing an infinity of the right sign rather than the largest finite value" },
	Rule { question: "bit reinterpretation", answer: "exact: the bits are the bits. A NaN's payload survives, because a reinterpret that normalised it would make a bit-packing trick lossy" },
];

/// One transcendental function and the accuracy a conforming implementation must reach.
///
/// AN ACCURACY BOUND IS PART OF THE CONTRACT AND NOT A QUALITY OF IMPLEMENTATION. Without one, a
/// conformance suite either demands bit-exactness - which no two backends achieve for a sine - or
/// demands nothing, which is what lets a backend ship a four-term approximation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Accuracy {
	pub operation: &'static str,
	pub max_ulp: u32,
	pub domain: &'static str,
}

pub const TRANSCENDENTAL_ACCURACY: &[Accuracy] = &[
	Accuracy { operation: "sqrt", max_ulp: 0, domain: "correctly rounded: it is an IEEE 754 operation and every implementation already has it" },
	Accuracy { operation: "inversesqrt", max_ulp: 2, domain: "the whole positive range" },
	Accuracy { operation: "sin, cos", max_ulp: 4, domain: "|x| <= 2^14; outside it the argument reduction dominates and the profile makes no claim" },
	Accuracy { operation: "tan", max_ulp: 8, domain: "|x| <= 2^14 and away from the poles" },
	Accuracy { operation: "asin, acos, atan", max_ulp: 8, domain: "the whole defined domain" },
	Accuracy { operation: "atan2", max_ulp: 8, domain: "the whole plane, with the quadrant exact" },
	Accuracy { operation: "exp, exp2", max_ulp: 4, domain: "the whole representable range" },
	Accuracy { operation: "log, log2", max_ulp: 4, domain: "the whole positive range" },
	Accuracy { operation: "pow", max_ulp: 16, domain: "positive base; a negative base with a non-integer exponent is a NaN" },
	Accuracy { operation: "reciprocal", max_ulp: 2, domain: "the whole range; on a StrictF32 path a reciprocal is a DIVISION and is correctly rounded" },
	// THE TWO COMPOSITIONS, FROZEN AS COMPOSITIONS rather than left as whatever a backend's library
	// happens to do. Both are named in the profile's required operation set and both reach a vertex
	// position in ordinary shaders, so "no strict definition, therefore refused" would refuse the
	// shaders the profile exists to describe. What makes them strict is that the definition below
	// uses only operations this table already fixes at zero ULP.
	Accuracy { operation: "length", max_ulp: 0, domain: "`sqrt(dot(v, v))`, with the dot product accumulated in COMPONENT ORDER and with separate multiply and add - the no-FMA rule applies to it as to every other strict expression. `sqrt` is correctly rounded, so the whole composition is" },
	Accuracy { operation: "normalize", max_ulp: 0, domain: "`v / length(v)`, a DIVISION and not a multiply by `inversesqrt`: the reciprocal square root carries two ULP and would make every normalised position backend-dependent, which is the one thing a strict path may not be. A vector whose length is zero or non-finite normalises to ITSELF rather than to a NaN, because one NaN in a lighting term makes a whole surface black" },
];

// ---------------------------------------------------------------------------------------------
// Memory layout.
// ---------------------------------------------------------------------------------------------

/// How a uniform block is laid out, which is the contract the host side writes bytes against.
pub const UNIFORM_LAYOUT: &[Rule] = &[
	Rule { question: "the base alignment of a scalar", answer: "its size: 4 for a 32-bit scalar" },
	Rule { question: "the base alignment of a two-component vector", answer: "twice the component size" },
	Rule { question: "the base alignment of a three- or four-component vector", answer: "FOUR times the component size. A three-component vector is aligned as four and occupies four, which is the rule everybody gets wrong once" },
	Rule { question: "the base alignment of an array", answer: "the element's alignment rounded up to 16, and every element is padded to that stride. An array of scalars therefore costs 16 bytes per element" },
	Rule { question: "the base alignment of a structure", answer: "the largest alignment of its members, rounded up to 16" },
	Rule { question: "the size of a structure", answer: "its last member's offset plus that member's size, rounded up to the structure's alignment - so a structure inside an array is padded to a multiple of 16" },
	Rule { question: "an explicit offset", answer: "permitted, and must be a multiple of the member's base alignment and must not overlap another member. A layout that violates either is REFUSED at module load rather than producing a reader that reads somebody else's bytes" },
];

/// How a matrix is stored, which decides whether a host writing rows produces a transposed matrix.
pub const MATRIX_LAYOUT: &[Rule] = &[
	Rule { question: "the order", answer: "COLUMN-MAJOR. A `mat4` is four `vec4` columns in memory, and `m[0]` is the first COLUMN. It is the convention the linear algebra in every graphics text uses, and a profile that chose the other one would transpose every matrix a reader copies from a book" },
	Rule { question: "the stride", answer: "each column is aligned and padded as a vector of its row count, so a `mat3` has a 16-byte column stride and occupies 48 bytes" },
	Rule { question: "a row-major declaration", answer: "permitted as an explicit decoration on a member, and then the stride applies to ROWS. It is a decoration rather than a mode, so one block may hold both and each member says which it is" },
	Rule { question: "multiplication order", answer: "`M * v` treats `v` as a COLUMN vector, so a transform chain is written right to left: `projection * view * model * position`" },
];

// ---------------------------------------------------------------------------------------------
// Stages, derivatives and sampling.
// ---------------------------------------------------------------------------------------------

pub const STAGE_RULES: &[Rule] = &[
	Rule { question: "which stages exist", answer: "vertex and fragment, and no others in version 1. A geometry or tessellation stage changes what a primitive IS, and the 3D profile's clipping and provoking-vertex rules are written for primitives the vertex stage produced" },
	Rule { question: "implicit LOD", answer: "available in the FRAGMENT stage only, because it needs the derivative of a texture coordinate across a 2x2 quad and no other stage has one" },
	Rule { question: "explicit LOD", answer: "available in every stage. A vertex-stage sample must name its level, and a module that asks for an implicit one there is REFUSED at load" },
	Rule { question: "helper lanes", answer: "a fragment invocation that exists only to complete a 2x2 quad RUNS, so its derivatives are correct, and WRITES NOTHING. Its stores are discarded and its atomics do not happen" },
	Rule { question: "a derivative after `discard`", answer: "the discarded invocation becomes a HELPER LANE and keeps running for the rest of the stage. A discard that stopped the invocation would leave a neighbour computing a derivative from a lane that had stopped, which is the classic garbage-on-a-silhouette bug" },
	Rule { question: "a derivative in non-uniform control flow", answer: "REFUSED at module load where it can be proven, and otherwise defined as the derivative of the values the lanes happen to hold. The refusal is the point: a derivative inside an `if` that some lanes of a quad did not take has no meaning, and a profile that left it undefined would make a shader behave differently on two quad shapes" },
	Rule { question: "the derivative itself", answer: "the FORWARD difference within the 2x2 quad: `ddx` is the right lane minus the left lane for both rows, `ddy` is the lower minus the upper for both columns. Both lanes of a pair get the same value" },
];

// ---------------------------------------------------------------------------------------------
// StrictF32, which is the rule that makes two backends place a vertex in the same place.
// ---------------------------------------------------------------------------------------------

pub const STRICT_F32_RULES: &[Rule] = &[
	Rule { question: "what it applies to", answer: "every DATA and CONTROL dependency of a vertex position: the arithmetic that produces it, the conditions of the branches that select it, the indices of the memory it is read from, and any sampling permitted to contribute to it" },
	Rule { question: "what it requires", answer: "IEEE 754 binary32 with round-to-nearest-even, no contraction, no reassociation, no reciprocal approximation, no subnormal flush, and no wider intermediate precision" },
	Rule { question: "a transcendental on a position path", answer: "PERMITTED ONLY WITH A STRICT DEFINITION. `sqrt` is correctly rounded and is allowed; `sin` has a 4-ULP bound and is NOT deterministic across backends, so a position that depends on it is REFUSED at module load" },
	Rule { question: "a conversion on a position path", answer: "allowed: every conversion rule above is exact or correctly rounded" },
	Rule { question: "a memory index on a position path", answer: "the index computation is itself on the path, so it is strict - an index that differed by one between backends would read a different vertex" },
	Rule { question: "what happens on a refusal", answer: "the MODULE is refused at load, with the operation and the dependency path named. Not at draw time: a shader that compiles and then refuses to draw is a failure nobody can attribute" },
	Rule { question: "fragment arithmetic", answer: "relaxed within the accuracy bounds above, and it does NOT acquire strict requirements by sharing the IR with a vertex stage. A colour that differs by one ULP is not a wrong picture; a position that does is a wrong triangle" },
];

// ---------------------------------------------------------------------------------------------
// The serialised form.
// ---------------------------------------------------------------------------------------------

pub const ENCODING_RULES: &[Rule] = &[
	Rule { question: "the header", answer: "a four-byte magic, the IR version, the module's own hash over everything after the header, and the counts of the sections that follow. The hash is over the CANONICAL form, so two encoders producing the same module produce the same bytes" },
	Rule { question: "instruction identity", answer: "every instruction has a STABLE NUMERIC ID that never changes meaning. An id is retired rather than reused: reusing one makes an old module decode as a different program" },
	Rule { question: "byte order", answer: "little-endian throughout, which is the order of every target this system has" },
	Rule { question: "canonical constants", answer: "a float constant is stored as its exact 32 bits, a NaN as the canonical quiet NaN with a zero payload unless the module declares a payload it needs, and `-0.0` as itself. An encoder that normalised `-0.0` to `0.0` would change the sign of a division" },
	Rule { question: "canonical ordering", answer: "declarations are sorted by id and instructions are in execution order, so the same program always encodes to the same bytes - which is what makes the module hash a cache key rather than a checksum" },
	Rule { question: "forward compatibility", answer: "an unknown instruction id is a REFUSAL of the module, not a skip. Skipping an instruction produces a program that runs and computes something else" },
	Rule { question: "what a consumer validates before executing", answer: "the hash, the version, every offset and count against the module's length, every type against its use, and the StrictF32 dependency paths. A module that fails any of these is refused whole" },
];

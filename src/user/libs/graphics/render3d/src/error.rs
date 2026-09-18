//! What this API refuses. THE SET IS CLOSED AND EVERY NAME IS IN THE PROFILE'S OWN LIST.
//!
//! A REFUSAL NAMES WHAT WAS WRONG OR IT IS NOT ONE. "Invalid" tells a caller that something is wrong
//! with something, which is what it already knew; every variant below names the thing it is about and
//! carries the numbers needed to correct it or to report it.
//!
//! THREE PAIRS ARE KEPT APART ON PURPOSE:
//!
//!   * `LimitExceeded` against `OutOfMemory`. The first is "this implementation cannot", which no
//!     amount of free memory changes; the second is "not right now", which a caller may retry or
//!     reduce into. Folding them leaves a caller unable to tell a permanent no from a temporary one,
//!     and the temporary one is the one worth retrying.
//!   * `NonFinite` against `InvalidTransform`. A NaN that arrived in a vertex is the caller's data;
//!     a matrix with no inverse is the caller's maths. Different causes, different fixes.
//!   * `TargetMismatch` against `IncompatiblePipeline`. The first is two attachments that cannot
//!     share a pass; the second is a pipeline that contradicts the pass it was built for. One is
//!     fixed by changing a target, the other by changing a state.
//!
//! AND `Unsupported` IS UNREACHABLE WITHIN PROFILE 1 BY CONSTRUCTION. Every entry in the profile is
//! implemented by a conforming backend, so nothing inside it may answer this; it exists for the
//! extensions that arrive after Profile 1, and a backend returning it for a profile feature is a
//! defect rather than a capability.

/// Why a 3D operation was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	/// A mesh whose geometry cannot be drawn.
	InvalidMesh { reason: MeshFault },
	/// A matrix that is not a usable transform: singular where an inverse is required.
	InvalidTransform,
	/// A format outside `Render3D Core Profile 1`, or one used where the profile says it may not be:
	/// an integer format as a blend target, a non-renderable format as an attachment.
	UnsupportedFormat { format: &'static str, used_as: &'static str },
	/// Attachments that cannot be used together in one pass.
	TargetMismatch { reason: AttachmentFault },
	/// A NaN or an infinity where a finite number was required. The caller's DATA, which is why it is
	/// not `InvalidTransform`.
	NonFinite { what: &'static str },
	/// A request past what this implementation can do. PERMANENT.
	LimitExceeded { limit: &'static str, ceiling: u64, asked: u64 },
	/// A request the Domain's budget cannot pay for now. TEMPORARY.
	OutOfMemory { bytes: u64 },
	/// A texture whose description cannot be one: a zero extent, a level count past what the extent
	/// has, a layer count of zero, a cube map that is not square.
	InvalidTexture { reason: &'static str },
	/// A render state that contradicts itself or the pass: a depth test with no depth attachment,
	/// per-sample shading with one sample, a scissor outside the viewport.
	InvalidRenderState { reason: &'static str },
	/// A shader the IR rules refuse: an unbounded loop, recursion, an out-of-range access, a stage
	/// whose declared inputs do not match the vertex layout.
	InvalidShader { reason: &'static str },
	/// A pipeline that cannot be used with the pass it was given: an attachment count that
	/// disagrees, a sample count that disagrees, a blend state with the wrong number of entries.
	IncompatiblePipeline { reason: &'static str },
	/// A feature outside Profile 1. UNREACHABLE WITHIN THE PROFILE by construction - see the note at
	/// the top of this file - and present for the extensions that come after it.
	Unsupported { feature: &'static str },
}

/// What is wrong with a mesh, as its own closed set rather than a string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MeshFault {
	/// An index at or past the vertex count.
	IndexOutOfRange { index: u32, vertices: u32 },
	/// A vertex stream shorter than the attributes declared to read from it.
	StreamTooShort { stream: u32, needs: u64, has: u64 },
	/// Fewer vertices than the topology's first primitive needs - two for a line, three for a
	/// triangle - which is a draw that can produce nothing and is a mistake rather than a no-op.
	TooFewVertices { topology: &'static str, needs: u32, has: u32 },
	/// An attribute whose offset and size leave the stride.
	AttributeOutsideStride { attribute: u32, offset: u32, size: u32, stride: u32 },
	/// Two attributes at one location. A stage reads a location and gets ONE value, so a layout that
	/// offers two is a description with two answers to one question - and which one a backend picks
	/// is exactly the sort of thing two backends decide differently.
	LocationDeclaredTwice { location: u32, attribute: u32, first: u32 },
}

/// Why two attachments cannot share a pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttachmentFault {
	/// Two attachments of different sizes. A pass has one viewport and one set of fragments.
	ExtentMismatch { width: u32, height: u32, expected_width: u32, expected_height: u32 },
	/// Two attachments with different sample counts. Coverage is one decision per fragment.
	SampleCountMismatch { samples: u32, expected: u32 },
	/// A resolve target with more than one sample, or one whose format is not the source's. A
	/// resolve is an average and not a conversion; a pass that wants both states both.
	ResolveMismatch { reason: &'static str },
	/// More attachments than the profile's limit admits.
	TooMany { count: u32, ceiling: u32 },
	/// A depth attachment where a colour one belongs, or the reverse.
	WrongKind { expected: &'static str },
}

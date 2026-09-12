//! THE CONTRACTS A RECORDING API HAS THAT A DRAWING API DOES NOT.
//!
//! A PREPARED LIST IS A CACHE, and a cache whose validity conditions are not enumerated is a cache
//! that eventually replays a drawing that is not the one recorded. Every dependency is named here, so
//! `is_compatible` is a list to check rather than a judgement - and so a test can change each one
//! ALONE and see the refusal it should cause.
//!
//! AND CONTENT IS NOT STRUCTURE. A new video frame in an image a list references changes what the
//! drawing looks like and nothing about what the drawing IS; a list that re-flattened every path for
//! it would be re-preparing sixty times a second for no reason. The two are tracked apart.

/// One thing a prepared list is bound to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Dependency {
	pub name: &'static str,
	/// Why a change to it invalidates the prepared list.
	pub why: &'static str,
}

/// EVERY DEPENDENCY A `PreparedDrawList` IS BOUND TO. `is_compatible` checks these and nothing else.
pub const PREPARED_DEPENDENCIES: &[Dependency] = &[
	Dependency { name: "profile version and hash", why: "a list prepared against a different profile may contain a command this one does not have, or the same command with a different meaning" },
	Dependency { name: "backend identity and version", why: "the scratch layout and the prepared form are the backend's own; another backend's bytes are not a prepared list, they are bytes" },
	Dependency { name: "target format", why: "quantisation, dithering and the final conversion are all chosen for the target's own format" },
	Dependency { name: "target colour space", why: "every paint was converted into the target's space during preparation" },
	Dependency { name: "physical extent and scale", why: "curves were flattened in DEVICE space, so a different scale is a different flattening" },
	Dependency { name: "immutable resource identity and layout generation", why: "a replaced path, gradient or immutable image is a different drawing under the same index" },
	Dependency { name: "glyph-cache generation", why: "a rasterised glyph the list refers to may have been evicted or re-rasterised at another size" },
	Dependency { name: "filter parameters", why: "a filter's radius decides the scratch reserved for it, which was reserved during preparation" },
];

/// A MISMATCH IS A TYPED RE-PREPARE REQUIREMENT and never a silent re-prepare.
///
/// SILENTLY RE-PREPARING HIDES THE COST. A caller replaying a list sixty times a second and getting a
/// full preparation each time has a performance bug it cannot see; being told which dependency
/// changed is what makes it findable.
pub const ON_MISMATCH: &str = "a typed re-prepare requirement naming the dependency that changed";

/// What a CONTENT change does, as opposed to a structural one.
pub const CONTENT_REFRESH: &str = "a new frame in a mutable image refreshes that image's upload and sampling cache alone; paths are not re-flattened, scratch is not re-reserved, and the prepared list stays valid";

/// The reusable builder's contract.
pub mod builder {
	/// WHAT RE-RECORDING WITHIN THE RESERVATION COSTS: nothing.
	///
	/// A TRANSFORM, AN OPACITY, A COLOUR, A SCROLL OFFSET OR AN IMAGE FRAME are what an animation
	/// changes between frames, and a builder that allocated for any of them would allocate sixty
	/// times a second forever.
	pub const WITHIN_RESERVATION: &str = "no allocation";
	/// Exceeding it is a typed outcome BEFORE replay rather than a failure during it: a list that
	/// half-drew and then refused has already put pixels on the screen.
	pub const BEYOND_RESERVATION: &str = "a typed limit or reservation outcome, returned before any replay begins";
	/// ONE IMMUTABLE SNAPSHOT IS MUTATED OR REPLACED AT A TIME, and a live prepared snapshot retains
	/// its own resources - so a frame being replayed cannot have the ground moved under it.
	pub const SNAPSHOTS: &str = "one mutated or replaced at a time; a live prepared snapshot retains its own resources";
	/// Re-preparation reuses reserved scratch when it is enough and REPORTS a larger requirement when
	/// the changed geometry needs one, rather than quietly growing.
	pub const SCRATCH_REUSE: &str = "reuse the reservation when it suffices; report the larger requirement explicitly when it does not";
}

/// THE PER-NODE FILTER CONTRACT, which is what makes a filter graph boundable.
pub mod filter {
	/// A graph and not a chain - and a DAG, because a cycle is a filter that never finishes.
	pub const SHAPE: &str = "a directed acyclic graph; a cycle is refused during preparation";
	/// EACH NODE DECLARES THE INPUT RECTANGLE IT NEEDS FOR A GIVEN OUTPUT RECTANGLE. Without that map
	/// the whole graph has to be computed over the whole surface, because nothing can say which part
	/// of the input a part of the output depends on - which is why a blur over a small dirty region
	/// costs a full-screen blur in implementations that skipped it.
	pub const BOUNDS_MAP: &str = "each node maps an output rectangle to the input rectangle it needs, and produces no pixel outside its declared output";
	/// Every node computes in the canonical intermediate, so a chain does not quantise between steps.
	pub const WORKING_FORMAT: &str = "the canonical premultiplied linear intermediate, for every node";
	/// What a node reads outside its input's own bounds.
	pub const EDGE_MODE: &str = "transparent black, unless the node states another and the graph records which";
	/// THE WHOLE GRAPH'S SCRATCH IS COMPUTED DURING PREPARATION and refused up front. A frame that
	/// cannot fit says so before it starts drawing rather than failing halfway through a filter chain,
	/// which leaves a half-drawn frame on the screen.
	pub const SCRATCH: &str = "computed for the whole graph during preparation, and refused up front";
}

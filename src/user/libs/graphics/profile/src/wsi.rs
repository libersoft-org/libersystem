//! THE WINDOW-SYSTEM PROFILE: what a surface is, how a frame gets to a screen, and what comes back.
//!
//! WHY IT IS FROZEN BEFORE THE IMPLEMENTATION. Every number here is one two sides round differently
//! if it is not stated: a fractional scale, the edge rule between two neighbouring surfaces, how many
//! images a queue negotiates, what "displayed" means on a backend that cannot see a scanout. Each of
//! those produces a defect nobody can reproduce - a one-pixel seam, a frame that never arrives, a
//! timestamp that is a command acknowledgement wearing a clock's name.
//!
//! AND THE HARD PART IS NOT THE LIST OF CALLS. It is the STATE MACHINE and the OUTCOMES: an image
//! that survives a resize, a present that is accepted and then discarded because the surface went
//! behind another one, an acquire that must answer rather than block because the service has ONE
//! dispatch loop. Those are frozen here so an implementation is checked against them rather than
//! against itself.

/// The scale factor's representation on the wire.
///
/// A RATIO AND NOT A BARE FLOAT. A fractional scale that two sides round differently produces an
/// off-by-one extent, and that is the bug that shows as a one-pixel seam nobody can reproduce.
pub const SCALE_REPRESENTATION: &str = "{ numerator: u32, denominator: u32 }, exact, never a float on the wire";

/// WHICH OF LOGICAL AND PHYSICAL IS AUTHORITATIVE.
///
/// THE PHYSICAL EXTENT IS, because it is the one that exists: a physical pixel is a thing on a
/// display and a logical pixel is a convention. The logical extent is DERIVED, and stating the
/// direction is what stops two implementations deriving it in opposite directions and disagreeing by
/// a pixel at every non-integer scale.
pub const AUTHORITATIVE_EXTENT: &str = "physical; the logical extent is derived from it and the scale, and never the other way round";

/// THE ONE LOGICAL-TO-PHYSICAL EDGE RULE, used by both neighbours of a shared edge.
pub const EDGE_RULE: &str = "an edge at logical coordinate `l` is the physical coordinate `round_half_away_from_zero(l * numerator / denominator)`, computed in integers from the ratio and never from a rounded float, so both neighbours of a shared edge compute the same number";

/// HOW DAMAGE ROUNDS when it crosses from logical to physical.
///
/// OUTWARD ON EVERY SIDE, and that is a CONSERVATIVE bound rather than a promise: two independently
/// rounded rectangles that were adjacent in logical space may overlap in physical space, and code
/// that assumed non-overlap from this rule would be assuming something it does not say.
pub const DAMAGE_ROUNDING: &str = "left and top round down, right and bottom round up; conservative, and not a promise of non-overlap between independently rounded geometry";

/// HOW INPUT MAPS BACK, frozen with the rule above because they are one decision.
pub const INPUT_MAPPING: &str = "a physical input coordinate maps to logical by the exact inverse ratio, rounding half away from zero, after the output transform is undone";

/// The output transforms a surface can be presented under.
pub const TRANSFORMS: &[&str] = &["Normal", "Rotate90", "Rotate180", "Rotate270", "FlippedNormal", "FlippedRotate90", "FlippedRotate180", "FlippedRotate270"];

/// One field of the atomic configuration snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConfigurationField {
	pub name: &'static str,
	pub why: &'static str,
	/// Whether a change to this field invalidates the surface's image generation.
	pub invalidates_images: bool,
}

/// THE SNAPSHOT. A client never applies a new scale with an old extent, which is what "atomic" is
/// for: the fields travel together or the client renders at a size nothing asked for.
pub const CONFIGURATION: &[ConfigurationField] = &[
	ConfigurationField { name: "serial", why: "what an acknowledgement names; a stale or unknown serial is refused rather than applied", invalidates_images: false },
	ConfigurationField { name: "generation", why: "which image generation this configuration belongs to; every present carries one and it must match", invalidates_images: false },
	ConfigurationField { name: "logical extent", why: "what an application lays out in", invalidates_images: true },
	ConfigurationField { name: "physical extent", why: "what the backend rasterises; the authoritative one", invalidates_images: true },
	ConfigurationField { name: "scale", why: "the ratio between them, exact", invalidates_images: true },
	ConfigurationField { name: "transform", why: "the output's orientation, which input mapping is the inverse of", invalidates_images: true },
	ConfigurationField { name: "output", why: "which output the surface is on, so a client can follow its refresh interval", invalidates_images: false },
	ConfigurationField { name: "presentable pixel format", why: "immutable within a generation; changing it is a new generation", invalidates_images: true },
	ConfigurationField { name: "colour information", why: "the space AND the luminances, because a colour-space name alone cannot tone map", invalidates_images: false },
	ConfigurationField { name: "subpixel layout", why: "which geometry an LCD mask may be rasterised for, and `None` when it is unknown", invalidates_images: false },
	ConfigurationField { name: "visibility", why: "whether this surface is the visible one; acquiring from a hidden surface answers NotVisible", invalidates_images: false },
	ConfigurationField { name: "focus", why: "focus is a property of a SURFACE and not of a connection", invalidates_images: false },
];

/// THE CONFIGURATION LIFECYCLE, in the order it happens.
pub const CONFIGURATION_LIFECYCLE: &str = "configure(snapshot) -> rebuild -> ack_configure(serial) -> first present Full; a present names its configuration serial and its image generation, and both must match the acknowledged current configuration at acceptance";

/// WHAT A SURFACE REPORTS ABOUT ITS OUTPUT COLOUR, beyond the space's name.
///
/// A CONVERSION THAT DOES NOT KNOW THE DESTINATION'S LUMINANCE IS GUESSING at the one number that
/// decides how the image looks: tone mapping an HDR frame for a four-hundred-nit panel and for a
/// thousand-nit panel are different operations, and the space's name is the same for both.
pub const COLOUR_METADATA: &[&str] = &["colour space", "SDR white luminance in cd/m²", "minimum luminance in cd/m²", "maximum luminance in cd/m²", "maximum frame-average luminance in cd/m²"];

/// One event a surface delivers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SurfaceEvent {
	pub name: &'static str,
	pub carries: &'static str,
	pub why: &'static str,
}

/// THE SURFACE EVENT SET. Separate events and not one "something changed", because a client that has
/// to re-read everything to find out what happened re-reads everything sixty times a second.
pub const EVENTS: &[SurfaceEvent] = &[
	SurfaceEvent { name: "Configure", carries: "one SurfaceConfiguration snapshot", why: "the atomic set above; the client rebuilds and acknowledges by serial" },
	SurfaceEvent { name: "ImageAvailable", carries: "nothing", why: "what a client waits on INSTEAD of blocking inside acquire, which is what keeps one dispatch loop alive" },
	SurfaceEvent { name: "PresentComplete", carries: "the present's serial, its outcome and its timestamp evidence", why: "a present's fate is reported per present rather than inferred from the next acquire" },
	SurfaceEvent { name: "CloseRequested", carries: "nothing", why: "a REQUEST and not a teardown: the application decides, which is what makes an unsaved-changes prompt possible" },
	SurfaceEvent { name: "VisibilityChanged", carries: "the new visibility", why: "a background client stops drawing rather than discovering it by a refusal" },
	SurfaceEvent { name: "FocusChanged", carries: "whether this surface has focus", why: "focus belongs to the surface, so the event does too" },
];

/// A state an image of the present queue can be in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImageState {
	pub name: &'static str,
	pub meaning: &'static str,
}

/// THE IMAGE STATES.
pub const IMAGE_STATES: &[ImageState] = &[
	ImageState { name: "Available", meaning: "the queue owns it and it can be acquired" },
	ImageState { name: "Acquired", meaning: "the client owns it and may draw into it" },
	ImageState { name: "PendingPresent", meaning: "the service owns it; it has been accepted and not yet completed" },
	ImageState { name: "Stale", meaning: "it belongs to a generation that no longer exists and is never presented into a new one" },
];

/// One transition of the image state machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transition {
	pub from: &'static str,
	pub event: &'static str,
	pub to: &'static str,
	pub why: &'static str,
}

/// THE WHOLE MACHINE, including the two edges an implementation writes and then discovers it needs.
pub const TRANSITIONS: &[Transition] = &[
	Transition { from: "Available", event: "acquire", to: "Acquired", why: "the ordinary path" },
	Transition { from: "Acquired", event: "present", to: "PendingPresent", why: "the ordinary path; `present` CONSUMES the acquired image, so there is no safe path back to the pixels" },
	Transition { from: "PendingPresent", event: "released", to: "Available", why: "the service is done with it, whatever the outcome was" },
	// THE EDGE AN IMPLEMENTATION FORGETS: a client that acquires and then decides not to draw must
	// have a way back that is not a present, or a resized window leaks an image per resize.
	Transition { from: "Acquired", event: "abandon", to: "Available", why: "a client that acquired and then decided not to draw returns it without presenting a frame nobody wanted" },
	Transition { from: "Available", event: "generation changed", to: "Stale", why: "its extent, scale, orientation or format is not the current one" },
	Transition { from: "Acquired", event: "generation changed", to: "Stale", why: "the client discards it rather than presenting it into a generation it was not drawn for" },
	Transition { from: "PendingPresent", event: "generation changed", to: "Stale", why: "it completes or is discarded FIRST - a frame in flight is not un-submitted - and is then stale" },
	Transition { from: "Stale", event: "reclaimed", to: "Available", why: "the queue rebuilds its images for the new generation" },
];

/// `acquire_next`'s answers.
///
/// IT DOES NOT BLOCK, and that is a property of the service rather than a preference: LSIDL handlers
/// are synchronous and one dispatch loop serves the GPU, the admin channel, the kill control and
/// every client. Blocking inside an acquire handler stops the only loop that could deliver the
/// release that would unblock it, so a conforming implementation would deadlock the whole service
/// under ordinary queue pressure.
pub const ACQUIRE_ANSWERS: &[&str] = &[
	"an image",
	"Again, when none is available - the client waits on ImageAvailable rather than inside the call",
	"NotVisible, when the surface is occluded or in the background",
	"OutOfDate, when the configuration has moved on",
];

/// One outcome of a present.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PresentOutcome {
	pub name: &'static str,
	pub meaning: &'static str,
}

/// THE PER-PRESENT OUTCOMES. A bare completion cannot express them, and "every present is displayed"
/// cannot hold for a background client that must also not be blocked forever.
pub const PRESENT_OUTCOMES: &[PresentOutcome] = &[
	PresentOutcome { name: "Displayed", meaning: "it reached logical presentation; on a backend that cannot observe scanout this means DRIVER-COMPLETED" },
	PresentOutcome { name: "DiscardedOccluded", meaning: "it was accepted while visible and completed while not; it kept its place in the order and never reached a screen" },
	PresentOutcome { name: "ReplacedByResize", meaning: "a generation change overtook it" },
	PresentOutcome { name: "DriverLost", meaning: "the backend went away under it" },
];

/// WHAT FIFO PROMISES, split so the promise is one that can be kept.
pub const FIFO_RULE: &str = "FIFO is the order of ACCEPTED presents, by present-call order and not by acquire order; while visible every accepted frame reaches logical presentation, and while occluded or backgrounded new acquires answer NotVisible and already-accepted frames still complete IN ORDER, possibly as discarded";

/// The present modes, with the one that is defined now.
pub const PRESENT_MODES: &[&str] = &["Fifo"];

/// The modes that have a PLACE rather than a meaning, so adding one later is not an argument about
/// what the enumeration was for.
pub const RESERVED_PRESENT_MODES: &[&str] = &["Mailbox", "Immediate"];

/// The negotiated image count. ADVERTISED AND NOT WRITTEN INTO THE INTERFACE - but a service free to
/// answer "one" turns the double-or-triple-buffering gate into a test of nothing.
pub const MIN_IMAGES: u32 = 2;
pub const MAX_IMAGES: u32 = 3;

/// One of the three facts the word "displayed" runs together.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CompletionFact {
	pub name: &'static str,
	pub meaning: &'static str,
	pub observable_today: bool,
}

/// THREE DIFFERENT FACTS. The current virtio-gpu path acknowledges `TRANSFER_TO_HOST_2D` and
/// `RESOURCE_FLUSH` and nothing else: there is no vblank, no page-flip timing and no evidence that a
/// scanout happened, so reporting a flush reply as a presentation timestamp would be reporting a
/// command acknowledgement as physical presentation.
pub const COMPLETION_FACTS: &[CompletionFact] = &[
	CompletionFact { name: "Accepted", meaning: "the service took the frame and it has a place in the order", observable_today: true },
	CompletionFact { name: "DriverCompleted", meaning: "the backend acknowledged the commands that carried it", observable_today: true },
	CompletionFact { name: "PhysicallyDisplayed", meaning: "a scanout of this frame was observed", observable_today: false },
];

/// How a timestamp is reported when it is not known.
///
/// TYPED AND NOT A BARE NUMBER, so a client that requires a real measurement refuses on a backend
/// that cannot produce one instead of pacing against a fabricated number.
pub const TIMESTAMP_EVIDENCE: &[&str] = &["Unavailable", "Estimated(t)", "Measured(t)"];

/// What the frame loop is given instead of a clock to sleep on.
pub const TIMING_CONTRACT: &[&str] = &["preferred frame deadline", "the output's refresh interval", "the completed present's timestamp evidence", "the present outcome"];

/// EVERY ONE OF THOSE IS OPTIONAL, because the current backend can supply none of them.
pub const TIMING_OPTIONALITY: &str = "every timing field is optional and typed as such; a backend that cannot supply one says so rather than inventing it";

/// The damage rules, which are an ABI rather than "a small cap".
pub const MAX_DAMAGE_RECTS: usize = 16;

/// One damage rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DamageRule {
	pub question: &'static str,
	pub answer: &'static str,
}

pub const DAMAGE_RULES: &[DamageRule] = &[
	DamageRule { question: "what an EMPTY list means", answer: "nothing changed: the frame is pixel-identical to the previously presented one, the present is still ordered and still completes, and the service may release the image without copying" },
	DamageRule { question: "how the whole surface is spelled", answer: "an explicit Full variant, never one rectangle covering the extent - a rectangle can be wrong by a pixel and a variant cannot" },
	DamageRule { question: "a rectangle outside the extent", answer: "a TYPED invalid, never a silent clamp" },
	DamageRule { question: "exceeding the cap", answer: "the caller's problem, solved by merging or by sending Full - never the service's, solved by dropping rectangles" },
	DamageRule { question: "overlapping rectangles", answer: "legal, and may be normalised by the backend" },
	DamageRule { question: "what the coordinates are in", answer: "the image space of the generation the present belongs to" },
	DamageRule { question: "damage relative to WHAT, when consecutive frames are drawn into different images", answer: "EVERY PRESENTED IMAGE CONTAINS A COMPLETE VALID FRAME; damage is a HINT about which pixels changed against the previously presented frame and is never permission to leave the rest undefined" },
	DamageRule { question: "the first present of a generation", answer: "Full" },
	DamageRule { question: "whether a backend may merge", answer: "it may, when merging is cheaper than transferring separately - what is forbidden is the unconditional union of everything, which turns two corners into most of the screen" },
];

/// THE COMPLETION MECHANISM: two ordinary channel pairs per surface, created when it is admitted.
///
/// NO NEW KERNEL OBJECT, no generic timeline, no ABI number and no signal right. `Event` is not it:
/// it lacks the authority split and the peer-close lifecycle this needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CompletionPair {
	pub name: &'static str,
	pub client_rights: &'static str,
	pub service_rights: &'static str,
}

pub const COMPLETION_PAIRS: &[CompletionPair] = &[
	CompletionPair { name: "PRODUCER_READY", client_rights: "SEND", service_rights: "RECEIVE | WAIT" },
	CompletionPair { name: "PRESENT_DONE", client_rights: "RECEIVE | WAIT", service_rights: "SEND" },
];

/// The depth each endpoint is created with, and what that bounds.
pub const COMPLETION_DEPTH: &str = "each endpoint is created with depth max_images, so at three images at most six messages are queued across both directions";

/// What a client endpoint may NOT do.
pub const COMPLETION_ATTENUATION: &str = "client endpoints carry neither DUPLICATE nor TRANSFER; a safe completion token may move inside its own process, and the raw endpoint is never exposed beside it";

/// THE MEMORY RULE for a v1 presentable image.
pub const MEMORY_VISIBILITY: &str = "a v1 presentable image is COHERENT CPU-VISIBLE memory: a send is a release and a receive is an acquire, in both directions, and there is no explicit flush or invalidate in the contract because there is no non-coherent import in v1";

/// THE PRE-COMPOSITOR VISIBILITY RULE, which is what makes "before a compositor exists" a contract
/// rather than an absence.
pub const VISIBILITY_RULE: &str = "at most ONE surface is visible and scanout-bound; other surfaces keep independent resources, generations and queues while hidden, acquiring from them answers NotVisible, frames accepted before a visibility change still settle in FIFO order with the appropriate discarded outcome, and focus belongs to the visible surface";

/// What v1 deliberately does NOT contract, so an implementation does not invent it.
pub const NOT_IN_V1: &[&str] = &["popup position", "z-order", "simultaneous composition", "a measured presentation timestamp", "buffer age", "deferred replies with a pending-call table"];

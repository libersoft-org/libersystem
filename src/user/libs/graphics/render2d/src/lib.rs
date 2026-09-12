//! `render2d`: the 2D drawing API, SEPARATE from any rasteriser.
//!
//! THE SEPARATION IS THE BOUNDARY A GPU BACKEND LATER ARRIVES AT. An API that writes into a slice is
//! a CPU API however carefully it is written, and the version of this file that had one would have
//! had to be replaced rather than extended. What a `Canvas` produces is a bounded, validated,
//! IMMUTABLE display list; what draws it is a `Backend`, of which `soft2d` is the first.
//!
//! AND THE INDIRECTION PAYS FOR ITSELF BEFORE THE GPU ARRIVES. The same list is cacheable per
//! component, analysable for damage, replayable, and testable WITHOUT ANY BACKEND AT ALL - which is
//! what lets recording, validation, transform algebra, limits and the encoding round trip all be host
//! tests rather than boot tests.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod backend;
pub mod blend;
pub mod boolean;
pub mod canvas;
pub mod damage;
pub mod encode;
pub mod filter;
pub mod flatten;
pub mod list;
pub mod paint;
pub mod path;
pub mod prepared;
pub mod query;
pub mod resource;
pub mod transform;

pub use backend::{Backend, Prepared, TargetDescription};
pub use blend::{Antialias, BlendMode, Operator};
pub use canvas::Canvas;
pub use filter::{FilterGraph, FilterNode};
pub use list::{Command, DRAW_LIST_VERSION, DrawList, DrawListBuilder, ImageRecord, RecordedGlyphRun, ResourceTable};
pub use paint::{Color, GradientStop, ImageQuality, Paint, SpreadMode};
pub use path::{Cap, FillRule, Join, Path, PathBuilder, StrokeStyle, Verb};
pub use prepared::{PreparedKey, RePrepare};
pub use resource::{FilterHandle, GlyphRunHandle, ImageHandle, PaintHandle, PathHandle, ResourceKind};
pub use transform::{StrokeScaling, Transform};

/// What went wrong, in terms the CALLER can act on.
///
/// THE DISTINCTION THAT MATTERS IS WHOSE FAULT IT IS. An out-of-range handle is a defect in the
/// recorder; a limit exceeded is a drawing that has to be split or simplified; an unbalanced save is
/// a control-flow bug three functions away. A single "invalid" would leave every one of them to be
/// found by reading the drawing code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	/// A handle naming an entry the list's table does not have. A defect in whatever built the list.
	UnknownResource { kind: ResourceKind, index: u32 },
	/// A profile ceiling met. The drawing is legal and too large: it has to be split or simplified,
	/// and the ceiling is named so the caller knows which way.
	LimitExceeded { limit: &'static str, ceiling: u64 },
	/// A `restore` without a `save`.
	UnbalancedSave,
	/// A `PopClip` without a `PushClip`, or a clip left open at the end.
	UnbalancedClip,
	/// An `end_layer` without a `begin_layer`, or a layer left open - which would silently lose
	/// everything recorded inside it.
	UnbalancedLayer,
	/// A drawing call before the path had a starting point.
	PathNotStarted,
	/// A filter node reading a node at or after itself, which is a graph that never finishes.
	FilterCycle,
	/// A geometry that has no image: a control point at or beyond the projective horizon.
	BeyondHorizon,
	/// A dash pattern with no positive length. A dasher walking it never advances, so it is refused
	/// where it is recorded rather than detected in the loop that draws.
	DegenerateDash,
	/// Storage this recording needed and could not have.
	Allocation,
	/// The frame was abandoned: its target was closed or resized while it was being drawn.
	///
	/// A FRAME NOBODY WILL SEE IS NOT WORTH FINISHING, and on a resize it would be finished at the
	/// wrong size. What was already written stays written - a backend stops at a boundary of its own
	/// choosing rather than leaving a shape cut in half.
	Cancelled,
}

#[cfg(test)]
mod tests;

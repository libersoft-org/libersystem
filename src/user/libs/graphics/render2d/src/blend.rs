//! THE COMPOSITING OPERATORS AND BLEND MODES - THE TYPES LIVE IN `graphics-core`.
//!
//! THEY ARE RE-EXPORTED HERE AND NOT REDECLARED. The arithmetic that uses them is `graphics-core`'s
//! one implementation of compositing, shared by this API's backends, by `pix` and by the display
//! path; an enumeration declared a second time here would be a second list to keep in step with the
//! frozen registry, and the first one somebody adds a mode to without the other.
//!
//! WHAT IS THIS LAYER'S OWN is the antialiasing choice below, which is a DRAWING decision rather than
//! a compositing one.

pub use graphics_core::composite::{ALL_BLEND_MODES, ALL_OPERATORS, BlendMode, Operator};

/// Whether an edge gets a coverage value.
///
/// ANTIALIASING IS REQUIRED AND THE ALIASED PATH IS EXPLICIT. A one-pixel aliased line is a real
/// need: a diagram's grid, a pixel-exact rule, a screenshot comparison. Making it the only thing
/// there is, or making it unreachable, are both failures. The contract is coverage in `0..=255`,
/// composited premultiplied, deterministic; the ALGORITHM is the backend's.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Antialias {
	#[default]
	On,
	Off,
}

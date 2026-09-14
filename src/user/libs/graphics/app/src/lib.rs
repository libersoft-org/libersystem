//! THE FRAME LOOP AN APPLICATION HAS, AND NOT THE ONE A RENDERER HAS.
//!
//! FRAME SCHEDULING LIVES ABOVE THE RENDERER. `soft2d` and `soft3d` render into targets and return;
//! they know nothing of DisplayService. Acquire, present, pacing, completion, resize, `out-of-date`
//! and focus belong here, shared by both - putting the present queue inside a renderer is what would
//! make the next backend reimplement a window system to draw a triangle.
//!
//! THE DECIDING HALF NEEDS NO KERNEL. `Pacing` is the whole policy - how many frames may be with the
//! service, when the next one is due, what a configuration change means, what to do while hidden -
//! and it is a value with no syscalls in it, so every rule below is a host fixture rather than a
//! property somebody has to boot a machine to check. `FrameLoop` is the thin half that wires it to a
//! real surface.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod pacing;

pub use pacing::{Pacing, Step, Timing};

#[cfg(feature = "runtime")]
mod frame_loop;

#[cfg(feature = "runtime")]
pub use frame_loop::{Frame, FrameLoop, Rebuilt};

#[cfg(test)]
mod tests;

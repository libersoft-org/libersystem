//! THE COMPLETE CPU IMPLEMENTATION OF `Render3D Core Profile 1`.
//!
//! A BACKEND AND NOT A SECOND API. Everything an application says reaches this crate as a
//! `render3d` command list; nothing here is callable in a way a future GPU backend could not also
//! serve. A feature the profile mandates that this cannot execute is a DEFECT, not an
//! `Unsupported` - which is what a closed profile is for.
//!
//! THE SPLIT THIS WHOLE CRATE IS ORGANISED AROUND. Geometry is BIT-EXACT and shading is not.
//! Coverage, clipping topology, the depth value a fragment carries and the identity it writes are
//! integer or quantised arithmetic and must be identical on every architecture; the colour a
//! fragment computes goes through `f32` transcendentals and is compared within a tolerance. Mixing
//! the two is what makes a conformance comparison either impossible to satisfy or worthless.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod clip;
pub mod fixed;
pub mod frame;
pub mod geometry;
pub mod interp;
pub mod interpreter;
pub mod pass;
pub mod raster;
pub mod texture;
pub mod value;

pub use clip::{Clipped, MAX_CLIPPED_VERTICES, MAX_TRIANGLES_AFTER_CLIP, Vertex};
pub use fixed::{MAX_RASTER_EXTENT, SUBPIXEL_BITS, Subpixel};
pub use frame::{Attachments, Draw, Pipeline, Prepared, Scissor, Source, Stats, execute as run, prepare};
pub use geometry::{Indices, Primitive};
pub use interpreter::{Fault, Outputs, Resources, execute};
pub use pass::{Colour, DepthStencil, Fragment, Operations};
pub use raster::{Bins, Facing, PixelBox, Setup};
pub use texture::{Border, Cache, Filter, Kind, Level, Sampler, Texel, Texture, Wrap};
pub use value::Val;

#[cfg(test)]
mod tests;

/// THE FIXTURE THAT MAKES "ALLOCATES NOTHING" A MEASUREMENT.
///
/// A steady-state frame allocating nothing is a claim, and a claim about allocation can only be
/// tested by counting allocations. This wraps the host allocator in a counter for the test build
/// alone; the counter is THREAD-LOCAL, so the test harness's own parallelism does not make one
/// fixture's count another's.
#[cfg(test)]
mod counted {
	use std::alloc::{GlobalAlloc, Layout, System};
	use std::cell::Cell;

	std::thread_local! {
		static COUNT: Cell<usize> = const { Cell::new(0) };
	}

	pub struct Counting;

	// SAFETY: every method forwards to the system allocator unchanged; the counter is a
	// thread-local `Cell<usize>` with no destructor, so touching it cannot itself allocate.
	unsafe impl GlobalAlloc for Counting {
		unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
			COUNT.with(|count| count.set(count.get() + 1));
			unsafe { System.alloc(layout) }
		}

		unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
			unsafe { System.dealloc(pointer, layout) }
		}

		unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
			// A REALLOC IS AN ALLOCATION. A vector that grows is exactly what a steady-state frame
			// must not do, and counting only `alloc` would miss every one of them.
			COUNT.with(|count| count.set(count.get() + 1));
			unsafe { System.realloc(pointer, layout, new_size) }
		}
	}

	/// How many allocations this thread has made.
	pub fn count() -> usize {
		COUNT.with(|count| count.get())
	}
}

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: counted::Counting = counted::Counting;

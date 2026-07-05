//! Graphics-free terminal model (L2) for LiberSystem.
//!
//! `Screen` is the cell grid plus the ANSI/CSI/OSC output parser, the cursor and
//! scroll region, the logical colour model, and the scrollback ring. It holds no
//! pixels and no framebuffer address: a renderer reads its snapshot/diff interface
//! (`cell`, `view_cell`, `dirty_take`, `take_scrolls`) to draw it, a non-graphical
//! consumer like `TextSink` reads the same model to serialize it to logical text lines,
//! and `RawSink` taps the byte stream one layer below the grid (L1) to capture or forward
//! it verbatim. The crate is `no_std` for the kernel and userspace builds, and pulls in
//! `std` only under `cargo test` so the model can be exercised on the host.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod ld;
mod raw;
mod render;
mod screen;
mod text;

pub use ld::{Echo, EchoBuf, LD_HIST_MAX, Ld};
pub use raw::RawSink;
pub use render::{CELL_H, CELL_W, Geometry, Raster, Surface, Term};
pub use screen::{Cell, Color, CursorShape, SCROLLBACK_ROWS, Screen, ScrollOp};
pub use text::TextSink;

#[cfg(test)]
mod tests;

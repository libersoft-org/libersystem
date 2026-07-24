#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub use wire as codec;
pub mod clock;
pub mod generated;

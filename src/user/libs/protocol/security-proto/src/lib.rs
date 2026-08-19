#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub use wire as codec;
pub mod generated;

// What the generated encoders do with a capability, run rather than read. Host-only.
#[cfg(test)]
mod capabilities;

//! Generated LSIDL bindings for LiberSystem.
//!
//! Versioned package modules under `generated` come from `src/idl/*.lsidl` via
//! `just gen`. `system` is a hand-written compatibility facade preserving the
//! original public paths while declarations migrate into domain packages.
//!
//! The crate is `no_std` for the kernel and userspace builds, and pulls in `std`
//! only under `cargo test` so the codec can be exercised on the host.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub use wire as codec;
pub mod generated;
pub mod system;

pub use network_proto::addr;
pub use time_proto::clock;

pub use storage_proto::path;

#[cfg(test)]
mod tests;

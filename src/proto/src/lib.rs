//! Generated LSIDL bindings for LiberSystem.
//!
//! The per-package modules (e.g. `system`) are generated from `src/idl/*.lsidl`
//! by `lsidl-gen` (run `just gen`); do not edit them by hand. The hand-written
//! parts are this file, the shared `codec` primitives, and the tests.
//!
//! The crate is `no_std` for the kernel and userspace builds, and pulls in `std`
//! only under `cargo test` so the codec can be exercised on the host.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod codec;
pub mod system;

// Hand-written helpers on the generated wire types (e.g. `Ipv4Addr::parse`).
mod addr;

#[cfg(test)]
mod tests;

//! The decisions CoreServices makes that depend on nothing but their inputs.
//!
//! Parsing a shell line, deciding whether an artifact name is a legal executable, bounding a module
//! graph, ordering a service shutdown: all of them are functions of their arguments, and none of
//! them needs a running system to be judged. They lived inside the `services` crate, which builds
//! twenty-odd binaries against the freestanding runtime, and `cargo test` therefore could not build
//! them at all - so the only thing that ever exercised them was a QEMU boot.

#![no_std]

extern crate alloc;

pub mod executable;
pub mod graph_limits;
pub mod service_lifecycle;
pub mod shell_language;

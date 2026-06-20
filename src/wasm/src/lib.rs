//! A minimal WebAssembly runtime for LiberSystem.
//!
//! It parses the binary module format (the integer subset we need) into a
//! [`Module`], then runs an exported function with a small stack-machine
//! interpreter ([`Instance`]). Imported functions are dispatched to a [`Host`],
//! which is how a WASI-style component reaches native services - the host maps an
//! import (e.g. a file read) onto an IPC call, capability-gated by what the host
//! wires up. It is `no_std` for the kernel and userspace builds, and pulls in
//! `std` only under `cargo test` so the runtime can be exercised on the host.
//!
//! This is deliberately small: enough to run the first capability-gated component,
//! not a full engine. The Component Model, the full WASI preview-2 world, control
//! flow (blocks / loops / branches), and floating point are later phases.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod interp;
pub mod module;
pub mod parser;

pub use interp::{Host, Instance, Trap, Value};
pub use module::{Module, ValType};
pub use parser::{ParseError, parse};

#[cfg(test)]
mod tests;

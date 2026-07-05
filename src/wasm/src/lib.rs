//! A minimal WebAssembly runtime for LiberSystem.
//!
//! It parses the binary module format into a [`Module`], decodes each function body
//! into a validated instruction stream, then runs an exported function with a small
//! stack-machine interpreter ([`Instance`]). Imported functions are dispatched to a
//! [`Host`], which is how a WASI-style component reaches native services - the host
//! maps an import (e.g. a file read) onto an IPC call, capability-gated by what the
//! host wires up. It is `no_std` for the kernel and userspace builds, and pulls in
//! `std` only under `cargo test` so the runtime can be exercised on the host.
//!
//! It supports the integer and floating-point instruction sets, structured control
//! flow (block / loop / if / else / br / br_if / br_table / return), globals, data
//! segments, and a single linear memory. The full Component Model and the complete
//! WASI preview-2 world are later steps.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod decode;
pub mod interp;
pub mod module;
pub mod parser;

pub use interp::{Host, Instance, Trap, Value};
pub use module::{Module, ValType};
pub use parser::{ParseError, parse};

#[cfg(test)]
mod tests;

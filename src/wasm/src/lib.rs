//! A minimal WebAssembly runtime for LiberSystem.
//!
//! It parses the binary module format into a [`Module`], VALIDATES it into a
//! [`ValidatedModule`] - types, indices, control flow and the host's own resource ceilings - and
//! then runs an exported function from that with a small stack-machine interpreter ([`Instance`]),
//! which accepts nothing else. Imported functions are dispatched to a
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
pub mod validate;

// The host side of the `liber:vfs@1` / `liber:log@1` world: which imports a component may have, and
// how its `(ptr, len)` becomes a slice of its own memory.
pub mod world;

pub use interp::{Budget, Host, Instance, Trap, Value};
pub use module::{Module, ValType};
pub use parser::{ParseError, parse};
pub use validate::{ValidatedModule, ValidationError, validate};

#[cfg(test)]
mod tests;

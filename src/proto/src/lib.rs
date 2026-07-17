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

// Image-internal dynamic-link smoke symbol. Like the generated Rust ABI around it,
// this is rebuilt with the complete system image and carries no cross-image promise.
#[unsafe(no_mangle)]
pub extern "C" fn liber_proto_probe() -> u64 {
	0x5052_4f54_4f4f_4b21
}

pub use wire as codec;
pub mod generated;
pub mod system;

// Hand-written helpers on the generated wire types (e.g. `Ipv4Addr::parse` /
// `Ipv4Addr::render` and MAC rendering).
pub mod addr;

// Hand-written ISO-8601 rendering for the generated `Timestamp` type.
pub mod clock;

// Hand-written `vol://` path resolution shared by the shell and the sandboxed tools.
pub mod path;

// The shell's pure line language (trim, flag normalization, `$NAME` expansion, and
// `NAME=VALUE` detection) - host-tested so it is exercised without booting.
pub mod shell;

#[cfg(test)]
mod tests;

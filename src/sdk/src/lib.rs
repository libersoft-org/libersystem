// liber-sdk - the `liber:vfs@1` / `liber:log@1` world, for Rust guests.
//
// A LiberSystem component is a WebAssembly module that imports this small, capability-oriented
// world and exports an entry point. The host (src/user/services/core/src/component_host.rs)
// resolves each import by name AND signature and wires it to a typed system service - never to
// ambient authority:
//
//   liber:vfs@1.read(ptr: u32, max: u32) -> i32     a granted file, read through StorageService
//   liber:vfs@1.write(ptr: u32, len: u32) -> i32    a granted file, written through StorageService
//   liber:log@1.log(ptr: u32, len: u32) -> i32      one structured entry, emitted to LogService
//
// The component never names a path, a channel, or a service: it only sees these three functions,
// and reaches exactly the capabilities the host was granted.
//
// NAMED AND VERSIONED, because this is a compatibility boundary. The world's whole identity used to
// be the strings `liber.read`, `liber.write` and `liber.log` - so the day a signature changed, an
// old module and a new one would both import `liber.read` and nothing could tell them apart. An
// interface gets its identity from its first external user, not from the first release.
//
// This crate is a LIBRARY. It was one `cdylib` holding the bindings, the demo's buffer, the
// transform and both exports; `examples/liber_component` is that example now, and everything here
// is what somebody else's component depends on.

#![no_std]

#[cfg(test)]
extern crate std;

mod status;
pub use status::{Error, STATUS_DENIED, STATUS_FAULT, STATUS_IO, STATUS_UNSUPPORTED, clamp_count};

// The world itself only exists for a wasm guest: the imports are resolved by the host at
// instantiation, and there is nothing on the other side of them anywhere else. Everything the SDK
// can decide WITHOUT the host - the status model, the clamping - is in `status`, which is why that
// module is not gated and has tests.
#[cfg(target_arch = "wasm32")]
mod world;
#[cfg(target_arch = "wasm32")]
pub use world::{log_message, read_input, write_output};

#[cfg(target_arch = "wasm32")]
mod panic;

#[cfg(test)]
mod tests;

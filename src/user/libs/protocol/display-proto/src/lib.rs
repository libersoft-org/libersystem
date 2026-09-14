#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub use wire as codec;
pub mod generated;

// The generated dispatch is the only place a `@rights` declaration turns into a refusal, so it is
// the only place worth testing it. Host-only: the guard calls a runtime symbol this module stubs.
#[cfg(test)]
mod authority;

// The other half of what a hostile message meets: the generated DECODERS and the generated
// DISPATCH, swept over the byte strings a client can send that are not requests.
#[cfg(test)]
mod hostile;

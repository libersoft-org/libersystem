//! THE ACPI SERVICE'S DECISIONS, pure and host-tested: what each namespace node the walk found is, and the row it is
//! reported to the kernel as (`node`); the handshakes that decide what the firmware lets this system do (`handshake`);
//! what a driver may evaluate on the node channel it was handed (`admission`); which control method answers a
//! general-purpose event or a GPIO-signalled one (`events`); and the device power states and the power resources
//! they share, counted across every holder (`power`); the processors' power objects, read (`processor`); and the
//! platform half of a sleep - `platform-sleep`'s `prepare` and `wake` in ACPI's order, and the sleep types (`sleep`).
//!
//! The service DESCRIBES and the kernel decides what is minted; the interpreter is `aml`, and the report encoding the
//! kernel reads is `platform::report`.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod admission;
pub mod events;
pub mod handshake;
pub mod node;
pub mod power;
pub mod processor;
pub mod properties;
pub mod sleep;

#[cfg(test)]
mod tests;

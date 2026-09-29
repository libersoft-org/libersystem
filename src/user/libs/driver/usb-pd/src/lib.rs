//! USB POWER DELIVERY FOR A SINK THE SYSTEM RUNS ITSELF, over a Type-C port controller (TCPCI): the message codec, the
//! power and request data objects with the selection rule, the timer table, and the Type-C sink and sink policy engines
//! as one transition function of events and time - pure leaves a driver drives with its clock and its bus.
//!
//! A SINK IN THE STANDARD POWER RANGE AND NOTHING MORE: no source role, no swap, no structured VDM as initiator, no
//! augmented offer, no Extended Power Range - each answered as the specification says a port without it answers.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod engine;
pub mod message;
pub mod pdo;
pub mod tcpci;
pub mod timer;

#[cfg(test)]
mod tests;

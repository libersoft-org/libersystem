//! POWER STATE, NORMALISED: the one place a battery's, a UPS's or a thermal zone's numbers become the
//! canonical values PowerService publishes.
//!
//! EVERY MEASUREMENT IS TAGGED - known, unknown, unsupported or invalid - and none of the last three
//! is ever encoded as zero or as the largest integer. An unknown charge is not an empty battery, a
//! rate without a direction is not a rate of zero, and a sentinel is not four billion milliwatts.
//!
//! THE ARITHMETIC IS CHECKED AND RATIONAL. A value arrives in its source's own unit - ACPI's milli-units
//! and tenths of a kelvin, HID's SI-linear units scaled by a power of ten - and is converted with
//! integer arithmetic wide enough to hold it exactly, truncating toward zero once, at the canonical
//! unit. An overflow is an INVALID value with a reason, never a clamped healthy one.
//!
//! WHO CALLS WHAT. A producer decodes its own format - a HID report, an AML method's package - and
//! hands `acpi` or `hid` the decoded numbers; those adapters are the only route from a source's units
//! to `schema::SourceState`. PowerService calls `canon::validate` on what a provider publishes and
//! never converts anything itself, so a producer that skipped the adapters is refused rather than
//! translated twice.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod acpi;
pub mod canon;
pub mod convert;
pub mod hid;

/// The canonical vocabulary, as `liber:power@1` defines it.
pub use power_proto::generated::liber::power::v1 as schema;

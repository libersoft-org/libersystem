//! A USB TYPE-C CONNECTOR'S PARTNER AS A SUPPLY: what a `ucsi-acpi` or `tcpci` binding measured of it, normalised
//! into a `usb-c` source.
//!
//! ONLY WHAT IS MEASURED. A UCSI 2.1 policy manager's VBUS voltage and average current readings, or a port
//! controller's VBUS voltage register, are voltage and current here; the negotiated contract is a LIMIT, not a
//! measurement, and never becomes one - it is `liber:typec@1`'s. A transport that measures nothing publishes
//! presence and the online state alone, with every measurement unsupported.
//!
//! WHILE NO PARTNER IS ATTACHED, NOTHING IS KNOWN. VBUS on an empty connector is not a partner's, and a source
//! reported absent may carry no known value - so a measurement the transport has is UNKNOWN in an unplugged
//! connector's first snapshot and after every detach, and one it lacks stays unsupported.
//!
//! A READING PAST THE BOUND IS INVALID, NEVER KNOWN. `canon::validate` refuses a `usb-c` record whose known voltage
//! is above 60 V, and a refused record ends the provider and every connector's source with it; the adapter
//! publishes such a reading as invalid (range) instead.

use crate::canon::{USB_C_MAX_MICROVOLTS, measure_i64, measure_u64, reported, unmeasured};
use crate::convert::Tagged;
use crate::schema::{AlarmKind, InvalidReason, SourceKind, SourceState, Tristate};

/// One measurement as the transport has it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reading<T> {
	/// The transport does not measure it.
	Unsupported,
	/// It does, and has no reading now - not ready, or not answering.
	Unknown,
	Value(T),
}

/// What a connector's transport knows of its partner as a supply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UsbC {
	/// Whether a partner is attached; `None` while the transport is not answering.
	pub attached: Option<bool>,
	/// Whether this machine sinks from it; `None` when not known.
	pub sinking: Option<bool>,
	/// VBUS, in microvolts.
	pub voltage: Reading<u64>,
	/// Microamps: negative delivered out of the partner into this machine, positive into the partner while this
	/// machine supplies it.
	pub current: Reading<i64>,
	/// A VBUS fault the transport reported - a port controller's alarm; `None` from a transport that reports none.
	pub fault: Option<bool>,
}

fn tagged<T>(reading: Reading<T>, attached: bool) -> Tagged<T> {
	match reading {
		Reading::Unsupported => Tagged::Unsupported,
		Reading::Value(value) if attached => Tagged::Known(value),
		Reading::Value(_) | Reading::Unknown => Tagged::Unknown,
	}
}

/// The partner, normalised as a `usb-c` source.
pub fn usb_c(u: &UsbC) -> SourceState {
	let mut state = unmeasured(SourceKind::UsbC);
	let attached = u.attached == Some(true);
	state.present = match u.attached {
		Some(true) => Tristate::Yes,
		Some(false) => Tristate::No,
		None => Tristate::Unknown,
	};
	state.online = match (u.attached, u.sinking) {
		(Some(false), _) => Tristate::No,
		(Some(true), Some(true)) => Tristate::Yes,
		(Some(true), Some(false)) => Tristate::No,
		_ => Tristate::Unknown,
	};
	let voltage = match tagged(u.voltage, attached) {
		Tagged::Known(microvolts) if microvolts > USB_C_MAX_MICROVOLTS => Tagged::Invalid(InvalidReason::Range),
		other => other,
	};
	state.voltage = measure_u64(voltage);
	state.current = measure_i64(tagged(u.current, attached));
	if let Some(fault) = u.fault {
		state.alarms.push(reported(AlarmKind::SourceFault, fault));
	}
	state
}

#[cfg(test)]
mod tests;

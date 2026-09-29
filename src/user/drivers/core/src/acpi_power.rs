// ACPI BATTERY, AC AND THERMAL ZONE - the parts of the `acpi_power` driver a host can test: which class a node is, what
// its methods' results mean once read off the node channel, and how a storm of notifications is bounded.
//
// THE METHODS, AS THE SPECIFICATION LAYS THEM OUT. A control-method battery (`PNP0C0A`): `_STA`, `_BIX` - or `_BIF`
// where the firmware has no `_BIX` - and `_BST`. An AC adapter (`ACPI0003`): `_STA` and `_PSR`. A thermal zone (published
// by the ACPI service under `THERMALZONE`): `_TMP`, `_RTV`, `_CRT`, `_HOT`, `_PSV` and `_AC0` onwards. What the integers
// MEAN is `power_model::acpi`'s; this reads them out of the values the node channel answers with - `aml::wire`'s
// encoding, whose decoder bounds the depth and the size - and refuses a result of the wrong shape by name.
//
// A STORM OF NOTIFICATIONS IS BOUNDED: a `Notify` refreshes the node's state at most once per `REFRESH_TICKS`; one
// arriving sooner is owed and coalesced into the refresh when it is due, however many arrive meanwhile.

use alloc::vec::Vec;
use aml::wire::Value;

/// What a node is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
	Battery,
	Ac,
	ThermalZone,
}

/// The class a row's match ids name, `_HID` first and each `_CID` after.
pub fn class_of<'a>(ids: impl IntoIterator<Item = &'a [u8]>) -> Option<Class> {
	for id in ids {
		match id {
			b"PNP0C0A" => return Some(Class::Battery),
			b"ACPI0003" => return Some(Class::Ac),
			b"THERMALZONE" => return Some(Class::ThermalZone),
			_ => {}
		}
	}
	None
}

/// Why a result is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Not a package, or a package of the wrong length.
	Shape(&'static str),
	/// An element that should be an integer is not one.
	NotInteger(&'static str),
}

/// An integer as ACPI's 32-bit fields carry it: a value past 32 bits reads as `0xFFFFFFFF`, the specification's
/// "unknown", rather than as its low half.
fn dword(value: &Value, what: &'static str) -> Result<u32, Refusal> {
	match value {
		Value::Integer(raw) => Ok(u32::try_from(*raw).unwrap_or(u32::MAX)),
		_ => Err(Refusal::NotInteger(what)),
	}
}

/// A method's integer result: `_STA`, `_PSR`, `_TMP`, a trip point.
pub fn integer(value: &Value) -> Result<u32, Refusal> {
	dword(value, "the method's result")
}

/// A battery's static description, from `_BIX` or `_BIF` - the fields both carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Information {
	pub power_unit: u32,
	pub design_capacity: u32,
	pub last_full_capacity: u32,
	pub design_warning: u32,
	pub design_low: u32,
}

/// `_BIX`: a revision, then twenty fields (twenty-one from revision 1) - the power unit at 1, the design and last
/// full capacities at 2 and 3, the warning and low levels at 6 and 7.
pub fn bix(value: &Value) -> Result<Information, Refusal> {
	let Value::Package(elements) = value else { return Err(Refusal::Shape("_BIX is not a package")) };
	if elements.len() < 20 {
		return Err(Refusal::Shape("_BIX has fewer than twenty elements"));
	}
	Ok(Information { power_unit: dword(&elements[1], "_BIX's power unit")?, design_capacity: dword(&elements[2], "_BIX's design capacity")?, last_full_capacity: dword(&elements[3], "_BIX's last full capacity")?, design_warning: dword(&elements[6], "_BIX's warning level")?, design_low: dword(&elements[7], "_BIX's low level")? })
}

/// `_BIF`: thirteen fields - the power unit at 0, the capacities at 1 and 2, the warning and low levels at 5 and 6.
pub fn bif(value: &Value) -> Result<Information, Refusal> {
	let Value::Package(elements) = value else { return Err(Refusal::Shape("_BIF is not a package")) };
	if elements.len() < 13 {
		return Err(Refusal::Shape("_BIF has fewer than thirteen elements"));
	}
	Ok(Information { power_unit: dword(&elements[0], "_BIF's power unit")?, design_capacity: dword(&elements[1], "_BIF's design capacity")?, last_full_capacity: dword(&elements[2], "_BIF's last full capacity")?, design_warning: dword(&elements[5], "_BIF's warning level")?, design_low: dword(&elements[6], "_BIF's low level")? })
}

/// `_BST`: the state, the present rate, the remaining capacity and the present voltage.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Status {
	pub state: u32,
	pub rate: u32,
	pub remaining: u32,
	pub voltage: u32,
}

pub fn bst(value: &Value) -> Result<Status, Refusal> {
	let Value::Package(elements) = value else { return Err(Refusal::Shape("_BST is not a package")) };
	if elements.len() < 4 {
		return Err(Refusal::Shape("_BST has fewer than four elements"));
	}
	Ok(Status { state: dword(&elements[0], "_BST's state")?, rate: dword(&elements[1], "_BST's rate")?, remaining: dword(&elements[2], "_BST's remaining capacity")?, voltage: dword(&elements[3], "_BST's voltage")? })
}

/// The battery, for `power_model::acpi::battery`.
pub fn battery(status: Option<u32>, information: &Information, now: &Status) -> power_model::acpi::Battery {
	power_model::acpi::Battery { status, power_unit: information.power_unit, design_capacity: information.design_capacity, last_full_capacity: information.last_full_capacity, design_warning: information.design_warning, design_low: information.design_low, state: now.state, rate: now.rate, remaining: now.remaining, voltage: now.voltage }
}

/// A thermal zone's results as read: the reading, whether it is relative, and each trip the zone defines.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Zone {
	pub temperature: u32,
	pub relative: bool,
	pub critical: Option<u32>,
	pub hot: Option<u32>,
	pub passive: Option<u32>,
	/// `_AC0` onwards, up to the first the zone does not define, at most ten.
	pub active: Vec<u32>,
}

impl Zone {
	pub fn thermal(&self) -> power_model::acpi::Thermal<'_> {
		power_model::acpi::Thermal { temperature: self.temperature, relative: self.relative, critical: self.critical, hot: self.hot, passive: self.passive, active: &self.active }
	}
}

/// How long a refresh bars the next: a tenth of a second.
pub const REFRESH_TICKS: u64 = rt::TICKS_PER_SECOND / 10;

/// THE STORM BOUND: whether a notification refreshes now, or is owed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Coalescer {
	barred_until: u64,
	owed: bool,
	/// Notifications folded into a later refresh, over the binding's life.
	pub coalesced: u64,
}

impl Coalescer {
	/// A notification at `now`: true to refresh now, false when it is owed to the next refresh.
	pub fn notified(&mut self, now: u64) -> bool {
		if now >= self.barred_until {
			self.barred_until = now + REFRESH_TICKS;
			return true;
		}
		if self.owed {
			self.coalesced += 1;
		}
		self.owed = true;
		false
	}

	/// When the owed refresh is due, if one is.
	pub fn due_at(&self) -> Option<u64> {
		self.owed.then_some(self.barred_until)
	}

	/// The owed refresh, taken once its time has come.
	pub fn take_due(&mut self, now: u64) -> bool {
		if !self.owed || now < self.barred_until {
			return false;
		}
		self.owed = false;
		self.barred_until = now + REFRESH_TICKS;
		true
	}
}

#[cfg(test)]
mod tests;

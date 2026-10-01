//! THE PROCESSORS' POWER OBJECTS, read: `_CST`, `_LPI`, `_PSS`, `_PCT`, `_PSD`, `_CPC`, `_PTC`, `_TSS`, `_TSD` - each as
//! the specification lays its package out, taken from the wire values the service evaluates them into. A package of the
//! wrong shape is refused by name (`Refusal`), never guessed at; what a state's entry IS - a halt, an MWAIT hint, a
//! register to read - is read from its register's address space the way the specification and every operating system
//! read it.
//!
//! A REGISTER is a Generic Register descriptor in a resource template (ACPI 6.5, 6.4.3.7): tag 0x82, the address space,
//! the width, the offset, the access size and the address.

use alloc::vec::Vec;

use aml::wire::Value;

/// Why an object could not be read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Not a package, or not of the length the object is.
	Shape,
	/// An element that is not the integer or the register it must be.
	Element,
	/// A register buffer that is no Generic Register descriptor.
	Register,
	/// More states than an object may hold here.
	TooMany,
}

/// THE ADDRESS SPACES, by their ACPI numbers.
pub const SPACE_MEMORY: u8 = 0;
pub const SPACE_IO: u8 = 1;
pub const SPACE_FIXED_HARDWARE: u8 = 0x7F;

/// A GENERIC REGISTER.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Gas {
	pub space: u8,
	pub bits: u8,
	pub offset: u8,
	pub access: u8,
	pub address: u64,
}

/// One Generic Register descriptor, from the resource template a buffer element holds.
pub fn gas(value: &Value) -> Result<Gas, Refusal> {
	let Value::Buffer(bytes) = value else { return Err(Refusal::Register) };
	if bytes.len() < 15 || bytes[0] != 0x82 || u16::from_le_bytes([bytes[1], bytes[2]]) < 12 {
		return Err(Refusal::Register);
	}
	let mut address = [0u8; 8];
	address.copy_from_slice(&bytes[7..15]);
	Ok(Gas { space: bytes[3], bits: bytes[4], offset: bytes[5], access: bytes[6], address: u64::from_le_bytes(address) })
}

fn integer(value: &Value) -> Result<u64, Refusal> {
	match value {
		Value::Integer(value) => Ok(*value),
		_ => Err(Refusal::Element),
	}
}

fn package(value: &Value) -> Result<&[Value], Refusal> {
	match value {
		Value::Package(elements) => Ok(elements),
		_ => Err(Refusal::Shape),
	}
}

fn u32_of(value: &Value) -> Result<u32, Refusal> {
	integer(value).map(|value| value.min(u64::from(u32::MAX)) as u32)
}

// ------------------------------------------------------------------ idle states

/// HOW AN IDLE STATE IS ENTERED, read from its register.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
	/// Functional fixed hardware of class 1: the halt.
	Halt,
	/// Functional fixed hardware of class 2: MWAIT with this hint (the register's address).
	Mwait { hint: u32 },
	/// A read of this register enters it (`P_LVLx`, an `_LPI` entry register).
	Register(Gas),
}

/// What a register says about the entry it names. Functional fixed hardware's bit offset is the class (x86: 1 the halt,
/// 2 native C-state - MWAIT, its hint in the address).
fn entry_of(register: &Gas) -> Entry {
	match (register.space, register.offset) {
		(SPACE_FIXED_HARDWARE, 1) => Entry::Halt,
		(SPACE_FIXED_HARDWARE, _) => Entry::Mwait { hint: register.address as u32 },
		_ => Entry::Register(*register),
	}
}

/// ONE IDLE STATE as `_CST` or `_LPI` states it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IdleState {
	pub entry: Entry,
	pub latency_us: u32,
	pub residency_us: u32,
	pub power_mw: u32,
	/// `_CST`'s type (1 to 3), or the `_LPI` state's place plus one.
	pub depth: u32,
	pub loses_context: bool,
	pub stops_timer: bool,
	pub bus_master_arbitration: bool,
}

/// The most idle states one object may hold here.
pub const MAX_IDLE_STATES: usize = 8;

/// `_CST`: `{ Count, { Register, Type, Latency, Power }... }`, ordered by type. A C3 entered by a port read wants
/// bus-master arbitration disabled around it, and a C3's timer may stop: both said, for the kernel's checks.
pub fn cst(value: &Value) -> Result<Vec<IdleState>, Refusal> {
	let elements = package(value)?;
	let count = integer(elements.first().ok_or(Refusal::Shape)?)? as usize;
	if count != elements.len() - 1 {
		return Err(Refusal::Shape);
	}
	if count > MAX_IDLE_STATES {
		return Err(Refusal::TooMany);
	}
	let mut out = Vec::new();
	for element in &elements[1..] {
		let fields = package(element)?;
		if fields.len() != 4 {
			return Err(Refusal::Shape);
		}
		let register = gas(&fields[0])?;
		let kind = u32_of(&fields[1])?;
		let latency_us = u32_of(&fields[2])?;
		let entry = entry_of(&register);
		out.push(IdleState {
			entry,
			latency_us,
			// `_CST` states no residency: an entry pays from about three times its exit latency, Linux's own rule.
			residency_us: latency_us.saturating_mul(3),
			power_mw: u32_of(&fields[3])?,
			depth: kind,
			loses_context: false,
			stops_timer: kind >= 3,
			bus_master_arbitration: kind >= 3 && matches!(entry, Entry::Register(register) if register.space == SPACE_IO),
		});
	}
	Ok(out)
}

/// `_LPI`: `{ Revision, LevelId, Count, { MinResidency, WakeLatency, Flags, ArchFlags, ResidencyFrequency,
/// EnabledParent, EntryMethod, ResidencyCounter, UsageCounter, Name }... }`. A state whose Flags bit 0 is clear is
/// disabled and left out; an entry method given as an integer (rather than a register) is a firmware-private one and
/// refuses the object. ArchFlags' bit 0 says the core's context is lost (its meaning on ARM; x86 defines none, and a
/// firmware that sets it is taken at its word). Whether the core's timer stops is not stated for x86: a state as deep as
/// a C3 is taken to stop it, as `_CST`'s C3 is, so the kernel enters it only where the timer always runs (ARAT).
pub fn lpi(value: &Value) -> Result<Vec<IdleState>, Refusal> {
	let elements = package(value)?;
	if elements.len() < 3 {
		return Err(Refusal::Shape);
	}
	let count = integer(&elements[2])? as usize;
	if count != elements.len() - 3 {
		return Err(Refusal::Shape);
	}
	if count > MAX_IDLE_STATES {
		return Err(Refusal::TooMany);
	}
	let mut out = Vec::new();
	for (place, element) in elements[3..].iter().enumerate() {
		let fields = package(element)?;
		if fields.len() < 10 {
			return Err(Refusal::Shape);
		}
		if integer(&fields[2])? & 1 == 0 {
			continue;
		}
		let arch = integer(&fields[3])?;
		let register = gas(&fields[6])?;
		out.push(IdleState { entry: entry_of(&register), latency_us: u32_of(&fields[1])?, residency_us: u32_of(&fields[0])?, power_mw: 0, depth: place as u32 + 1, loses_context: arch & 1 != 0, stops_timer: place >= 2, bus_master_arbitration: false });
	}
	Ok(out)
}

// ------------------------------------------------------------------ performance

/// ONE `_PSS` STATE.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PerfState {
	pub core_mhz: u32,
	pub power_mw: u32,
	pub latency_us: u32,
	pub bus_master_latency_us: u32,
	pub control: u32,
	pub status: u32,
}

/// The most performance or throttling states one object may hold here.
pub const MAX_PERF_STATES: usize = 32;

/// `_PSS`: `{ { CoreFrequency, Power, Latency, BusMasterLatency, Control, Status }... }`, fastest first.
pub fn pss(value: &Value) -> Result<Vec<PerfState>, Refusal> {
	let elements = package(value)?;
	if elements.len() > MAX_PERF_STATES {
		return Err(Refusal::TooMany);
	}
	let mut out = Vec::new();
	for element in elements {
		let fields = package(element)?;
		if fields.len() != 6 {
			return Err(Refusal::Shape);
		}
		out.push(PerfState { core_mhz: u32_of(&fields[0])?, power_mw: u32_of(&fields[1])?, latency_us: u32_of(&fields[2])?, bus_master_latency_us: u32_of(&fields[3])?, control: u32_of(&fields[4])?, status: u32_of(&fields[5])? });
	}
	Ok(out)
}

/// `_PCT` and `_PTC`: `{ ControlRegister, StatusRegister }`.
pub fn control_status(value: &Value) -> Result<(Gas, Gas), Refusal> {
	let elements = package(value)?;
	if elements.len() != 2 {
		return Err(Refusal::Shape);
	}
	Ok((gas(&elements[0])?, gas(&elements[1])?))
}

/// `_PSD` and `_TSD`'s one domain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Domain {
	pub domain: u32,
	pub coordination: u32,
	pub processors: u32,
}

/// `_PSD` and `_TSD`: `{ { NumEntries (5), Revision (0), Domain, CoordType, NumProcessors } }`.
pub fn domain(value: &Value) -> Result<Domain, Refusal> {
	let elements = package(value)?;
	let fields = package(elements.first().ok_or(Refusal::Shape)?)?;
	if fields.len() != 5 || integer(&fields[0])? != 5 {
		return Err(Refusal::Shape);
	}
	let coordination = u32_of(&fields[3])?;
	if !matches!(coordination, 0xFC..=0xFE) {
		return Err(Refusal::Element);
	}
	Ok(Domain { domain: u32_of(&fields[2])?, coordination, processors: u32_of(&fields[4])? })
}

/// CPPC'S LEVELS AND REGISTERS.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cppc {
	pub highest: u32,
	pub nominal: u32,
	pub lowest: u32,
	pub desired: Gas,
	pub minimum: Option<Gas>,
	pub maximum: Option<Gas>,
	/// The energy-performance preference register (element 19), where the revision has one.
	pub preference: Option<Gas>,
}

// A `_CPC` element that may be a register or a value: a register buffer whose address is zero is "not supported".
fn optional_register(value: &Value) -> Result<Option<Gas>, Refusal> {
	match value {
		Value::Buffer(_) => {
			let register = gas(value)?;
			Ok((register.address != 0 || register.space == SPACE_FIXED_HARDWARE).then_some(register))
		}
		Value::Integer(_) => Ok(None),
		_ => Err(Refusal::Element),
	}
}

/// `_CPC` (revision 2 or 3): `{ NumEntries, Revision, HighestPerformance, NominalPerformance, LowestNonlinear,
/// LowestPerformance, GuaranteedPerformance, DesiredPerformance, MinimumPerformance, MaximumPerformance, ...,
/// EnergyPerformancePreference (19), ... }`. The levels are read where they are integers - a level held in a register is
/// refused, since the service evaluates and does not read registers; the desired-performance register is required.
pub fn cpc(value: &Value) -> Result<Cppc, Refusal> {
	let elements = package(value)?;
	if elements.len() < 10 {
		return Err(Refusal::Shape);
	}
	let desired = optional_register(&elements[7])?.ok_or(Refusal::Element)?;
	let preference = match elements.get(19) {
		Some(element) => optional_register(element)?,
		None => None,
	};
	Ok(Cppc { highest: u32_of(&elements[2])?, nominal: u32_of(&elements[3])?, lowest: u32_of(&elements[5])?, desired, minimum: optional_register(&elements[8])?, maximum: optional_register(&elements[9])?, preference })
}

/// ONE `_TSS` STATE.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ThrottleState {
	pub percent: u32,
	pub power_mw: u32,
	pub latency_us: u32,
	pub control: u32,
	pub status: u32,
}

/// `_TSS`: `{ { Percent, Power, Latency, Control, Status }... }`, T0 first.
pub fn tss(value: &Value) -> Result<Vec<ThrottleState>, Refusal> {
	let elements = package(value)?;
	if elements.len() > MAX_PERF_STATES {
		return Err(Refusal::TooMany);
	}
	let mut out = Vec::new();
	for element in elements {
		let fields = package(element)?;
		if fields.len() != 5 {
			return Err(Refusal::Shape);
		}
		out.push(ThrottleState { percent: u32_of(&fields[0])?, power_mw: u32_of(&fields[1])?, latency_us: u32_of(&fields[2])?, control: u32_of(&fields[3])?, status: u32_of(&fields[4])? });
	}
	Ok(out)
}

/// A PROCESSOR'S UID, as the MADT keys it: a `Processor` object's processor ID, and a processor device's `_UID` - an
/// integer, or a string spelling one in decimal or with `0x` in hexadecimal. None for a `_UID` that names no number.
pub fn uid_of(processor_id: Option<u8>, uid: Option<&str>) -> Option<u32> {
	if let Some(id) = processor_id {
		return Some(u32::from(id));
	}
	let text = uid?.trim();
	match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
		Some(hex) => u32::from_str_radix(hex, 16).ok(),
		None => text.parse().ok(),
	}
}

/// THE KERNEL'S CORE A PROCESSOR IS: its UID matched against the processor UIDs of the MADT's local APIC and x2APIC
/// entries, and the APIC ID that entry gives matched against the hardware ID each of the kernel's cores reports - `madt`
/// is (UID, APIC ID) as the table lists them, `cores` the hardware IDs by core. None for a processor the MADT does not
/// list, or that no running core is.
pub fn core_of(uid: u32, madt: &[(u32, u32)], cores: &[u64]) -> Option<u32> {
	let (_, apic) = madt.iter().find(|(held, _)| *held == uid)?;
	cores.iter().position(|&id| id == u64::from(*apic)).map(|at| at as u32)
}

#[cfg(test)]
mod tests;

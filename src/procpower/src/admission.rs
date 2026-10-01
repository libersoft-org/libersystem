//! WHETHER THE KERNEL MAY TAKE A REGISTER a processor table names - decided against everything else it holds, the
//! first time a table names it (`holdings`).
//!
//! A PORT is refused when it is in the reserved set - a port the kernel drives, the FADT's blocks, or one another kernel
//! item installed - or in a live grant; otherwise it joins the reserved set under processor power, so no later mint can
//! take it. MEMORY is refused when it is RAM, a range the kernel holds for anything else, inside a claim, or inside a
//! device's BAR; otherwise it is admitted, mapped uncached, and no claim may take it while a table names it. ONE
//! FIXTURE EXCEPTION, in the development build alone: a register inside the BAR of the firmware-held `ivshmem-plain`
//! function (1af4:1110) - the fixture's page, which the harness reads to see which state the governor asked for - is
//! admitted; every shipping build refuses it as it refuses any other BAR.

use crate::{Register, Space};

/// What the register's address range overlaps, as the kernel's tables say it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
	/// A port in the reserved set.
	Reserved,
	/// A port in a live grant.
	Granted,
	/// RAM.
	Ram,
	/// A range the kernel holds for something else.
	KernelHeld,
	/// Inside a claim's range.
	Claimed,
	/// Inside a device's BAR.
	Bar,
	/// Not a port or a memory address.
	Space,
}

/// One device BAR, and whether it is the fixture's - the firmware-held `ivshmem-plain` function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bar {
	pub base: u64,
	pub len: u64,
	pub fixture: bool,
}

/// THE KERNEL'S TABLES, as the decision reads them: port spans as (first, one past the last), memory as (base, len).
pub struct World<'a> {
	pub reserved_ports: &'a [(u32, u32)],
	pub granted_ports: &'a [(u32, u32)],
	pub ram: &'a [(u64, u64)],
	pub kernel_held: &'a [(u64, u64)],
	pub claimed: &'a [(u64, u64)],
	pub bars: &'a [Bar],
	/// The development build's fixture exception is compiled in.
	pub fixture_exception: bool,
}

/// How a register is admitted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admitted {
	/// A port, installed in the reserved set.
	Port,
	/// Memory, mapped uncached.
	Memory,
	/// Memory inside the fixture's BAR, under the development exception.
	FixtureMemory,
}

fn overlaps(base: u64, end: u64, first: u64, stop: u64) -> bool {
	base < stop && first < end
}

/// THE DECISION for a register `check_register` passed.
pub fn admit(register: &Register, world: &World<'_>) -> Result<Admitted, Refused> {
	let bytes = u64::from(register.bytes().unwrap_or(1));
	let (base, end) = (register.address, register.address.saturating_add(bytes));
	match register.space {
		Space::SystemIo => {
			if world.reserved_ports.iter().any(|&(first, stop)| overlaps(base, end, u64::from(first), u64::from(stop))) {
				return Err(Refused::Reserved);
			}
			if world.granted_ports.iter().any(|&(first, stop)| overlaps(base, end, u64::from(first), u64::from(stop))) {
				return Err(Refused::Granted);
			}
			Ok(Admitted::Port)
		}
		Space::SystemMemory => {
			if world.ram.iter().any(|&(first, len)| overlaps(base, end, first, first.saturating_add(len))) {
				return Err(Refused::Ram);
			}
			if world.kernel_held.iter().any(|&(first, len)| overlaps(base, end, first, first.saturating_add(len))) {
				return Err(Refused::KernelHeld);
			}
			if world.claimed.iter().any(|&(first, len)| overlaps(base, end, first, first.saturating_add(len))) {
				return Err(Refused::Claimed);
			}
			match world.bars.iter().find(|bar| overlaps(base, end, bar.base, bar.base.saturating_add(bar.len))) {
				Some(bar) if bar.fixture && world.fixture_exception && base >= bar.base && end <= bar.base.saturating_add(bar.len) => Ok(Admitted::FixtureMemory),
				Some(_) => Err(Refused::Bar),
				None => Ok(Admitted::Memory),
			}
		}
		_ => Err(Refused::Space),
	}
}

#[cfg(test)]
mod tests;

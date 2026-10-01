//! THE REGISTERS PROCESSOR POWER HOLDS, counted per table.
//!
//! Firmware commonly gives every core the same `_CST` package, so one register is named by many cores' tables - and a
//! core's table is replaced by another, or by itself when ProcessorPowerService restarts. A register is TAKEN with the
//! first table that names it - checked against everything else the kernel holds and admitted - and LET GO with the
//! last, and a REPLACEMENT is one step: the new table's registers are taken before the old one's are counted down, so
//! the same table installed again changes nothing and a refused table leaves the old one standing.

use alloc::vec::Vec;

use crate::Register;

/// Every register held, with the number of installed tables naming it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Holdings {
	held: Vec<(Register, u32)>,
}

impl Holdings {
	pub const fn new() -> Holdings {
		Holdings { held: Vec::new() }
	}

	/// How many installed tables name `register`.
	pub fn count(&self, register: &Register) -> u32 {
		self.held.iter().find(|(held, _)| held == register).map_or(0, |(_, count)| *count)
	}

	/// THE REGISTERS A NEW TABLE MUST TAKE: those of `new` no installed table holds. Each is checked and admitted by
	/// the caller before `replace` is called; one refused refuses the table. None where the heap cannot hold the list -
	/// the kernel calls this on a syscall's path, where a short heap must be a refusal.
	pub fn to_take(&self, new: &[Register]) -> Option<Vec<Register>> {
		let mut wanted: Vec<Register> = Vec::new();
		wanted.try_reserve_exact(new.len()).ok()?;
		wanted.extend(new.iter().filter(|register| self.count(register) == 0).copied());
		Some(wanted)
	}

	/// THE ROOM A REPLACEMENT NEEDS, asked for before anything is counted: so `replace` after it cannot fail.
	pub fn reserve(&mut self, new: &[Register]) -> bool {
		self.held.try_reserve(new.len()).is_ok()
	}

	/// THE REPLACEMENT, once every register `to_take` named is admitted: `new`'s registers counted up, then `old`'s
	/// counted down. Answers the registers no table names any more - the caller lets them go - or None, with nothing
	/// counted, where the heap cannot hold the change.
	pub fn replace(&mut self, old: &[Register], new: &[Register]) -> Option<Vec<Register>> {
		let mut released: Vec<Register> = Vec::new();
		if released.try_reserve_exact(old.len()).is_err() || !self.reserve(new) {
			return None;
		}
		for register in new {
			match self.held.iter_mut().find(|(held, _)| held == register) {
				Some((_, count)) => *count += 1,
				None => self.held.push((*register, 1)),
			}
		}
		let mut released = Vec::new();
		for register in old {
			if let Some(at) = self.held.iter().position(|(held, _)| held == register) {
				self.held[at].1 -= 1;
				if self.held[at].1 == 0 {
					self.held.swap_remove(at);
					released.push(*register);
				}
			}
		}
		Some(released)
	}

	/// Every register held.
	pub fn registers(&self) -> impl Iterator<Item = &Register> {
		self.held.iter().map(|(register, _)| register)
	}
}

#[cfg(test)]
mod tests;

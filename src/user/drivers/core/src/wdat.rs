// THE WATCHDOG ACTION TABLE, RUN AS THE REGISTER READS AND WRITES IT LISTS - never AML, never anything else.
//
// A WDAT describes a watchdog by its ACTIONS - reset the count, set it, start, stop, read the status - each a short
// list of instructions: read a register and compare, read a register as the countdown, write a value, write the
// countdown, optionally preserving the register's other bits. This module loads a table against the registers the
// kernel minted for its row - the port ranges and the declared system-memory registers - and refuses what it
// cannot run safely: an instruction naming a register outside them is never executed, and a table whose required
// actions (or a `SET_REBOOT` it lists) cannot be run is not a watchdog at all.

use acpi::{AddressSpace, Gas};
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

// The table's flags.
pub const FLAG_ENABLED: u8 = 1 << 0;
// The firmware stops the timer in the ACPI sleep states - S3 and deeper - and never in suspend to idle.
pub const FLAG_STOPPED_IN_SLEEP: u8 = 1 << 7;

// The actions.
pub const RESET: u8 = 0x01;
pub const QUERY_CURRENT_COUNTDOWN: u8 = 0x04;
pub const QUERY_COUNTDOWN: u8 = 0x05;
pub const SET_COUNTDOWN: u8 = 0x06;
pub const QUERY_RUNNING: u8 = 0x08;
pub const SET_RUNNING: u8 = 0x09;
pub const QUERY_STOPPED: u8 = 0x0A;
pub const SET_STOPPED: u8 = 0x0B;
pub const QUERY_REBOOT: u8 = 0x10;
pub const SET_REBOOT: u8 = 0x11;
pub const QUERY_SHUTDOWN: u8 = 0x12;
pub const SET_SHUTDOWN: u8 = 0x13;
pub const QUERY_STATUS: u8 = 0x20;
pub const SET_STATUS: u8 = 0x21;

const KNOWN: [u8; 14] = [
	RESET,
	QUERY_CURRENT_COUNTDOWN,
	QUERY_COUNTDOWN,
	SET_COUNTDOWN,
	QUERY_RUNNING,
	SET_RUNNING,
	QUERY_STOPPED,
	SET_STOPPED,
	QUERY_REBOOT,
	SET_REBOOT,
	QUERY_SHUTDOWN,
	SET_SHUTDOWN,
	QUERY_STATUS,
	SET_STATUS,
];

// A table without these is not a watchdog.
pub const REQUIRED: [u8; 4] = [RESET, SET_COUNTDOWN, SET_RUNNING, QUERY_RUNNING];

// The instructions.
const READ_VALUE: u8 = 0;
const READ_COUNTDOWN: u8 = 1;
const WRITE_VALUE: u8 = 2;
const WRITE_COUNTDOWN: u8 = 3;
const PRESERVE: u8 = 0x80;

// WHERE A REGISTER IS REACHED: a port inside a minted range, or a declared memory register by its index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
	Port { port: u16, width: u8 },
	Declared { index: usize, width: u8 },
}

// WHAT THE KERNEL MINTED FOR THE ROW: its port ranges, and its declared memory registers in declaration order -
// the order `memory_registers` gives, which is the order the kernel declared them in.
#[derive(Clone, Debug, Default)]
pub struct Minted {
	pub ports: Vec<(u16, u16)>,
	pub memory: Vec<(u64, u8)>,
}

impl Minted {
	fn reach(&self, register: &Gas) -> Option<Reach> {
		let width = acpi::wdat_register_width(register);
		if !matches!(width, 1 | 2 | 4) {
			return None;
		}
		match register.space {
			AddressSpace::SystemIo => {
				let port = u16::try_from(register.address).ok()?;
				let end = u32::from(port) + u32::from(width);
				self.ports.iter().any(|&(base, len)| u32::from(base) <= u32::from(port) && end <= u32::from(base) + u32::from(len)).then_some(Reach::Port { port, width })
			}
			AddressSpace::SystemMemory => self.memory.iter().position(|&(address, declared)| address == register.address && declared == width).map(|index| Reach::Declared { index, width }),
			_ => None,
		}
	}
}

// THE TABLE'S SYSTEM-MEMORY REGISTERS in the order the kernel declares them: each distinct (address, width) at its
// first appearance.
pub fn memory_registers(bytes: &[u8]) -> Vec<(u64, u8)> {
	let mut out: Vec<(u64, u8)> = Vec::new();
	let _ = acpi::wdat_instructions(bytes, |entry| {
		if let Ok(entry) = entry
			&& entry.register.space == AddressSpace::SystemMemory
		{
			let register = (entry.register.address, acpi::wdat_register_width(&entry.register));
			if !out.contains(&register) {
				out.push(register);
			}
		}
	});
	out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Step {
	action: u8,
	op: u8,
	preserve: bool,
	reach: Reach,
	bit_offset: u8,
	value: u32,
	mask: u32,
}

// WHY A TABLE IS NOT A WATCHDOG.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
	// Not a WDAT, or an entry whose register is not a structure.
	Malformed,
	// Its ENABLED flag is clear.
	NotEnabled,
	// A required action is absent.
	Missing(u8),
	// A required action - or a `SET_REBOOT` the table lists - names a register outside what was minted, or an
	// instruction this interpreter does not know.
	Unrunnable(u8),
}

// A LOADED TABLE: its header, and every action it can run.
#[derive(Clone, Debug)]
pub struct Table {
	pub period_ms: u32,
	pub min_count: u32,
	pub max_count: u32,
	pub flags: u8,
	steps: Vec<Step>,
	// Known actions the table listed and this interpreter will not run - a register outside, an unknown
	// instruction - and unknown action codes it ignored.
	pub dropped: Vec<u8>,
}

impl Table {
	pub fn load(bytes: &[u8], minted: &Minted) -> Result<Table, Refusal> {
		let header = acpi::Wdat::new(bytes).map_err(|_| Refusal::Malformed)?;
		if header.flags & FLAG_ENABLED == 0 {
			return Err(Refusal::NotEnabled);
		}
		let mut steps: Vec<Step> = Vec::new();
		let mut bad: Vec<u8> = Vec::new();
		let mut listed: Vec<u8> = Vec::new();
		let mut dropped: Vec<u8> = Vec::new();
		let mut malformed = false;
		acpi::wdat_instructions(bytes, |entry| {
			let Ok(entry) = entry else {
				malformed = true;
				return;
			};
			if !KNOWN.contains(&entry.action) {
				if !dropped.contains(&entry.action) {
					dropped.push(entry.action);
				}
				return;
			}
			if !listed.contains(&entry.action) {
				listed.push(entry.action);
			}
			let op = entry.instruction & !PRESERVE;
			match (op <= WRITE_COUNTDOWN, minted.reach(&entry.register)) {
				(true, Some(reach)) => steps.push(Step { action: entry.action, op, preserve: entry.instruction & PRESERVE != 0, reach, bit_offset: entry.register.bit_offset, value: entry.value, mask: entry.mask }),
				_ => {
					if !bad.contains(&entry.action) {
						bad.push(entry.action);
					}
				}
			}
		})
		.map_err(|_| Refusal::Malformed)?;
		if malformed {
			return Err(Refusal::Malformed);
		}
		for action in REQUIRED {
			if !listed.contains(&action) {
				return Err(Refusal::Missing(action));
			}
		}
		for &action in &bad {
			if REQUIRED.contains(&action) || action == SET_REBOOT {
				return Err(Refusal::Unrunnable(action));
			}
		}
		// AN ACTION WITH ONE UNRUNNABLE INSTRUCTION IS NOT RUN AT ALL: half of an action is a register left in a state
		// the table never describes.
		steps.retain(|step| !bad.contains(&step.action));
		dropped.extend(bad);
		Ok(Table { period_ms: header.timer_period_ms, min_count: header.minimum_count, max_count: header.maximum_count, flags: header.flags, steps, dropped })
	}

	pub fn has(&self, action: u8) -> bool {
		self.steps.iter().any(|step| step.action == action)
	}

	// RUN ONE ACTION with `param` (the countdown a SET_COUNTDOWN writes), answering what its last read gave - for a
	// query, whether the value compared equal (1 or 0) or the countdown read. `None` when the action is not in the
	// table or a register access failed part way.
	pub fn run(&self, action: u8, param: u32, bus: &mut impl Bus) -> Option<u32> {
		if !self.has(action) {
			return None;
		}
		let mut answer = 0u32;
		for step in self.steps.iter().filter(|step| step.action == action) {
			let shift = u32::from(step.bit_offset).min(31);
			match step.op {
				READ_VALUE => {
					let x = (bus.read(step.reach)? >> shift) & step.mask;
					answer = u32::from(x == step.value);
				}
				READ_COUNTDOWN => {
					answer = (bus.read(step.reach)? >> shift) & step.mask;
				}
				_ => {
					let source = if step.op == WRITE_VALUE { step.value } else { param };
					let mut x = (source & step.mask) << shift;
					if step.preserve {
						let kept = bus.read(step.reach)? & !(step.mask << shift);
						x |= kept;
					}
					if !bus.write(step.reach, x) {
						return None;
					}
				}
			}
		}
		Some(answer)
	}
}

// HOW A LOADED TABLE REACHES ITS REGISTERS: the driver's ports and its declared registers.
pub trait Bus {
	fn read(&mut self, reach: Reach) -> Option<u32>;
	fn write(&mut self, reach: Reach, value: u32) -> bool;
}

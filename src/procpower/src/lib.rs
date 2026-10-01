//! THE PROCESSOR'S POWER, AS THE KERNEL DECIDES IT: the checks a table passes before it is installed, the idle governor
//! that picks a core's idle state, the performance governor that picks its level, the bound the live latency requests
//! set, and the count that keeps a register held while any table names it. The kernel holds the tables and carries out
//! the choices - the register writes, MWAIT, the firmware calls - and asks this crate what to do; a host suite drives
//! every decision here with no kernel at all.
//!
//! WHERE THE TABLES COME FROM. ProcessorPowerService evaluates the firmware's `_CST`/`_LPI` and `_PSS`/`_PCT`/`_PSD`/
//! `_CPC`/`_PTC`/`_TSS` through the ACPI service and installs what it read; the device tree's `idle-states` are read by
//! the kernel itself on the two device-tree ports. Either way a table is checked here first, WHOLE: one state or one
//! register refused refuses the table, and the table it would have replaced stands.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

pub mod admission;
pub mod governor;
pub mod holdings;
pub mod latency;
pub mod perf;
pub mod records;

/// At most this many idle states in one core's table.
pub const MAX_IDLE_STATES: usize = 8;
/// At most this many performance states, and as many throttling states, in one core's table.
pub const MAX_PERF_STATES: usize = 32;
/// The largest share of a core's time the scheduler injects as idle, in thousandths.
pub const MAX_INJECT_PERMILLE: u32 = 500;

// ------------------------------------------------------------------ registers

/// THE ADDRESS SPACE A REGISTER IS IN, as ACPI's Generic Address Structure numbers it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Space {
	/// 0: a physical address.
	SystemMemory,
	/// 1: an I/O port.
	SystemIo,
	/// 0x0A: the platform communications channel - excluded.
	PlatformChannel,
	/// 0x7F: functional fixed hardware - an MWAIT hint in an idle table, a model-specific register anywhere else.
	FixedHardware,
	/// Anything else: PCI configuration, the embedded controller, SMBus - no processor control register of these is
	/// written by this kernel.
	Other(u8),
}

impl Space {
	pub fn from_id(id: u8) -> Space {
		match id {
			0 => Space::SystemMemory,
			1 => Space::SystemIo,
			0x0A => Space::PlatformChannel,
			0x7F => Space::FixedHardware,
			other => Space::Other(other),
		}
	}
}

/// ONE REGISTER a table names: its space, its width in bits and its address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Register {
	pub space: Space,
	pub bits: u8,
	pub address: u64,
}

impl Register {
	/// Its width in bytes, for a register the kernel reads or writes: 1, 2, 4 or 8.
	pub fn bytes(&self) -> Option<u8> {
		match self.bits {
			8 => Some(1),
			16 => Some(2),
			32 => Some(4),
			64 if self.space == Space::SystemMemory => Some(8),
			_ => None,
		}
	}
}

/// WHY A TABLE IS REFUSED - whole, whatever its other entries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// No state at all.
	Empty,
	/// More states than the table holds.
	TooManyStates,
	/// A model-specific register: every form of processor control through one is excluded.
	ModelSpecificRegister,
	/// The platform communications channel: excluded.
	PlatformChannel,
	/// A register in a space this kernel writes no processor register in.
	OtherSpace,
	/// A register whose width the kernel cannot access, or a port past the port space.
	BadRegister,
	/// A `_CST` C3 that needs bus-master arbitration control, which this kernel does not drive.
	BusMasterArbitration,
	/// A firmware call the architecture has no such call for - PSCI off aarch64, the SBI off riscv64, MWAIT off x86.
	WrongArchitecture,
	/// States that are not in order of exit latency, or a latency of zero past the first.
	OutOfOrder,
	/// A window whose floor is faster than its cap, or a level past the table.
	BadWindow,
}

/// THE ARCHITECTURE a table is checked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
	X86_64,
	Aarch64,
	Riscv64,
}

/// A register the kernel itself reads or writes: in memory or in the port space, of a width it can access.
pub fn check_register(register: &Register, arch: Arch) -> Result<(), Refusal> {
	match register.space {
		Space::FixedHardware => return Err(Refusal::ModelSpecificRegister),
		Space::PlatformChannel => return Err(Refusal::PlatformChannel),
		Space::Other(_) => return Err(Refusal::OtherSpace),
		Space::SystemIo if arch != Arch::X86_64 => return Err(Refusal::OtherSpace),
		Space::SystemIo | Space::SystemMemory => {}
	}
	let bytes = register.bytes().ok_or(Refusal::BadRegister)?;
	let fits = match register.space {
		Space::SystemIo => register.address.checked_add(u64::from(bytes)).is_some_and(|end| end <= 0x1_0000),
		_ => register.address != 0 && register.address.checked_add(u64::from(bytes)).is_some(),
	};
	if !fits {
		return Err(Refusal::BadRegister);
	}
	Ok(())
}

// ------------------------------------------------------------------ idle states

/// HOW AN IDLE STATE IS ENTERED - the idle loop's last act.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
	/// `hlt` or `wfi`: what the CPU offers with no table.
	Halt,
	/// MWAIT with this hint (x86, from `_CST` or `_LPI` alone).
	Mwait { hint: u32 },
	/// A read of a register, then `hlt` (x86's `P_LVLx` ports, an `_LPI` entry register in memory).
	Register(Register),
	/// PSCI CPU_SUSPEND with this power state parameter (aarch64).
	Psci { parameter: u32 },
	/// The SBI's HART_SUSPEND with this suspend type (riscv64).
	SbiSuspend { parameter: u32 },
}

/// ONE IDLE STATE: how it is entered, what leaving it costs, how long it must last to pay, and what it loses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IdleState {
	pub entry: Entry,
	pub exit_latency_us: u32,
	pub target_residency_us: u32,
	/// The core's context is lost: entered only where the core resumes through the per-core resume path.
	pub loses_context: bool,
	/// The core's timer stops: entered only where it keeps running anyway.
	pub stops_timer: bool,
	/// A `_CST` C3 that wants bus-master arbitration disabled around it.
	pub bus_master_arbitration: bool,
}

/// A CORE'S IDLE TABLE, checked whole: one to eight states, each entered in a form this architecture has, in order of
/// exit latency, with no bus-master arbitration and no model-specific register.
pub fn check_idle_table(states: &[IdleState], arch: Arch) -> Result<(), Refusal> {
	if states.is_empty() {
		return Err(Refusal::Empty);
	}
	if states.len() > MAX_IDLE_STATES {
		return Err(Refusal::TooManyStates);
	}
	for state in states {
		if state.bus_master_arbitration {
			return Err(Refusal::BusMasterArbitration);
		}
		match (state.entry, arch) {
			(Entry::Halt, _) => {}
			(Entry::Mwait { .. }, Arch::X86_64) | (Entry::Psci { .. }, Arch::Aarch64) | (Entry::SbiSuspend { .. }, Arch::Riscv64) => {}
			(Entry::Mwait { .. } | Entry::Psci { .. } | Entry::SbiSuspend { .. }, _) => return Err(Refusal::WrongArchitecture),
			(Entry::Register(register), _) => check_register(&register, arch)?,
		}
	}
	if states.windows(2).any(|pair| pair[1].exit_latency_us < pair[0].exit_latency_us) {
		return Err(Refusal::OutOfOrder);
	}
	Ok(())
}

/// WHAT THIS CORE CAN DO, which decides which installed states it may enter.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Cpu {
	/// MONITOR/MWAIT is offered (x86).
	pub mwait: bool,
	/// The counter the clock is computed from keeps running in every idle state (x86's invariant TSC; the other two
	/// architectures' counters always do).
	pub invariant_counter: bool,
	/// The core's timer keeps running in every idle state (x86's ARAT; no `local-timer-stop` in the tree).
	pub timer_always_running: bool,
	/// The port resumes a core whose context was lost.
	pub context_resume: bool,
}

/// WHY AN INSTALLED STATE IS NOT ENTERED on this core - said once, on the log, and the state left alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unenterable {
	/// MWAIT is not offered.
	NoMwait,
	/// Deeper than the first state, and the counter the clock is computed from may stop in it.
	CounterMayStop,
	/// It stops the timer, and no broadcast timer stands in.
	TimerStops,
	/// It loses the core's context, and this port has no per-core resume path.
	ContextLost,
}

/// Whether a state of a checked table may be entered on a core that can do `cpu`, as the `index`th of its table.
pub fn enterable(state: &IdleState, index: usize, cpu: &Cpu, arch: Arch) -> Result<(), Unenterable> {
	if matches!(state.entry, Entry::Mwait { .. }) && !cpu.mwait {
		return Err(Unenterable::NoMwait);
	}
	if index > 0 && arch == Arch::X86_64 && !cpu.invariant_counter {
		return Err(Unenterable::CounterMayStop);
	}
	if state.stops_timer && !cpu.timer_always_running {
		return Err(Unenterable::TimerStops);
	}
	if state.loses_context && !cpu.context_resume {
		return Err(Unenterable::ContextLost);
	}
	Ok(())
}

/// Every register an idle table names, each once - None where the heap cannot hold them.
pub fn idle_registers(states: &[IdleState]) -> Option<Vec<Register>> {
	let mut out: Vec<Register> = Vec::new();
	out.try_reserve_exact(states.len()).ok()?;
	out.extend(states.iter().filter_map(|state| match state.entry {
		Entry::Register(register) => Some(register),
		_ => None,
	}));
	out.sort_unstable();
	out.dedup();
	Some(out)
}

#[cfg(test)]
mod tests;

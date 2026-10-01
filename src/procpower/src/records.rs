//! THE ABI'S RECORDS, READ: a table as `SYS_PROCESSOR_IDLE_TABLE` and `SYS_PROCESSOR_PERF_TABLE` carry it, turned into
//! the values this crate checks and decides on - by the kernel at install, and by ProcessorPowerService, which builds
//! the records and reads back the levels a table has and what each one writes.

use alloc::vec::Vec;

use crate::perf;
use crate::{Entry, IdleState, Register, Space};

/// A register as the ABI carries it.
pub fn register_of(raw: &abi::ProcessorRegister) -> Register {
	Register { space: Space::from_id(raw.space), bits: raw.bits, address: raw.address }
}

/// An idle state as the ABI carries it; None for an entry method this crate does not know.
pub fn idle_state_of(raw: &abi::ProcessorIdleState) -> Option<IdleState> {
	let entry = match raw.entry {
		abi::IDLE_ENTRY_HALT => Entry::Halt,
		abi::IDLE_ENTRY_MWAIT => Entry::Mwait { hint: raw.parameter },
		abi::IDLE_ENTRY_REGISTER => Entry::Register(register_of(&raw.register)),
		abi::IDLE_ENTRY_PSCI => Entry::Psci { parameter: raw.parameter },
		abi::IDLE_ENTRY_SBI => Entry::SbiSuspend { parameter: raw.parameter },
		_ => return None,
	};
	Some(IdleState { entry, exit_latency_us: raw.exit_latency_us, target_residency_us: raw.target_residency_us, loses_context: raw.flags & abi::IDLE_LOSES_CONTEXT != 0, stops_timer: raw.flags & abi::IDLE_STOPS_TIMER != 0, bus_master_arbitration: raw.flags & abi::IDLE_BUS_MASTER_ARBITRATION != 0 })
}

/// WHY A PERFORMANCE TABLE IS NOT READ: a control kind, a coordination type or a count this crate does not know - or no
/// memory for its states, which the kernel answers as a short heap and not as a bad table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unread {
	Unknown,
	NoMemory,
}

/// A performance table as the ABI carries it.
pub fn perf_table_of(raw: &abi::ProcessorPerfTable) -> Result<perf::Table, Unread> {
	let control = match raw.control_kind {
		abi::PERF_TABLE_STATES => {
			let count = raw.state_count as usize;
			if count > abi::PROCESSOR_MAX_PERF_STATES {
				return Err(Unread::Unknown);
			}
			let mut states = Vec::new();
			states.try_reserve_exact(count).map_err(|_| Unread::NoMemory)?;
			states.extend(raw.states[..count].iter().map(|state| perf::PerfState { core_mhz: state.core_mhz, power_mw: state.power_mw, latency_us: state.latency_us, control: state.control, status: state.status }));
			perf::Control::States { control: register_of(&raw.control), status: register_of(&raw.status), states }
		}
		abi::PERF_TABLE_CPPC => perf::Control::Cppc { desired: register_of(&raw.control), minimum: (raw.status.address != 0).then(|| register_of(&raw.status)), maximum: (raw.maximum.address != 0).then(|| register_of(&raw.maximum)), preference: (raw.preference.address != 0).then(|| register_of(&raw.preference)), highest: raw.highest, nominal: raw.nominal, lowest: raw.lowest },
		_ => return Err(Unread::Unknown),
	};
	let domain = match raw.coordination {
		0 => None,
		value => Some(perf::Domain { domain: raw.domain, coordination: perf::Coordination::from_type(value).ok_or(Unread::Unknown)?, processors: raw.processors }),
	};
	let throttle = match raw.throttle_count as usize {
		0 => None,
		count if count <= abi::PROCESSOR_MAX_PERF_STATES => {
			let mut states = Vec::new();
			states.try_reserve_exact(count).map_err(|_| Unread::NoMemory)?;
			states.extend(raw.throttle[..count].iter().map(|state| perf::ThrottleState { percent: state.percent, latency_us: state.latency_us, control: state.control }));
			Some(perf::Throttle { control: register_of(&raw.throttle_control), states })
		}
		_ => return Err(Unread::Unknown),
	};
	Ok(perf::Table { control, domain, throttle })
}

#[cfg(test)]
mod tests;

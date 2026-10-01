//! A DEVICE'S POWER STATE, THROUGH ITS NODE CHANNEL - what a driver bound to a firmware node asks the ACPI service for,
//! rather than evaluating `_PSx` itself: the power resources behind a state are shared with other devices and counted
//! by the service. The numbering is the channel's: 0 to 3 are D0 to D3hot, 4 is D3cold.
//!
//! THE PATTERN A DRIVER FOLLOWS: D0 once its node is handed (and again when a restarted service hands it again, since
//! the new instance counts from nothing); at a `SUSPEND`, the state `for_sleep` answers; at the `RESUME`, D0 before the
//! device is touched; at a stop, D3cold.

use alloc::format;
use alloc::string::String;
use driver_protocol::SleepState;
use ipc_client::ChannelTransport;
use proto::system::acpi_node;
use rt::*;

pub const D0: u8 = 0;
pub const D3_HOT: u8 = 3;
pub const D3_COLD: u8 = 4;

// How long the service may take to switch a state: `_ON`, `_PSx` and `_OFF` are firmware code.
const TICKS: u64 = TICKS_PER_SECOND * 5;

/// A state's name, for what a driver says.
pub fn name(state: u8) -> &'static str {
	match state {
		0 => "D0",
		1 => "D1",
		2 => "D2",
		3 => "D3hot",
		4 => "D3cold",
		_ => "no state",
	}
}

/// A SLEEP'S TARGET as the channel numbers it: 0 suspend to idle, 3 RAM, 4 disk.
pub fn target(state: SleepState) -> u8 {
	match state {
		SleepState::Idle => 0,
		SleepState::Ram => 3,
		SleepState::Disk => 4,
	}
}

/// ENTER `state`. A node with neither `_PSx` nor `_PRx` answers ok. `Err` says why not - refused, a method that
/// failed, or no answer.
pub fn set(node: u64, state: u8) -> Result<(), String> {
	if node == 0 {
		return Ok(());
	}
	match acpi_node::Client::with_deadline(ChannelTransport { chan: node }, clock() + TICKS).set_power_state(&state) {
		Some(Ok(())) => Ok(()),
		Some(Err(error)) => Err(format!("{} was refused - {error:?}", name(state))),
		None => Err(format!("the ACPI service did not answer {}", name(state))),
	}
}

/// THE STATE A SLEEP LETS THE DEVICE ENTER, from its node's `_SxD` and `_SxW`: where it still wakes the machine when
/// `wake`, the deepest otherwise. Unanswered, D0 when it is to wake - a device kept on still wakes - and D3cold when not.
pub fn for_sleep(node: u64, state: SleepState, wake: bool) -> u8 {
	let unanswered = if wake { D0 } else { D3_COLD };
	if node == 0 {
		return unanswered;
	}
	match acpi_node::Client::with_deadline(ChannelTransport { chan: node }, clock() + TICKS).sleep_power_state(&target(state), &wake) {
		Some(Ok(state)) => state,
		_ => unanswered,
	}
}

//! THE GENERIC TELEPHONE BEARER SERVICE THIS HOST SERVES - TMAP's Call Gateway, the LE Audio counterpart of HFP's audio
//! gateway: the call a voice session declares, as LE earbuds read and are notified it, and the commands they write.
//!
//! THERE IS NO TELEPHONE BEHIND IT. The bearer is AudioService's call relay: at most one call, index 1, in the state
//! the sessions declare; with none declared the list is empty and every command is answered "operation not possible".
//! Accept on a ringing call is the session's answer, Terminate its hang-up - a rejection while it still rings.

use crate::le_audio::uuid;
use alloc::vec::Vec;

/// The call a session declares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Call {
	None,
	Incoming,
	Outgoing,
	Active,
	Held,
}

/// What an earbud asked of the call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
	Answer,
	HangUp,
	Reject,
}

/// The control point's opcodes, and its answers' result codes.
pub mod opcode {
	pub const ACCEPT: u8 = 0x00;
	pub const TERMINATE: u8 = 0x01;
}

pub mod result {
	pub const SUCCESS: u8 = 0x00;
	pub const OPCODE_NOT_SUPPORTED: u8 = 0x01;
	pub const OPERATION_NOT_POSSIBLE: u8 = 0x02;
	pub const INVALID_CALL_INDEX: u8 = 0x03;
}

/// The one call's index.
pub const CALL_INDEX: u8 = 1;
/// The Content Control ID this bearer is known by in a stream's metadata.
pub const CCID: u8 = 1;

const NOTIFY: u8 = 0x10;
const READ: u8 = 0x02;
const WRITE: u8 = 0x08;
const WRITE_WITHOUT_RESPONSE: u8 = 0x04;

/// THE CALL STATE characteristic's value: one entry - index, state, flags - or none.
pub fn call_state(call: Call) -> Vec<u8> {
	// The states TBS numbers, and the outgoing flag.
	let (state, flags) = match call {
		Call::None => return Vec::new(),
		Call::Incoming => (0x00, 0x00),
		Call::Outgoing => (0x02, 0x01),
		Call::Active => (0x03, 0x00),
		Call::Held => (0x04, 0x00),
	};
	alloc::vec![CALL_INDEX, state, flags]
}

/// The List Current Calls characteristic's value: each call's length, index, state, flags and URI - which is empty.
pub fn current_calls(call: Call) -> Vec<u8> {
	let entry = call_state(call);
	if entry.is_empty() {
		return entry;
	}
	let mut out = alloc::vec![entry.len() as u8];
	out.extend_from_slice(&entry);
	out
}

/// GTBS's characteristics, as the GATT server takes them: kind, properties, value, writable.
pub fn characteristics(call: Call) -> Vec<(u16, u8, Vec<u8>, bool)> {
	alloc::vec![
		(uuid::BEARER_PROVIDER_NAME, READ | NOTIFY, b"LiberSystem".to_vec(), false),
		(uuid::BEARER_UCI, READ, b"un000".to_vec(), false),
		// Wi-Fi: a call that goes anywhere goes over the network.
		(uuid::BEARER_TECHNOLOGY, READ | NOTIFY, alloc::vec![0x04], false),
		(uuid::BEARER_URI_SCHEMES, READ | NOTIFY, b"tel".to_vec(), false),
		(uuid::BEARER_LIST_CURRENT_CALLS, READ | NOTIFY, current_calls(call), false),
		(uuid::CONTENT_CONTROL_ID, READ, alloc::vec![CCID], false),
		(uuid::STATUS_FLAGS, READ | NOTIFY, alloc::vec![0, 0], false),
		(uuid::CALL_STATE, READ | NOTIFY, call_state(call), false),
		(uuid::CALL_CONTROL_POINT, WRITE | WRITE_WITHOUT_RESPONSE | NOTIFY, Vec::new(), true),
		(uuid::CALL_CONTROL_POINT_OPTIONAL_OPCODES, READ, alloc::vec![0, 0], false),
		(uuid::TERMINATION_REASON, NOTIFY, Vec::new(), false),
		(uuid::INCOMING_CALL, READ | NOTIFY, Vec::new(), false),
	]
}

/// ONE CONTROL POINT WRITE against the declared call: the notification that answers it, and the command it relays.
pub fn control_point(write: &[u8], call: Call) -> (Vec<u8>, Option<Command>) {
	let (Some(&op), index) = (write.first(), write.get(1).copied()) else { return (alloc::vec![0, 0, result::OPERATION_NOT_POSSIBLE], None) };
	let index = index.unwrap_or(0);
	let answer = |code: u8| alloc::vec![op, index, code];
	if op != opcode::ACCEPT && op != opcode::TERMINATE {
		return (answer(result::OPCODE_NOT_SUPPORTED), None);
	}
	if call == Call::None {
		return (answer(result::OPERATION_NOT_POSSIBLE), None);
	}
	if index != CALL_INDEX {
		return (answer(result::INVALID_CALL_INDEX), None);
	}
	let command = match (op, call) {
		(opcode::ACCEPT, Call::Incoming) => Command::Answer,
		(opcode::ACCEPT, _) => return (answer(result::OPERATION_NOT_POSSIBLE), None),
		(_, Call::Incoming) => Command::Reject,
		_ => Command::HangUp,
	};
	(answer(result::SUCCESS), Some(command))
}

#[cfg(test)]
mod tests;

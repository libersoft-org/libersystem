//! THE AUDIO STREAM CONTROL SERVICE, AS ITS CLIENT: an ASE's state as its characteristic reports it, the control
//! point's operations this host writes, and the control point's answer.
//!
//! WHAT AN ASE IS. One stream endpoint on the earbud - a sink this host plays to, or a source it records from - moved
//! through Codec Configured, QoS Configured, Enabling and Streaming by the operations here, each ASE carrying the
//! parameters its state has. A notification the server sends reports a state; the control point's own notification
//! says whether each ASE an operation named took it.

use crate::le_audio::{self, Config};
use alloc::vec::Vec;

/// The ASE states.
pub mod state {
	pub const IDLE: u8 = 0x00;
	pub const CODEC_CONFIGURED: u8 = 0x01;
	pub const QOS_CONFIGURED: u8 = 0x02;
	pub const ENABLING: u8 = 0x03;
	pub const STREAMING: u8 = 0x04;
	pub const DISABLING: u8 = 0x05;
	pub const RELEASING: u8 = 0x06;
}

/// The control point's operations.
pub mod opcode {
	pub const CONFIG_CODEC: u8 = 0x01;
	pub const CONFIG_QOS: u8 = 0x02;
	pub const ENABLE: u8 = 0x03;
	pub const RECEIVER_START_READY: u8 = 0x04;
	pub const DISABLE: u8 = 0x05;
	pub const RECEIVER_STOP_READY: u8 = 0x06;
	pub const UPDATE_METADATA: u8 = 0x07;
	pub const RELEASE: u8 = 0x08;
}

/// The target latencies a Config Codec names.
pub const LOW_LATENCY: u8 = 0x01;
pub const BALANCED: u8 = 0x02;
/// The 2M PHY, as a target and as a QoS parameter.
pub const PHY_2M: u8 = 0x02;

/// What a Codec Configured ASE prefers: its retransmissions, its latency and the presentation delays it can do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Preferences {
	pub retransmissions: u8,
	pub max_latency_ms: u16,
	pub delay_min_us: u32,
	pub delay_max_us: u32,
}

/// An ASE's state, with what each state carries.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum State {
	Idle,
	CodecConfigured { preferences: Preferences, config: Option<Config> },
	QosConfigured { cig: u8, cis: u8 },
	Enabling { cig: u8, cis: u8 },
	Streaming { cig: u8, cis: u8 },
	Disabling { cig: u8, cis: u8 },
	Releasing,
}

/// One ASE characteristic's value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ase {
	pub id: u8,
	pub state: State,
}

fn u24(bytes: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?, 0]))
}

/// An ASE's value, as read or notified. `None` for a state this service does not have, or parameters that run short.
pub fn parse_ase(value: &[u8]) -> Option<Ase> {
	let (&id, rest) = value.split_first()?;
	let (&code, parameters) = rest.split_first()?;
	let both = |parameters: &[u8]| Some((*parameters.first()?, *parameters.get(1)?));
	let state = match code {
		state::IDLE => State::Idle,
		state::CODEC_CONFIGURED => {
			let fixed = parameters.get(..23)?;
			let preferences = Preferences { retransmissions: fixed[2], max_latency_ms: u16::from_le_bytes([fixed[3], fixed[4]]), delay_min_us: u24(fixed, 5)?, delay_max_us: u24(fixed, 8)? };
			let len = usize::from(fixed[22]);
			let field = parameters.get(23..23 + len)?;
			let config = if fixed[17..22] == le_audio::LC3_ID { Config::parse(field) } else { None };
			State::CodecConfigured { preferences, config }
		}
		state::QOS_CONFIGURED => {
			let (cig, cis) = both(parameters)?;
			parameters.get(..15)?;
			State::QosConfigured { cig, cis }
		}
		state::ENABLING | state::STREAMING | state::DISABLING => {
			let (cig, cis) = both(parameters)?;
			match code {
				state::ENABLING => State::Enabling { cig, cis },
				state::STREAMING => State::Streaming { cig, cis },
				_ => State::Disabling { cig, cis },
			}
		}
		state::RELEASING => State::Releasing,
		_ => return None,
	};
	Some(Ase { id, state })
}

/// `Config Codec`: each ASE with its target latency and its LC3 configuration, on the 2M PHY.
pub fn config_codec(entries: &[(u8, u8, Config)]) -> Vec<u8> {
	let mut out = alloc::vec![opcode::CONFIG_CODEC, entries.len() as u8];
	for (ase, latency, config) in entries {
		out.extend_from_slice(&[*ase, *latency, PHY_2M]);
		out.extend_from_slice(&le_audio::LC3_ID);
		let field = config.encode();
		out.push(field.len() as u8);
		out.extend_from_slice(&field);
	}
	out
}

/// One ASE's QoS: the CIS it streams on and the stream's parameters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Qos {
	pub ase: u8,
	pub cig: u8,
	pub cis: u8,
	pub sdu_interval_us: u32,
	pub max_sdu: u16,
	pub retransmissions: u8,
	pub max_latency_ms: u16,
	pub presentation_delay_us: u32,
}

/// `Config QoS`: unframed, on the 2M PHY.
pub fn config_qos(entries: &[Qos]) -> Vec<u8> {
	let mut out = alloc::vec![opcode::CONFIG_QOS, entries.len() as u8];
	for qos in entries {
		out.extend_from_slice(&[qos.ase, qos.cig, qos.cis]);
		out.extend_from_slice(&qos.sdu_interval_us.to_le_bytes()[..3]);
		out.extend_from_slice(&[0x00, PHY_2M]);
		out.extend_from_slice(&qos.max_sdu.to_le_bytes());
		out.push(qos.retransmissions);
		out.extend_from_slice(&qos.max_latency_ms.to_le_bytes());
		out.extend_from_slice(&qos.presentation_delay_us.to_le_bytes()[..3]);
	}
	out
}

/// `Enable`: each ASE with the streaming contexts it is for.
pub fn enable(entries: &[(u8, u16)]) -> Vec<u8> {
	let mut out = alloc::vec![opcode::ENABLE, entries.len() as u8];
	for (ase, contexts) in entries {
		let metadata = le_audio::streaming_contexts(*contexts);
		out.push(*ase);
		out.push(metadata.len() as u8);
		out.extend_from_slice(&metadata);
	}
	out
}

/// An operation that names ASEs and nothing else: Receiver Start Ready, Disable, Receiver Stop Ready, Release.
pub fn ases_only(opcode: u8, ases: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![opcode, ases.len() as u8];
	out.extend_from_slice(ases);
	out
}

/// The control point's answer: the operation, and each ASE's response code with its reason - code 0 is success.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Response {
	pub opcode: u8,
	pub ases: Vec<(u8, u8, u8)>,
}

pub fn parse_response(value: &[u8]) -> Option<Response> {
	let (&opcode, rest) = value.split_first()?;
	let (&count, rest) = rest.split_first()?;
	// A COUNT OF 0xFF answers an operation the server could not even read - its length or its opcode.
	if count == 0xff {
		return Some(Response { opcode, ases: alloc::vec![(0, *rest.get(1)?, *rest.get(2)?)] });
	}
	let entries = rest.get(..usize::from(count) * 3)?;
	Some(Response { opcode, ases: entries.chunks_exact(3).map(|entry| (entry[0], entry[1], entry[2])).collect() })
}

#[cfg(test)]
mod tests;

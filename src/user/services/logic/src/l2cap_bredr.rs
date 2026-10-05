//! L2CAP FOR BR/EDR: the signalling channel's commands, the configuration options, the channel table with its two-way
//! configuration, and enhanced retransmission mode - each a pure decision the service carries out.
//!
//! WHAT IS DIFFERENT FROM LE. LE's fixed channels need no signalling to exist; every classic profile opens a DYNAMIC
//! channel by PSM, and both sides then configure it - each side's request names what IT will receive (its MTU, its
//! mode), each side answers the other's - so a channel is open only when both directions are configured. Several
//! commands may share one signalling PDU, and an unknown command is answered with a reject rather than ignored, since
//! the peer waits for an answer.
//!
//! THE BOUNDS ARE `bt_bounds`'S: every dynamic channel is offered and accepts `BREDR_MTU`, a channel table holds
//! `CHANNELS_PER_LINK`, and at most `ERTM_CHANNELS_PER_LINK` of them run enhanced retransmission.

use crate::bt_bounds::{BREDR_MTU, CHANNELS_PER_LINK, ERTM_CHANNELS_PER_LINK, ERTM_MONITOR_MS, ERTM_RETRANSMISSION_MS, ERTM_WINDOW};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

/// The signalling channel of an ACL-U link.
pub const SIGNALLING_CID: u16 = 0x0001;
/// The first channel id this host allocates.
pub const FIRST_DYNAMIC_CID: u16 = 0x0040;
/// The MTU the specification guarantees on a BR/EDR channel when none is configured.
pub const DEFAULT_MTU: u16 = 672;
/// The smallest MTU a BR/EDR channel may configure.
pub const MINIMUM_MTU: u16 = 48;

/// The protocols this stack opens or answers, by PSM.
pub mod psm {
	pub const SDP: u16 = 0x0001;
	pub const RFCOMM: u16 = 0x0003;
	pub const BNEP: u16 = 0x000F;
	pub const HID_CONTROL: u16 = 0x0011;
	pub const HID_INTERRUPT: u16 = 0x0013;
	pub const AVCTP: u16 = 0x0017;
	pub const AVDTP: u16 = 0x0019;
	pub const AVCTP_BROWSING: u16 = 0x001B;
}

/// A PSM is valid when its least significant octet is odd and its most significant octet's lowest bit is zero.
pub const fn valid_psm(psm: u16) -> bool {
	psm & 0x0001 == 1 && psm & 0x0100 == 0
}

/// The signalling command codes.
pub mod code {
	pub const COMMAND_REJECT: u8 = 0x01;
	pub const CONNECTION_REQUEST: u8 = 0x02;
	pub const CONNECTION_RESPONSE: u8 = 0x03;
	pub const CONFIGURATION_REQUEST: u8 = 0x04;
	pub const CONFIGURATION_RESPONSE: u8 = 0x05;
	pub const DISCONNECTION_REQUEST: u8 = 0x06;
	pub const DISCONNECTION_RESPONSE: u8 = 0x07;
	pub const ECHO_REQUEST: u8 = 0x08;
	pub const ECHO_RESPONSE: u8 = 0x09;
	pub const INFORMATION_REQUEST: u8 = 0x0A;
	pub const INFORMATION_RESPONSE: u8 = 0x0B;
}

/// A connection response's result.
pub mod connection {
	pub const SUCCESSFUL: u16 = 0x0000;
	pub const PENDING: u16 = 0x0001;
	pub const PSM_NOT_SUPPORTED: u16 = 0x0002;
	pub const SECURITY_BLOCK: u16 = 0x0003;
	pub const NO_RESOURCES: u16 = 0x0004;
	pub const INVALID_SOURCE_CID: u16 = 0x0006;
	pub const SOURCE_CID_ALREADY_ALLOCATED: u16 = 0x0007;
}

/// A configuration response's result.
pub mod configuration {
	pub const SUCCESS: u16 = 0x0000;
	pub const UNACCEPTABLE_PARAMETERS: u16 = 0x0001;
	pub const REJECTED: u16 = 0x0002;
	pub const UNKNOWN_OPTIONS: u16 = 0x0003;
}

/// A command reject's reason.
pub mod reject {
	pub const NOT_UNDERSTOOD: u16 = 0x0000;
	pub const MTU_EXCEEDED: u16 = 0x0001;
	pub const INVALID_CID: u16 = 0x0002;
}

/// The information types this host asks and answers.
pub mod information {
	pub const EXTENDED_FEATURES: u16 = 0x0002;
	pub const FIXED_CHANNELS: u16 = 0x0003;
	pub const SUCCESS: u16 = 0x0000;
	pub const NOT_SUPPORTED: u16 = 0x0001;
	/// The extended features this host has: enhanced retransmission mode and the FCS option.
	pub const FEATURES: u32 = (1 << 3) | (1 << 5);
	/// The fixed channels it has on BR/EDR: the signalling channel, and the BR/EDR Security Manager that derives an LE
	/// key from a Secure Connections link key.
	pub const CHANNELS: u64 = (1 << 1) | (1 << SECURITY_MANAGER_BIT);
	/// The fixed-channel mask's bit for the BR/EDR Security Manager: its channel id.
	pub const SECURITY_MANAGER_BIT: u32 = 7;
}

/// The BR/EDR Security Manager's fixed channel - cross-transport key derivation runs on it.
pub const SECURITY_MANAGER_CID: u16 = 0x0007;

/// A channel's mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
	Basic,
	/// Enhanced retransmission, with its parameters.
	Ertm(ErtmParameters),
}

/// The retransmission and flow control option's parameters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ErtmParameters {
	pub tx_window: u8,
	pub max_transmit: u8,
	pub retransmission_ms: u16,
	pub monitor_ms: u16,
	pub mps: u16,
}

impl ErtmParameters {
	/// What this host asks for: `bt_bounds`' window and the specification's default time-outs; segments of the MTU.
	pub const fn ours() -> ErtmParameters {
		ErtmParameters { tx_window: ERTM_WINDOW as u8, max_transmit: 3, retransmission_ms: ERTM_RETRANSMISSION_MS as u16, monitor_ms: ERTM_MONITOR_MS as u16, mps: BREDR_MTU as u16 }
	}
}

/// The configuration options this host reads and writes.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Options {
	pub mtu: Option<u16>,
	pub flush_timeout: Option<u16>,
	pub mode: Option<Mode>,
	/// The FCS option: 0 none, 1 the 16-bit FCS.
	pub fcs: Option<u8>,
	/// A mode this host does not run - streaming, or the old retransmission and flow control modes: answered with an
	/// unacceptable result naming one it does.
	pub unsupported_mode: Option<u8>,
	/// Options this host does not know that the peer did not mark as hints: they must be named in the response.
	pub unknown: Vec<u8>,
}

/// One signalling command.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
	CommandReject {
		reason: u16,
		data: Vec<u8>,
	},
	ConnectionRequest {
		psm: u16,
		scid: u16,
	},
	ConnectionResponse {
		dcid: u16,
		scid: u16,
		result: u16,
		status: u16,
	},
	ConfigurationRequest {
		dcid: u16,
		continuation: bool,
		options: Options,
	},
	ConfigurationResponse {
		scid: u16,
		continuation: bool,
		result: u16,
		options: Options,
	},
	DisconnectionRequest {
		dcid: u16,
		scid: u16,
	},
	DisconnectionResponse {
		dcid: u16,
		scid: u16,
	},
	EchoRequest(Vec<u8>),
	EchoResponse(Vec<u8>),
	InformationRequest {
		info_type: u16,
	},
	InformationResponse {
		info_type: u16,
		result: u16,
		data: Vec<u8>,
	},
	/// A command this host does not know: its code, answered with a reject.
	Unknown(u8),
}

/// One command with the identifier that pairs a response with its request.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Signal {
	pub identifier: u8,
	pub command: Command,
}

/// Why a signalling PDU was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// A command whose length runs past the PDU, or one too short for what it is.
	Truncated,
	/// An identifier of zero, which the specification reserves.
	ZeroIdentifier,
	/// An option whose length runs past the request.
	BadOption,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
	Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?, *bytes.get(at + 3)?]))
}

/// The configuration options of a request or response.
pub fn decode_options(mut bytes: &[u8]) -> Result<Options, Refusal> {
	let mut options = Options::default();
	while !bytes.is_empty() {
		if bytes.len() < 2 {
			return Err(Refusal::BadOption);
		}
		let (kind, len) = (bytes[0], bytes[1] as usize);
		let Some(value) = bytes.get(2..2 + len) else { return Err(Refusal::BadOption) };
		let hint = kind & 0x80 != 0;
		match kind & 0x7F {
			0x01 if len == 2 => options.mtu = u16_at(value, 0),
			0x02 if len == 2 => options.flush_timeout = u16_at(value, 0),
			0x04 if len == 9 => {
				let parameters = ErtmParameters { tx_window: value[1], max_transmit: value[2], retransmission_ms: u16_at(value, 3).unwrap_or(0), monitor_ms: u16_at(value, 5).unwrap_or(0), mps: u16_at(value, 7).unwrap_or(0) };
				match value[0] {
					0x00 => options.mode = Some(Mode::Basic),
					0x03 => options.mode = Some(Mode::Ertm(parameters)),
					// STREAMING AND THE OLD MODES ARE NOT USED.
					other => options.unsupported_mode = Some(other),
				}
			}
			0x05 if len == 1 => options.fcs = Some(value[0]),
			// QoS, extended flow specification and extended window: known to the specification, ignored as hints are.
			0x03 | 0x06 | 0x07 => {}
			other if !hint => options.unknown.push(other),
			_ => {}
		}
		bytes = &bytes[2 + len..];
	}
	Ok(options)
}

pub fn encode_options(options: &Options, out: &mut Vec<u8>) {
	if let Some(mtu) = options.mtu {
		out.extend_from_slice(&[0x01, 2]);
		out.extend_from_slice(&mtu.to_le_bytes());
	}
	if let Some(flush) = options.flush_timeout {
		out.extend_from_slice(&[0x02, 2]);
		out.extend_from_slice(&flush.to_le_bytes());
	}
	if let Some(mode) = options.mode {
		let (value, p) = match mode {
			Mode::Basic => (0x00, ErtmParameters { tx_window: 0, max_transmit: 0, retransmission_ms: 0, monitor_ms: 0, mps: 0 }),
			Mode::Ertm(p) => (0x03, p),
		};
		out.extend_from_slice(&[0x04, 9, value, p.tx_window, p.max_transmit]);
		out.extend_from_slice(&p.retransmission_ms.to_le_bytes());
		out.extend_from_slice(&p.monitor_ms.to_le_bytes());
		out.extend_from_slice(&p.mps.to_le_bytes());
	}
	if let Some(fcs) = options.fcs {
		out.extend_from_slice(&[0x05, 1, fcs]);
	}
	for &kind in &options.unknown {
		out.extend_from_slice(&[kind, 0]);
	}
}

/// EVERY COMMAND OF ONE SIGNALLING PDU, in order. A command whose length runs past the PDU ends the walk with a refusal.
pub fn decode(mut bytes: &[u8]) -> Result<Vec<Signal>, Refusal> {
	let mut out = Vec::new();
	while !bytes.is_empty() {
		if bytes.len() < 4 {
			return Err(Refusal::Truncated);
		}
		let (code_, identifier, len) = (bytes[0], bytes[1], u16::from_le_bytes([bytes[2], bytes[3]]) as usize);
		let data = bytes.get(4..4 + len).ok_or(Refusal::Truncated)?;
		if identifier == 0 {
			return Err(Refusal::ZeroIdentifier);
		}
		let need = |n: usize| if data.len() < n { Err(Refusal::Truncated) } else { Ok(()) };
		let command = match code_ {
			code::COMMAND_REJECT => {
				need(2)?;
				Command::CommandReject { reason: u16_at(data, 0).unwrap_or(0), data: data[2..].to_vec() }
			}
			code::CONNECTION_REQUEST => {
				need(4)?;
				Command::ConnectionRequest { psm: u16_at(data, 0).unwrap_or(0), scid: u16_at(data, 2).unwrap_or(0) }
			}
			code::CONNECTION_RESPONSE => {
				need(8)?;
				Command::ConnectionResponse { dcid: u16_at(data, 0).unwrap_or(0), scid: u16_at(data, 2).unwrap_or(0), result: u16_at(data, 4).unwrap_or(0), status: u16_at(data, 6).unwrap_or(0) }
			}
			code::CONFIGURATION_REQUEST => {
				need(4)?;
				Command::ConfigurationRequest { dcid: u16_at(data, 0).unwrap_or(0), continuation: u16_at(data, 2).unwrap_or(0) & 1 != 0, options: decode_options(&data[4..])? }
			}
			code::CONFIGURATION_RESPONSE => {
				need(6)?;
				Command::ConfigurationResponse { scid: u16_at(data, 0).unwrap_or(0), continuation: u16_at(data, 2).unwrap_or(0) & 1 != 0, result: u16_at(data, 4).unwrap_or(0), options: decode_options(&data[6..])? }
			}
			code::DISCONNECTION_REQUEST => {
				need(4)?;
				Command::DisconnectionRequest { dcid: u16_at(data, 0).unwrap_or(0), scid: u16_at(data, 2).unwrap_or(0) }
			}
			code::DISCONNECTION_RESPONSE => {
				need(4)?;
				Command::DisconnectionResponse { dcid: u16_at(data, 0).unwrap_or(0), scid: u16_at(data, 2).unwrap_or(0) }
			}
			code::ECHO_REQUEST => Command::EchoRequest(data.to_vec()),
			code::ECHO_RESPONSE => Command::EchoResponse(data.to_vec()),
			code::INFORMATION_REQUEST => {
				need(2)?;
				Command::InformationRequest { info_type: u16_at(data, 0).unwrap_or(0) }
			}
			code::INFORMATION_RESPONSE => {
				need(4)?;
				Command::InformationResponse { info_type: u16_at(data, 0).unwrap_or(0), result: u16_at(data, 2).unwrap_or(0), data: data[4..].to_vec() }
			}
			other => Command::Unknown(other),
		};
		out.push(Signal { identifier, command });
		bytes = &bytes[4 + len..];
	}
	Ok(out)
}

/// One signal as the bytes of a signalling PDU's payload.
pub fn encode(signal: &Signal) -> Vec<u8> {
	let mut data: Vec<u8> = Vec::new();
	let code_ = match &signal.command {
		Command::CommandReject { reason, data: extra } => {
			data.extend_from_slice(&reason.to_le_bytes());
			data.extend_from_slice(extra);
			code::COMMAND_REJECT
		}
		Command::ConnectionRequest { psm, scid } => {
			data.extend_from_slice(&psm.to_le_bytes());
			data.extend_from_slice(&scid.to_le_bytes());
			code::CONNECTION_REQUEST
		}
		Command::ConnectionResponse { dcid, scid, result, status } => {
			for value in [dcid, scid, result, status] {
				data.extend_from_slice(&value.to_le_bytes());
			}
			code::CONNECTION_RESPONSE
		}
		Command::ConfigurationRequest { dcid, continuation, options } => {
			data.extend_from_slice(&dcid.to_le_bytes());
			data.extend_from_slice(&u16::from(*continuation).to_le_bytes());
			encode_options(options, &mut data);
			code::CONFIGURATION_REQUEST
		}
		Command::ConfigurationResponse { scid, continuation, result, options } => {
			data.extend_from_slice(&scid.to_le_bytes());
			data.extend_from_slice(&u16::from(*continuation).to_le_bytes());
			data.extend_from_slice(&result.to_le_bytes());
			encode_options(options, &mut data);
			code::CONFIGURATION_RESPONSE
		}
		Command::DisconnectionRequest { dcid, scid } => {
			data.extend_from_slice(&dcid.to_le_bytes());
			data.extend_from_slice(&scid.to_le_bytes());
			code::DISCONNECTION_REQUEST
		}
		Command::DisconnectionResponse { dcid, scid } => {
			data.extend_from_slice(&dcid.to_le_bytes());
			data.extend_from_slice(&scid.to_le_bytes());
			code::DISCONNECTION_RESPONSE
		}
		Command::EchoRequest(bytes) => {
			data.extend_from_slice(bytes);
			code::ECHO_REQUEST
		}
		Command::EchoResponse(bytes) => {
			data.extend_from_slice(bytes);
			code::ECHO_RESPONSE
		}
		Command::InformationRequest { info_type } => {
			data.extend_from_slice(&info_type.to_le_bytes());
			code::INFORMATION_REQUEST
		}
		Command::InformationResponse { info_type, result, data: extra } => {
			data.extend_from_slice(&info_type.to_le_bytes());
			data.extend_from_slice(&result.to_le_bytes());
			data.extend_from_slice(extra);
			code::INFORMATION_RESPONSE
		}
		Command::Unknown(other) => *other,
	};
	let mut out = Vec::with_capacity(4 + data.len());
	out.push(code_);
	out.push(signal.identifier);
	out.extend_from_slice(&(data.len() as u16).to_le_bytes());
	out.extend_from_slice(&data);
	out
}

// ------------------------------------------------------------------------------------------------------------ channels

/// Where one channel's setup is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
	/// This host asked for it and waits for the peer's response.
	WaitConnect,
	/// Connected; each direction's configuration is done or not.
	Config {
		ours_done: bool,
		theirs_done: bool,
	},
	Open,
	/// This host asked to disconnect it.
	WaitDisconnect,
}

/// One dynamic channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Channel {
	pub local_cid: u16,
	pub remote_cid: u16,
	pub psm: u16,
	pub state: State,
	/// What the peer may send this host: this host's MTU, the bound.
	pub our_mtu: u16,
	/// What this host may send the peer, as its configuration request said.
	pub peer_mtu: u16,
	pub mode: Mode,
	/// Whether this host asked for enhanced retransmission when it opened the channel.
	pub wants_ertm: bool,
	/// The identifier of this host's outstanding request on the channel.
	pub pending: u8,
}

/// What a signal makes the service do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Out {
	/// Send this signal.
	Send(Signal),
	/// A channel is open both ways: its local id.
	Opened(u16),
	/// A channel this host asked for was refused: its local id and the peer's result.
	Refused(u16, u16),
	/// A channel is closed: its local id.
	Closed(u16),
	/// The answer to an information request this host made: its type, result and data.
	Information { info_type: u16, result: u16, data: Vec<u8> },
}

/// Whether this host serves an inbound connection for a PSM, and if not, why.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admission {
	Accept,
	/// Nothing here serves the PSM.
	NotSupported,
	/// The peer is not trusted for the profile the PSM names, or the link is not secure enough for it.
	Security,
}

/// THE CHANNELS OF ONE ACL LINK: allocation, the two-way configuration, disconnection and the identifiers that pair
/// requests with responses.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Channels {
	channels: Vec<Channel>,
	next_identifier: u8,
}

impl Default for Channels {
	fn default() -> Channels {
		Channels::new()
	}
}

impl Channels {
	pub const fn new() -> Channels {
		Channels { channels: Vec::new(), next_identifier: 1 }
	}

	pub fn get(&self, local_cid: u16) -> Option<&Channel> {
		self.channels.iter().find(|channel| channel.local_cid == local_cid)
	}

	pub fn by_remote(&self, remote_cid: u16) -> Option<&Channel> {
		self.channels.iter().find(|channel| channel.remote_cid == remote_cid && channel.remote_cid != 0)
	}

	pub fn len(&self) -> usize {
		self.channels.len()
	}

	pub fn is_empty(&self) -> bool {
		self.channels.is_empty()
	}

	fn identifier(&mut self) -> u8 {
		let identifier = self.next_identifier;
		self.next_identifier = if identifier == 0xFF { 1 } else { identifier + 1 };
		identifier
	}

	fn free_cid(&self) -> Option<u16> {
		(FIRST_DYNAMIC_CID..FIRST_DYNAMIC_CID + CHANNELS_PER_LINK as u16 * 4).find(|cid| self.get(*cid).is_none())
	}

	fn ertm_count(&self) -> usize {
		self.channels.iter().filter(|channel| channel.wants_ertm || matches!(channel.mode, Mode::Ertm(_))).count()
	}

	/// This host's configuration request for a channel: its MTU, and enhanced retransmission where it wants it.
	fn configure(&mut self, at: usize) -> Signal {
		let identifier = self.identifier();
		let channel = &mut self.channels[at];
		channel.pending = identifier;
		let mut options = Options { mtu: Some(channel.our_mtu), ..Options::default() };
		if channel.wants_ertm {
			options.mode = Some(Mode::Ertm(ErtmParameters::ours()));
			options.fcs = Some(1);
		}
		Signal { identifier, command: Command::ConfigurationRequest { dcid: channel.remote_cid, continuation: false, options } }
	}

	/// OPEN A CHANNEL to `psm`: its local id and the connection request, or `None` when the table is full - or holds as
	/// many ERTM channels as it may and this one wants another.
	pub fn open(&mut self, psm: u16, ertm: bool) -> Option<(u16, Signal)> {
		if self.channels.len() >= CHANNELS_PER_LINK || (ertm && self.ertm_count() >= ERTM_CHANNELS_PER_LINK) || !valid_psm(psm) {
			return None;
		}
		let local_cid = self.free_cid()?;
		let identifier = self.identifier();
		self.channels.push(Channel { local_cid, remote_cid: 0, psm, state: State::WaitConnect, our_mtu: BREDR_MTU as u16, peer_mtu: DEFAULT_MTU, mode: Mode::Basic, wants_ertm: ertm, pending: identifier });
		Some((local_cid, Signal { identifier, command: Command::ConnectionRequest { psm, scid: local_cid } }))
	}

	/// ASK THE PEER ONE INFORMATION TYPE - its fixed channels, before cross-transport key derivation.
	pub fn information_request(&mut self, info_type: u16) -> Signal {
		Signal { identifier: self.identifier(), command: Command::InformationRequest { info_type } }
	}

	/// CLOSE ONE: the disconnection request, or nothing for a channel this table does not hold.
	pub fn close(&mut self, local_cid: u16) -> Option<Signal> {
		let identifier = self.identifier();
		let channel = self.channels.iter_mut().find(|channel| channel.local_cid == local_cid)?;
		channel.state = State::WaitDisconnect;
		channel.pending = identifier;
		Some(Signal { identifier, command: Command::DisconnectionRequest { dcid: channel.remote_cid, scid: channel.local_cid } })
	}

	/// The link went: every channel is closed with it.
	pub fn drop_all(&mut self) -> Vec<u16> {
		self.channels.drain(..).map(|channel| channel.local_cid).collect()
	}

	/// ONE SIGNAL FROM THE PEER, and what the service must do about it. `admit` decides an inbound connection by PSM.
	pub fn on_signal(&mut self, signal: Signal, admit: &mut dyn FnMut(u16) -> Admission) -> Vec<Out> {
		let mut out = Vec::new();
		let identifier = signal.identifier;
		match signal.command {
			Command::ConnectionRequest { psm, scid } => {
				let refuse = |result: u16| Out::Send(Signal { identifier, command: Command::ConnectionResponse { dcid: 0, scid, result, status: 0 } });
				if self.by_remote(scid).is_some() {
					out.push(refuse(connection::SOURCE_CID_ALREADY_ALLOCATED));
					return out;
				}
				if scid < FIRST_DYNAMIC_CID {
					out.push(refuse(connection::INVALID_SOURCE_CID));
					return out;
				}
				match admit(psm) {
					Admission::NotSupported => out.push(refuse(connection::PSM_NOT_SUPPORTED)),
					Admission::Security => out.push(refuse(connection::SECURITY_BLOCK)),
					Admission::Accept => {
						let Some(local_cid) = self.free_cid().filter(|_| self.channels.len() < CHANNELS_PER_LINK) else {
							out.push(refuse(connection::NO_RESOURCES));
							return out;
						};
						self.channels.push(Channel { local_cid, remote_cid: scid, psm, state: State::Config { ours_done: false, theirs_done: false }, our_mtu: BREDR_MTU as u16, peer_mtu: DEFAULT_MTU, mode: Mode::Basic, wants_ertm: false, pending: 0 });
						out.push(Out::Send(Signal { identifier, command: Command::ConnectionResponse { dcid: local_cid, scid, result: connection::SUCCESSFUL, status: 0 } }));
						let at = self.channels.len() - 1;
						out.push(Out::Send(self.configure(at)));
					}
				}
			}
			Command::ConnectionResponse { dcid, scid, result, .. } => {
				let Some(at) = self.channels.iter().position(|channel| channel.local_cid == scid && channel.state == State::WaitConnect) else { return out };
				match result {
					connection::SUCCESSFUL => {
						self.channels[at].remote_cid = dcid;
						self.channels[at].state = State::Config { ours_done: false, theirs_done: false };
						out.push(Out::Send(self.configure(at)));
					}
					connection::PENDING => {}
					refused => {
						self.channels.remove(at);
						out.push(Out::Refused(scid, refused));
					}
				}
			}
			Command::ConfigurationRequest { dcid, continuation, options } => {
				let Some(at) = self.channels.iter().position(|channel| channel.local_cid == dcid) else {
					out.push(Out::Send(Signal { identifier, command: Command::CommandReject { reason: reject::INVALID_CID, data: [dcid.to_le_bytes(), 0u16.to_le_bytes()].concat() } }));
					return out;
				};
				let scid = self.channels[at].remote_cid;
				if !options.unknown.is_empty() {
					out.push(Out::Send(Signal { identifier, command: Command::ConfigurationResponse { scid, continuation: false, result: configuration::UNKNOWN_OPTIONS, options: Options { unknown: options.unknown, ..Options::default() } } }));
					return out;
				}
				// A MODE THIS HOST DOES NOT RUN is answered with the one it does.
				if options.unsupported_mode.is_some() {
					let ours = if self.channels[at].wants_ertm { Mode::Ertm(ErtmParameters::ours()) } else { Mode::Basic };
					out.push(Out::Send(Signal { identifier, command: Command::ConfigurationResponse { scid, continuation: false, result: configuration::UNACCEPTABLE_PARAMETERS, options: Options { mode: Some(ours), ..Options::default() } } }));
					return out;
				}
				// THE PEER'S MTU is what this host may send; one below the minimum is answered with the minimum.
				if let Some(mtu) = options.mtu
					&& mtu < MINIMUM_MTU
				{
					out.push(Out::Send(Signal { identifier, command: Command::ConfigurationResponse { scid, continuation: false, result: configuration::UNACCEPTABLE_PARAMETERS, options: Options { mtu: Some(MINIMUM_MTU), ..Options::default() } } }));
					return out;
				}
				// THE MODE: basic always; enhanced retransmission when this host wanted it too, or room for one is left.
				let mut mode = self.channels[at].mode;
				if let Some(asked) = options.mode {
					match asked {
						Mode::Basic if self.channels[at].wants_ertm => {
							out.push(Out::Send(Signal { identifier, command: Command::ConfigurationResponse { scid, continuation: false, result: configuration::UNACCEPTABLE_PARAMETERS, options: Options { mode: Some(Mode::Ertm(ErtmParameters::ours())), ..Options::default() } } }));
							return out;
						}
						Mode::Basic => mode = Mode::Basic,
						Mode::Ertm(parameters) => {
							let room = self.channels[at].wants_ertm || self.ertm_count() < ERTM_CHANNELS_PER_LINK;
							if !room || parameters.tx_window == 0 || parameters.mps < MINIMUM_MTU {
								out.push(Out::Send(Signal { identifier, command: Command::ConfigurationResponse { scid, continuation: false, result: configuration::UNACCEPTABLE_PARAMETERS, options: Options { mode: Some(Mode::Basic), ..Options::default() } } }));
								return out;
							}
							mode = Mode::Ertm(parameters);
						}
					}
				}
				let channel = &mut self.channels[at];
				channel.peer_mtu = options.mtu.unwrap_or(DEFAULT_MTU);
				channel.mode = mode;
				let mut accepted = Options { mtu: options.mtu, ..Options::default() };
				if let Mode::Ertm(parameters) = mode {
					accepted.mode = Some(Mode::Ertm(parameters));
				}
				out.push(Out::Send(Signal { identifier, command: Command::ConfigurationResponse { scid, continuation, result: configuration::SUCCESS, options: accepted } }));
				if !continuation && let State::Config { ours_done, .. } = channel.state {
					channel.state = if ours_done { State::Open } else { State::Config { ours_done, theirs_done: true } };
					if channel.state == State::Open {
						out.push(Out::Opened(channel.local_cid));
					}
				}
			}
			Command::ConfigurationResponse { scid, result, options, .. } => {
				let Some(at) = self.channels.iter().position(|channel| channel.local_cid == scid) else { return out };
				if self.channels[at].pending != identifier {
					return out;
				}
				match result {
					configuration::SUCCESS => {
						let channel = &mut self.channels[at];
						if let State::Config { theirs_done, .. } = channel.state {
							channel.state = if theirs_done { State::Open } else { State::Config { ours_done: true, theirs_done } };
							if channel.state == State::Open {
								out.push(Out::Opened(channel.local_cid));
							}
						}
					}
					// UNACCEPTABLE: the peer's counter-proposal for the mode is taken where it is basic and this host can
					// live with it; anything else ends the channel.
					configuration::UNACCEPTABLE_PARAMETERS if options.mode == Some(Mode::Basic) => {
						self.channels[at].wants_ertm = false;
						out.push(Out::Send(self.configure(at)));
					}
					_ => {
						if let Some(signal) = self.close(scid) {
							out.push(Out::Send(signal));
						}
					}
				}
			}
			Command::DisconnectionRequest { dcid, scid } => {
				let Some(at) = self.channels.iter().position(|channel| channel.local_cid == dcid && channel.remote_cid == scid) else {
					out.push(Out::Send(Signal { identifier, command: Command::CommandReject { reason: reject::INVALID_CID, data: [scid.to_le_bytes(), dcid.to_le_bytes()].concat() } }));
					return out;
				};
				self.channels.remove(at);
				out.push(Out::Send(Signal { identifier, command: Command::DisconnectionResponse { dcid, scid } }));
				out.push(Out::Closed(dcid));
			}
			Command::DisconnectionResponse { scid, .. } => {
				if let Some(at) = self.channels.iter().position(|channel| channel.local_cid == scid && channel.state == State::WaitDisconnect) {
					self.channels.remove(at);
					out.push(Out::Closed(scid));
				}
			}
			Command::EchoRequest(data) => out.push(Out::Send(Signal { identifier, command: Command::EchoResponse(data) })),
			Command::InformationRequest { info_type } => {
				let (result, data) = match info_type {
					information::EXTENDED_FEATURES => (information::SUCCESS, information::FEATURES.to_le_bytes().to_vec()),
					information::FIXED_CHANNELS => (information::SUCCESS, information::CHANNELS.to_le_bytes().to_vec()),
					_ => (information::NOT_SUPPORTED, Vec::new()),
				};
				out.push(Out::Send(Signal { identifier, command: Command::InformationResponse { info_type, result, data } }));
			}
			// A REJECT ENDS what this host asked with that identifier.
			Command::CommandReject { .. } => {
				if let Some(at) = self.channels.iter().position(|channel| channel.pending == identifier) {
					let gone = self.channels.remove(at);
					out.push(if gone.state == State::WaitConnect { Out::Refused(gone.local_cid, connection::PSM_NOT_SUPPORTED) } else { Out::Closed(gone.local_cid) });
				}
			}
			Command::Unknown(_) => out.push(Out::Send(Signal { identifier, command: Command::CommandReject { reason: reject::NOT_UNDERSTOOD, data: Vec::new() } })),
			Command::InformationResponse { info_type, result, data } => out.push(Out::Information { info_type, result, data }),
			Command::EchoResponse(_) => {}
		}
		out
	}
}

/// The peer's fixed channels, from an information response: whether it has the BR/EDR Security Manager.
pub fn peer_has_security_manager(data: &[u8]) -> bool {
	data.get(..8).and_then(|bytes| <[u8; 8]>::try_from(bytes).ok()).is_some_and(|mask| u64::from_le_bytes(mask) & (1 << information::SECURITY_MANAGER_BIT) != 0)
}

/// The peer's extended features, from an information response: whether it has enhanced retransmission.
pub fn peer_has_ertm(data: &[u8]) -> bool {
	u32_at(data, 0).is_some_and(|features| features & (1 << 3) != 0)
}

// ---------------------------------------------------------------------------------------------- enhanced retransmission

/// The 16-bit FCS ERTM frames carry: the CRC with polynomial x^16 + x^15 + x^2 + 1, least significant bit first, from
/// zero, over the basic header, the control field, the SDU length where there is one, and the payload.
pub fn fcs(bytes: &[u8]) -> u16 {
	let mut crc: u16 = 0;
	for &byte in bytes {
		crc ^= u16::from(byte);
		for _ in 0..8 {
			crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
		}
	}
	crc
}

/// How an I-frame's payload belongs to its SDU.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sar {
	Unsegmented,
	/// The first segment: it carries the SDU's length.
	Start,
	End,
	Continuation,
}

/// A supervisory frame's function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Supervisory {
	ReceiverReady,
	Reject,
	ReceiverNotReady,
	SelectiveReject,
}

/// The enhanced control field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
	Information { tx_seq: u8, req_seq: u8, sar: Sar, f: bool },
	Supervisory { kind: Supervisory, req_seq: u8, p: bool, f: bool },
}

impl Control {
	pub fn encode(&self) -> u16 {
		match *self {
			Control::Information { tx_seq, req_seq, sar, f } => {
				let sar = match sar {
					Sar::Unsegmented => 0,
					Sar::Start => 1,
					Sar::End => 2,
					Sar::Continuation => 3,
				};
				(u16::from(tx_seq & 0x3F) << 1) | (u16::from(f) << 7) | (u16::from(req_seq & 0x3F) << 8) | (sar << 14)
			}
			Control::Supervisory { kind, req_seq, p, f } => {
				let s = match kind {
					Supervisory::ReceiverReady => 0,
					Supervisory::Reject => 1,
					Supervisory::ReceiverNotReady => 2,
					Supervisory::SelectiveReject => 3,
				};
				1 | (s << 2) | (u16::from(p) << 4) | (u16::from(f) << 7) | (u16::from(req_seq & 0x3F) << 8)
			}
		}
	}

	pub fn decode(value: u16) -> Control {
		let req_seq = ((value >> 8) & 0x3F) as u8;
		let f = value & (1 << 7) != 0;
		if value & 1 == 0 {
			let sar = match value >> 14 {
				0 => Sar::Unsegmented,
				1 => Sar::Start,
				2 => Sar::End,
				_ => Sar::Continuation,
			};
			Control::Information { tx_seq: ((value >> 1) & 0x3F) as u8, req_seq, sar, f }
		} else {
			let kind = match (value >> 2) & 3 {
				0 => Supervisory::ReceiverReady,
				1 => Supervisory::Reject,
				2 => Supervisory::ReceiverNotReady,
				_ => Supervisory::SelectiveReject,
			};
			Control::Supervisory { kind, req_seq, p: value & (1 << 4) != 0, f }
		}
	}
}

/// One ERTM frame's body - control field, the SDU length on a start, the payload - with the FCS appended, ready to go
/// behind a basic header naming `remote_cid`.
pub fn frame(remote_cid: u16, control: Control, sdu_len: Option<u16>, payload: &[u8]) -> Vec<u8> {
	let body_len = 2 + if sdu_len.is_some() { 2 } else { 0 } + payload.len() + 2;
	let mut out = Vec::with_capacity(4 + body_len);
	out.extend_from_slice(&(body_len as u16).to_le_bytes());
	out.extend_from_slice(&remote_cid.to_le_bytes());
	out.extend_from_slice(&control.encode().to_le_bytes());
	if let Some(len) = sdu_len {
		out.extend_from_slice(&len.to_le_bytes());
	}
	out.extend_from_slice(payload);
	let check = fcs(&out);
	out.extend_from_slice(&check.to_le_bytes());
	out
}

/// Why an ERTM frame was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameRefusal {
	Short,
	BadFcs,
	/// A start whose SDU length is past the MTU, or a segment that would take an SDU past its declared length.
	TooLong,
}

/// ONE RECEIVED ERTM FRAME, whole L2CAP PDU in: its control field, the SDU length on a start, and the payload - the FCS
/// checked and taken off.
pub fn parse_frame(pdu: &[u8]) -> Result<(Control, Option<u16>, &[u8]), FrameRefusal> {
	if pdu.len() < 4 + 2 + 2 {
		return Err(FrameRefusal::Short);
	}
	let (covered, check) = pdu.split_at(pdu.len() - 2);
	if fcs(covered) != u16::from_le_bytes([check[0], check[1]]) {
		return Err(FrameRefusal::BadFcs);
	}
	let control = Control::decode(u16::from_le_bytes([covered[4], covered[5]]));
	let body = &covered[6..];
	match control {
		Control::Information { sar: Sar::Start, .. } => {
			if body.len() < 2 {
				return Err(FrameRefusal::Short);
			}
			Ok((control, Some(u16::from_le_bytes([body[0], body[1]])), &body[2..]))
		}
		_ => Ok((control, None, body)),
	}
}

/// What the ERTM state machine asks the service to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ErtmOut {
	/// Send this PDU on the link.
	Send(Vec<u8>),
	/// An SDU arrived whole.
	Deliver(Vec<u8>),
	/// The peer did not answer a poll `max_transmit` times, or broke the protocol: the channel is disconnected.
	Fail,
}

// The distance from `from` to `to` in sequence space.
const fn ahead(from: u8, to: u8) -> u8 {
	to.wrapping_sub(from) & 0x3F
}

/// ONE ERTM CHANNEL, BOTH DIRECTIONS: the transmit window and its acknowledgements, retransmission on a reject, the poll
/// and the final bit when the retransmission timer expires, the monitor timer while a poll waits, and in-order
/// reassembly of what arrives - an out-of-order frame answered with one reject. Time is passed in, in milliseconds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ertm {
	remote_cid: u16,
	peer: ErtmParameters,
	mtu: usize,
	// TRANSMIT: the next sequence number, the oldest unacknowledged one, what waits for its first send, and what was
	// sent and not acknowledged, with how often it went.
	next_tx_seq: u8,
	expected_ack_seq: u8,
	waiting: VecDeque<(Control, Option<u16>, Vec<u8>)>,
	unacked: VecDeque<(u8, Option<u16>, Sar, Vec<u8>, u8)>,
	retransmission_at: Option<u64>,
	monitor_at: Option<u64>,
	polls: u8,
	remote_busy: bool,
	// RECEIVE: the next sequence number expected, whether a reject is outstanding, and the SDU being assembled.
	expected_tx_seq: u8,
	rejected: bool,
	assembling: Option<(usize, Vec<u8>)>,
	unacked_received: u8,
}

impl Ertm {
	/// A channel configured with the peer's parameters - its window bounds what this host may have outstanding - and the
	/// MTU this host accepts.
	pub fn new(remote_cid: u16, peer: ErtmParameters, mtu: usize) -> Ertm {
		Ertm { remote_cid, peer, mtu, next_tx_seq: 0, expected_ack_seq: 0, waiting: VecDeque::new(), unacked: VecDeque::new(), retransmission_at: None, monitor_at: None, polls: 0, remote_busy: false, expected_tx_seq: 0, rejected: false, assembling: None, unacked_received: 0 }
	}

	/// The next deadline the service must call `tick` at.
	pub fn deadline(&self) -> Option<u64> {
		match (self.retransmission_at, self.monitor_at) {
			(Some(a), Some(b)) => Some(a.min(b)),
			(a, b) => a.or(b),
		}
	}

	/// Frames sent and not acknowledged.
	pub fn outstanding(&self) -> usize {
		self.unacked.len()
	}

	fn window_open(&self) -> bool {
		!self.remote_busy && self.monitor_at.is_none() && self.unacked.len() < usize::from(self.peer.tx_window.max(1))
	}

	fn i_frame(&self, tx_seq: u8, sar: Sar, sdu_len: Option<u16>, payload: &[u8], f: bool) -> Vec<u8> {
		frame(self.remote_cid, Control::Information { tx_seq, req_seq: self.expected_tx_seq, sar, f }, sdu_len, payload)
	}

	fn s_frame(&self, kind: Supervisory, p: bool, f: bool) -> Vec<u8> {
		frame(self.remote_cid, Control::Supervisory { kind, req_seq: self.expected_tx_seq, p, f }, None, &[])
	}

	/// SEND AN SDU: segmented to the peer's MPS, each segment queued, as many sent as the window allows.
	pub fn send(&mut self, sdu: &[u8], now_ms: u64) -> Vec<ErtmOut> {
		let mps = usize::from(self.peer.mps.max(1));
		if sdu.len() <= mps {
			self.waiting.push_back((Control::Information { tx_seq: 0, req_seq: 0, sar: Sar::Unsegmented, f: false }, None, sdu.to_vec()));
		} else {
			let mut chunks = sdu.chunks(mps).peekable();
			let mut first = true;
			while let Some(chunk) = chunks.next() {
				let sar = if first {
					Sar::Start
				} else if chunks.peek().is_none() {
					Sar::End
				} else {
					Sar::Continuation
				};
				let len = first.then_some(sdu.len() as u16);
				self.waiting.push_back((Control::Information { tx_seq: 0, req_seq: 0, sar, f: false }, len, chunk.to_vec()));
				first = false;
			}
		}
		self.pump(now_ms)
	}

	fn pump(&mut self, now_ms: u64) -> Vec<ErtmOut> {
		let mut out = Vec::new();
		while self.window_open() {
			let Some((control, sdu_len, payload)) = self.waiting.pop_front() else { break };
			let Control::Information { sar, .. } = control else { continue };
			let tx_seq = self.next_tx_seq;
			self.next_tx_seq = (self.next_tx_seq + 1) & 0x3F;
			out.push(ErtmOut::Send(self.i_frame(tx_seq, sar, sdu_len, &payload, false)));
			self.unacked.push_back((tx_seq, sdu_len, sar, payload, 1));
			self.unacked_received = 0;
			if self.retransmission_at.is_none() {
				self.retransmission_at = Some(now_ms + u64::from(self.peer.retransmission_ms.max(1)));
			}
		}
		out
	}

	// Everything up to `req_seq` is acknowledged.
	fn acknowledge(&mut self, req_seq: u8, now_ms: u64) -> bool {
		let distance = ahead(self.expected_ack_seq, req_seq);
		if usize::from(distance) > self.unacked.len() {
			return false;
		}
		for _ in 0..distance {
			self.unacked.pop_front();
		}
		self.expected_ack_seq = req_seq;
		self.retransmission_at = if self.unacked.is_empty() { None } else { Some(now_ms + u64::from(self.peer.retransmission_ms.max(1))) };
		true
	}

	fn retransmit_all(&mut self, now_ms: u64) -> Result<Vec<ErtmOut>, ()> {
		let mut out = Vec::new();
		let max = self.peer.max_transmit;
		let frames: Vec<(u8, Option<u16>, Sar, Vec<u8>)> = self.unacked.iter().map(|(seq, len, sar, payload, _)| (*seq, *len, *sar, payload.clone())).collect();
		for entry in self.unacked.iter_mut() {
			entry.4 = entry.4.saturating_add(1);
			if max != 0 && entry.4 > max {
				return Err(());
			}
		}
		for (seq, len, sar, payload) in frames {
			out.push(ErtmOut::Send(self.i_frame(seq, sar, len, &payload, false)));
		}
		if !self.unacked.is_empty() {
			self.retransmission_at = Some(now_ms + u64::from(self.peer.retransmission_ms.max(1)));
		}
		Ok(out)
	}

	/// A TIMER CAME DUE: the retransmission timer polls the peer (an RR with the P bit) and waits on the monitor timer;
	/// the monitor timer polls again, until `max_transmit` polls went unanswered.
	pub fn tick(&mut self, now_ms: u64) -> Vec<ErtmOut> {
		let mut out = Vec::new();
		let due = |at: Option<u64>| at.is_some_and(|at| at <= now_ms);
		if due(self.retransmission_at) || due(self.monitor_at) {
			self.retransmission_at = None;
			self.polls = self.polls.saturating_add(1);
			if self.peer.max_transmit != 0 && self.polls > self.peer.max_transmit {
				out.push(ErtmOut::Fail);
				return out;
			}
			self.monitor_at = Some(now_ms + u64::from(self.peer.monitor_ms.max(1)));
			out.push(ErtmOut::Send(self.s_frame(Supervisory::ReceiverReady, true, false)));
		}
		out
	}

	/// ONE RECEIVED PDU on the channel.
	pub fn receive(&mut self, pdu: &[u8], now_ms: u64) -> Vec<ErtmOut> {
		let mut out = Vec::new();
		let (control, sdu_len, payload) = match parse_frame(pdu) {
			Ok(parsed) => parsed,
			// A FRAME WITH A BAD FCS IS DROPPED: the retransmission recovers it.
			Err(FrameRefusal::BadFcs) | Err(FrameRefusal::Short) => return out,
			Err(FrameRefusal::TooLong) => {
				out.push(ErtmOut::Fail);
				return out;
			}
		};
		match control {
			Control::Supervisory { kind, req_seq, p, f } => {
				if !self.acknowledge(req_seq, now_ms) {
					out.push(ErtmOut::Fail);
					return out;
				}
				self.remote_busy = kind == Supervisory::ReceiverNotReady;
				if f && self.monitor_at.is_some() {
					self.monitor_at = None;
					self.polls = 0;
					match self.retransmit_all(now_ms) {
						Ok(frames) => out.extend(frames),
						Err(()) => {
							out.push(ErtmOut::Fail);
							return out;
						}
					}
				} else if kind == Supervisory::Reject || kind == Supervisory::SelectiveReject {
					match self.retransmit_all(now_ms) {
						Ok(frames) => out.extend(frames),
						Err(()) => {
							out.push(ErtmOut::Fail);
							return out;
						}
					}
				}
				// A POLL IS ANSWERED with the final bit.
				if p {
					out.push(ErtmOut::Send(self.s_frame(Supervisory::ReceiverReady, false, true)));
				}
				out.extend(self.pump(now_ms));
			}
			Control::Information { tx_seq, req_seq, sar, f } => {
				if !self.acknowledge(req_seq, now_ms) {
					out.push(ErtmOut::Fail);
					return out;
				}
				if f && self.monitor_at.is_some() {
					self.monitor_at = None;
					self.polls = 0;
				}
				if tx_seq != self.expected_tx_seq {
					// OUT OF SEQUENCE: one reject asks for everything from the expected frame again; a duplicate of one
					// already delivered is dropped quietly.
					if ahead(self.expected_tx_seq, tx_seq) < 32 && !self.rejected {
						self.rejected = true;
						out.push(ErtmOut::Send(self.s_frame(Supervisory::Reject, false, false)));
					}
					return out;
				}
				self.rejected = false;
				self.expected_tx_seq = (self.expected_tx_seq + 1) & 0x3F;
				match sar {
					Sar::Unsegmented => {
						self.assembling = None;
						out.push(ErtmOut::Deliver(payload.to_vec()));
					}
					Sar::Start => {
						let total = usize::from(sdu_len.unwrap_or(0));
						if total > self.mtu || payload.len() > total {
							out.push(ErtmOut::Fail);
							return out;
						}
						self.assembling = Some((total, payload.to_vec()));
					}
					Sar::Continuation | Sar::End => {
						let Some((total, mut sdu)) = self.assembling.take() else {
							out.push(ErtmOut::Fail);
							return out;
						};
						if sdu.len() + payload.len() > total {
							out.push(ErtmOut::Fail);
							return out;
						}
						sdu.extend_from_slice(payload);
						if sar == Sar::End {
							if sdu.len() != total {
								out.push(ErtmOut::Fail);
								return out;
							}
							out.push(ErtmOut::Deliver(sdu));
						} else {
							self.assembling = Some((total, sdu));
						}
					}
				}
				// ACKNOWLEDGED BY THE NEXT FRAME THIS HOST SENDS, or by an RR once half the peer's window waits.
				self.unacked_received = self.unacked_received.saturating_add(1);
				out.extend(self.pump(now_ms));
				if self.unacked_received >= (ERTM_WINDOW as u8 / 2).max(1) {
					self.unacked_received = 0;
					out.push(ErtmOut::Send(self.s_frame(Supervisory::ReceiverReady, false, false)));
				}
			}
		}
		out
	}
}

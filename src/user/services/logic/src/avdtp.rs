//! AVDTP, THE STREAMING PROTOCOL UNDER A2DP: its signalling messages, the SBC codec information they carry, the choice
//! of one configuration from what a peer offers, and the media packets a stream sends - each a pure function of bytes.
//!
//! THE SIGNALLING CHANNEL is an L2CAP channel on PSM 0x19; every message is a header byte (transaction label, packet
//! type, message type), a signal identifier, and parameters. A configured stream then opens a SECOND channel on the same
//! PSM for its media: RTP packets, each an SBC payload header and whole SBC frames.
//!
//! ONE PACKET A MESSAGE. Fragmentation of signalling (start, continue, end) exists for messages longer than the
//! channel's MTU; nothing this profile sends comes near it, and a fragmented message from a peer is refused as not
//! understood rather than half-read.

use alloc::vec::Vec;

/// The PSM both AVDTP channels use.
pub const PSM: u16 = 0x0019;

/// Signal identifiers.
pub mod signal {
	pub const DISCOVER: u8 = 0x01;
	pub const GET_CAPABILITIES: u8 = 0x02;
	pub const SET_CONFIGURATION: u8 = 0x03;
	pub const GET_CONFIGURATION: u8 = 0x04;
	pub const RECONFIGURE: u8 = 0x05;
	pub const OPEN: u8 = 0x06;
	pub const START: u8 = 0x07;
	pub const CLOSE: u8 = 0x08;
	pub const SUSPEND: u8 = 0x09;
	pub const ABORT: u8 = 0x0a;
	pub const SECURITY_CONTROL: u8 = 0x0b;
	pub const GET_ALL_CAPABILITIES: u8 = 0x0c;
	pub const DELAY_REPORT: u8 = 0x0d;
}

/// Message types: the low two bits of the header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Command,
	GeneralReject,
	Accept,
	Reject,
}

/// Error codes a reject carries.
pub mod error {
	pub const BAD_HEADER_FORMAT: u8 = 0x01;
	pub const BAD_LENGTH: u8 = 0x11;
	pub const BAD_ACP_SEID: u8 = 0x12;
	pub const SEP_IN_USE: u8 = 0x13;
	pub const SEP_NOT_IN_USE: u8 = 0x14;
	pub const BAD_SERV_CATEGORY: u8 = 0x17;
	pub const BAD_PAYLOAD_FORMAT: u8 = 0x18;
	pub const NOT_SUPPORTED_COMMAND: u8 = 0x19;
	pub const INVALID_CAPABILITIES: u8 = 0x1a;
	pub const BAD_STATE: u8 = 0x31;
	pub const UNSUPPORTED_CONFIGURATION: u8 = 0x29;
}

/// Service categories in a capability or configuration list.
pub mod category {
	pub const MEDIA_TRANSPORT: u8 = 0x01;
	pub const MEDIA_CODEC: u8 = 0x07;
	pub const DELAY_REPORTING: u8 = 0x08;
}

/// ONE SIGNALLING MESSAGE: who it answers, what kind it is, which signal, and its parameters.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
	pub label: u8,
	pub kind: Kind,
	pub signal: u8,
	pub params: Vec<u8>,
}

impl Message {
	pub fn command(label: u8, signal: u8, params: &[u8]) -> Message {
		Message { label: label & 0x0f, kind: Kind::Command, signal, params: params.to_vec() }
	}

	pub fn accept(label: u8, signal: u8, params: &[u8]) -> Message {
		Message { label, kind: Kind::Accept, signal, params: params.to_vec() }
	}

	pub fn reject(label: u8, signal: u8, params: &[u8]) -> Message {
		Message { label, kind: Kind::Reject, signal, params: params.to_vec() }
	}

	pub fn encode(&self) -> Vec<u8> {
		let kind = match self.kind {
			Kind::Command => 0,
			Kind::GeneralReject => 1,
			Kind::Accept => 2,
			Kind::Reject => 3,
		};
		let mut out = Vec::with_capacity(2 + self.params.len());
		out.push((self.label << 4) | kind);
		out.push(self.signal & 0x3f);
		out.extend_from_slice(&self.params);
		out
	}

	/// One message, or `None` for one this side does not read: shorter than a header, or fragmented.
	pub fn decode(bytes: &[u8]) -> Option<Message> {
		let (&header, rest) = bytes.split_first()?;
		if (header >> 2) & 3 != 0 {
			return None;
		}
		let kind = match header & 3 {
			0 => Kind::Command,
			1 => Kind::GeneralReject,
			2 => Kind::Accept,
			_ => Kind::Reject,
		};
		// A GENERAL REJECT carries no signal byte in older peers; one that does is read the same.
		let (&signal, params) = match rest.split_first() {
			Some(found) => found,
			None if kind == Kind::GeneralReject => (&0u8, &[][..]),
			None => return None,
		};
		Some(Message { label: header >> 4, kind, signal: signal & 0x3f, params: params.to_vec() })
	}
}

// ------------------------------------------------------------------ stream end-points

/// The type of an end-point: what it does with the media.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
	Source,
	Sink,
}

/// ONE STREAM END-POINT, as a discovery answers it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Endpoint {
	pub seid: u8,
	pub in_use: bool,
	/// Audio is media type 0.
	pub media: u8,
	pub role: Role,
}

impl Endpoint {
	pub fn encode(&self) -> [u8; 2] {
		[(self.seid << 2) | (u8::from(self.in_use) << 1), (self.media << 4) | (u8::from(self.role == Role::Sink) << 3)]
	}
}

/// A discovery's answer: every end-point the peer has.
pub fn endpoints(params: &[u8]) -> Vec<Endpoint> {
	params.chunks_exact(2).map(|pair| Endpoint { seid: pair[0] >> 2, in_use: pair[0] & 2 != 0, media: pair[1] >> 4, role: if pair[1] & 8 != 0 { Role::Sink } else { Role::Source } }).collect()
}

// ------------------------------------------------------------------ SBC's codec information

/// SBC's codec type in the media codec category.
pub const CODEC_SBC: u8 = 0x00;

/// SBC'S CODEC INFORMATION: four bytes, each a set of flags in a capability and exactly one in a configuration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sbc {
	/// 16k 0x8, 32k 0x4, 44.1k 0x2, 48k 0x1.
	pub frequencies: u8,
	/// Mono 0x8, dual 0x4, stereo 0x2, joint 0x1.
	pub modes: u8,
	/// 4 0x8, 8 0x4, 12 0x2, 16 0x1.
	pub blocks: u8,
	/// 4 0x2, 8 0x1.
	pub subbands: u8,
	/// SNR 0x2, loudness 0x1.
	pub allocations: u8,
	pub min_bitpool: u8,
	pub max_bitpool: u8,
}

impl Sbc {
	/// WHAT THIS SYSTEM OFFERS: every frequency, mode, block length, subband count and allocation, bitpool 2 to 53 - the
	/// high-quality joint stereo the specification recommends at its top.
	pub const OFFERED: Sbc = Sbc { frequencies: 0xf, modes: 0xf, blocks: 0xf, subbands: 0x3, allocations: 0x3, min_bitpool: 2, max_bitpool: 53 };

	pub fn encode(&self) -> [u8; 4] {
		[(self.frequencies << 4) | self.modes, (self.blocks << 4) | (self.subbands << 2) | self.allocations, self.min_bitpool, self.max_bitpool]
	}

	pub fn decode(bytes: &[u8]) -> Option<Sbc> {
		let bytes: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
		Some(Sbc { frequencies: bytes[0] >> 4, modes: bytes[0] & 0xf, blocks: bytes[1] >> 4, subbands: (bytes[1] >> 2) & 3, allocations: bytes[1] & 3, min_bitpool: bytes[2], max_bitpool: bytes[3] })
	}

	/// ONE CONFIGURATION FROM WHAT BOTH SIDES SUPPORT: the highest frequency - 48 kHz before 44.1 - joint stereo before
	/// stereo, dual and mono, sixteen blocks, eight subbands, loudness, and the highest bitpool both allow up to 53.
	/// `None` when they share no value of some field.
	pub fn choose(&self, ours: &Sbc) -> Option<crate::sbc::Config> {
		let first = |set: u8, order: &[(u8, u32)]| order.iter().find(|(bit, _)| set & bit != 0).map(|(_, value)| *value);
		let frequency = first(self.frequencies & ours.frequencies, &[(0x1, 48_000), (0x2, 44_100), (0x4, 32_000), (0x8, 16_000)])?;
		let mode = match first(self.modes & ours.modes, &[(0x1, 3), (0x2, 2), (0x4, 1), (0x8, 0)])? {
			3 => crate::sbc::Mode::JointStereo,
			2 => crate::sbc::Mode::Stereo,
			1 => crate::sbc::Mode::DualChannel,
			_ => crate::sbc::Mode::Mono,
		};
		let blocks = first(self.blocks & ours.blocks, &[(0x1, 16), (0x2, 12), (0x4, 8), (0x8, 4)])? as u8;
		let subbands = first(self.subbands & ours.subbands, &[(0x1, 8), (0x2, 4)])? as u8;
		let allocation = if self.allocations & ours.allocations & 0x1 != 0 {
			crate::sbc::Allocation::Loudness
		} else if self.allocations & ours.allocations & 0x2 != 0 {
			crate::sbc::Allocation::Snr
		} else {
			return None;
		};
		let low = self.min_bitpool.max(ours.min_bitpool).max(2);
		let high = self.max_bitpool.min(ours.max_bitpool);
		if low > high {
			return None;
		}
		// THE BITPOOL THE MODE CAN CARRY: a single channel's is half the stereo one at the same quality.
		let ceiling = match mode {
			crate::sbc::Mode::Mono | crate::sbc::Mode::DualChannel => 32,
			_ => 53,
		};
		let bitpool = high.min(ceiling).max(low);
		let config = crate::sbc::Config { frequency, blocks, mode, allocation, subbands, bitpool };
		config.valid().then_some(config)
	}

	/// The one configuration a set of single flags names, as a configuration message carries it.
	pub fn config(&self) -> Option<crate::sbc::Config> {
		let single = |set: u8| set.count_ones() == 1;
		if !(single(self.frequencies) && single(self.modes) && single(self.blocks) && single(self.subbands) && single(self.allocations)) {
			return None;
		}
		let chosen = self.choose(self)?;
		// A configuration states its bitpool range; the stream is coded at its top.
		let config = crate::sbc::Config { bitpool: self.max_bitpool, ..chosen };
		config.valid().then_some(config)
	}

	/// THE SINGLE FLAGS a configuration message carries for `config`.
	pub fn of(config: &crate::sbc::Config) -> Sbc {
		let frequencies = match config.frequency {
			16_000 => 0x8,
			32_000 => 0x4,
			44_100 => 0x2,
			_ => 0x1,
		};
		let modes = match config.mode {
			crate::sbc::Mode::Mono => 0x8,
			crate::sbc::Mode::DualChannel => 0x4,
			crate::sbc::Mode::Stereo => 0x2,
			crate::sbc::Mode::JointStereo => 0x1,
		};
		let blocks = match config.blocks {
			4 => 0x8,
			8 => 0x4,
			12 => 0x2,
			_ => 0x1,
		};
		let subbands = if config.subbands == 4 { 0x2 } else { 0x1 };
		let allocations = if config.allocation == crate::sbc::Allocation::Snr { 0x2 } else { 0x1 };
		Sbc { frequencies, modes, blocks, subbands, allocations, min_bitpool: config.bitpool.min(53).max(2), max_bitpool: config.bitpool }
	}
}

// ------------------------------------------------------------------ capability lists

/// What a capability or configuration list says: the SBC codec information if it carries SBC, and whether delay
/// reporting is in it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Capabilities {
	pub transport: bool,
	pub sbc: Option<Sbc>,
	pub other_codec: bool,
	pub delay_reporting: bool,
}

/// Read a list of service categories. `None` for one that runs past its own length.
pub fn capabilities(mut bytes: &[u8]) -> Option<Capabilities> {
	let mut out = Capabilities::default();
	while !bytes.is_empty() {
		let category = *bytes.first()?;
		let length = usize::from(*bytes.get(1)?);
		let value = bytes.get(2..2 + length)?;
		match category {
			category::MEDIA_TRANSPORT => out.transport = true,
			category::DELAY_REPORTING => out.delay_reporting = true,
			category::MEDIA_CODEC if value.len() >= 2 => {
				if value[0] >> 4 == 0 && value[1] == CODEC_SBC {
					out.sbc = Sbc::decode(&value[2..]);
				} else {
					out.other_codec = true;
				}
			}
			_ => {}
		}
		bytes = &bytes[2 + length..];
	}
	Some(out)
}

/// Write a list: the media transport, SBC's information, and delay reporting where asked.
pub fn capability_list(sbc: &Sbc, delay_reporting: bool) -> Vec<u8> {
	let mut out = alloc::vec![category::MEDIA_TRANSPORT, 0, category::MEDIA_CODEC, 6, 0x00, CODEC_SBC];
	out.extend_from_slice(&sbc.encode());
	if delay_reporting {
		out.extend_from_slice(&[category::DELAY_REPORTING, 0]);
	}
	out
}

// ------------------------------------------------------------------ media packets

/// The RTP payload type A2DP's media packets carry - one of the dynamic ones.
pub const PAYLOAD_TYPE: u8 = 96;
/// An RTP header's length, with no contributing sources or extension.
pub const RTP_HEADER: usize = 12;

/// ONE MEDIA PACKET: an RTP header, the SBC payload header saying how many frames follow, and the frames.
pub fn media_packet(sequence: u16, timestamp: u32, ssrc: u32, frames: u8, payload: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(RTP_HEADER + 1 + payload.len());
	out.extend_from_slice(&[0x80, PAYLOAD_TYPE & 0x7f]);
	out.extend_from_slice(&sequence.to_be_bytes());
	out.extend_from_slice(&timestamp.to_be_bytes());
	out.extend_from_slice(&ssrc.to_be_bytes());
	out.push(frames & 0x0f);
	out.extend_from_slice(payload);
	out
}

/// A received media packet: its sequence number, timestamp, frame count and the SBC frames. `None` for one that is
/// not RTP version 2, is fragmented, or is shorter than its headers.
pub fn read_media(packet: &[u8]) -> Option<(u16, u32, u8, &[u8])> {
	if packet.len() < RTP_HEADER + 1 || packet[0] >> 6 != 2 {
		return None;
	}
	let contributors = usize::from(packet[0] & 0x0f);
	let extension = packet[0] & 0x10 != 0;
	let mut at = RTP_HEADER + 4 * contributors;
	if extension {
		let length = usize::from(u16::from_be_bytes([*packet.get(at + 2)?, *packet.get(at + 3)?]));
		at += 4 + 4 * length;
	}
	let header = *packet.get(at)?;
	// A FRAGMENTED SBC FRAME is not one this side reassembles.
	if header & 0x80 != 0 {
		return None;
	}
	let sequence = u16::from_be_bytes([packet[2], packet[3]]);
	let timestamp = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
	Some((sequence, timestamp, header & 0x0f, &packet[at + 1..]))
}

/// HOW MANY WHOLE FRAMES OF `frame_length` FIT IN ONE PACKET within `mtu`: at most fifteen, the payload header's limit.
pub fn frames_per_packet(mtu: usize, frame_length: usize) -> usize {
	(mtu.saturating_sub(RTP_HEADER + 1) / frame_length.max(1)).clamp(1, 15)
}

#[cfg(test)]
mod tests;

//! RFCOMM - the serial ports HFP, HSP, SPP and OBEX run over, on one L2CAP channel per link: its frames with their
//! FCS, the multiplexer's control commands on DLCI 0, and the session that opens, credits and closes the DLCs.
//!
//! WHO IS WHO. The side that opened the multiplexer (the SABM on DLCI 0) is the INITIATOR. A DLCI is a server channel
//! and a direction bit: a server channel on the responder is reached on DLCI 2N, one on the initiator on 2N + 1. The
//! address's C/R bit says command or response together with who sends it - a command from the initiator has it set,
//! one from the responder clear, and responses the other way round.
//!
//! CREDIT-BASED FLOW CONTROL ONLY. Every DLC is negotiated with the convergence layer that carries credits (the
//! parameter negotiation's 0xF0, answered 0xE0), and data leaves only against credits the peer granted; a peer that
//! does not take credit-based flow control is refused, since the old flow control commands stop a whole session.
//!
//! THE BOUNDS ARE `bt_bounds`' AND THE FRAME'S: at most `RFCOMM_CHANNELS_PER_LINK` DLCs, a frame size no larger than
//! the L2CAP MTU allows, and a credit count of at most 255 outstanding either way.

use crate::bt_bounds::{BREDR_MTU, RFCOMM_CHANNELS_PER_LINK};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

/// The largest information field this side offers: the L2CAP MTU less the frame's header and FCS.
pub const MAX_FRAME: usize = BREDR_MTU - 6;
/// The default frame size before negotiation.
pub const DEFAULT_FRAME: usize = 127;
/// The credits this side grants a DLC when it opens, and again whenever the peer's run low.
pub const INITIAL_CREDITS: u8 = 7;

// ----------------------------------------------------------------------------------------------------------- the FCS

const fn crc_table() -> [u8; 256] {
	let mut table = [0u8; 256];
	let mut n = 0;
	while n < 256 {
		let mut crc = n as u8;
		let mut bit = 0;
		while bit < 8 {
			crc = if crc & 1 != 0 { (crc >> 1) ^ 0xE0 } else { crc >> 1 };
			bit += 1;
		}
		table[n] = crc;
		n += 1;
	}
	table
}

const CRC: [u8; 256] = crc_table();

/// THE FCS: the reflected CRC-8 of x^8 + x^2 + x + 1 from all ones, complemented - over the address and control fields
/// of a UIH frame, and the length too for every other frame.
pub fn fcs(bytes: &[u8]) -> u8 {
	let mut crc: u8 = 0xFF;
	for &byte in bytes {
		crc = CRC[(crc ^ byte) as usize];
	}
	0xFF - crc
}

// ----------------------------------------------------------------------------------------------------------- frames

/// A frame's type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	/// Set asynchronous balanced mode: open a DLC (or the multiplexer on DLCI 0).
	Sabm,
	/// Unnumbered acknowledgement.
	Ua,
	/// Disconnected mode: a refusal, or "no such DLC".
	Dm,
	Disc,
	/// Unnumbered information with header check: data, and the multiplexer's commands on DLCI 0.
	Uih,
}

impl Kind {
	const fn control(self) -> u8 {
		match self {
			Kind::Sabm => 0x2F,
			Kind::Ua => 0x63,
			Kind::Dm => 0x0F,
			Kind::Disc => 0x43,
			Kind::Uih => 0xEF,
		}
	}

	fn from_control(control: u8) -> Option<Kind> {
		match control & !0x10 {
			0x2F => Some(Kind::Sabm),
			0x63 => Some(Kind::Ua),
			0x0F => Some(Kind::Dm),
			0x43 => Some(Kind::Disc),
			0xEF => Some(Kind::Uih),
			_ => None,
		}
	}
}

/// One frame.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
	pub dlci: u8,
	/// The address's C/R bit as sent.
	pub cr: bool,
	pub kind: Kind,
	/// The poll/final bit; on a UIH frame of a DLC with credits, it says a credit byte follows the length.
	pub pf: bool,
	pub credits: Option<u8>,
	pub info: Vec<u8>,
}

/// Why a frame was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameRefusal {
	Short,
	/// The address's or the length's extension bit says more follows than this reader takes.
	Extension,
	UnknownControl(u8),
	BadFcs,
	/// The length disagrees with the frame, or is past the frame size.
	Length,
}

pub fn encode(frame: &Frame) -> Vec<u8> {
	let mut out = Vec::with_capacity(frame.info.len() + 6);
	out.push((frame.dlci << 2) | (u8::from(frame.cr) << 1) | 1);
	out.push(frame.kind.control() | if frame.pf { 0x10 } else { 0 });
	let len = frame.info.len();
	if len <= 127 {
		out.push(((len as u8) << 1) | 1);
	} else {
		out.push((len as u8) << 1);
		out.push((len >> 7) as u8);
	}
	let check = if frame.kind == Kind::Uih { fcs(&out[..2]) } else { fcs(&out) };
	if let Some(credits) = frame.credits {
		out.push(credits);
	}
	out.extend_from_slice(&frame.info);
	out.push(check);
	out
}

/// ONE FRAME, from an L2CAP SDU. `credit_flow` says whether a UIH frame with the P/F bit carries a credit byte - true
/// on a DLC with credit-based flow control, never on DLCI 0.
pub fn decode(bytes: &[u8], credit_flow: impl Fn(u8) -> bool, max_info: usize) -> Result<Frame, FrameRefusal> {
	if bytes.len() < 4 {
		return Err(FrameRefusal::Short);
	}
	let address = bytes[0];
	if address & 1 == 0 {
		return Err(FrameRefusal::Extension);
	}
	let control = bytes[1];
	let kind = Kind::from_control(control).ok_or(FrameRefusal::UnknownControl(control))?;
	let (len, header) = if bytes[2] & 1 == 1 { ((bytes[2] >> 1) as usize, 3) } else { ((bytes[2] >> 1) as usize | (bytes[3] as usize) << 7, 4) };
	let dlci = address >> 2;
	let pf = control & 0x10 != 0;
	let has_credits = kind == Kind::Uih && pf && dlci != 0 && credit_flow(dlci);
	let check_at = bytes.len() - 1;
	let expected = if kind == Kind::Uih { fcs(&bytes[..2]) } else { fcs(&bytes[..header]) };
	if bytes[check_at] != expected {
		return Err(FrameRefusal::BadFcs);
	}
	let body_start = header + usize::from(has_credits);
	if body_start + len != check_at || len > max_info {
		return Err(FrameRefusal::Length);
	}
	Ok(Frame { dlci, cr: address & 2 != 0, kind, pf, credits: has_credits.then(|| bytes[header]), info: bytes[body_start..check_at].to_vec() })
}

// --------------------------------------------------------------------------------------------- multiplexer commands

/// The multiplexer's control commands this stack sends and answers.
pub mod mcc {
	pub const PN: u8 = 0x20;
	pub const TEST: u8 = 0x08;
	pub const FCON: u8 = 0x28;
	pub const FCOFF: u8 = 0x18;
	pub const MSC: u8 = 0x38;
	pub const NSC: u8 = 0x04;
	pub const RPN: u8 = 0x24;
	pub const RLS: u8 = 0x14;
}

/// The convergence layer a parameter negotiation asks for (credits) and the one its answer accepts them with.
pub const CL_CREDITS_REQUEST: u8 = 0xF0;
pub const CL_CREDITS_ACCEPT: u8 = 0xE0;

/// The V.24 signals a modem status command carries: ready to communicate, ready to receive, data valid - what an
/// open serial port says.
pub const V24_READY: u8 = 0x01 | 0x04 | 0x08 | 0x80;

/// One multiplexer control command.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
	/// Parameter negotiation: the DLCI, the convergence layer, the priority, the frame size and the initial credits.
	Pn {
		dlci: u8,
		cl: u8,
		priority: u8,
		frame: u16,
		credits: u8,
	},
	/// Modem status: the DLCI and its V.24 signals.
	Msc {
		dlci: u8,
		signals: u8,
	},
	/// Remote port negotiation: the DLCI and, when the command sets values, the seven bytes after it.
	Rpn {
		dlci: u8,
		values: Option<[u8; 7]>,
	},
	/// Remote line status.
	Rls {
		dlci: u8,
		status: u8,
	},
	Test(Vec<u8>),
	FlowOn,
	FlowOff,
	/// Non-supported command: the type this side did not understand.
	Nsc(u8),
	/// A type this stack does not know, answered with NSC.
	Unknown(u8),
}

/// A command and whether it is one (the type field's C/R bit) rather than a response.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Mcc {
	pub command: bool,
	pub body: Command,
}

pub fn encode_mcc(mcc: &Mcc) -> Vec<u8> {
	let mut value: Vec<u8> = Vec::new();
	let kind = match &mcc.body {
		Command::Pn { dlci, cl, priority, frame, credits } => {
			value.extend_from_slice(&[*dlci & 0x3F, *cl, *priority & 0x3F, 0]);
			value.extend_from_slice(&frame.to_le_bytes());
			value.extend_from_slice(&[0, *credits & 0x07]);
			mcc::PN
		}
		Command::Msc { dlci, signals } => {
			value.extend_from_slice(&[(*dlci << 2) | 0x03, *signals | 1]);
			mcc::MSC
		}
		Command::Rpn { dlci, values } => {
			value.push((*dlci << 2) | 0x03);
			if let Some(values) = values {
				value.extend_from_slice(values);
			}
			mcc::RPN
		}
		Command::Rls { dlci, status } => {
			value.extend_from_slice(&[(*dlci << 2) | 0x03, *status]);
			mcc::RLS
		}
		Command::Test(bytes) => {
			value.extend_from_slice(bytes);
			mcc::TEST
		}
		Command::FlowOn => mcc::FCON,
		Command::FlowOff => mcc::FCOFF,
		Command::Nsc(kind) => {
			value.push(*kind);
			mcc::NSC
		}
		Command::Unknown(kind) => *kind,
	};
	let mut out = Vec::with_capacity(value.len() + 2);
	out.push((kind << 2) | (u8::from(mcc.command) << 1) | 1);
	out.push(((value.len() as u8) << 1) | 1);
	out.extend_from_slice(&value);
	out
}

/// ONE MULTIPLEXER COMMAND from a UIH frame's information on DLCI 0.
pub fn decode_mcc(bytes: &[u8]) -> Option<Mcc> {
	if bytes.len() < 2 || bytes[0] & 1 == 0 || bytes[1] & 1 == 0 {
		return None;
	}
	let kind = bytes[0] >> 2;
	let command = bytes[0] & 2 != 0;
	let len = (bytes[1] >> 1) as usize;
	let value = bytes.get(2..2 + len)?;
	let body = match kind {
		mcc::PN if len == 8 => Command::Pn { dlci: value[0] & 0x3F, cl: value[1], priority: value[2] & 0x3F, frame: u16::from_le_bytes([value[4], value[5]]), credits: value[7] & 0x07 },
		mcc::MSC if len >= 2 => Command::Msc { dlci: value[0] >> 2, signals: value[1] },
		mcc::RPN if len == 1 => Command::Rpn { dlci: value[0] >> 2, values: None },
		mcc::RPN if len == 8 => Command::Rpn { dlci: value[0] >> 2, values: Some([value[1], value[2], value[3], value[4], value[5], value[6], value[7]]) },
		mcc::RLS if len == 2 => Command::Rls { dlci: value[0] >> 2, status: value[1] },
		mcc::TEST => Command::Test(value.to_vec()),
		mcc::FCON => Command::FlowOn,
		mcc::FCOFF => Command::FlowOff,
		mcc::NSC if len == 1 => Command::Nsc(value[0]),
		other => Command::Unknown(other),
	};
	Some(Mcc { command, body })
}

// ------------------------------------------------------------------------------------------------------------ session

/// Where a DLC is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DlcState {
	/// This side asked for its parameters and waits for the answer.
	WaitPn,
	/// This side sent its SABM and waits for the UA.
	WaitUa,
	Open,
	WaitDisc,
}

/// One data link connection.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Dlc {
	pub dlci: u8,
	pub state: DlcState,
	/// The frame size both sides agreed.
	pub frame: usize,
	/// Credits the peer granted this side: frames it may send.
	pub tx_credits: u16,
	/// Credits this side granted the peer and it has not used.
	pub rx_credits: u16,
	/// Whether each side's modem status has been said.
	pub msc_sent: bool,
	pub msc_received: bool,
	/// Data waiting for credits.
	pub queue: VecDeque<Vec<u8>>,
}

/// What the session asks the service to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Out {
	/// Send this frame on the RFCOMM L2CAP channel.
	Send(Vec<u8>),
	/// A DLC is open both ways - its modem status said in both directions: its server channel.
	Opened(u8),
	/// Data arrived on a DLC.
	Data(u8, Vec<u8>),
	/// A DLC this side asked for was refused, or one is closed: its server channel.
	Closed(u8),
	/// The multiplexer is open; or closed, with every DLC.
	SessionOpen,
	SessionClosed,
}

/// Whether this side serves an inbound DLC on a server channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admission {
	Accept,
	Refuse,
}

/// ONE RFCOMM SESSION over one L2CAP channel.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Session {
	initiator: bool,
	mux_open: bool,
	mux_waiting: bool,
	dlcs: Vec<Dlc>,
	// DLCs asked for before the multiplexer was open, by server channel.
	waiting: Vec<u8>,
	l2cap_mtu: usize,
}

impl Session {
	/// A session this side opens (`initiator`) or answers, over an L2CAP channel whose peer MTU is `l2cap_mtu`.
	pub fn new(initiator: bool, l2cap_mtu: usize) -> Session {
		Session { initiator, mux_open: false, mux_waiting: false, dlcs: Vec::new(), waiting: Vec::new(), l2cap_mtu }
	}

	pub fn is_open(&self) -> bool {
		self.mux_open
	}

	pub fn dlc(&self, channel: u8) -> Option<&Dlc> {
		self.dlcs.iter().find(|dlc| dlc.dlci >> 1 == channel)
	}

	/// The DLCI a server channel is reached on from this side: the peer's channels are 2N when this side initiated.
	pub fn outbound_dlci(&self, channel: u8) -> u8 {
		(channel << 1) | u8::from(!self.initiator)
	}

	// A command's C/R bit from this side, and a response's.
	fn cr_command(&self) -> bool {
		self.initiator
	}

	fn cr_response(&self) -> bool {
		!self.initiator
	}

	fn frame_size(&self) -> usize {
		MAX_FRAME.min(self.l2cap_mtu.saturating_sub(6)).max(1)
	}

	fn send(&self, dlci: u8, kind: Kind, command: bool, pf: bool, credits: Option<u8>, info: Vec<u8>) -> Out {
		Out::Send(encode(&Frame { dlci, cr: if command { self.cr_command() } else { self.cr_response() }, kind, pf, credits, info }))
	}

	fn send_mcc(&self, mcc: Mcc) -> Out {
		// MULTIPLEXER COMMANDS ride UIH frames on DLCI 0 as commands, whichever side sends them.
		self.send(0, Kind::Uih, true, false, None, encode_mcc(&mcc))
	}

	/// OPEN THE DLC to a server channel on the peer: the multiplexer first where it is not open, then the parameter
	/// negotiation. `None` when the session holds as many DLCs as it may.
	pub fn connect(&mut self, channel: u8) -> Option<Vec<Out>> {
		if self.dlcs.len() + self.waiting.len() >= RFCOMM_CHANNELS_PER_LINK || !(1..=30).contains(&channel) {
			return None;
		}
		let mut out = Vec::new();
		if !self.mux_open {
			self.waiting.push(channel);
			if !self.mux_waiting {
				self.mux_waiting = true;
				out.push(self.send(0, Kind::Sabm, true, true, None, Vec::new()));
			}
			return Some(out);
		}
		out.push(self.negotiate(channel));
		Some(out)
	}

	fn negotiate(&mut self, channel: u8) -> Out {
		let dlci = self.outbound_dlci(channel);
		let frame = self.frame_size();
		self.dlcs.push(Dlc { dlci, state: DlcState::WaitPn, frame, tx_credits: 0, rx_credits: u16::from(INITIAL_CREDITS), msc_sent: false, msc_received: false, queue: VecDeque::new() });
		self.send_mcc(Mcc { command: true, body: Command::Pn { dlci, cl: CL_CREDITS_REQUEST, priority: 7, frame: frame as u16, credits: INITIAL_CREDITS } })
	}

	/// CLOSE A DLC, or the whole session when none is left open.
	pub fn disconnect(&mut self, channel: u8) -> Vec<Out> {
		let mut out = Vec::new();
		if let Some(dlc) = self.dlcs.iter_mut().find(|dlc| dlc.dlci >> 1 == channel) {
			dlc.state = DlcState::WaitDisc;
			let dlci = dlc.dlci;
			out.push(self.send(dlci, Kind::Disc, true, true, None, Vec::new()));
		}
		out
	}

	/// SEND DATA on an open DLC: as many frames as its credits allow, the rest queued; frames no larger than the
	/// agreed size. False for a DLC that is not open.
	pub fn write(&mut self, channel: u8, data: &[u8]) -> Option<Vec<Out>> {
		let at = self.dlcs.iter().position(|dlc| dlc.dlci >> 1 == channel && dlc.state == DlcState::Open)?;
		let frame = self.dlcs[at].frame.max(1);
		for chunk in data.chunks(frame) {
			self.dlcs[at].queue.push_back(chunk.to_vec());
		}
		Some(self.drain(at))
	}

	fn drain(&mut self, at: usize) -> Vec<Out> {
		let mut out = Vec::new();
		while self.dlcs[at].tx_credits > 0 {
			let Some(chunk) = self.dlcs[at].queue.pop_front() else { break };
			self.dlcs[at].tx_credits -= 1;
			let dlci = self.dlcs[at].dlci;
			// THE CREDITS GO BACK WITH DATA where the peer's are low.
			let grant = self.top_up(at);
			out.push(self.send(dlci, Kind::Uih, true, grant.is_some(), grant, chunk));
		}
		out
	}

	// Credits to grant the peer now, when it holds fewer than half of what this side gives.
	fn top_up(&mut self, at: usize) -> Option<u8> {
		let dlc = &mut self.dlcs[at];
		if dlc.rx_credits < u16::from(INITIAL_CREDITS) / 2 + 1 {
			let grant = u16::from(INITIAL_CREDITS) - dlc.rx_credits;
			dlc.rx_credits += grant;
			return Some(grant as u8);
		}
		None
	}

	/// The session's L2CAP channel went: every DLC with it.
	pub fn drop_all(&mut self) -> Vec<Out> {
		let mut out: Vec<Out> = self.dlcs.drain(..).filter(|dlc| dlc.state == DlcState::Open).map(|dlc| Out::Closed(dlc.dlci >> 1)).collect();
		self.mux_open = false;
		out.push(Out::SessionClosed);
		out
	}

	/// ONE FRAME FROM THE PEER. `admit` decides an inbound DLC by server channel.
	pub fn receive(&mut self, bytes: &[u8], admit: &mut dyn FnMut(u8) -> Admission) -> Vec<Out> {
		let mut out = Vec::new();
		let open: Vec<u8> = self.dlcs.iter().filter(|dlc| dlc.state == DlcState::Open).map(|dlc| dlc.dlci).collect();
		let frame = match decode(bytes, |dlci| open.contains(&dlci), self.frame_size()) {
			Ok(frame) => frame,
			// A FRAME WITH A BAD FCS, OR NO FRAME AT ALL, IS DROPPED: RFCOMM has no retransmission of its own, and L2CAP's
			// reliable link does not corrupt.
			Err(_) => return out,
		};
		match (frame.dlci, frame.kind) {
			(0, Kind::Sabm) => {
				self.mux_open = true;
				out.push(self.send(0, Kind::Ua, false, true, None, Vec::new()));
				out.push(Out::SessionOpen);
			}
			(0, Kind::Ua) if self.mux_waiting => {
				self.mux_waiting = false;
				self.mux_open = true;
				out.push(Out::SessionOpen);
				for channel in core::mem::take(&mut self.waiting) {
					out.push(self.negotiate(channel));
				}
			}
			(0, Kind::Dm) => {
				self.mux_waiting = false;
				for channel in core::mem::take(&mut self.waiting) {
					out.push(Out::Closed(channel));
				}
				out.push(Out::SessionClosed);
			}
			(0, Kind::Disc) => {
				out.push(self.send(0, Kind::Ua, false, true, None, Vec::new()));
				out.extend(self.drop_all());
			}
			(0, Kind::Uih) => {
				if let Some(mcc) = decode_mcc(&frame.info) {
					out.extend(self.multiplexer(mcc, admit));
				}
			}
			(dlci, Kind::Sabm) => {
				// AN INBOUND DLC: negotiated with credits first, admitted by its channel.
				let negotiated = self.dlcs.iter().position(|dlc| dlc.dlci == dlci && dlc.state == DlcState::WaitUa && !self.is_outbound(dlci));
				match negotiated {
					Some(at) if admit(dlci >> 1) == Admission::Accept => {
						self.dlcs[at].state = DlcState::Open;
						out.push(self.send(dlci, Kind::Ua, false, true, None, Vec::new()));
						out.push(self.send_mcc(Mcc { command: true, body: Command::Msc { dlci, signals: V24_READY } }));
						self.dlcs[at].msc_sent = true;
					}
					_ => {
						if let Some(at) = self.dlcs.iter().position(|dlc| dlc.dlci == dlci) {
							self.dlcs.remove(at);
						}
						out.push(self.send(dlci, Kind::Dm, false, true, None, Vec::new()));
					}
				}
			}
			(dlci, Kind::Ua) => {
				if let Some(at) = self.dlcs.iter().position(|dlc| dlc.dlci == dlci) {
					match self.dlcs[at].state {
						DlcState::WaitUa => {
							self.dlcs[at].state = DlcState::Open;
							self.dlcs[at].msc_sent = true;
							out.push(self.send_mcc(Mcc { command: true, body: Command::Msc { dlci, signals: V24_READY } }));
						}
						DlcState::WaitDisc => {
							self.dlcs.remove(at);
							out.push(Out::Closed(dlci >> 1));
						}
						_ => {}
					}
				}
			}
			(dlci, Kind::Dm) => {
				if let Some(at) = self.dlcs.iter().position(|dlc| dlc.dlci == dlci) {
					self.dlcs.remove(at);
					out.push(Out::Closed(dlci >> 1));
				}
			}
			(dlci, Kind::Disc) => {
				out.push(self.send(dlci, Kind::Ua, false, true, None, Vec::new()));
				if let Some(at) = self.dlcs.iter().position(|dlc| dlc.dlci == dlci) {
					let was_open = self.dlcs[at].state == DlcState::Open;
					self.dlcs.remove(at);
					if was_open {
						out.push(Out::Closed(dlci >> 1));
					}
				}
			}
			(dlci, Kind::Uih) => {
				let Some(at) = self.dlcs.iter().position(|dlc| dlc.dlci == dlci && dlc.state == DlcState::Open) else {
					out.push(self.send(dlci, Kind::Dm, false, true, None, Vec::new()));
					return out;
				};
				if let Some(granted) = frame.credits {
					self.dlcs[at].tx_credits = (self.dlcs[at].tx_credits + u16::from(granted)).min(255);
				}
				if !frame.info.is_empty() {
					// DATA PAST THE CREDITS THIS SIDE GAVE is a peer that ignores flow control: dropped.
					if self.dlcs[at].rx_credits == 0 {
						return out;
					}
					self.dlcs[at].rx_credits -= 1;
					out.push(Out::Data(dlci >> 1, frame.info));
					if let Some(grant) = self.top_up(at) {
						out.push(self.send(dlci, Kind::Uih, true, true, Some(grant), Vec::new()));
					}
				}
				out.extend(self.drain(at));
			}
		}
		out
	}

	fn is_outbound(&self, dlci: u8) -> bool {
		dlci & 1 == u8::from(!self.initiator)
	}

	fn opened(&self, at: usize) -> bool {
		let dlc = &self.dlcs[at];
		dlc.state == DlcState::Open && dlc.msc_sent && dlc.msc_received
	}

	fn multiplexer(&mut self, mcc: Mcc, admit: &mut dyn FnMut(u8) -> Admission) -> Vec<Out> {
		let mut out = Vec::new();
		match mcc.body {
			Command::Pn { dlci, cl, frame, credits, priority } if mcc.command => {
				// THE PEER NEGOTIATES AN INBOUND DLC. Credits are required; the frame size is the smaller of the two.
				if cl != CL_CREDITS_REQUEST || self.dlcs.len() >= RFCOMM_CHANNELS_PER_LINK || admit(dlci >> 1) == Admission::Refuse {
					out.push(self.send(dlci, Kind::Dm, false, true, None, Vec::new()));
					return out;
				}
				let agreed = usize::from(frame).min(self.frame_size()).max(1);
				match self.dlcs.iter().position(|dlc| dlc.dlci == dlci) {
					Some(at) => {
						self.dlcs[at].frame = agreed;
						self.dlcs[at].tx_credits = u16::from(credits);
					}
					None => self.dlcs.push(Dlc { dlci, state: DlcState::WaitUa, frame: agreed, tx_credits: u16::from(credits), rx_credits: u16::from(INITIAL_CREDITS), msc_sent: false, msc_received: false, queue: VecDeque::new() }),
				}
				out.push(self.send_mcc(Mcc { command: false, body: Command::Pn { dlci, cl: CL_CREDITS_ACCEPT, priority, frame: agreed as u16, credits: INITIAL_CREDITS } }));
			}
			Command::Pn { dlci, cl, frame, credits, .. } => {
				// THE ANSWER TO THIS SIDE'S NEGOTIATION: credits accepted, then the SABM.
				let Some(at) = self.dlcs.iter().position(|dlc| dlc.dlci == dlci && dlc.state == DlcState::WaitPn) else { return out };
				if cl != CL_CREDITS_ACCEPT || frame == 0 {
					self.dlcs.remove(at);
					out.push(Out::Closed(dlci >> 1));
					return out;
				}
				self.dlcs[at].frame = usize::from(frame).min(self.dlcs[at].frame);
				self.dlcs[at].tx_credits = u16::from(credits);
				self.dlcs[at].state = DlcState::WaitUa;
				out.push(self.send(dlci, Kind::Sabm, true, true, None, Vec::new()));
			}
			Command::Msc { dlci, signals } if mcc.command => {
				out.push(self.send_mcc(Mcc { command: false, body: Command::Msc { dlci, signals } }));
				if let Some(at) = self.dlcs.iter().position(|dlc| dlc.dlci == dlci) {
					let was = self.opened(at);
					self.dlcs[at].msc_received = true;
					if !self.dlcs[at].msc_sent && self.dlcs[at].state == DlcState::Open {
						self.dlcs[at].msc_sent = true;
						out.push(self.send_mcc(Mcc { command: true, body: Command::Msc { dlci, signals: V24_READY } }));
					}
					if !was && self.opened(at) {
						out.push(Out::Opened(dlci >> 1));
						out.extend(self.drain(at));
					}
				}
			}
			Command::Msc { .. } => {}
			Command::Rpn { dlci, values } if mcc.command => {
				// THE PORT SETTINGS: what a serial adapter would apply, a DLC here carries bytes and agrees to them.
				out.push(self.send_mcc(Mcc { command: false, body: Command::Rpn { dlci, values: Some(values.unwrap_or([0x03, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00])) } }));
			}
			Command::Rls { dlci, status } if mcc.command => out.push(self.send_mcc(Mcc { command: false, body: Command::Rls { dlci, status } })),
			Command::Test(bytes) if mcc.command => out.push(self.send_mcc(Mcc { command: false, body: Command::Test(bytes) })),
			Command::FlowOn | Command::FlowOff if mcc.command => {
				let body = mcc.body.clone();
				out.push(self.send_mcc(Mcc { command: false, body }));
			}
			Command::Unknown(kind) if mcc.command => out.push(self.send_mcc(Mcc { command: false, body: Command::Nsc(kind) })),
			_ => {}
		}
		out
	}
}

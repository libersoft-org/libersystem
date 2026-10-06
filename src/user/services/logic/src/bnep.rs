//! BNEP, THE PAN USER'S HALF: the connection setup a PANU asks of a phone's network access point, and Ethernet frames
//! carried both ways in BNEP's general and compressed forms.
//!
//! WHAT IS HERE. The setup request names the NAP as the destination service and the PANU as the source; the access
//! point's answer opens the link or refuses it. Afterwards every frame this host sends goes in the most compressed form
//! the addresses allow - both addresses the link's own, only one, or neither - and every frame that arrives, in any of
//! the four, is handed on as the whole Ethernet frame NetworkService's stack reads. Extension headers are skipped; the
//! access point's filter requests are answered "unsupported request", as a PANU that filters nothing may; a control
//! message this side does not know is answered "command not understood".
//!
//! BOUNDS. A frame's payload is at most 1500 bytes - the Ethernet MTU BNEP carries - and anything that runs past its
//! own packet is refused.

use alloc::vec::Vec;

/// The Ethernet payload BNEP carries, and the L2CAP MTU its channel needs for it.
pub const MTU: usize = 1500;
pub const L2CAP_MTU: u16 = 1691;
/// The PAN service classes: the user's and the network access point's.
pub const PANU: u16 = 0x1115;
pub const NAP: u16 = 0x1116;

pub mod kind {
	pub const GENERAL: u8 = 0x00;
	pub const CONTROL: u8 = 0x01;
	pub const COMPRESSED: u8 = 0x02;
	pub const COMPRESSED_SOURCE_ONLY: u8 = 0x03;
	pub const COMPRESSED_DESTINATION_ONLY: u8 = 0x04;
}

pub mod control {
	pub const NOT_UNDERSTOOD: u8 = 0x00;
	pub const SETUP_REQUEST: u8 = 0x01;
	pub const SETUP_RESPONSE: u8 = 0x02;
	pub const FILTER_NET_TYPE_SET: u8 = 0x03;
	pub const FILTER_NET_TYPE_RESPONSE: u8 = 0x04;
	pub const FILTER_MULTI_ADDR_SET: u8 = 0x05;
	pub const FILTER_MULTI_ADDR_RESPONSE: u8 = 0x06;
}

/// A setup response's success, and the filter responses' "unsupported request".
pub const SETUP_SUCCESS: u16 = 0x0000;
pub const FILTER_UNSUPPORTED: u16 = 0x0001;

/// Where the link is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
	/// The setup request is out.
	Setting,
	Open,
	/// The access point refused the setup with this code, or broke the protocol.
	Refused(u16),
}

/// What a frame from the peer meant.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Received {
	/// The setup was answered: open, or refused.
	Setup(State),
	/// An Ethernet frame, whole: destination, source, type and payload.
	Frame(Vec<u8>),
	/// A control message this side answers: the reply to send.
	Reply(Vec<u8>),
	Nothing,
}

/// Why a packet was not read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	Short,
	TooLong,
	Unknown,
}

/// ONE PANU LINK: this host's address, the access point's, and where the setup is.
pub struct Panu {
	pub local: [u8; 6],
	pub remote: [u8; 6],
	state: State,
}

impl Panu {
	/// A link to the access point at `remote`, from `local`: the setup request to send.
	pub fn new(local: [u8; 6], remote: [u8; 6]) -> (Panu, Vec<u8>) {
		let mut request = alloc::vec![kind::CONTROL, control::SETUP_REQUEST, 2];
		request.extend_from_slice(&NAP.to_be_bytes());
		request.extend_from_slice(&PANU.to_be_bytes());
		(Panu { local, remote, state: State::Setting }, request)
	}

	pub fn state(&self) -> State {
		self.state
	}

	/// AN ETHERNET FRAME TO SEND, as NetworkService wrote it: destination, source, type, payload - in the most
	/// compressed form its addresses allow. `None` before the link is open, for a frame too short to have a header,
	/// or for a payload past the MTU.
	pub fn send(&self, frame: &[u8]) -> Option<Vec<u8>> {
		if self.state != State::Open || frame.len() < 14 || frame.len() - 14 > MTU {
			return None;
		}
		let (destination, source, rest) = (&frame[..6], &frame[6..12], &frame[12..]);
		let to_peer = destination == self.remote;
		let from_us = source == self.local;
		let mut out = Vec::with_capacity(frame.len() + 1);
		match (to_peer, from_us) {
			(true, true) => out.push(kind::COMPRESSED),
			(false, true) => {
				out.push(kind::COMPRESSED_DESTINATION_ONLY);
				out.extend_from_slice(destination);
			}
			(true, false) => {
				out.push(kind::COMPRESSED_SOURCE_ONLY);
				out.extend_from_slice(source);
			}
			(false, false) => {
				out.push(kind::GENERAL);
				out.extend_from_slice(destination);
				out.extend_from_slice(source);
			}
		}
		out.extend_from_slice(rest);
		Some(out)
	}

	/// ONE BNEP PACKET FROM THE PEER.
	pub fn receive(&mut self, packet: &[u8]) -> Result<Received, Refusal> {
		let &first = packet.first().ok_or(Refusal::Short)?;
		let extended = first & 0x80 != 0;
		let kind = first & 0x7f;
		let mut at = 1;
		// THE ADDRESSES each form carries; what it leaves out is the link's own.
		let (destination, source): ([u8; 6], [u8; 6]) = match kind {
			kind::GENERAL => {
				let addresses = packet.get(1..13).ok_or(Refusal::Short)?;
				at = 13;
				(addresses[..6].try_into().unwrap_or_default(), addresses[6..].try_into().unwrap_or_default())
			}
			kind::COMPRESSED => (self.local, self.remote),
			kind::COMPRESSED_SOURCE_ONLY => {
				let source = packet.get(1..7).ok_or(Refusal::Short)?;
				at = 7;
				(self.local, source.try_into().unwrap_or_default())
			}
			kind::COMPRESSED_DESTINATION_ONLY => {
				let destination = packet.get(1..7).ok_or(Refusal::Short)?;
				at = 7;
				(destination.try_into().unwrap_or_default(), self.remote)
			}
			kind::CONTROL => return self.control(&packet[1..]),
			_ => return Err(Refusal::Unknown),
		};
		let ethertype = packet.get(at..at + 2).ok_or(Refusal::Short)?;
		at += 2;
		if extended {
			at = skip_extensions(packet, at)?;
		}
		let payload = &packet[at..];
		if payload.len() > MTU {
			return Err(Refusal::TooLong);
		}
		if self.state != State::Open {
			return Ok(Received::Nothing);
		}
		let mut frame = Vec::with_capacity(14 + payload.len());
		frame.extend_from_slice(&destination);
		frame.extend_from_slice(&source);
		frame.extend_from_slice(ethertype);
		frame.extend_from_slice(payload);
		Ok(Received::Frame(frame))
	}

	fn control(&mut self, body: &[u8]) -> Result<Received, Refusal> {
		let &code = body.first().ok_or(Refusal::Short)?;
		match code {
			control::SETUP_RESPONSE => {
				let answer = body.get(1..3).ok_or(Refusal::Short)?;
				let answer = u16::from_be_bytes([answer[0], answer[1]]);
				if self.state == State::Setting {
					self.state = if answer == SETUP_SUCCESS { State::Open } else { State::Refused(answer) };
				}
				Ok(Received::Setup(self.state))
			}
			// THE PEER'S FILTERS: unsupported - this side sends what NetworkService sends and takes what arrives.
			control::FILTER_NET_TYPE_SET => Ok(Received::Reply(reply(control::FILTER_NET_TYPE_RESPONSE, FILTER_UNSUPPORTED))),
			control::FILTER_MULTI_ADDR_SET => Ok(Received::Reply(reply(control::FILTER_MULTI_ADDR_RESPONSE, FILTER_UNSUPPORTED))),
			// A SETUP ASKED OF THIS SIDE: a PANU serves no network, so the request is refused as "not allowed".
			control::SETUP_REQUEST => Ok(Received::Reply(reply(control::SETUP_RESPONSE, 0x0004))),
			control::NOT_UNDERSTOOD | control::FILTER_NET_TYPE_RESPONSE | control::FILTER_MULTI_ADDR_RESPONSE => Ok(Received::Nothing),
			unknown => Ok(Received::Reply(alloc::vec![kind::CONTROL, control::NOT_UNDERSTOOD, unknown])),
		}
	}
}

fn reply(code: u8, value: u16) -> Vec<u8> {
	let mut out = alloc::vec![kind::CONTROL, code];
	out.extend_from_slice(&value.to_be_bytes());
	out
}

// Past every extension header: a type with its own "another follows" bit, a length and that many bytes.
fn skip_extensions(packet: &[u8], mut at: usize) -> Result<usize, Refusal> {
	loop {
		let header = packet.get(at..at + 2).ok_or(Refusal::Short)?;
		let (more, len) = (header[0] & 0x80 != 0, usize::from(header[1]));
		at += 2 + len;
		if at > packet.len() {
			return Err(Refusal::Short);
		}
		if !more {
			return Ok(at);
		}
	}
}

#[cfg(test)]
mod tests;

//! BT - the Block Transfer interface (IPMI 2.0, 11): three registers, control (0), the buffer (1) and the interrupt
//! mask (2), and a whole message at a time through the buffer, with a length byte and a sequence number.
//!
//! A REQUEST: wait until the BMC is neither busy nor holding the host's attention flag, clear the write pointer, write
//! the length, the network function, the sequence number, the command and the data, and raise H2B_ATN. A RESPONSE:
//! wait for B2H_ATN, set H_BUSY, clear B2H_ATN, clear the read pointer, read the length and that many bytes, clear
//! H_BUSY. H_BUSY IS A TOGGLE, written with a one either way.
//!
//! WHAT IS REFUSED: a length byte past the output buffer the BMC's capabilities reported, or past `MAX_RESPONSE`; a
//! response whose sequence number, network function or command is not the request's.

use crate::{Failure, MAX_RESPONSE, Registers, Request, Response, wait};
use alloc::vec::Vec;

pub const CONTROL: u8 = 0;
pub const BUFFER: u8 = 1;
pub const INTMASK: u8 = 2;

pub const CLR_WR_PTR: u8 = 1 << 0;
pub const CLR_RD_PTR: u8 = 1 << 1;
pub const H2B_ATN: u8 = 1 << 2;
pub const B2H_ATN: u8 = 1 << 3;
pub const SMS_ATN: u8 = 1 << 4;
pub const H_BUSY: u8 = 1 << 6;
pub const B_BUSY: u8 = 1 << 7;

/// Get BT Interface Capabilities (App, 0x36).
pub const GET_BT_CAPABILITIES: u8 = 0x36;

/// What the BMC says its BT interface takes, from Get BT Interface Capabilities.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities {
	/// Requests it can hold at once.
	pub outstanding: u8,
	/// Its input buffer, in bytes, the length byte included.
	pub input: u8,
	/// Its output buffer, in bytes, the length byte included.
	pub output: u8,
	/// The longest it takes from request to response, in seconds.
	pub response_seconds: u8,
	pub retries: u8,
}

impl Capabilities {
	/// What is assumed before the capabilities are read: the smallest buffers the specification allows.
	pub const BEFORE: Capabilities = Capabilities { outstanding: 1, input: 0x40, output: 0x40, response_seconds: 5, retries: 0 };
}

/// Decode Get BT Interface Capabilities' data (after the completion code).
pub fn capabilities(data: &[u8]) -> Option<Capabilities> {
	if data.len() < 5 {
		return None;
	}
	let capabilities = Capabilities { outstanding: data[0], input: data[1], output: data[2], response_seconds: data[3], retries: data[4] };
	// A BUFFER TOO SMALL FOR A HEADER is a BMC that cannot be spoken to at all.
	if capabilities.input < 5 || capabilities.output < 6 || capabilities.outstanding == 0 {
		return None;
	}
	Some(capabilities)
}

/// A BT interface's per-binding state: the next sequence number and the capabilities in force.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bt {
	pub capabilities: Capabilities,
	sequence: u8,
}

impl Default for Bt {
	fn default() -> Bt {
		Bt { capabilities: Capabilities::BEFORE, sequence: 0 }
	}
}

impl Bt {
	/// One transaction: the request with the next sequence number, and the response held against it.
	pub fn transact<R: Registers + ?Sized>(&mut self, registers: &mut R, request: &Request, deadline_ms: u64) -> Result<Response, Failure> {
		let length = 3 + request.data.len();
		if length + 1 > self.capabilities.input as usize || length > crate::MAX_REQUEST {
			return Err(Failure::RequestTooLong);
		}
		let sequence = self.sequence;
		self.sequence = self.sequence.wrapping_add(1);
		// THE BMC NOT BUSY, and not still holding a request of ours.
		wait(registers, CONTROL, deadline_ms, |control| control & (B_BUSY | H2B_ATN) == 0).ok_or(Failure::Deadline)?;
		if registers.read(CONTROL) & H_BUSY != 0 {
			registers.write(CONTROL, H_BUSY);
		}
		registers.write(CONTROL, CLR_WR_PTR);
		registers.write(BUFFER, length as u8);
		registers.write(BUFFER, (request.netfn << 2) | (request.lun & 3));
		registers.write(BUFFER, sequence);
		registers.write(BUFFER, request.cmd);
		for &byte in &request.data {
			registers.write(BUFFER, byte);
		}
		registers.write(CONTROL, H2B_ATN);
		wait(registers, CONTROL, deadline_ms, |control| control & B2H_ATN != 0).ok_or(Failure::Deadline)?;
		registers.write(CONTROL, H_BUSY);
		registers.write(CONTROL, B2H_ATN);
		registers.write(CONTROL, CLR_RD_PTR);
		let length = registers.read(BUFFER) as usize;
		// THE LENGTH BYTE IS BELIEVED ONLY UNDER BOTH BOUNDS, and the bytes it names are still read out so the interface
		// is left where it expects to be; what they held is refused.
		let refused = if length + 1 > self.capabilities.output as usize || length > MAX_RESPONSE + 1 {
			Some(Failure::TooLong)
		} else if length < 4 {
			Some(Failure::Protocol("a BT response shorter than its header"))
		} else {
			None
		};
		let mut bytes = Vec::with_capacity(length.min(MAX_RESPONSE + 1));
		for _ in 0..length.min(MAX_RESPONSE + 1) {
			bytes.push(registers.read(BUFFER));
		}
		registers.write(CONTROL, H_BUSY);
		if let Some(failure) = refused {
			return Err(failure);
		}
		// Network function and LUN, sequence, command, completion code, data.
		if bytes[0] >> 2 != request.netfn | 1 || bytes[2] != request.cmd || bytes[1] != sequence {
			return Err(Failure::Mismatch);
		}
		Ok(Response { cc: bytes[3], data: bytes[4..].to_vec() })
	}

	/// The capabilities, asked of the BMC and put in force: what a binding does first, and again after a resume.
	pub fn read_capabilities<R: Registers + ?Sized>(&mut self, registers: &mut R, deadline_ms: u64) -> Result<Capabilities, Failure> {
		let request = Request::new(crate::netfn::APP, GET_BT_CAPABILITIES, &[]);
		let response = self.transact(registers, &request, deadline_ms)?;
		if !response.ok() {
			return Err(Failure::Protocol("the BMC refused Get BT Interface Capabilities"));
		}
		let found = capabilities(&response.data).ok_or(Failure::Protocol("Get BT Interface Capabilities answered a buffer too small to use"))?;
		self.capabilities = found;
		Ok(found)
	}
}

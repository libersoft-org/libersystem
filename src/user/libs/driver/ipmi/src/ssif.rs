//! SSIF - the SMBus System Interface (IPMI 2.0, 12): a request is an SMBus block write, a response an SMBus block
//! read whose length the BMC sends, each at most 32 bytes; a longer message is cut into parts.
//!
//! WRITES: one block with command 0x02 when the request fits in 32 bytes; otherwise a start (0x06) of 32 bytes,
//! middles (0x07) of 32 and an end (0x08) with the rest. READS: command 0x03 - NACKed while the BMC prepares its answer,
//! so it is retried, pausing, until the deadline or `NACK_RETRIES`; a first block beginning 0x00 0x01 is the start of a
//! multi-part response, whose following blocks (command 0x09) each begin with their block number, counting from zero,
//! the last with 0xFF. A block out of order, a count past 32 or a response past `MAX_RESPONSE` is refused.
//!
//! What the BMC takes is asked first with Get System Interface Capabilities (App 0x57, SSIF): the transaction support,
//! PEC, and its largest request and response. PEC is used only when both the BMC and the controller offer it - which
//! is the caller's `Smbus`, built with or without it.

use crate::{Failure, MAX_RESPONSE, Request, Response, response};
use alloc::vec::Vec;

pub const WRITE_SINGLE: u8 = 0x02;
pub const READ_SINGLE: u8 = 0x03;
pub const WRITE_START: u8 = 0x06;
pub const WRITE_MIDDLE: u8 = 0x07;
pub const WRITE_END: u8 = 0x08;
pub const READ_MIDDLE: u8 = 0x09;
pub const READ_RETRY: u8 = 0x0A;

/// The most one SMBus block carries.
pub const BLOCK: usize = 32;
/// How many NACKed reads a response may take before the transaction fails, beside its deadline.
pub const NACK_RETRIES: u32 = 500;

/// Get System Interface Capabilities (App, 0x57).
pub const GET_SYSTEM_INTERFACE_CAPABILITIES: u8 = 0x57;

/// Why the bus refused a transaction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BusError {
	/// The device did not acknowledge - the BMC is not ready.
	Nack,
	/// Anything else the controller reported.
	Failed,
}

/// The two SMBus transactions SSIF is spoken over, on one address, with the clock the deadline is measured against.
pub trait Smbus {
	fn block_write(&mut self, command: u8, data: &[u8]) -> Result<(), BusError>;
	/// The block the device sent: its bytes, after the count it sent first.
	fn block_read(&mut self, command: u8) -> Result<Vec<u8>, BusError>;
	fn now_ms(&mut self) -> u64;
	fn pause(&mut self);
}

/// How the BMC takes multi-part messages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Parts {
	Single,
	/// Start and end only.
	StartEnd,
	/// Start, middle and end.
	Middle,
}

/// What Get System Interface Capabilities says of the SSIF interface.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities {
	pub parts: Parts,
	pub pec: bool,
	pub version: u8,
	/// Its largest request, in bytes.
	pub input: u8,
	/// Its largest response, in bytes.
	pub output: u8,
}

impl Capabilities {
	/// What is assumed before they are read: one part each way.
	pub const BEFORE: Capabilities = Capabilities { parts: Parts::Single, pec: false, version: 0, input: BLOCK as u8, output: BLOCK as u8 };
}

/// Decode the capabilities' data (after the completion code): a reserved byte, then the support byte, then the two
/// sizes.
pub fn capabilities(data: &[u8]) -> Option<Capabilities> {
	if data.len() < 4 {
		return None;
	}
	let support = data[1];
	let parts = match support >> 6 {
		0 => Parts::Single,
		1 => Parts::StartEnd,
		2 => Parts::Middle,
		_ => return None,
	};
	Some(Capabilities { parts, pec: support & (1 << 3) != 0, version: support & 7, input: data[2], output: data[3] })
}

/// One SSIF binding's state: the capabilities in force.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ssif {
	pub capabilities: Capabilities,
}

impl Default for Ssif {
	fn default() -> Ssif {
		Ssif { capabilities: Capabilities::BEFORE }
	}
}

impl Ssif {
	/// One transaction: the request written in as many parts as it needs, the response read in as many as it comes in,
	/// and held against the request.
	pub fn transact<B: Smbus + ?Sized>(&self, bus: &mut B, request: &Request, deadline_ms: u64) -> Result<Response, Failure> {
		let bytes = request.bytes();
		if bytes.len() > self.capabilities.input.max(BLOCK as u8) as usize || bytes.len() > crate::MAX_REQUEST {
			return Err(Failure::RequestTooLong);
		}
		self.write(bus, &bytes)?;
		let read = self.read(bus, deadline_ms)?;
		response(request, &read)
	}

	fn write<B: Smbus + ?Sized>(&self, bus: &mut B, bytes: &[u8]) -> Result<(), Failure> {
		if bytes.len() <= BLOCK {
			return bus.block_write(WRITE_SINGLE, bytes).map_err(|_| Failure::Bus);
		}
		if self.capabilities.parts == Parts::Single {
			return Err(Failure::RequestTooLong);
		}
		let mut chunks = bytes.chunks(BLOCK).peekable();
		let mut first = true;
		while let Some(chunk) = chunks.next() {
			let last = chunks.peek().is_none();
			let command = if first {
				WRITE_START
			} else if last {
				WRITE_END
			} else if self.capabilities.parts == Parts::Middle {
				WRITE_MIDDLE
			} else {
				return Err(Failure::RequestTooLong);
			};
			bus.block_write(command, chunk).map_err(|_| Failure::Bus)?;
			first = false;
		}
		Ok(())
	}

	fn read<B: Smbus + ?Sized>(&self, bus: &mut B, deadline_ms: u64) -> Result<Vec<u8>, Failure> {
		let first = read_block(bus, READ_SINGLE, deadline_ms)?;
		// A SINGLE-PART RESPONSE is the block itself.
		if !(first.len() == BLOCK && first[0] == 0x00 && first[1] == 0x01) {
			return Ok(first);
		}
		let mut out = first[2..].to_vec();
		let mut expected: u8 = 0;
		loop {
			let block = read_block(bus, READ_MIDDLE, deadline_ms)?;
			let Some((&number, data)) = block.split_first() else { return Err(Failure::Protocol("an empty block in a multi-part SSIF response")) };
			if out.len() + data.len() > MAX_RESPONSE || out.len() + data.len() > self.capabilities.output.max(BLOCK as u8) as usize {
				return Err(Failure::TooLong);
			}
			if number == 0xFF {
				out.extend_from_slice(data);
				return Ok(out);
			}
			if number != expected {
				return Err(Failure::Protocol("an SSIF response block out of order"));
			}
			// A middle block is full: a short one that is not the end is not a middle block.
			if block.len() != BLOCK {
				return Err(Failure::Protocol("a short middle block in a multi-part SSIF response"));
			}
			out.extend_from_slice(data);
			expected = expected.checked_add(1).ok_or(Failure::TooLong)?;
		}
	}

	/// The capabilities, asked of the BMC and put in force.
	pub fn read_capabilities<B: Smbus + ?Sized>(&mut self, bus: &mut B, deadline_ms: u64) -> Result<Capabilities, Failure> {
		let request = Request::new(crate::netfn::APP, GET_SYSTEM_INTERFACE_CAPABILITIES, &[0]);
		let response = self.transact(bus, &request, deadline_ms)?;
		if !response.ok() {
			return Err(Failure::Protocol("the BMC refused Get System Interface Capabilities"));
		}
		let found = capabilities(&response.data).ok_or(Failure::Protocol("Get System Interface Capabilities answered too little"))?;
		self.capabilities = found;
		Ok(found)
	}
}

// ONE BLOCK READ, retried while the BMC NACKs it, pausing, within the deadline and the retry bound.
fn read_block<B: Smbus + ?Sized>(bus: &mut B, command: u8, deadline_ms: u64) -> Result<Vec<u8>, Failure> {
	let mut nacks = 0u32;
	loop {
		match bus.block_read(command) {
			Ok(block) => {
				if block.is_empty() || block.len() > BLOCK {
					return Err(Failure::Protocol("an SMBus block count of zero or past 32"));
				}
				return Ok(block);
			}
			Err(BusError::Nack) => {
				nacks += 1;
				if nacks >= NACK_RETRIES {
					return Err(Failure::Protocol("the BMC NACKed its response past the retry bound"));
				}
				if bus.now_ms() >= deadline_ms {
					return Err(Failure::Deadline);
				}
				bus.pause();
			}
			Err(BusError::Failed) => return Err(Failure::Bus),
		}
	}
}

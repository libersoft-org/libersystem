//! IPMI below the binding: how a host reaches its baseboard management controller (BMC) over the three system
//! interfaces - KCS and BT through registers, SSIF over SMBus - with one message layer above them, and the encodings
//! of what the BMC holds: its identity, the sensor data repository, the system event log, FRU inventory, the
//! watchdog, the chassis and the LAN configuration.
//!
//! WHAT THIS IS AND IS NOT. It is a PROTOCOL AND A SET OF PARSERS and it binds nothing: the `ipmi` driver binds, links
//! this statically, and supplies the registers (claimed ports or a mapped BAR, behind `Registers`) or the SMBus client
//! (behind `ssif::Smbus`). Which is what lets it close on host tests over scripted models, including the states QEMU's
//! interfaces never show a guest: a KCS interface stuck in each state, a BT sequence number never sent, SSIF blocks out
//! of order.
//!
//! EVERY FORM POLLS. A status wait spins a bounded number of register reads and then pauses between reads (`wait`);
//! no interrupt is used on any interface. EVERY LENGTH IS BOUNDED BEFORE IT IS BELIEVED: a response is at most
//! `MAX_RESPONSE` bytes, a transaction at most `TRANSACTION_MS`, and every record a parser reads is checked against the
//! length it states and the length that arrived.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod bt;
pub mod chassis;
pub mod event;
pub mod fru;
pub mod identity;
pub mod kcs;
pub mod lan;
pub mod queue;
pub mod sdr;
pub mod sel;
pub mod ssif;
pub mod watchdog;

use alloc::vec::Vec;

/// THE RESPONSE BOUND: network function, command, completion code and data, at most this many bytes - Linux's
/// `IPMI_MAX_MSG_LENGTH`. A response past it is refused, whatever the interface's own buffer would carry.
pub const MAX_RESPONSE: usize = 272;
/// The largest request this layer sends: network function, command and data.
pub const MAX_REQUEST: usize = 272;
/// A TRANSACTION'S DEADLINE: five seconds, above QEMU's external BMC's four, so that its own timeout answer (0xC3)
/// arrives before this layer gives up.
pub const TRANSACTION_MS: u64 = 5_000;
/// How long recovering an interface left busy at a deadline may take: a KCS abort.
pub const RECOVERY_MS: u64 = 1_000;

/// The network functions this layer speaks (requests; a response's is the request's plus one).
pub mod netfn {
	pub const CHASSIS: u8 = 0x00;
	pub const SENSOR_EVENT: u8 = 0x04;
	pub const APP: u8 = 0x06;
	pub const STORAGE: u8 = 0x0A;
	pub const TRANSPORT: u8 = 0x0C;
}

/// Completion codes this layer acts on.
pub mod cc {
	pub const OK: u8 = 0x00;
	/// Node busy: the BMC could not take the command now.
	pub const NODE_BUSY: u8 = 0xC0;
	pub const INVALID_COMMAND: u8 = 0xC1;
	/// The command timed out - what QEMU's external BMC answers after four silent seconds.
	pub const TIMEOUT: u8 = 0xC3;
	/// The reservation a request named was cancelled.
	pub const RESERVATION_CANCELLED: u8 = 0xC5;
	pub const REQUEST_TRUNCATED: u8 = 0xC6;
	pub const LENGTH_INVALID: u8 = 0xC7;
	pub const CANNOT_RETURN_BYTES: u8 = 0xCA;
	pub const NOT_PRESENT: u8 = 0xCB;
	pub const NOT_SUPPORTED_IN_STATE: u8 = 0xD5;
	/// Initialisation in progress - what QEMU answers while its external BMC is disconnected.
	pub const BMC_INIT: u8 = 0xD2;
	/// Get SEL Info while an erase is in progress.
	pub const ERASE_IN_PROGRESS: u8 = 0x81;
}

/// One request: network function, logical unit, command and data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Request {
	pub netfn: u8,
	pub lun: u8,
	pub cmd: u8,
	pub data: Vec<u8>,
}

impl Request {
	pub fn new(netfn: u8, cmd: u8, data: &[u8]) -> Request {
		Request { netfn, lun: 0, cmd, data: data.to_vec() }
	}

	/// The first two bytes of the request as KCS and SSIF carry it.
	pub fn header(&self) -> [u8; 2] {
		[(self.netfn << 2) | (self.lun & 3), self.cmd]
	}

	/// The request as KCS and SSIF carry it: the network function and LUN, the command, the data.
	pub fn bytes(&self) -> Vec<u8> {
		let mut out = Vec::with_capacity(2 + self.data.len());
		out.extend_from_slice(&self.header());
		out.extend_from_slice(&self.data);
		out
	}
}

/// One response: the completion code and the data after it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Response {
	pub cc: u8,
	pub data: Vec<u8>,
}

impl Response {
	pub fn ok(&self) -> bool {
		self.cc == cc::OK
	}
}

/// Why a transaction produced no response.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Failure {
	/// The deadline passed with the interface still working.
	Deadline,
	/// The response was longer than `MAX_RESPONSE` or than the interface said it could carry.
	TooLong,
	/// The response was not the request's: another network function, command or sequence number.
	Mismatch,
	/// The interface broke its own protocol - a KCS state out of place, a BT length past its buffer, an SSIF block
	/// out of order; the text says which.
	Protocol(&'static str),
	/// The KCS interface reported an error status, whose code this is.
	Interface(u8),
	/// The bus under SSIF refused the transaction.
	Bus,
	/// The request is longer than the interface or this layer takes.
	RequestTooLong,
}

/// Parse a response as KCS and SSIF carry it - network function and LUN, command, completion code, data - and hold it
/// against `request`.
pub fn response(request: &Request, bytes: &[u8]) -> Result<Response, Failure> {
	if bytes.len() > MAX_RESPONSE {
		return Err(Failure::TooLong);
	}
	if bytes.len() < 3 {
		return Err(Failure::Protocol("a response shorter than its header"));
	}
	if bytes[0] >> 2 != request.netfn | 1 || bytes[1] != request.cmd {
		return Err(Failure::Mismatch);
	}
	Ok(Response { cc: bytes[2], data: bytes[3..].to_vec() })
}

/// A system interface's registers, as whoever claimed them reaches them - a driver's port range or mapped BAR, a
/// host test's model - with the clock the interface's deadlines are measured against. `index` is the register's
/// number (KCS: 0 data, 1 status and command; BT: 0 control, 1 buffer, 2 interrupt mask); its spacing is the
/// implementation's.
pub trait Registers {
	fn read(&mut self, index: u8) -> u8;
	fn write(&mut self, index: u8, value: u8);
	/// Milliseconds on a clock that only moves forward.
	fn now_ms(&mut self) -> u64;
	/// Give up the processor until the next tick.
	fn pause(&mut self);
}

/// How many reads a status wait spins through before it starts pausing between them.
pub const SPIN_READS: u32 = 64;

/// THE STATUS WAIT EVERY FORM USES: read until `done` holds, spinning `SPIN_READS` reads and then pausing between
/// reads, until `deadline_ms`. The last read's value when it held, `None` at the deadline.
pub fn wait<R: Registers + ?Sized>(registers: &mut R, index: u8, deadline_ms: u64, mut done: impl FnMut(u8) -> bool) -> Option<u8> {
	let mut spins = 0u32;
	loop {
		let value = registers.read(index);
		if done(value) {
			return Some(value);
		}
		if registers.now_ms() >= deadline_ms {
			return None;
		}
		if spins < SPIN_READS {
			spins += 1;
		} else {
			registers.pause();
		}
	}
}

/// THE BMC'S AVAILABILITY: three transactions in a row that end in a timeout (0xC3), an initialisation in progress
/// (0xD2) or this layer's own deadline mark it unavailable; any response that is none of those marks it available.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Availability {
	strikes: u8,
	unavailable: bool,
}

/// How many such transactions in a row mark a BMC unavailable.
pub const STRIKES: u8 = 3;

/// What an outcome did to the BMC's availability.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
	None,
	/// It became unavailable with this outcome.
	Lost,
	/// It answered again.
	Back,
}

impl Availability {
	pub fn available(&self) -> bool {
		!self.unavailable
	}

	/// One transaction's outcome.
	pub fn observe(&mut self, outcome: &Result<Response, Failure>) -> Change {
		let silent = match outcome {
			Ok(response) => response.cc == cc::TIMEOUT || response.cc == cc::BMC_INIT,
			Err(Failure::Deadline) => true,
			Err(_) => false,
		};
		if silent {
			self.strikes = self.strikes.saturating_add(1);
			if self.strikes >= STRIKES && !self.unavailable {
				self.unavailable = true;
				return Change::Lost;
			}
			return Change::None;
		}
		self.strikes = 0;
		if self.unavailable && outcome.is_ok() {
			self.unavailable = false;
			return Change::Back;
		}
		Change::None
	}
}

/// Read a little-endian u16 at `at`, when the bytes are there.
pub(crate) fn le16(bytes: &[u8], at: usize) -> Option<u16> {
	Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

/// Read a little-endian u32 at `at`, when the bytes are there.
pub(crate) fn le32(bytes: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?, *bytes.get(at + 3)?]))
}

#[cfg(test)]
mod tests;

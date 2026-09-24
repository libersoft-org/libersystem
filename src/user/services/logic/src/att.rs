//! THE ATTRIBUTE PROTOCOL, AS THE DECISIONS A CLIENT MAKES ABOUT A SERVER IT DOES NOT TRUST.
//!
//! Discovery is a LOOP DRIVEN BY THE PEER'S OWN ANSWERS: the client asks about a handle range, the
//! server answers with attributes, and the client asks again from one past the last one it was
//! given. Every part of that is the peer's number, which makes three defects available to a server
//! that is broken or hostile, and all three are silent:
//!
//!   A HANDLE THAT DOES NOT MOVE FORWARD. The next request starts one past the last handle
//!     returned, so a server answering with a handle at or below where the range began makes the
//!     client ask the identical question for ever. This is the one that hangs a service rather than
//!     corrupting it, which is why it is first.
//!   A HANDLE OUTSIDE THE RANGE THAT WAS ASKED ABOUT. An answer about attributes nobody asked for
//!     is an answer about somebody else's attributes, and a client that recorded it would bind a
//!     characteristic it never discovered.
//!   A LIST WHOSE ENTRIES DO NOT DIVIDE IT. These responses are a length byte and then a run of
//!     fixed-size entries; a length that does not divide the remainder means the client and the
//!     server disagree about where each entry begins, and every field read after the first is
//!     offset.
//!
//! AND DISCOVERY IS BOUNDED BY A COUNT AS WELL AS BY THE RANGE. A server with sixty-five thousand
//! well-formed attributes is not malformed and is not a mouse; the cap is what makes a peer's
//! attribute table a thing this service walks rather than a thing it is given.
//!
//! WHAT IS NOT HERE: this is the boot mouse subset and nothing else. Read, write, the four discovery
//! responses and a notification are what it uses; every other opcode is refused by name rather than
//! being half-implemented.

/// The default ATT MTU, which the boot mouse subset runs on without negotiating a larger one.
pub const DEFAULT_MTU: usize = 23;

/// The largest MTU this client will agree to. Nothing here needs more than the default; the bound
/// exists so that a server proposing sixty thousand is refused rather than sized for.
pub const MAX_MTU: usize = 64;

/// The most attributes one discovery will record. See the note above.
pub const MAX_DISCOVERED: usize = 32;

/// The handle range every discovery starts over.
pub const FIRST_HANDLE: u16 = 0x0001;
pub const LAST_HANDLE: u16 = 0xffff;

/// The opcodes this subset speaks.
pub mod op {
	pub const ERROR_RESPONSE: u8 = 0x01;
	pub const EXCHANGE_MTU_REQUEST: u8 = 0x02;
	pub const EXCHANGE_MTU_RESPONSE: u8 = 0x03;
	pub const FIND_INFORMATION_REQUEST: u8 = 0x04;
	pub const FIND_INFORMATION_RESPONSE: u8 = 0x05;
	pub const READ_BY_TYPE_REQUEST: u8 = 0x08;
	pub const READ_BY_TYPE_RESPONSE: u8 = 0x09;
	pub const READ_REQUEST: u8 = 0x0a;
	pub const READ_RESPONSE: u8 = 0x0b;
	pub const READ_BY_GROUP_TYPE_REQUEST: u8 = 0x10;
	pub const READ_BY_GROUP_TYPE_RESPONSE: u8 = 0x11;
	pub const WRITE_REQUEST: u8 = 0x12;
	pub const WRITE_RESPONSE: u8 = 0x13;
	pub const HANDLE_VALUE_NOTIFICATION: u8 = 0x1b;
	pub const WRITE_COMMAND: u8 = 0x52;
}

/// Why a server's answer was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// No bytes at all: a PDU is at least its opcode.
	Empty,
	/// An opcode this subset does not speak. It is named rather than ignored, because a client that
	/// ignored unknown opcodes would sit through a server's error responses waiting for data.
	UnknownOpcode(u8),
	/// The opcode is one this subset speaks and is not the one this request was waiting for.
	Unexpected { got: u8, want: u8 },
	/// Longer than the MTU the two ends agreed on. A server exceeding it is a server whose bytes
	/// would not fit the buffer the agreement sized.
	OverMtu { len: usize, mtu: usize },
	/// A response too short to hold the fields its own format requires.
	Truncated { len: usize, needed: usize },
	/// A list whose entry length does not divide what follows it, or is zero.
	Ragged { entry: usize, bytes: usize },
	/// A handle at or before where the range began: the next request would repeat this one.
	HandleDidNotAdvance { got: u16, from: u16 },
	/// A handle outside the range that was asked about.
	HandleOutOfRange { got: u16, from: u16, to: u16 },
	/// More attributes than this client will record.
	TooMany,
}

/// The error a server reported, which is an answer and not a refusal: a server saying "attribute not
/// found" is how discovery ENDS, and a client that treated it as a fault would never finish.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ServerError {
	pub request: u8,
	pub handle: u16,
	pub code: u8,
}

/// The one error code that means "there is nothing more of that kind", which ends a discovery loop.
pub const ATTRIBUTE_NOT_FOUND: u8 = 0x0a;

/// What an answer turned out to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer<'a> {
	/// The server refused, and the refusal is data. `Refusal` above is for answers that are not
	/// well formed at all.
	Error(ServerError),
	/// A well-formed response of the opcode that was expected, with its bytes after the opcode.
	Response(&'a [u8]),
}

/// Read one ATT PDU as the answer to a request of `want`.
///
/// AN ERROR RESPONSE IS AN ANSWER TO ANY REQUEST, which is why it is admitted whatever `want` is -
/// it carries the opcode it is about, and a client that demanded its own opcode back would treat
/// every refusal as a protocol violation.
pub fn answer<'a>(pdu: &'a [u8], want: u8, mtu: usize) -> Result<Answer<'a>, Refusal> {
	if pdu.is_empty() {
		return Err(Refusal::Empty);
	}
	if pdu.len() > mtu {
		return Err(Refusal::OverMtu { len: pdu.len(), mtu });
	}
	let opcode = pdu[0];
	if opcode == op::ERROR_RESPONSE {
		if pdu.len() < 5 {
			return Err(Refusal::Truncated { len: pdu.len(), needed: 5 });
		}
		return Ok(Answer::Error(ServerError { request: pdu[1], handle: u16::from_le_bytes([pdu[2], pdu[3]]), code: pdu[4] }));
	}
	if !known(opcode) {
		return Err(Refusal::UnknownOpcode(opcode));
	}
	if opcode != want {
		return Err(Refusal::Unexpected { got: opcode, want });
	}
	Ok(Answer::Response(&pdu[1..]))
}

/// Whether this subset speaks the opcode at all.
pub const fn known(opcode: u8) -> bool {
	matches!(opcode, op::ERROR_RESPONSE | op::EXCHANGE_MTU_REQUEST | op::EXCHANGE_MTU_RESPONSE | op::FIND_INFORMATION_REQUEST | op::FIND_INFORMATION_RESPONSE | op::READ_BY_TYPE_REQUEST | op::READ_BY_TYPE_RESPONSE | op::READ_REQUEST | op::READ_RESPONSE | op::READ_BY_GROUP_TYPE_REQUEST | op::READ_BY_GROUP_TYPE_RESPONSE | op::WRITE_REQUEST | op::WRITE_RESPONSE | op::HANDLE_VALUE_NOTIFICATION | op::WRITE_COMMAND)
}

/// The MTU the two ends will use, from what this client asked for and what the server answered.
///
/// THE SMALLER OF THE TWO AND NEVER BELOW THE DEFAULT. A server answering with less than 23 is
/// answering with a number the protocol does not allow, and a client that believed it would size a
/// buffer smaller than the one PDU every server may send.
pub fn agreed_mtu(asked: usize, answered: usize) -> usize {
	asked.min(answered).clamp(DEFAULT_MTU, MAX_MTU)
}

/// One entry of a `read-by-group-type` or `read-by-type` response: a handle and the bytes with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry<'a> {
	pub handle: u16,
	pub value: &'a [u8],
}

/// Walk the entries of a length-prefixed response, checking every rule above.
///
/// `from`..=`to` is the range this request asked about. The walk stops at the first entry that
/// breaks a rule and reports which, because a list is only interpretable up to its first
/// disagreement: the entries after a ragged one are read at the wrong offsets.
pub struct Entries<'a> {
	bytes: &'a [u8],
	entry: usize,
	at: usize,
	from: u16,
	to: u16,
	last: Option<u16>,
	count: usize,
	fault: Option<Refusal>,
}

impl<'a> Entries<'a> {
	/// Read the length byte and bound everything after it.
	pub fn new(body: &'a [u8], from: u16, to: u16) -> Result<Entries<'a>, Refusal> {
		if body.is_empty() {
			return Err(Refusal::Truncated { len: 0, needed: 1 });
		}
		let entry = body[0] as usize;
		let rest = &body[1..];
		// A ZERO ENTRY LENGTH WOULD NEVER ADVANCE THE WALK, which is the same shape of defect as a
		// handle that does not move: an infinite loop inside a parser, on bytes a peer chose.
		if entry < 2 || rest.is_empty() || rest.len() % entry != 0 {
			return Err(Refusal::Ragged { entry, bytes: rest.len() });
		}
		Ok(Entries { bytes: rest, entry, at: 0, from, to, last: None, count: 0, fault: None })
	}

	/// The same walk over a list whose entry length is not its first byte.
	///
	/// FIND INFORMATION IS THE ONE RESPONSE SHAPED THIS WAY: its first byte is a FORMAT - one for
	/// sixteen-bit UUIDs, two for 128-bit ones - and the entry length follows from it. Reading that
	/// byte as a length would size every entry as one byte, which the ragged check refuses, so the
	/// caller names the length and every other rule applies unchanged.
	pub fn with_entry(rest: &'a [u8], entry: usize, from: u16, to: u16) -> Result<Entries<'a>, Refusal> {
		if entry < 2 || rest.is_empty() || rest.len() % entry != 0 {
			return Err(Refusal::Ragged { entry, bytes: rest.len() });
		}
		Ok(Entries { bytes: rest, entry, at: 0, from, to, last: None, count: 0, fault: None })
	}

	/// The refusal that ended the walk, when one did.
	pub const fn fault(&self) -> Option<Refusal> {
		self.fault
	}

	/// The last handle the walk accepted, which is where the next request begins.
	///
	/// NOT `last`: that name is `Iterator`'s, and a method of this type by that name would shadow the
	/// one every caller of a walk expects - silently, since both return an `Option`.
	pub const fn last_handle(&self) -> Option<u16> {
		self.last
	}
}

impl<'a> Iterator for Entries<'a> {
	type Item = Entry<'a>;

	fn next(&mut self) -> Option<Entry<'a>> {
		if self.fault.is_some() || self.at + self.entry > self.bytes.len() {
			return None;
		}
		let raw = &self.bytes[self.at..self.at + self.entry];
		let handle = u16::from_le_bytes([raw[0], raw[1]]);
		// THE RANGE FIRST, because an answer about somebody else's attributes is a different fault
		// from one that merely fails to make progress, and a client that reported the second for the
		// first would look for a stuck loop where the peer had answered the wrong question.
		if handle < self.from || handle > self.to {
			self.fault = Some(Refusal::HandleOutOfRange { got: handle, from: self.from, to: self.to });
			return None;
		}
		let floor = self.last.unwrap_or(self.from);
		let advanced = match self.last {
			None => handle >= floor,
			Some(previous) => handle > previous,
		};
		if !advanced {
			self.fault = Some(Refusal::HandleDidNotAdvance { got: handle, from: floor });
			return None;
		}
		if self.count >= MAX_DISCOVERED {
			self.fault = Some(Refusal::TooMany);
			return None;
		}
		self.last = Some(handle);
		self.count += 1;
		self.at += self.entry;
		Some(Entry { handle, value: &raw[2..] })
	}
}

/// Where the next discovery request begins, or `None` when the range is exhausted.
///
/// AT THE TOP OF THE RANGE THE DISCOVERY IS OVER, and the addition that would find that out wraps:
/// `0xffff + 1` is zero, which starts the whole walk again. This is the other half of the
/// non-advancing handle and is a defect in the CLIENT rather than a hostile server.
pub const fn next_range(last: u16, to: u16) -> Option<(u16, u16)> {
	if last >= to || last == LAST_HANDLE { None } else { Some((last + 1, to)) }
}

#[cfg(test)]
mod tests;

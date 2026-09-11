//! Reading a DNS answer, and deciding whether it is an answer to a question this host asked.
//!
//! ANY DATAGRAM FROM PORT 53 USED TO BE AN ANSWER. That is the whole of the old correlation, and it
//! makes a spoofed or cross-query reply indistinguishable from the real one: an off-path sender needs
//! only to know that a lookup is happening. Correlation here compares the transaction ID, the
//! normalized question, its type and class, and the caller compares the family, the scoped server and
//! both ports - and a reply that fails any of them is not an answer to anything.
//!
//! AND COMPARING IS ONLY HALF OF IT. RFC 5452 section 9.2 asks for unpredictable query IDs AND
//! unpredictable source ports, because those two fields are the whole of a stub resolver's off-path
//! defence: a resolver that increments its transaction ID by one and sends from a fixed port
//! satisfies every comparison above against a forgery that guessed the next one. This module holds
//! the comparison and the in-flight table that makes a drawn tuple unique and retires it; where the
//! randomness comes from is the caller's, because only the caller knows what this profile has.
//!
//! EVERY BOUND IS A NUMBER AND EVERY TRAVERSAL IS BOUNDED. A compression pointer must target a
//! STRICTLY EARLIER offset - that backward rule is what makes a loop impossible rather than merely
//! bounded - and the count on top of it bounds the legal-but-absurd case that is still an attack.

use alloc::vec::Vec;

/// The fixed DNS header.
pub const HEADER_LEN: usize = 12;

/// The most answer records parsed from one response. A response declaring more is refused and the
/// query is NOT retried against the same server, which is what stops a hostile server costing an
/// unbounded number of parses.
pub const MAX_ANSWER_RECORDS: usize = 32;

/// CNAME hops followed before the answer is refused as a loop or a chain too long to be a delegation.
pub const MAX_CNAME_CHAIN: usize = 8;

/// Compression pointers followed while decoding ONE name.
pub const MAX_COMPRESSION_JUMPS: usize = 16;

/// Addresses one lookup answers with. Beyond it the rest are DROPPED, not refused: the list is
/// already ordered, so the ones kept are the ones that would have been tried first, and a name with
/// nine addresses is well provisioned rather than wrong.
pub const MAX_RETURNED_ADDRESSES: usize = 8;

/// The longest legal name, so a legal name is never refused for being long.
pub const MAX_NAME_LEN: usize = 253;

pub const TYPE_A: u16 = 1;
pub const TYPE_NS: u16 = 2;
pub const TYPE_CNAME: u16 = 5;
pub const TYPE_AAAA: u16 = 28;
pub const CLASS_IN: u16 = 1;

/// Why a response was not an answer.
///
/// THE CAUSES ARE KEPT APART because they mean different things to a caller: a malformed response
/// says the server is broken, no record says the name does not exist, and a mismatch says something
/// else answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Shorter than a header, or a field runs past the end.
	Malformed,
	/// A compression pointer forward or in a loop, or too many of them.
	BadCompression,
	/// It is a query, the wrong opcode, or it answers a different question.
	NotOurs,
	/// The server said so: name error, refused, server failure.
	NameError,
	ServerFailure,
	/// More records than this host will parse.
	TooManyRecords,
	/// A CNAME chain longer than a delegation can be, or one that loops.
	ChainTooLong,
	/// The answer is complete but carries no address record.
	NoAddress,
	/// The server set TC: the answer did not fit and must be asked again over TCP.
	Truncated,
}

/// What one lookup asked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Question {
	/// The name, lowercased and without a trailing dot - the form comparisons are made in, so
	/// `EXAMPLE.com` and `example.com.` are one question rather than two.
	pub name: Vec<u8>,
	pub qtype: u16,
	pub qclass: u16,
}

/// The identity of one in-flight query.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tuple {
	pub transaction: u16,
	pub source_port: u16,
}

/// An address a lookup answered with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Address {
	V4([u8; 4]),
	V6([u8; 16]),
}

/// What a response yielded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Answer {
	pub addresses: Vec<Address>,
	/// The smallest time-to-live among the records used, in seconds.
	pub ttl: u32,
}

/// Normalize a name for comparison: lowercase, no trailing dot.
///
/// ONE FORM, BECAUSE TWO WOULD BE COMPARED. DNS names are case-insensitive and a server may echo the
/// question in any case it likes - several deliberately randomize it - so a comparison against the
/// bytes as sent rejects correct answers.
pub fn normalize(name: &[u8]) -> Vec<u8> {
	let trimmed: &[u8] = match name.last() {
		Some(b'.') => &name[..name.len() - 1],
		_ => name,
	};
	trimmed.iter().map(|byte| byte.to_ascii_lowercase()).collect()
}

/// Encode a name into the wire's length-prefixed labels.
pub fn encode_name(name: &[u8], out: &mut Vec<u8>) -> bool {
	if name.len() > MAX_NAME_LEN {
		return false;
	}
	for label in name.split(|byte| *byte == b'.') {
		if label.is_empty() || label.len() > 63 {
			return false;
		}
		out.push(label.len() as u8);
		out.extend_from_slice(label);
	}
	out.push(0);
	true
}

/// Decode a name at `offset`, following compression pointers.
///
/// Returns the name and the offset just past the name AS IT APPEARED HERE - which is not where the
/// pointer led, because a compressed name occupies two bytes wherever it is used.
pub fn decode_name(message: &[u8], offset: usize) -> Result<(Vec<u8>, usize), Refusal> {
	let mut name: Vec<u8> = Vec::new();
	let mut at: usize = offset;
	let mut after: Option<usize> = None;
	let mut jumps: usize = 0;
	loop {
		let length: u8 = *message.get(at).ok_or(Refusal::Malformed)?;
		if length & 0xc0 == 0xc0 {
			let low: u8 = *message.get(at + 1).ok_or(Refusal::Malformed)?;
			let target: usize = usize::from(u16::from_be_bytes([length & 0x3f, low]));
			// STRICTLY EARLIER, WHICH IS WHAT MAKES A LOOP IMPOSSIBLE. A pointer that may go forward
			// or to itself can be chased for ever, and a counter alone only bounds how long.
			if target >= at {
				return Err(Refusal::BadCompression);
			}
			jumps += 1;
			if jumps > MAX_COMPRESSION_JUMPS {
				return Err(Refusal::BadCompression);
			}
			if after.is_none() {
				after = Some(at + 2);
			}
			at = target;
			continue;
		}
		if length & 0xc0 != 0 {
			return Err(Refusal::Malformed);
		}
		if length == 0 {
			at += 1;
			break;
		}
		let start: usize = at + 1;
		let end: usize = start + usize::from(length);
		let label: &[u8] = message.get(start..end).ok_or(Refusal::Malformed)?;
		if !name.is_empty() {
			name.push(b'.');
		}
		name.extend_from_slice(label);
		if name.len() > MAX_NAME_LEN {
			return Err(Refusal::Malformed);
		}
		at = end;
	}
	Ok((normalize(&name), after.unwrap_or(at)))
}

/// Build a query for `question` with the given identity.
pub fn build_query(tuple: Tuple, question: &Question) -> Option<Vec<u8>> {
	let mut message: Vec<u8> = Vec::with_capacity(HEADER_LEN + question.name.len() + 6);
	message.extend_from_slice(&tuple.transaction.to_be_bytes());
	// Recursion desired, and nothing else: a stub resolver asks its server to do the work.
	message.extend_from_slice(&0x0100u16.to_be_bytes());
	message.extend_from_slice(&1u16.to_be_bytes());
	message.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
	if !encode_name(&question.name, &mut message) {
		return None;
	}
	message.extend_from_slice(&question.qtype.to_be_bytes());
	message.extend_from_slice(&question.qclass.to_be_bytes());
	Some(message)
}

/// Read a response, checking that it answers `question` under `tuple`.
pub fn parse_response(message: &[u8], tuple: Tuple, question: &Question) -> Result<Answer, Refusal> {
	if message.len() < HEADER_LEN {
		return Err(Refusal::Malformed);
	}
	if u16::from_be_bytes([message[0], message[1]]) != tuple.transaction {
		return Err(Refusal::NotOurs);
	}
	let flags: u16 = u16::from_be_bytes([message[2], message[3]]);
	// QR must say response, and the opcode must be the standard query this host asked.
	if flags & 0x8000 == 0 || (flags >> 11) & 0x0f != 0 {
		return Err(Refusal::NotOurs);
	}
	if flags & 0x0200 != 0 {
		// TC: the answer did not fit. AAAA and CNAME sets are exactly the answers that overflow, so
		// this is an ordinary outcome and the caller asks again over TCP.
		return Err(Refusal::Truncated);
	}
	match flags & 0x000f {
		0 => {}
		2 => return Err(Refusal::ServerFailure),
		3 => return Err(Refusal::NameError),
		_ => return Err(Refusal::ServerFailure),
	}
	let counts: [u16; 4] = [
		u16::from_be_bytes([message[4], message[5]]),
		u16::from_be_bytes([message[6], message[7]]),
		u16::from_be_bytes([message[8], message[9]]),
		u16::from_be_bytes([message[10], message[11]]),
	];
	if counts[0] != 1 {
		return Err(Refusal::NotOurs);
	}
	if usize::from(counts[1]) > MAX_ANSWER_RECORDS {
		return Err(Refusal::TooManyRecords);
	}
	let mut at: usize = HEADER_LEN;
	let (asked, past) = decode_name(message, at)?;
	at = past;
	if at + 4 > message.len() {
		return Err(Refusal::Malformed);
	}
	let qtype: u16 = u16::from_be_bytes([message[at], message[at + 1]]);
	let qclass: u16 = u16::from_be_bytes([message[at + 2], message[at + 3]]);
	at += 4;
	// THE ECHOED QUESTION MUST BE THE ONE ASKED. A cross-query answer carries somebody else's.
	if asked != question.name || qtype != question.qtype || qclass != question.qclass {
		return Err(Refusal::NotOurs);
	}

	// Walk the answer section, following CNAMEs from the owner this host asked about.
	let mut owner: Vec<u8> = question.name.clone();
	let mut addresses: Vec<Address> = Vec::new();
	let mut ttl: u32 = u32::MAX;
	let mut hops: usize = 0;
	let mut seen: Vec<Vec<u8>> = Vec::new();
	let mut records: Vec<(Vec<u8>, u16, u16, u32, usize, usize)> = Vec::new();
	for _ in 0..counts[1] {
		let (name, past) = decode_name(message, at)?;
		at = past;
		if at + 10 > message.len() {
			return Err(Refusal::Malformed);
		}
		let rtype: u16 = u16::from_be_bytes([message[at], message[at + 1]]);
		let rclass: u16 = u16::from_be_bytes([message[at + 2], message[at + 3]]);
		let record_ttl: u32 = u32::from_be_bytes([message[at + 4], message[at + 5], message[at + 6], message[at + 7]]);
		let rdlen: usize = usize::from(u16::from_be_bytes([message[at + 8], message[at + 9]]));
		at += 10;
		if at + rdlen > message.len() {
			return Err(Refusal::Malformed);
		}
		records.push((name, rtype, rclass, record_ttl, at, rdlen));
		at += rdlen;
	}

	loop {
		let mut followed: bool = false;
		for (name, rtype, rclass, record_ttl, start, rdlen) in &records {
			if *rclass != CLASS_IN || *name != owner {
				continue;
			}
			match *rtype {
				TYPE_A if *rdlen == 4 => {
					addresses.push(Address::V4([message[*start], message[*start + 1], message[*start + 2], message[*start + 3]]));
					ttl = ttl.min(*record_ttl);
				}
				TYPE_AAAA if *rdlen == 16 => {
					let mut octets = [0u8; 16];
					octets.copy_from_slice(&message[*start..*start + 16]);
					addresses.push(Address::V6(octets));
					ttl = ttl.min(*record_ttl);
				}
				TYPE_CNAME => {
					let (target, _) = decode_name(message, *start)?;
					// A CHAIN THAT REVISITS A NAME IS A LOOP, and counting hops alone would follow it
					// until the count ran out rather than seeing what it is.
					if seen.contains(&target) {
						return Err(Refusal::ChainTooLong);
					}
					hops += 1;
					if hops > MAX_CNAME_CHAIN {
						return Err(Refusal::ChainTooLong);
					}
					seen.push(owner.clone());
					owner = target;
					ttl = ttl.min(*record_ttl);
					followed = true;
				}
				_ => {}
			}
			if followed {
				break;
			}
		}
		if !followed {
			break;
		}
	}

	// A LINK-LOCAL ANSWER CANNOT ESCAPE UNSCOPED. `fe80::1` names a different host on each link, and
	// nothing in a DNS answer says which link it meant - so the resolver cannot supply the provenance
	// the scoped form requires, and an address that would have to travel without it is dropped rather
	// than handed out as though it were routable.
	addresses.retain(|address| !matches!(address, Address::V6(octets) if octets[0] == 0xfe && octets[1] & 0xc0 == 0x80));
	if addresses.is_empty() {
		return Err(Refusal::NoAddress);
	}
	// BEYOND THE BOUND THE REST ARE DROPPED. The list is already in the order this host will try
	// them, so the ones kept are the ones that would have been tried first.
	addresses.truncate(MAX_RETURNED_ADDRESSES);
	Ok(Answer { addresses, ttl: if ttl == u32::MAX { 0 } else { ttl } })
}

/// The in-flight queries, so a drawn identity is unique and a finished one matches nothing.
#[derive(Debug, Default)]
pub struct InFlight {
	live: Vec<Tuple>,
}

impl InFlight {
	pub fn new() -> InFlight {
		InFlight::default()
	}

	pub fn len(&self) -> usize {
		self.live.len()
	}

	pub fn is_empty(&self) -> bool {
		self.live.is_empty()
	}

	pub fn holds(&self, tuple: Tuple) -> bool {
		self.live.contains(&tuple)
	}

	/// Draw an identity from `entropy`, redrawing while it collides with a live one.
	///
	/// REDRAWN RATHER THAN REUSED. Two live queries sharing an identity would each accept the other's
	/// answer, which is the same failure as having no correlation at all - arrived at by accident
	/// instead of by an attacker.
	pub fn draw(&mut self, mut entropy: impl FnMut() -> (u16, u16), attempts: usize) -> Option<Tuple> {
		for _ in 0..attempts {
			let (transaction, port) = entropy();
			// An ephemeral port, spread over the range RFC 6335 leaves to them.
			let source_port: u16 = 49152 + (port % (65535 - 49152));
			let tuple = Tuple { transaction, source_port };
			if !self.holds(tuple) {
				self.live.push(tuple);
				return Some(tuple);
			}
		}
		None
	}

	/// The query finished or expired.
	///
	/// RETIRED, SO A LATE ANSWER MATCHES NOTHING. A resolver that kept the identity would accept an
	/// answer to a question nobody is waiting for - which is exactly the window a forgery wants.
	pub fn retire(&mut self, tuple: Tuple) -> bool {
		let before: usize = self.live.len();
		self.live.retain(|held| *held != tuple);
		before != self.live.len()
	}
}

#[cfg(test)]
mod tests;

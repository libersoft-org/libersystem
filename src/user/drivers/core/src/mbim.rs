// MBIM FRAMING, VALIDATED BEFORE ANYTHING IS COPIED (USB MBIM 1.0): control messages reassembled from
// their fragments, and IP datagrams found in NCM transfer blocks by session.
//
// WHAT THIS IS FOR. The in-guest modem fixture speaks these records now, and the USB CDC-MBIM class
// module will parse real ones with the same code; `cdc.rs`'s NCM signatures are not MBIM support, and
// ModemService parses none of this - it sees typed provider messages. So the bounds live here, once.
//
// BOUNDS FIRST, EVERY TIME. A fragment's total count, a message's total length, the device's aggregate of
// open assemblies, the number of assemblies, and every offset in a transfer block are checked against
// what arrived and against the negotiated limits before a byte is copied or a buffer allocated. A
// duplicate fragment that repeats what arrived is ignored; one that says something else is a conflict
// and ends its assembly. NDP chains are walked with a visited set, so a cycle is a refusal and not a loop.

use alloc::vec::Vec;

pub const MESSAGE_HEADER: usize = 12;
pub const FRAGMENT_HEADER: usize = 8;
/// The contract's maxima, which a device negotiates down from.
pub const MAX_FRAGMENTS: u32 = 256;
pub const MAX_MESSAGE: usize = 64 * 1024;
pub const MAX_AGGREGATE: usize = 256 * 1024;
pub const MAX_ASSEMBLIES: usize = 4;
/// An assembly left incomplete for two seconds is dropped (100 Hz ticks).
pub const ASSEMBLY_TICKS: u64 = 200;

pub const NTH16_SIGNATURE: u32 = 0x484d_434e;
pub const NTH16_LENGTH: usize = 12;
/// "IPS" followed by the session number in the top byte.
pub const IPS_SIGNATURE: u32 = 0x0053_5049;
pub const NDP16_MINIMUM: usize = 16;
/// The most NDPs one transfer block may chain.
pub const MAX_NDPS: usize = 16;

// The message types this framing carries fragments for: command, command-done, indicate-status.
const COMMAND_MSG: u32 = 0x0000_0003;
const COMMAND_DONE: u32 = 0x8000_0003;
const INDICATE_STATUS: u32 = 0x8000_0007;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
	/// Shorter than its headers, or its own length field disagrees with what arrived.
	Malformed,
	/// A count, a length or an aggregate past the negotiated bound.
	Bound,
	/// A fragment out of order, or one whose total or type disagrees with its assembly.
	Order,
	/// A duplicate fragment that says something the first did not.
	Conflict,
	/// An offset or a length reaching past the block, or an NDP chain that loops.
	Offset,
	/// A datagram tagged for a session that is not the active one.
	Session,
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
	Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// One fragment, as it arrived.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fragment<'a> {
	pub message_type: u32,
	pub transaction: u32,
	pub total: u32,
	pub current: u32,
	pub payload: &'a [u8],
}

/// The negotiated limits, each at most the contract's maximum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
	pub fragments: u32,
	pub message: usize,
	pub aggregate: usize,
	pub assemblies: usize,
}

impl Limits {
	/// Negotiated DOWN: whatever a device asks for, never more than the contract allows.
	pub fn negotiate(fragments: u32, message: usize, aggregate: usize, assemblies: usize) -> Self {
		Self { fragments: fragments.clamp(1, MAX_FRAGMENTS), message: message.clamp(MESSAGE_HEADER + FRAGMENT_HEADER, MAX_MESSAGE), aggregate: aggregate.clamp(1, MAX_AGGREGATE), assemblies: assemblies.clamp(1, MAX_ASSEMBLIES) }
	}
}

/// One control transfer, parsed: a message header whose length is the transfer's, and - for the three
/// fragmented types - a fragment header.
pub fn parse_fragment(transfer: &[u8]) -> Result<Fragment<'_>, Refused> {
	let message_type = le32(transfer, 0).ok_or(Refused::Malformed)?;
	let length = le32(transfer, 4).ok_or(Refused::Malformed)? as usize;
	let transaction = le32(transfer, 8).ok_or(Refused::Malformed)?;
	if length != transfer.len() || length < MESSAGE_HEADER {
		return Err(Refused::Malformed);
	}
	if !matches!(message_type, COMMAND_MSG | COMMAND_DONE | INDICATE_STATUS) {
		return Ok(Fragment { message_type, transaction, total: 1, current: 0, payload: &transfer[MESSAGE_HEADER..] });
	}
	let total = le32(transfer, MESSAGE_HEADER).ok_or(Refused::Malformed)?;
	let current = le32(transfer, MESSAGE_HEADER + 4).ok_or(Refused::Malformed)?;
	if total == 0 || current >= total {
		return Err(Refused::Order);
	}
	Ok(Fragment { message_type, transaction, total, current, payload: &transfer[MESSAGE_HEADER + FRAGMENT_HEADER..] })
}

/// A reassembled message.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
	pub message_type: u32,
	pub transaction: u32,
	pub body: Vec<u8>,
}

struct Assembly {
	message_type: u32,
	transaction: u32,
	total: u32,
	next: u32,
	started: u64,
	body: Vec<u8>,
	// Where each fragment's payload begins, so a duplicate can be compared with what arrived.
	starts: Vec<usize>,
}

pub struct Assembler {
	limits: Limits,
	assemblies: Vec<Assembly>,
}

impl Assembler {
	pub fn new(limits: Limits) -> Self {
		Self { limits, assemblies: Vec::new() }
	}

	fn aggregate(&self) -> usize {
		self.assemblies.iter().map(|assembly| assembly.body.len()).sum()
	}

	pub fn open(&self) -> usize {
		self.assemblies.len()
	}

	/// Feed one fragment. A complete message comes back; an incomplete one is held; a refusal drops the
	/// assembly the fragment belonged to, and nothing it had gathered stays allocated.
	pub fn feed(&mut self, fragment: &Fragment, now: u64) -> Result<Option<Message>, Refused> {
		if fragment.total > self.limits.fragments {
			return Err(Refused::Bound);
		}
		let at = self.assemblies.iter().position(|assembly| assembly.transaction == fragment.transaction && assembly.message_type == fragment.message_type);
		let Some(at) = at else {
			if fragment.current != 0 {
				return Err(Refused::Order);
			}
			if fragment.payload.len() > self.limits.message {
				return Err(Refused::Bound);
			}
			if fragment.total == 1 {
				return Ok(Some(Message { message_type: fragment.message_type, transaction: fragment.transaction, body: fragment.payload.to_vec() }));
			}
			if self.assemblies.len() >= self.limits.assemblies || self.aggregate().checked_add(fragment.payload.len()).is_none_or(|total| total > self.limits.aggregate) {
				return Err(Refused::Bound);
			}
			self.assemblies.push(Assembly { message_type: fragment.message_type, transaction: fragment.transaction, total: fragment.total, next: 1, started: now, body: fragment.payload.to_vec(), starts: alloc::vec![0] });
			return Ok(None);
		};
		let assembly = &self.assemblies[at];
		if fragment.total != assembly.total {
			self.assemblies.remove(at);
			return Err(Refused::Order);
		}
		if fragment.current < assembly.next {
			// A DUPLICATE: harmless if it says what the first said, a conflict if it does not.
			let from = assembly.starts[fragment.current as usize];
			let to = assembly.starts.get(fragment.current as usize + 1).copied().unwrap_or(assembly.body.len());
			if assembly.body[from..to] == *fragment.payload {
				return Ok(None);
			}
			self.assemblies.remove(at);
			return Err(Refused::Conflict);
		}
		if fragment.current != assembly.next {
			self.assemblies.remove(at);
			return Err(Refused::Order);
		}
		let grown = assembly.body.len().checked_add(fragment.payload.len());
		let aggregate = self.aggregate().checked_add(fragment.payload.len());
		if grown.is_none_or(|len| len > self.limits.message) || aggregate.is_none_or(|len| len > self.limits.aggregate) {
			self.assemblies.remove(at);
			return Err(Refused::Bound);
		}
		let assembly = &mut self.assemblies[at];
		assembly.starts.push(assembly.body.len());
		assembly.body.extend_from_slice(fragment.payload);
		assembly.next += 1;
		if assembly.next == assembly.total {
			let done = self.assemblies.remove(at);
			return Ok(Some(Message { message_type: done.message_type, transaction: done.transaction, body: done.body }));
		}
		Ok(None)
	}

	/// Drop assemblies older than two seconds; answers how many went.
	pub fn expire(&mut self, now: u64) -> usize {
		let before = self.assemblies.len();
		self.assemblies.retain(|assembly| now.saturating_sub(assembly.started) < ASSEMBLY_TICKS);
		before - self.assemblies.len()
	}
}

/// A message cut into control transfers of at most `max_transfer` bytes, headers included.
pub fn fragments(message_type: u32, transaction: u32, body: &[u8], max_transfer: usize) -> Vec<Vec<u8>> {
	let room = max_transfer.saturating_sub(MESSAGE_HEADER + FRAGMENT_HEADER).max(1);
	let pieces: Vec<&[u8]> = if body.is_empty() { alloc::vec![&body[..0]] } else { body.chunks(room).collect() };
	let total = pieces.len() as u32;
	pieces
		.iter()
		.enumerate()
		.map(|(current, piece)| {
			let length = MESSAGE_HEADER + FRAGMENT_HEADER + piece.len();
			let mut transfer = Vec::with_capacity(length);
			transfer.extend_from_slice(&message_type.to_le_bytes());
			transfer.extend_from_slice(&(length as u32).to_le_bytes());
			transfer.extend_from_slice(&transaction.to_le_bytes());
			transfer.extend_from_slice(&total.to_le_bytes());
			transfer.extend_from_slice(&(current as u32).to_le_bytes());
			transfer.extend_from_slice(piece);
			transfer
		})
		.collect()
}

/// The datagrams of one NCM transfer block (NTH16 and chained NDP16s) that belong to `session`, as
/// ranges into the block. Every NDP index and datagram range is checked inside the block's own length;
/// a chain that revisits an NDP is refused.
pub fn datagrams(block: &[u8], session: u8, mtu: u16) -> Result<Vec<core::ops::Range<usize>>, Refused> {
	if le32(block, 0) != Some(NTH16_SIGNATURE) || le16(block, 4) != Some(NTH16_LENGTH as u16) {
		return Err(Refused::Malformed);
	}
	let block_length = usize::from(le16(block, 8).ok_or(Refused::Malformed)?);
	if block_length > block.len() || block_length < NTH16_LENGTH {
		return Err(Refused::Offset);
	}
	let block = &block[..block_length];
	let mut ndp = usize::from(le16(block, 10).ok_or(Refused::Malformed)?);
	let mut visited: Vec<usize> = Vec::new();
	let mut found = Vec::new();
	while ndp != 0 {
		if visited.contains(&ndp) || visited.len() >= MAX_NDPS {
			return Err(Refused::Offset);
		}
		visited.push(ndp);
		if ndp % 4 != 0 || ndp < NTH16_LENGTH || ndp.checked_add(NDP16_MINIMUM).is_none_or(|end| end > block.len()) {
			return Err(Refused::Offset);
		}
		let signature = le32(block, ndp).ok_or(Refused::Offset)?;
		let length = usize::from(le16(block, ndp + 4).ok_or(Refused::Offset)?);
		let next = usize::from(le16(block, ndp + 6).ok_or(Refused::Offset)?);
		if length < NDP16_MINIMUM || ndp + length > block.len() || length % 4 != 0 {
			return Err(Refused::Offset);
		}
		if signature & 0x00ff_ffff != IPS_SIGNATURE {
			return Err(Refused::Malformed);
		}
		let tagged = (signature >> 24) as u8;
		// The pointer table: (index, length) pairs, ending at (0, 0) or at the NDP's end.
		let mut pointer = ndp + 8;
		while pointer + 4 <= ndp + length {
			let index = usize::from(le16(block, pointer).ok_or(Refused::Offset)?);
			let len = usize::from(le16(block, pointer + 2).ok_or(Refused::Offset)?);
			pointer += 4;
			if index == 0 && len == 0 {
				break;
			}
			if index < NTH16_LENGTH || index.checked_add(len).is_none_or(|end| end > block.len()) || len == 0 {
				return Err(Refused::Offset);
			}
			if len > usize::from(mtu) {
				return Err(Refused::Bound);
			}
			if tagged != session {
				return Err(Refused::Session);
			}
			found.push(index..index + len);
		}
		ndp = next;
	}
	Ok(found)
}

/// One transfer block carrying `datagrams` for `session`: an NTH16, the datagrams, and one NDP16.
pub fn block(session: u8, datagrams: &[&[u8]], sequence: u16) -> Vec<u8> {
	let mut out = alloc::vec![0u8; NTH16_LENGTH];
	let mut pointers: Vec<(u16, u16)> = Vec::new();
	for datagram in datagrams {
		while out.len() % 4 != 0 {
			out.push(0);
		}
		pointers.push((out.len() as u16, datagram.len() as u16));
		out.extend_from_slice(datagram);
	}
	while out.len() % 4 != 0 {
		out.push(0);
	}
	let ndp = out.len();
	let ndp_length = (8 + (pointers.len() + 1) * 4).next_multiple_of(4).max(NDP16_MINIMUM);
	out.extend_from_slice(&(IPS_SIGNATURE | (u32::from(session) << 24)).to_le_bytes());
	out.extend_from_slice(&(ndp_length as u16).to_le_bytes());
	out.extend_from_slice(&0u16.to_le_bytes());
	for (index, length) in &pointers {
		out.extend_from_slice(&index.to_le_bytes());
		out.extend_from_slice(&length.to_le_bytes());
	}
	out.extend_from_slice(&[0, 0, 0, 0]);
	while out.len() < ndp + ndp_length {
		out.push(0);
	}
	let total = out.len() as u16;
	out[0..4].copy_from_slice(&NTH16_SIGNATURE.to_le_bytes());
	out[4..6].copy_from_slice(&(NTH16_LENGTH as u16).to_le_bytes());
	out[6..8].copy_from_slice(&sequence.to_le_bytes());
	out[8..10].copy_from_slice(&total.to_le_bytes());
	out[10..12].copy_from_slice(&(ndp as u16).to_le_bytes());
	out
}

#[cfg(test)]
mod tests;

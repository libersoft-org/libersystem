// THE BLOCK WIRE, IN ONE PLACE INSTEAD OF FOUR.
//
// Three programs speak this protocol today and each of them writes it out from memory:
// `driver.virtio-blk` serves it, `driver.xhci` serves it for a USB mass-storage device behind its
// own claim, and StorageService is the client of both. Every one of them declares `OP_READ = 0`,
// `OP_WRITE = 1`, `OP_CAPACITY = 2`, `OP_FLUSH = 3` privately, and every one of them takes the
// sixteen request bytes apart index by index at the point of use. The roadmap names the rule this
// breaks - the wire is owned by the first implemented slice that publishes a block provider - and
// says why nobody owns it: it was never extracted, so each new block driver copies it again.
//
// WHAT A HAND-COPIED WIRE ACTUALLY COSTS IS ALREADY VISIBLE HERE, and it is not a hypothetical about
// some future edit. The two servers disagree TODAY about what a refused request says. `virtio-blk`
// answers `STATUS_INVALID`, which exists precisely so that a caller can tell a request it got wrong
// from a device that failed one it got right; the USB path answers `STATUS_ERR` for the same
// refusal, so on that server the distinction the constant was introduced for does not reach the
// client. Neither is a typo - each is a correct implementation of one end's own copy of the rule.
// That divergence is recorded rather than repaired here, because repairing it changes what a server
// this module does not test replies with.
//
// THIS MODULE DECIDES AND ENCODES; IT NEVER SENDS. The channel belongs to the driver and to the
// service, and a layer that both framed and transferred would be a second place for the two ends to
// disagree, which is what this module exists to end. It lives in the crate both ends already share
// and it is pure, so a host test can watch each refusal happen.
//
// The REQUEST contract - what a server may refuse before its device is asked - is not here. It is
// `drivers::blk`, it is host-tested, and it is about a request's meaning rather than its bytes.

// The operations. The op is the leading `u32` of every request and these four are all of them; a
// fifth would be a wire change, which is why `Request::decode` hands an unknown one back rather
// than folding it into an error the caller cannot distinguish from a malformed frame.
pub const OP_READ: u32 = 0;
pub const OP_WRITE: u32 = 1;
pub const OP_CAPACITY: u32 = 2;
pub const OP_FLUSH: u32 = 3;

// The reply statuses, and WHY THERE ARE THREE RATHER THAN TWO. `STATUS_ERR` is the device failing a
// request the caller got right. `STATUS_INVALID` is the caller getting the request wrong, refused
// before the device was asked at all - a count of zero or above what one request may carry, a range
// past the last sector, or a write whose transferred handle is not a readable memory object at
// least as long as the request says. A client that cannot tell those apart has no way to know
// whether retrying is sensible.
pub const STATUS_OK: u32 = 0;
pub const STATUS_ERR: u32 = 1;
pub const STATUS_INVALID: u32 = 2;

// A request is exactly sixteen bytes: `[op u32][lba u64][count u32]`, little endian.
pub const REQUEST_LEN: usize = 16;
// An ordinary reply is the status alone. A read's sectors and a write's source ride as a
// transferred memory object beside it, never in the payload.
pub const REPLY_LEN: usize = 4;
// A capacity reply is the status, the medium's size in BYTES, and the largest sector count one
// request to this server may carry: `[status u32][bytes u64][max sectors u32]`.
pub const CAPACITY_REPLY_LEN: usize = 16;
// AND THE FIRST TWELVE OF THOSE ARE THE PART EVERY SERVER HAS ALWAYS SENT. The per-request bound was
// added after the size was, so a server predating it answers twelve bytes and StorageService reads a
// size out of them and falls back to its own bound. That rule was `len >= 12` written once inside
// the service, which made it a property of that client rather than of the protocol; it is named here
// so the two lengths mean different things on purpose rather than by accident.
pub const CAPACITY_SIZE_LEN: usize = 12;

// One request as sent. `count` is THE WIRE VALUE, not an admitted one: admission is
// `drivers::blk::request_range` and it refuses rather than clamps, so a decoder that quietly
// narrowed the count would hide the very thing the refusal exists to catch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Request {
	pub op: u32,
	pub lba: u64,
	pub count: u32,
}

impl Request {
	// Decode a received frame. `None` when the frame is too short to BE a request: a server that
	// indexed sixteen bytes out of a shorter message would either panic or read whatever the
	// receive buffer held from the message before it.
	//
	// A LONGER FRAME IS ACCEPTED, and that is deliberate rather than lax. The servers receive into a
	// fixed buffer and check `len >= 16`; holding this decoder to equality would refuse frames those
	// servers accept today, which would be this module changing the protocol while claiming to
	// record it.
	pub fn decode(bytes: &[u8]) -> Option<Request> {
		let head: &[u8; REQUEST_LEN] = bytes.get(..REQUEST_LEN)?.try_into().ok()?;
		Some(Request::from_bytes(head))
	}

	// The same decode for a caller that already HAS the sixteen bytes as an array. It cannot fail
	// and so it does not return an option: a server holding a `&[u8; 16]` that had to unwrap one
	// would carry a panic on a branch its own type makes unreachable, and an unreachable panic in a
	// driver is still a panic in a driver.
	pub fn from_bytes(bytes: &[u8; REQUEST_LEN]) -> Request {
		let mut op = [0u8; 4];
		op.copy_from_slice(&bytes[0..4]);
		let mut lba = [0u8; 8];
		lba.copy_from_slice(&bytes[4..12]);
		let mut count = [0u8; 4];
		count.copy_from_slice(&bytes[12..16]);
		Request { op: u32::from_le_bytes(op), lba: u64::from_le_bytes(lba), count: u32::from_le_bytes(count) }
	}

	pub fn encode(&self) -> [u8; REQUEST_LEN] {
		let mut out = [0u8; REQUEST_LEN];
		out[0..4].copy_from_slice(&self.op.to_le_bytes());
		out[4..12].copy_from_slice(&self.lba.to_le_bytes());
		out[12..16].copy_from_slice(&self.count.to_le_bytes());
		out
	}

	// Whether this operation carries a memory object FROM the client with the request. Only a write
	// does. A server reads this rather than testing the op inline, because the two places that get
	// it wrong are opposite and both silent: a server that expects no handle on a write leaks the
	// client's object, and one that expects a handle on a read closes something nobody sent.
	pub fn carries_source(op: u32) -> bool {
		op == OP_WRITE
	}

	// Whether this operation answers WITH a memory object. Only a read does.
	pub fn answers_with_data(op: u32) -> bool {
		op == OP_READ
	}
}

// The ordinary reply.
pub fn reply(status: u32) -> [u8; REPLY_LEN] {
	status.to_le_bytes()
}

// The capacity reply. `max_sectors` is saturated into the `u32` the wire carries rather than
// truncated: a server whose bound exceeds four billion sectors would otherwise publish a small
// number, and a client would then send requests the server refuses for a reason it never stated.
pub fn capacity_reply(bytes: u64, max_sectors: u64) -> [u8; CAPACITY_REPLY_LEN] {
	let mut out = [0u8; CAPACITY_REPLY_LEN];
	out[0..4].copy_from_slice(&STATUS_OK.to_le_bytes());
	out[4..12].copy_from_slice(&bytes.to_le_bytes());
	out[12..16].copy_from_slice(&(max_sectors.min(u32::MAX as u64) as u32).to_le_bytes());
	out
}

// What a client learned from a capacity reply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capacity {
	pub bytes: u64,
	pub max_sectors: u32,
}

// Read a status out of any reply. `None` for a frame too short to hold one.
pub fn decode_status(bytes: &[u8]) -> Option<u32> {
	if bytes.len() < REPLY_LEN {
		return None;
	}
	let mut status = [0u8; 4];
	status.copy_from_slice(&bytes[0..4]);
	Some(u32::from_le_bytes(status))
}

// The medium's size alone, from a reply that may predate the per-request bound. Same refusals as
// below: too short, or a status that is not `STATUS_OK`.
pub fn decode_capacity_bytes(bytes: &[u8]) -> Option<u64> {
	if bytes.len() < CAPACITY_SIZE_LEN || decode_status(bytes)? != STATUS_OK {
		return None;
	}
	let mut size = [0u8; 8];
	size.copy_from_slice(&bytes[4..12]);
	Some(u64::from_le_bytes(size))
}

// Read a capacity reply. `None` when the frame is too short, and `None` AGAIN when the status is not
// `STATUS_OK` - a failed capacity query carries no size, and the eight bytes where one would be are
// whatever the server's reply buffer held. A client that read them anyway would mount a medium
// whose size it invented.
pub fn decode_capacity(bytes: &[u8]) -> Option<Capacity> {
	if bytes.len() < CAPACITY_REPLY_LEN {
		return None;
	}
	let size = decode_capacity_bytes(bytes)?;
	let mut most = [0u8; 4];
	most.copy_from_slice(&bytes[12..16]);
	Some(Capacity { bytes: size, max_sectors: u32::from_le_bytes(most) })
}

#[cfg(test)]
mod tests;

// THE LOCAL STREAM WIRE, which a `local-stream` provider serves and a consumer speaks.
//
// One driver publishes it today - virtio-vsock - and the wire is here rather than in that driver for
// the reason the block wire is here: a contract written inside its first implementation is copied by
// its second, and this tree has already paid for that once with three divergent copies of the block
// request.
//
// A LOCAL STREAM IS NOT A SOCKET API. There is no listen, no accept and no address family: a
// consumer names a PORT on the one peer the driver is attached to, and gets bytes or a refusal. The
// item that owns the driver is explicit that this must not become an ambient bypass around
// NetworkService, and a wire that cannot express a remote address cannot be used as one.
//
// This module decides and never sends.

/// `[op u32][arg u32]`, then the payload for a send.
pub const REQUEST_LEN: usize = 8;
/// `[status u32][len u32]`, then the payload for a receive.
pub const REPLY_LEN: usize = 8;

/// Open the connection this endpoint carries. `arg` is the peer's port.
pub const OP_CONNECT: u32 = 0;
/// Write the payload that follows the header. `arg` is its length.
pub const OP_SEND: u32 = 1;
/// Read up to `arg` bytes. The reply's payload is what came back, and a length of zero with
/// `STATUS_OK` is "nothing yet" rather than end of stream, which is `STATUS_CLOSED`.
pub const OP_RECEIVE: u32 = 2;
/// Stop one or both directions. `arg` carries the shutdown flags.
pub const OP_SHUTDOWN: u32 = 3;
/// WHICH END THIS IS. The answer's payload is the eight-byte context id the peer assigned this
/// machine, little-endian.
///
/// It is part of the wire rather than a diagnostic because a local stream's address is NEGOTIATED:
/// the host chooses the id and the guest is told, so a program that has to be reachable - a
/// provisioning agent waiting to be called, a diagnostic that has to say where it is - cannot know
/// it any other way. A driver that kept it to itself would make every consumer of this transport
/// depend on out-of-band configuration for the one number the device already carries.
pub const OP_IDENTITY: u32 = 4;
/// The context id is eight bytes.
pub const IDENTITY_LEN: usize = 8;

/// Shutdown flags, as a CONSUMER names them: which direction IT is finished with. They are named
/// from this end deliberately - the transport underneath describes a shutdown from the SENDER's
/// point of view, so "I will read no more" and "the peer will send no more" are the same fact
/// spelled from opposite ends, and a wire that passed the device's own flags straight through would
/// close the wrong half of every connection that closed one.
pub const SHUTDOWN_READ: u32 = 1 << 0;
pub const SHUTDOWN_WRITE: u32 = 1 << 1;

pub const STATUS_OK: u32 = 0;
/// The driver tried and the peer or the device refused.
pub const STATUS_ERR: u32 = 1;
/// The request does not parse, names a length this wire cannot carry, or asks for something in a
/// state that cannot answer it.
pub const STATUS_INVALID: u32 = 2;
/// The connection is over in the direction asked about. A distinct answer from `STATUS_ERR`: a
/// consumer retries an error and stops on this one, and collapsing them makes a closed stream look
/// like a failing one for ever.
pub const STATUS_CLOSED: u32 = 3;

/// The largest payload one request or one reply carries. A stream is not a bulk transport: this is
/// sized so a request and its payload fit one page, and a consumer moving more makes more calls.
pub const MAX_PAYLOAD: u32 = 4096 - REQUEST_LEN as u32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Request {
	pub op: u32,
	pub arg: u32,
}

impl Request {
	/// Decode a request and the payload that came with it.
	///
	/// THE DECLARED LENGTH IS CHECKED AGAINST WHAT ARRIVED. A send whose header claims four
	/// kilobytes in a message that carried eight bytes is refused rather than read past: the caller
	/// is another address space, and its header is a claim.
	pub fn decode(bytes: &[u8]) -> Option<(Request, &[u8])> {
		if bytes.len() < REQUEST_LEN {
			return None;
		}
		let op = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
		let arg = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
		let payload = &bytes[REQUEST_LEN..];
		if op == OP_SEND {
			if arg > MAX_PAYLOAD || arg as usize != payload.len() {
				return None;
			}
		} else if !payload.is_empty() {
			return None;
		}
		Some((Request { op, arg }, payload))
	}

	pub fn encode(&self) -> [u8; REQUEST_LEN] {
		let mut out = [0u8; REQUEST_LEN];
		out[0..4].copy_from_slice(&self.op.to_le_bytes());
		out[4..8].copy_from_slice(&self.arg.to_le_bytes());
		out
	}
}

pub fn reply(status: u32, len: u32) -> [u8; REPLY_LEN] {
	let mut out = [0u8; REPLY_LEN];
	out[0..4].copy_from_slice(&status.to_le_bytes());
	out[4..8].copy_from_slice(&len.to_le_bytes());
	out
}

/// Decode a reply: its status, the count it carries, and its payload.
///
/// WHAT `len` COUNTS DEPENDS ON THE REQUEST, and there are exactly two cases. A receive answers with
/// bytes, and then `len` is how many of them there are. A send answers with a count and no bytes -
/// HOW MUCH OF THE WRITE WENT, which is not always all of it, because the peer's window is finite
/// and a short write is an honest answer where a dropped tail is not.
///
/// So the rule checked here is the one that holds in both cases: A REPLY THAT CARRIES BYTES MUST
/// COUNT THEM. A reply carrying bytes and claiming a different number is refused, which is the
/// framing error; a reply carrying none is a count, and the caller knows what it asked for.
pub fn decode_reply(bytes: &[u8]) -> Option<(u32, u32, &[u8])> {
	if bytes.len() < REPLY_LEN {
		return None;
	}
	let status = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
	let len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
	let payload = &bytes[REPLY_LEN..];
	if len > MAX_PAYLOAD || payload.len() > MAX_PAYLOAD as usize {
		return None;
	}
	if !payload.is_empty() && len as usize != payload.len() {
		return None;
	}
	Some((status, len, payload))
}

#[cfg(test)]
mod tests;

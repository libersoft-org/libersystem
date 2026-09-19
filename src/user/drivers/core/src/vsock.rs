// THE VIRTIO-VSOCK DECISIONS, WITH NO DEVICE BEHIND THEM.
//
// vsock is a stream transport between a guest and its host, addressed by a context id and a port
// rather than by an IP address. Its packets are a fixed header and a payload, and almost everything
// that can go wrong with it is arithmetic on numbers the PEER chose: the credit window, the sequence
// of bytes each side believes the other has taken, and the connection state a packet's operation
// moves it to.
//
// CREDIT IS WHERE A VSOCK DRIVER IS WRONG, and it is wrong silently in both directions. Too much
// credit believed and the guest sends bytes the host has no room for, which the host drops; too
// little and the connection stalls with neither side at fault. The window is computed from two
// free-running counters that WRAP, so the arithmetic has to be modular or a connection dies after
// four gibibytes.
//
// The item that owns this driver is explicit that vsock is not an ambient bypass: it is a transport
// for provisioning, diagnostics and agents, and nothing here grants anything. This module decides;
// it never opens a connection.

/// The packet header is forty-four bytes, and every multi-byte field in it is little-endian.
pub const HEADER_LEN: usize = 44;

/// Header field offsets.
pub const HDR_SRC_CID: usize = 0;
pub const HDR_DST_CID: usize = 8;
pub const HDR_SRC_PORT: usize = 16;
pub const HDR_DST_PORT: usize = 20;
pub const HDR_LEN: usize = 24;
pub const HDR_TYPE: usize = 28;
pub const HDR_OP: usize = 30;
pub const HDR_FLAGS: usize = 32;
pub const HDR_BUF_ALLOC: usize = 36;
pub const HDR_FWD_CNT: usize = 40;

/// The only packet type this driver speaks: a reliable stream.
pub const TYPE_STREAM: u16 = 1;

/// Operations.
pub const OP_INVALID: u16 = 0;
pub const OP_REQUEST: u16 = 1;
pub const OP_RESPONSE: u16 = 2;
pub const OP_RST: u16 = 3;
pub const OP_SHUTDOWN: u16 = 4;
pub const OP_RW: u16 = 5;
pub const OP_CREDIT_UPDATE: u16 = 6;
pub const OP_CREDIT_REQUEST: u16 = 7;

/// Shutdown flags: which direction the peer is closing.
pub const SHUTDOWN_RECEIVE: u32 = 1 << 0;
pub const SHUTDOWN_SEND: u32 = 1 << 1;

/// The context id the host always has. A guest's own id is read from the device configuration, and
/// these two are the ones a driver must never confuse: a packet addressed to the hypervisor's id is
/// not addressed to the host.
pub const CID_HOST: u64 = 2;
pub const CID_HYPERVISOR: u64 = 0;
pub const CID_ANY: u64 = 0xFFFF_FFFF;

/// One packet header, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
	pub src_cid: u64,
	pub dst_cid: u64,
	pub src_port: u32,
	pub dst_port: u32,
	pub len: u32,
	pub kind: u16,
	pub op: u16,
	pub flags: u32,
	/// How large the peer says its receive buffer is.
	pub buf_alloc: u32,
	/// How many bytes the peer says it has taken out of that buffer, free-running and wrapping.
	pub fwd_cnt: u32,
}

impl Header {
	pub fn decode(bytes: &[u8]) -> Option<Header> {
		if bytes.len() < HEADER_LEN {
			return None;
		}
		let u32_at = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
		let u64_at = |at: usize| (u32_at(at) as u64) | ((u32_at(at + 4) as u64) << 32);
		Some(Header { src_cid: u64_at(HDR_SRC_CID), dst_cid: u64_at(HDR_DST_CID), src_port: u32_at(HDR_SRC_PORT), dst_port: u32_at(HDR_DST_PORT), len: u32_at(HDR_LEN), kind: u16::from_le_bytes([bytes[HDR_TYPE], bytes[HDR_TYPE + 1]]), op: u16::from_le_bytes([bytes[HDR_OP], bytes[HDR_OP + 1]]), flags: u32_at(HDR_FLAGS), buf_alloc: u32_at(HDR_BUF_ALLOC), fwd_cnt: u32_at(HDR_FWD_CNT) })
	}

	pub fn encode(&self) -> [u8; HEADER_LEN] {
		let mut out = [0u8; HEADER_LEN];
		out[HDR_SRC_CID..HDR_SRC_CID + 8].copy_from_slice(&self.src_cid.to_le_bytes());
		out[HDR_DST_CID..HDR_DST_CID + 8].copy_from_slice(&self.dst_cid.to_le_bytes());
		out[HDR_SRC_PORT..HDR_SRC_PORT + 4].copy_from_slice(&self.src_port.to_le_bytes());
		out[HDR_DST_PORT..HDR_DST_PORT + 4].copy_from_slice(&self.dst_port.to_le_bytes());
		out[HDR_LEN..HDR_LEN + 4].copy_from_slice(&self.len.to_le_bytes());
		out[HDR_TYPE..HDR_TYPE + 2].copy_from_slice(&self.kind.to_le_bytes());
		out[HDR_OP..HDR_OP + 2].copy_from_slice(&self.op.to_le_bytes());
		out[HDR_FLAGS..HDR_FLAGS + 4].copy_from_slice(&self.flags.to_le_bytes());
		out[HDR_BUF_ALLOC..HDR_BUF_ALLOC + 4].copy_from_slice(&self.buf_alloc.to_le_bytes());
		out[HDR_FWD_CNT..HDR_FWD_CNT + 4].copy_from_slice(&self.fwd_cnt.to_le_bytes());
		out
	}
}

/// How many bytes may be sent to a peer right now.
///
/// THE COUNTERS WRAP AND THE ARITHMETIC MUST WRAP WITH THEM. `sent` is everything this side has ever
/// written and `fwd_cnt` everything the peer says it has taken, both free-running `u32`s: after four
/// gibibytes they roll over, and a subtraction that is not modular produces an enormous number one
/// way and zero the other. The first sends into a buffer that is not there; the second stalls a
/// healthy connection for ever.
///
/// A PEER CLAIMING TO HAVE TAKEN MORE THAN WAS SENT IS REFUSED rather than trusted: that is a window
/// larger than its own buffer, and sending into it is the hostile-credit case the item names.
pub fn may_send(buf_alloc: u32, fwd_cnt: u32, sent: u32) -> Option<u32> {
	let in_flight = sent.wrapping_sub(fwd_cnt);
	if in_flight > buf_alloc {
		return None;
	}
	Some(buf_alloc - in_flight)
}

/// Which of this driver's local ports a packet names, as a slot in its stream table.
///
/// A LOCAL PORT IS `base + slot` BY CONSTRUCTION, so routing an arriving packet to the stream it
/// belongs to is arithmetic rather than a search over open connections - and the port a packet
/// carries is the ONLY thing that distinguishes two streams to the same host on the same context id.
///
/// AND A PORT OUTSIDE THE RANGE BELONGS TO NOBODY. Masking it into the range, which is what a driver
/// indexing by `port % slots` would do, applies a stranger's bytes to whichever stream the remainder
/// lands on; answering `None` is what lets the caller refuse the packet instead.
pub fn slot_of(dst_port: u32, base: u32, slots: u32) -> Option<usize> {
	let offset = dst_port.checked_sub(base)?;
	(offset < slots).then_some(offset as usize)
}

/// The event queue's message: one little-endian `u32`.
pub const EVENT_LEN: usize = 4;

/// The transport was reset. Every connection this driver holds is gone, whatever its own state
/// machine last recorded - the other end of all of them has been taken away underneath it.
pub const EVENT_TRANSPORT_RESET: u32 = 0;

/// What an event buffer says, or `None` for one that is too short.
///
/// THE NUMBER IS ANSWERED RATHER THAN INTERPRETED, and the caller decides which ones it acts on. A
/// driver that treated anything it did not recognise as a reset would close every connection on a
/// message the specification may give a meaning to later, which is the failure mode of reading a
/// closed enumeration out of an open one.
pub fn event(bytes: &[u8]) -> Option<u32> {
	let head = bytes.get(..EVENT_LEN)?;
	Some(u32::from_le_bytes([head[0], head[1], head[2], head[3]]))
}

/// Why a packet is not acted on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Too short to be a header.
	Short,
	/// A packet type this driver does not speak.
	Type,
	/// Its payload length is larger than what arrived, or larger than this driver will hold.
	Length,
	/// It is addressed to a context or port this side is not using.
	NotOurs,
	/// An operation this driver does not model.
	Operation,
}

/// Admit one received packet: it must be a stream packet, addressed here, with a payload that is
/// actually present.
///
/// THE LENGTH IS CHECKED AGAINST WHAT ARRIVED AND NOT ONLY AGAINST A BOUND, which is the difference
/// between refusing a hostile header and reading past the buffer it came in: a header claiming four
/// kilobytes in a packet that carried sixty-four bytes is not a large packet, it is a lie.
pub fn admit(header: &Header, arrived: usize, our_cid: u64, our_port: u32, most: u32) -> Result<(), Refusal> {
	if header.kind != TYPE_STREAM {
		return Err(Refusal::Type);
	}
	if header.dst_cid != our_cid || header.dst_port != our_port {
		return Err(Refusal::NotOurs);
	}
	if header.len > most || (header.len as usize) > arrived.saturating_sub(HEADER_LEN) {
		return Err(Refusal::Length);
	}
	Ok(())
}

/// What a connection is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
	/// Nothing here.
	Closed,
	/// A request has gone out and no response has come back.
	Connecting,
	/// Open in both directions.
	Open,
	/// The peer has stopped sending, or this side has stopped; bytes may still flow the other way.
	HalfClosed { peer_done: bool, we_done: bool },
}

/// What a packet does to a connection.
///
/// A RESET FROM ANY STATE CLOSES IT, including from `Connecting`, which is how a refused connection
/// is told from one that is merely slow. And a SHUTDOWN is directional: a peer that has stopped
/// sending has not stopped receiving, and closing the whole connection on one would lose the bytes
/// this side still had to write.
pub fn advance(state: State, op: u16, flags: u32) -> State {
	match (state, op) {
		(_, OP_RST) => State::Closed,
		(State::Connecting, OP_RESPONSE) => State::Open,
		(State::Connecting, _) => state,
		(State::Open | State::HalfClosed { .. }, OP_SHUTDOWN) => {
			let (peer_done, we_done) = match state {
				State::HalfClosed { peer_done, we_done } => (peer_done, we_done),
				_ => (false, false),
			};
			// The peer's SEND flag means it will send no more, which closes OUR receive direction.
			let peer_done = peer_done || flags & SHUTDOWN_SEND != 0;
			let we_done = we_done || flags & SHUTDOWN_RECEIVE != 0;
			if peer_done && we_done { State::Closed } else { State::HalfClosed { peer_done, we_done } }
		}
		_ => state,
	}
}

/// Whether a connection may still carry payload in each direction.
pub fn may_write(state: State) -> bool {
	matches!(state, State::Open | State::HalfClosed { we_done: false, .. })
}

pub fn may_read(state: State) -> bool {
	matches!(state, State::Open | State::HalfClosed { peer_done: false, .. })
}

#[cfg(test)]
mod tests;

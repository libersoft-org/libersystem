//! How far a connection has actually TRANSMITTED, as opposed to how far it has decided to send.
//!
//! THESE ARE NOT THE SAME NUMBER, and a Packet Too Big is the place where the difference is a
//! correctness question rather than an accounting one. `SND.NXT` moves when the send queue cuts a
//! segment; the segment then goes to the layer below, which may have to resolve the next hop first
//! and RETAIN the frame in a bounded queue while it does. Until that frame reaches the driver, its
//! sequence space has never been on the wire - so no router can have seen it, and a quotation of it
//! is a forgery. Validating against `SND.NXT` accepts that forgery; validating against what was
//! actually handed off does not.
//!
//! AND THE BOUND IS A HIGH-WATER MARK, not a cursor. Go-Back-N rewinds `SND.NXT` to the oldest
//! unacknowledged byte so the queue is cut again at a smaller segment size; the bytes it rewinds
//! over HAVE been transmitted, and a second Packet Too Big about them is legitimate. A bound that
//! rewound with the queue would refuse the second report and leave the flow retrying at a size the
//! path has already refused twice.
//!
//! A COMPLETION FOR A CONNECTION THAT IS GONE ADVANCES NOTHING. A control block is reused: the frame
//! a cancelled or failed operation left in the layer below can complete after the block has been
//! handed to a new connection, and a bound that took the sequence at face value would let one
//! connection's stale handoff declare another connection's unsent data transmitted.

/// The most segments one connection may have waiting on address resolution at once. The queue
/// below is itself bounded per neighbour; this is the share of it one flow can hold.
pub const MAX_HELD: usize = 16;

/// A segment handed to the layer below and not yet on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Held {
	token: u64,
	epoch: u32,
	/// The sequence number after this segment's last byte, FIN included.
	end: u32,
}

/// What a completion did to the bound.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handoff {
	/// The frame reached the driver and the bound moved to the end of its sequence space.
	Transmitted { end: u32 },
	/// It reached the driver, but its sequence space was already covered - a retransmission
	/// completing behind the high-water mark.
	Covered,
	/// It never reached the driver: cancelled, resolution failed, or the interface went away.
	Retired,
	/// No held segment owns this token in this connection's current epoch.
	Unknown,
}

/// One connection's transmitted bound.
#[derive(Clone, Copy, Debug)]
pub struct TransmitBound {
	transmitted: u32,
	epoch: u32,
	held: [Option<Held>; MAX_HELD],
}

impl TransmitBound {
	/// A bound for a connection whose first sequence number is `iss`.
	pub fn new(iss: u32) -> TransmitBound {
		TransmitBound { transmitted: iss, epoch: 0, held: [None; MAX_HELD] }
	}

	/// How far this connection has actually put sequence space on the wire.
	pub fn transmitted(&self) -> u32 {
		self.transmitted
	}

	pub fn epoch(&self) -> u32 {
		self.epoch
	}

	/// How many segments are waiting on the layer below for this connection right now.
	pub fn held(&self) -> usize {
		self.held.iter().filter(|slot| slot.is_some_and(|entry| entry.epoch == self.epoch)).count()
	}

	/// Reuse this control block for a new connection starting at `iss`.
	///
	/// THE OLD ENTRIES ARE KEPT AND MARKED STALE rather than forgotten: their completions are still
	/// coming, and a completion whose token nothing recognises is indistinguishable from a forged
	/// one. Keeping them lets the stale case be REFUSED rather than merely unmatched.
	pub fn reset(&mut self, iss: u32) {
		self.epoch = self.epoch.wrapping_add(1);
		self.transmitted = iss;
	}

	/// A frame that went straight to the driver, ending at `end`.
	pub fn advance(&mut self, end: u32) -> Handoff {
		// SIGNED, BECAUSE SEQUENCE NUMBERS WRAP. An unsigned comparison refuses every segment for
		// half the sequence space, which is a connection that stops accepting Packet Too Big after
		// two gigabytes.
		if (end.wrapping_sub(self.transmitted) as i32) <= 0 {
			return Handoff::Covered;
		}
		self.transmitted = end;
		Handoff::Transmitted { end }
	}

	/// Record a segment the layer below retained while it resolves the next hop.
	///
	/// Returns false when this connection already holds its share, which the caller must treat as a
	/// send that did not happen: the sequence space must not be counted as transmitted.
	pub fn hold(&mut self, token: u64, end: u32) -> bool {
		let entry = Held { token, epoch: self.epoch, end };
		if let Some(slot) = self.held.iter_mut().find(|slot| slot.is_none()) {
			*slot = Some(entry);
			return true;
		}
		// A stale slot is reclaimable: its connection is gone, so its completion can no longer
		// change anything even if it does arrive.
		let stale = self.epoch;
		if let Some(slot) = self.held.iter_mut().find(|slot| slot.is_some_and(|held| held.epoch != stale)) {
			*slot = Some(entry);
			return true;
		}
		false
	}

	/// The layer below reported this token's frame reached the driver.
	pub fn on_sent(&mut self, token: u64) -> Handoff {
		let Some(entry) = self.take(token) else {
			return Handoff::Unknown;
		};
		// A COMPLETION FROM A CONNECTION THAT IS GONE IS NOT THIS CONNECTION'S. Its sequence numbers
		// belong to a different stream and mean nothing in this one.
		if entry.epoch != self.epoch {
			return Handoff::Retired;
		}
		self.advance(entry.end)
	}

	/// The layer below reported this token's frame was cancelled or never sent.
	pub fn on_retired(&mut self, token: u64) -> Handoff {
		match self.take(token) {
			Some(_) => Handoff::Retired,
			None => Handoff::Unknown,
		}
	}

	fn take(&mut self, token: u64) -> Option<Held> {
		let slot = self.held.iter_mut().find(|slot| slot.is_some_and(|held| held.token == token))?;
		slot.take()
	}
}

#[cfg(test)]
mod tests;

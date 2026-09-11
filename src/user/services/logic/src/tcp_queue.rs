//! What a connection still owes the wire: the bytes it has sent and not had acknowledged, the bytes
//! a caller handed it and it has not sent yet, and the FIN at the end of them.
//!
//! THE QUEUE IS THE SENDER. Everything else in this milestone that mentions retransmission depends
//! on something owning the unacknowledged bytes, and the code this replaces owned none of them: it
//! built one segment from the caller's buffer, sent it once, advanced the sequence and forgot it.
//! That is not a slow TCP, it is a lossy one - and it reported the loss as success.
//!
//! WHAT `send` RETURNS, AND WHY IT IS ACCEPTED BYTES. The count is bytes ACCEPTED INTO THIS QUEUE.
//! They are COPIED before the call returns, so a caller may reuse its buffer immediately - the
//! alternative pins a shared-memory handle for the whole retry schedule, which is up to ten minutes.
//! It may be FEWER than offered, which is the backpressure, and zero is a typed `Again` rather than
//! a success. Acknowledgement is deliberately NOT observable here: an operation held open until the
//! peer acknowledges blocks a caller across the entire retransmission schedule and makes "close with
//! data in flight" a state no caller can reach.
//!
//! THE FIN IS SEQUENCE SPACE AND IS HELD LIKE A BYTE. It consumes a sequence number, so the
//! connection owns it exactly as it owns data: it is retransmitted under the same timer, under the
//! same backoff, and the connection is not finished until it is acknowledged.
//!
//! RETRANSMISSION IS GO-BACK-N, which is what a sender without SACK can honestly do: the unsent
//! boundary rewinds to the oldest unacknowledged byte and the queue is sent again from there. The
//! useful consequence is that RESEGMENTATION AFTER A SMALLER PATH MTU is not a separate mechanism -
//! the rewound bytes are cut at whatever segment size is in force when they go out again.

use alloc::vec::Vec;

/// The most unacknowledged bytes ONE connection may hold.
pub const MAX_UNACKED_PER_FLOW: usize = 256 * 1024;

/// The most across the whole service. The queue is told what is left of it rather than keeping the
/// aggregate itself: one connection cannot see the others, and a budget each connection kept its own
/// copy of would be a budget nothing enforced.
pub const MAX_UNACKED_TOTAL: usize = 4 * 1024 * 1024;

/// One segment the queue wants put on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Segment {
	/// The sequence number of its first byte.
	pub sequence: u32,
	/// Where its bytes start in what `pending` returns.
	pub offset: usize,
	pub len: usize,
	/// Whether this segment carries the FIN, which occupies the sequence number after its data.
	pub fin: bool,
	/// Whether these bytes have been on the wire before. Karn's rule refuses a round-trip
	/// measurement from an acknowledgement of one of these.
	pub retransmitted: bool,
}

impl Segment {
	/// How much sequence space it consumes: its bytes, and one more for a FIN.
	pub fn sequence_len(&self) -> u32 {
		self.len as u32 + u32::from(self.fin)
	}
}

/// What an acknowledgement did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AckOutcome {
	/// It covered new sequence space, and this much was retired.
	Advanced { bytes: u32, covered_fin: bool },
	/// The same acknowledgement again with nothing new. Three of these mean loss.
	Duplicate,
	/// Older than what is already acknowledged. Stale, and ignored.
	Old,
	/// Past anything this connection has sent. A peer cannot acknowledge what it has not seen.
	Invalid,
}

/// One connection's transmit queue.
#[derive(Debug)]
pub struct SendQueue {
	/// Unacknowledged and unsent bytes, oldest first. `bytes[0]` is sequence `snd_una`.
	bytes: Vec<u8>,
	snd_una: u32,
	snd_nxt: u32,
	/// A FIN has been queued behind the data, and the sequence number it will occupy once every
	/// queued byte has been sent.
	fin_queued: bool,
	fin_sent: bool,
	fin_acknowledged: bool,
	/// Whether the bytes currently outstanding have been on the wire before.
	retransmitted: bool,
	/// The last acknowledgement seen, for telling a duplicate from a repeat.
	last_ack: u32,
	duplicates: u8,
}

impl SendQueue {
	/// A queue whose first byte will be `iss`.
	pub fn new(iss: u32) -> SendQueue {
		SendQueue { bytes: Vec::new(), snd_una: iss, snd_nxt: iss, fin_queued: false, fin_sent: false, fin_acknowledged: false, retransmitted: false, last_ack: iss, duplicates: 0 }
	}

	pub fn snd_una(&self) -> u32 {
		self.snd_una
	}

	pub fn snd_nxt(&self) -> u32 {
		self.snd_nxt
	}

	/// Bytes sent and not acknowledged - what the congestion window is measured against.
	pub fn flight(&self) -> u32 {
		self.snd_nxt.wrapping_sub(self.snd_una)
	}

	/// Everything still owed: unacknowledged and unsent together.
	pub fn pending(&self) -> &[u8] {
		&self.bytes
	}

	/// Bytes accepted but not yet put on the wire.
	pub fn unsent(&self) -> usize {
		self.bytes.len().saturating_sub(self.flight() as usize)
	}

	pub fn is_empty(&self) -> bool {
		self.bytes.is_empty() && !self.fin_queued
	}

	pub fn fin_queued(&self) -> bool {
		self.fin_queued
	}

	pub fn fin_sent(&self) -> bool {
		self.fin_sent
	}

	pub fn fin_acknowledged(&self) -> bool {
		self.fin_acknowledged
	}

	/// Has the peer acknowledged everything, FIN included?
	pub fn fully_acknowledged(&self) -> bool {
		self.bytes.is_empty() && (!self.fin_queued || self.fin_acknowledged)
	}

	/// Have the bytes currently outstanding been transmitted before?
	pub fn outstanding_was_retransmitted(&self) -> bool {
		self.retransmitted
	}

	pub fn duplicates(&self) -> u8 {
		self.duplicates
	}

	/// Copy as much of `data` as the budgets allow, and say how much was taken.
	///
	/// `aggregate_remaining` is what is left of the service-wide budget. Both ceilings apply, and the
	/// smaller one decides - a connection alone cannot fill the service, and the service's room does
	/// not let one connection past its own limit.
	pub fn accept(&mut self, data: &[u8], aggregate_remaining: usize) -> usize {
		if self.fin_queued {
			// NOTHING GOES BEHIND THE FIN. A byte accepted after it would have to occupy a sequence
			// number the FIN already took, and a caller told "accepted" would believe it was sent.
			return 0;
		}
		let per_flow: usize = MAX_UNACKED_PER_FLOW.saturating_sub(self.bytes.len());
		let take: usize = data.len().min(per_flow).min(aggregate_remaining);
		self.bytes.extend_from_slice(&data[..take]);
		take
	}

	/// Queue the FIN behind everything already accepted.
	pub fn queue_fin(&mut self) -> bool {
		if self.fin_queued {
			return false;
		}
		self.fin_queued = true;
		true
	}

	/// The next segment to put on the wire, at most `smss` bytes and within `usable` of window.
	///
	/// A FIN GOES OUT ALONE ONLY WHEN THERE IS NOTHING LEFT TO CARRY IT, which is what keeps it
	/// behind every byte a caller handed over before closing.
	pub fn next_segment(&mut self, smss: usize, usable: u32) -> Option<Segment> {
		let flight: u32 = self.flight();
		let offset: usize = flight as usize;
		let available: usize = self.bytes.len().saturating_sub(offset);
		let room: usize = (usable as usize).min(smss);
		if available > 0 {
			if room == 0 {
				return None;
			}
			let len: usize = available.min(room);
			let fin: bool = self.fin_queued && !self.fin_sent && len == available;
			let segment = Segment { sequence: self.snd_nxt, offset, len, fin, retransmitted: self.retransmitted };
			self.snd_nxt = self.snd_nxt.wrapping_add(segment.sequence_len());
			self.fin_sent |= fin;
			return Some(segment);
		}
		if self.fin_queued && !self.fin_sent {
			// A FIN NEEDS NO WINDOW. It carries no data, so a closed receive window does not hold it
			// back - which is what lets a connection finish against a peer that has stopped reading.
			let segment = Segment { sequence: self.snd_nxt, offset, len: 0, fin: true, retransmitted: self.retransmitted };
			self.snd_nxt = self.snd_nxt.wrapping_add(1);
			self.fin_sent = true;
			return Some(segment);
		}
		None
	}

	/// Rewind to the oldest unacknowledged byte so everything outstanding is sent again.
	///
	/// GO-BACK-N, AND RESEGMENTATION FALLS OUT OF IT. The bytes go back on the wire cut at whatever
	/// segment size is in force now, so a validated Packet Too Big needs no separate mechanism: lower
	/// the segment size and rewind.
	pub fn rewind(&mut self) {
		self.snd_nxt = self.snd_una;
		self.fin_sent = false;
		self.retransmitted = true;
	}

	/// The peer acknowledged up to `ack`.
	pub fn on_ack(&mut self, ack: u32) -> AckOutcome {
		if ack == self.snd_una {
			// A repeat of what is already known. It is a DUPLICATE only while something is
			// outstanding for it to be pointing at; with an empty flight it is just a window update.
			if self.flight() == 0 {
				return AckOutcome::Old;
			}
			self.duplicates = self.duplicates.saturating_add(1);
			self.last_ack = ack;
			return AckOutcome::Duplicate;
		}
		// Signed differences, because sequence numbers wrap: `ack > snd_una` is wrong for half the
		// space and would retire the whole queue on a stale acknowledgement.
		if (ack.wrapping_sub(self.snd_una) as i32) < 0 {
			return AckOutcome::Old;
		}
		if (self.snd_nxt.wrapping_sub(ack) as i32) < 0 {
			// A peer cannot acknowledge what it has not been sent.
			return AckOutcome::Invalid;
		}
		let mut advanced: u32 = ack.wrapping_sub(self.snd_una);
		let mut covered_fin: bool = false;
		// The FIN occupies the sequence number after the last byte, so an acknowledgement that
		// covers it is one byte longer than the data it retires.
		if self.fin_sent && !self.fin_acknowledged && advanced as usize > self.bytes.len() {
			covered_fin = true;
			self.fin_acknowledged = true;
			advanced -= 1;
		}
		let retire: usize = (advanced as usize).min(self.bytes.len());
		self.bytes.drain(..retire);
		self.snd_una = ack;
		self.duplicates = 0;
		self.last_ack = ack;
		// What is outstanding NOW is whatever was sent beyond this acknowledgement, and that is
		// fresh only when nothing is left over from the retransmission.
		if self.flight() == 0 {
			self.retransmitted = false;
		}
		AckOutcome::Advanced { bytes: advanced + u32::from(covered_fin), covered_fin }
	}
}

#[cfg(test)]
mod tests;

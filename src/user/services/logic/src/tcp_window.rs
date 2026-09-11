//! How much a sender may have on the wire at once, and what loss does to it.
//!
//! RFC 5681's CORE, WITH RFC 6928'S INITIAL WINDOW, AND THE TWO LOSS RESPONSES KEPT APART. That last
//! part is the whole reason this is a module rather than a field: a timeout and three duplicate
//! acknowledgements mean DIFFERENT things about the path, and an implementation with one "halve the
//! window" response applies the fast-recovery shape to timeouts as well - putting half a window back
//! onto a path that had just stopped delivering anything.
//!
//!   A TIMEOUT says nothing is getting through. `ssthresh` takes half the flight and `cwnd` drops to
//!     ONE segment: recovery re-enters slow start from the bottom.
//!   THREE DUPLICATE ACKs say segments ARE getting through - each duplicate is a segment that
//!     arrived - and one is missing. `cwnd` becomes `ssthresh + 3*SMSS`, inflating by one segment
//!     per further duplicate, and returns to `ssthresh` on the acknowledgement that covers what was
//!     outstanding when recovery began.
//!
//! WHAT THIS IS NOT. There is no CUBIC, no BBR, no ECN and no SACK-based recovery here; that is the
//! "advanced congestion control" this milestone refuses. The line it does hold is that a sender must
//! not ignore loss and must not ignore the peer's window.

/// RFC 6928 section 2's byte cap on the initial window. Ten segments of a large MSS is not what that
/// document specifies, and the cap is what makes the difference.
pub const INITIAL_WINDOW_CAP: u32 = 14_600;

/// How many duplicate acknowledgements mean loss rather than reordering.
pub const DUPLICATE_ACK_THRESHOLD: u8 = 3;

/// The initial congestion window: `min(10*SMSS, max(2*SMSS, 14600))`, RFC 6928 section 2.
pub fn initial_window(smss: u32) -> u32 {
	(10 * smss).min((2 * smss).max(INITIAL_WINDOW_CAP))
}

/// What a duplicate acknowledgement asked the sender to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DuplicateAction {
	/// Fewer than three: reordering, so far as this sender can tell. Nothing to do.
	Wait,
	/// The third: retransmit the segment the duplicates are pointing at, and enter fast recovery.
	FastRetransmit,
	/// A further duplicate while in recovery: another segment left the network, so one more may be
	/// sent.
	Inflate,
}

/// One connection's congestion state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CongestionWindow {
	smss: u32,
	cwnd: u32,
	ssthresh: u32,
	duplicates: u8,
	/// Where the flight ended when fast recovery began. Recovery ends on the acknowledgement that
	/// covers it, which is what stops a burst of duplicates from holding the window inflated.
	recovery_point: Option<u32>,
	/// Bytes acknowledged toward the next one-segment increase in congestion avoidance, where the
	/// window grows by one segment per round trip rather than per acknowledgement.
	toward_next_segment: u32,
}

impl CongestionWindow {
	pub fn new(smss: u32) -> CongestionWindow {
		let smss: u32 = smss.max(1);
		CongestionWindow {
			smss,
			cwnd: initial_window(smss),
			// UNBOUNDED UNTIL LOSS SAYS OTHERWISE. RFC 5681 section 3.1 permits an arbitrarily high
			// initial value; what matters is that the connection starts in slow start, which this
			// gives without a second flag saying so.
			ssthresh: u32::MAX,
			duplicates: 0,
			recovery_point: None,
			toward_next_segment: 0,
		}
	}

	pub fn cwnd(&self) -> u32 {
		self.cwnd
	}

	pub fn ssthresh(&self) -> u32 {
		self.ssthresh
	}

	pub fn smss(&self) -> u32 {
		self.smss
	}

	pub fn in_recovery(&self) -> bool {
		self.recovery_point.is_some()
	}

	pub fn duplicates(&self) -> u8 {
		self.duplicates
	}

	/// Is the sender in slow start?
	pub fn in_slow_start(&self) -> bool {
		self.cwnd < self.ssthresh
	}

	/// A smaller path MTU lowers the segment size, and the window is expressed in segments as much
	/// as in bytes - so the floor a loss response drops to moves with it.
	pub fn set_smss(&mut self, smss: u32) {
		self.smss = smss.max(1);
	}

	/// How many more bytes may be put on the wire, given what is already outstanding and what the
	/// peer is willing to hold.
	///
	/// THE SMALLER OF THE TWO WINDOWS, ALWAYS. The congestion window is what the PATH will take and
	/// the advertised window is what the PEER will take; a sender that respected only one of them
	/// would either congest the path or overrun the receiver.
	pub fn usable(&self, flight: u32, peer_window: u32) -> u32 {
		self.cwnd.min(peer_window).saturating_sub(flight)
	}

	/// A retransmission timeout: nothing is getting through.
	pub fn on_timeout(&mut self, flight: u32) {
		self.ssthresh = (flight / 2).max(2 * self.smss);
		self.cwnd = self.smss;
		self.duplicates = 0;
		self.recovery_point = None;
		self.toward_next_segment = 0;
	}

	/// A duplicate acknowledgement arrived.
	pub fn on_duplicate_ack(&mut self, flight: u32, snd_nxt: u32) -> DuplicateAction {
		if self.recovery_point.is_some() {
			// Each further duplicate is one more segment that has LEFT the network, so the window
			// may hold one more.
			self.cwnd = self.cwnd.saturating_add(self.smss);
			return DuplicateAction::Inflate;
		}
		self.duplicates = self.duplicates.saturating_add(1);
		if self.duplicates < DUPLICATE_ACK_THRESHOLD {
			return DuplicateAction::Wait;
		}
		self.ssthresh = (flight / 2).max(2 * self.smss);
		self.cwnd = self.ssthresh.saturating_add(3 * self.smss);
		self.recovery_point = Some(snd_nxt);
		self.toward_next_segment = 0;
		DuplicateAction::FastRetransmit
	}

	/// An acknowledgement advanced the sequence space by `acked` bytes, to `ack`.
	pub fn on_new_ack(&mut self, acked: u32, ack: u32) {
		self.duplicates = 0;
		if let Some(point) = self.recovery_point {
			// RECOVERY ENDS WHEN WHAT WAS OUTSTANDING IS COVERED, not on the first new
			// acknowledgement: a partial one still leaves the hole that started it.
			if !sequence_reaches(ack, point) {
				return;
			}
			self.cwnd = self.ssthresh;
			self.recovery_point = None;
			self.toward_next_segment = 0;
			return;
		}
		if self.cwnd < self.ssthresh {
			// Slow start: one more segment's worth per acknowledgement, and never more than one
			// segment per acknowledgement however much it covered.
			self.cwnd = self.cwnd.saturating_add(acked.min(self.smss));
			return;
		}
		// Congestion avoidance: one segment per ROUND TRIP, which is one segment per `cwnd` bytes
		// acknowledged rather than one per acknowledgement.
		self.toward_next_segment = self.toward_next_segment.saturating_add(acked);
		if self.toward_next_segment >= self.cwnd {
			self.toward_next_segment -= self.cwnd;
			self.cwnd = self.cwnd.saturating_add(self.smss);
		}
	}
}

/// The zero-window probe schedule.
///
/// PERSIST IS ITS OWN MECHANISM AND NOT A RETRANSMISSION, and confusing the two is a correctness
/// failure rather than a performance one. When a peer advertises a zero window the sender has nothing
/// outstanding - so no retransmission timer is running, and nothing will ever fire. The window
/// reopens with an acknowledgement that carries no data, and an acknowledgement can be LOST. Without
/// a probe the connection then waits for an update that will never be repeated: both sides are
/// healthy, neither is at fault, and the connection is deadlocked permanently.
///
/// IT NEVER GIVES UP, which is the other half of the point. The retry limit exists to end a
/// connection whose peer has stopped answering; a peer advertising a zero window IS answering, and is
/// entitled to keep its window shut for as long as its reader is busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Persist {
	due_ms: Option<u64>,
	interval_ms: u32,
}

impl Default for Persist {
	fn default() -> Persist {
		Persist::new()
	}
}

impl Persist {
	pub fn new() -> Persist {
		Persist { due_ms: None, interval_ms: 0 }
	}

	/// When the next probe is due, or `None` while the window is open.
	pub fn due_ms(&self) -> Option<u64> {
		self.due_ms
	}

	pub fn running(&self) -> bool {
		self.due_ms.is_some()
	}

	pub fn interval_ms(&self) -> u32 {
		self.interval_ms
	}

	/// The peer advertised `window` while `waiting` bytes are owed.
	///
	/// A window of zero with nothing to send is not a deadlock - there is nothing the update would
	/// release - so the schedule starts only when this side actually has something waiting on it.
	pub fn on_window(&mut self, window: u32, waiting: usize, now_ms: u64, rto_ms: u32) {
		if window > 0 || waiting == 0 {
			self.due_ms = None;
			self.interval_ms = 0;
			return;
		}
		if self.due_ms.is_none() {
			self.interval_ms = rto_ms.clamp(crate::tcp_rto::MIN_RTO_MS, crate::tcp_rto::MAX_RTO_MS);
			self.due_ms = Some(now_ms + u64::from(self.interval_ms));
		}
	}

	/// A probe is due: send one, and wait longer for the next.
	pub fn fire(&mut self, now_ms: u64) -> bool {
		let Some(due) = self.due_ms else {
			return false;
		};
		if now_ms < due {
			return false;
		}
		self.interval_ms = self.interval_ms.saturating_mul(2).min(crate::tcp_rto::MAX_RTO_MS);
		self.due_ms = Some(now_ms + u64::from(self.interval_ms));
		true
	}
}

/// Has the sequence space reached `point`, in arithmetic that survives the wrap?
///
/// A COMPARISON AND NOT A SUBTRACTION ORDER. Sequence numbers wrap, so `ack >= point` is wrong for
/// half the space; the difference read as a signed quantity is the standard's own test.
pub fn sequence_reaches(ack: u32, point: u32) -> bool {
	(ack.wrapping_sub(point) as i32) >= 0
}

#[cfg(test)]
mod tests;

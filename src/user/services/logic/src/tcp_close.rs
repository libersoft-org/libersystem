//! How a connection finishes, and why one side has to wait afterwards.
//!
//! THE CLOSING HANDSHAKE IS NOT SYMMETRIC, AND THE ASYMMETRY IS THE WHOLE DIFFICULTY. Each side
//! sends a FIN and each acknowledges the other's, but the LAST acknowledgement of the exchange is
//! never itself acknowledged - nothing can be, or the exchange would not end. So the side that sends
//! it cannot know it arrived, and if it frees its control block immediately it has nothing left to
//! answer the peer's retransmitted FIN with. The peer then retransmits until it gives up and reports
//! a connection failure for a conversation that completed perfectly.
//!
//! TCP'S ANSWER IS A STATE AND NOT A QUEUE ENTRY. TIME-WAIT retains the control block, with the
//! four-tuple reserved, for twice the maximum segment lifetime - long enough that any copy of the
//! peer's FIN still in the network has expired.
//!
//! THE CONDITION IS STATED LOCALLY, because "the side that sent the first FIN" is not a local fact.
//! Under a simultaneous close both endpoints send from ESTABLISHED and each receives the other's
//! before its own is acknowledged; a rule phrased on "first" lets BOTH conclude they are not it and
//! free immediately, losing the recovery on both sides. The condition here is decidable from what
//! one endpoint has sent and received: it has SENT a FIN, had it ACKNOWLEDGED, and has ACKNOWLEDGED
//! the peer's. That is the same condition RFC 9293 section 3.6 Figure 13 draws.
//!
//! A RETAINED CONTROL BLOCK IS A CONTROL BLOCK. It holds its budget entry and its scheduler deadline
//! until the timer expires, and a simultaneous close holds TWO where an ordinary close holds one.
//! That is the honest cost of the state, and it is written here rather than discovered when the
//! budget will not close.

/// The maximum segment lifetime this profile assumes.
///
/// RFC 9293 section 3.4.2 names two minutes as the value the specification assumes and permits an
/// implementation to choose a smaller one. Thirty seconds is what this milestone's fixtures can wait
/// out, and it is written here for the same reason every other number in this profile is.
pub const MSL_MS: u64 = 30_000;

/// How long a control block is retained after the closing handshake completes.
pub const TIME_WAIT_MS: u64 = 2 * MSL_MS;

/// Where one endpoint is in the closing handshake.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CloseState {
	/// Open in both directions.
	Established,
	/// This side sent a FIN and it is not acknowledged; the peer's has not arrived.
	FinWait1,
	/// This side's FIN is acknowledged; the peer's has not arrived.
	FinWait2,
	/// The FINs crossed: the peer's arrived while this side's was still unacknowledged.
	Closing,
	/// The peer closed first and this side has not.
	CloseWait,
	/// The peer closed first, this side answered with its own FIN, and that FIN is outstanding.
	LastAck,
	/// Both FINs are sent and acknowledged. The control block is retained until this instant.
	TimeWait { until_ms: u64 },
	/// Gone.
	Closed,
}

/// One endpoint's view of the closing handshake.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Closing {
	state: CloseState,
	fin_sent: bool,
	fin_acknowledged: bool,
	peer_fin_seen: bool,
}

impl Default for Closing {
	fn default() -> Closing {
		Closing::new()
	}
}

impl Closing {
	pub fn new() -> Closing {
		Closing { state: CloseState::Established, fin_sent: false, fin_acknowledged: false, peer_fin_seen: false }
	}

	pub fn state(&self) -> CloseState {
		self.state
	}

	/// Is the control block still owed - held, accounted, and holding its four-tuple?
	///
	/// A CONNECTION IN TIME-WAIT IS STILL HELD. Reporting it as gone is how a budget comes to
	/// disagree with what the system is actually holding.
	pub fn retained(&self) -> bool {
		self.state != CloseState::Closed
	}

	/// When this endpoint next needs waking, if it does.
	pub fn deadline_ms(&self) -> Option<u64> {
		match self.state {
			CloseState::TimeWait { until_ms } => Some(until_ms),
			_ => None,
		}
	}

	/// This side sent its FIN.
	pub fn on_fin_sent(&mut self, now_ms: u64) {
		if self.fin_sent {
			return;
		}
		self.fin_sent = true;
		self.state = match self.state {
			// The peer closed first, so this FIN is the last segment of the exchange and its
			// acknowledgement ends the connection here - no TIME-WAIT on this side.
			CloseState::CloseWait => CloseState::LastAck,
			CloseState::Established => CloseState::FinWait1,
			other => other,
		};
		let _ = now_ms;
	}

	/// The peer acknowledged this side's FIN.
	pub fn on_fin_acknowledged(&mut self, now_ms: u64) {
		if !self.fin_sent || self.fin_acknowledged {
			return;
		}
		self.fin_acknowledged = true;
		self.state = match self.state {
			CloseState::FinWait1 => CloseState::FinWait2,
			// The crossed-FIN path: this side already acknowledged the peer's FIN, so both are now
			// accounted for and the wait begins.
			CloseState::Closing => CloseState::TimeWait { until_ms: now_ms + TIME_WAIT_MS },
			CloseState::LastAck => CloseState::Closed,
			other => other,
		};
	}

	/// A FIN arrived from the peer. Returns whether it must be acknowledged.
	///
	/// A FIN ARRIVING IN TIME-WAIT IS THE LOST-FINAL-ACK CASE: the peer never saw this side's last
	/// acknowledgement and is asking again. It is answered, and the timer RESTARTS - which is why the
	/// wait is a timer that can be pushed out rather than one that runs once.
	pub fn on_peer_fin(&mut self, now_ms: u64) -> bool {
		match self.state {
			CloseState::TimeWait { .. } => {
				self.state = CloseState::TimeWait { until_ms: now_ms + TIME_WAIT_MS };
				true
			}
			CloseState::Established => {
				self.peer_fin_seen = true;
				self.state = CloseState::CloseWait;
				true
			}
			CloseState::FinWait1 => {
				self.peer_fin_seen = true;
				self.state = CloseState::Closing;
				true
			}
			CloseState::FinWait2 => {
				self.peer_fin_seen = true;
				self.state = CloseState::TimeWait { until_ms: now_ms + TIME_WAIT_MS };
				true
			}
			// A repeat in a state that has already seen one is still answered: the peer is asking
			// because it did not hear, and silence would leave it retransmitting until it gave up.
			CloseState::CloseWait | CloseState::Closing | CloseState::LastAck => true,
			CloseState::Closed => false,
		}
	}

	/// Time passed. Returns true when the control block may finally be released.
	pub fn tick(&mut self, now_ms: u64) -> bool {
		if let CloseState::TimeWait { until_ms } = self.state
			&& now_ms >= until_ms
		{
			self.state = CloseState::Closed;
			return true;
		}
		self.state == CloseState::Closed
	}

	/// Has the peer closed its half?
	pub fn peer_finished(&self) -> bool {
		self.peer_fin_seen
	}
}

#[cfg(test)]
mod tests;

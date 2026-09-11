//! Trying a name's addresses one at a time, and knowing which attempt an answer belongs to.
//!
//! SEQUENTIAL AND NOT A RACE. One active attempt at a time is what makes the budgets mean anything: a
//! parallel open charges every candidate at once, and a caller's RPC deadline becomes the fallback
//! mechanism - which is a fallback nobody wrote down and nobody can reason about.
//!
//! A NONFINAL CANDIDATE EXPIRES AFTER THREE SECONDS, including the time spent resolving its next hop,
//! and the clock is NOT restarted by a retransmission or by re-selecting an unsent source. The FINAL
//! candidate - a singleton literal included - gets the complete SYN schedule instead, because there
//! is nothing left to fall back to and abandoning it early would abandon the connection.
//!
//! AND A LATE EVENT FROM A RETIRED ATTEMPT CHANGES NOTHING. A SYN-ACK, a quoted error or a send
//! completion from an attempt that has already been given up on arrives after its successor has
//! started; a sequence that acted on it would hand the caller a socket to the wrong peer.

/// How long a nonfinal candidate gets.
pub const NONFINAL_MS: u64 = 3000;

/// What the caller should do next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
	/// Start this candidate. `deadline_ms` is `None` for the final one, which runs the full schedule.
	Start { index: usize, deadline_ms: Option<u64> },
	/// An attempt is running and its deadline has not passed.
	Waiting,
	/// One of them connected.
	Connected { index: usize },
	/// Every candidate has been tried and none answered.
	Exhausted,
}

/// One open, over a caller's ordered candidates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sequence {
	candidates: usize,
	/// Which candidate is running, and when it must be given up on.
	active: Option<usize>,
	deadline_ms: Option<u64>,
	next: usize,
	winner: Option<usize>,
	exhausted: bool,
	/// How many attempts have been started, for a caller asserting that they do not overlap.
	started: usize,
}

impl Sequence {
	pub fn new(candidates: usize) -> Sequence {
		Sequence { candidates, active: None, deadline_ms: None, next: 0, winner: None, exhausted: false, started: 0 }
	}

	pub fn active(&self) -> Option<usize> {
		self.active
	}

	pub fn winner(&self) -> Option<usize> {
		self.winner
	}

	pub fn started(&self) -> usize {
		self.started
	}

	pub fn deadline_ms(&self) -> Option<u64> {
		self.deadline_ms
	}

	/// Start the next candidate, if there is one and nothing is running.
	pub fn begin(&mut self, now_ms: u64) -> Step {
		if let Some(index) = self.winner {
			return Step::Connected { index };
		}
		if self.active.is_some() {
			return Step::Waiting;
		}
		if self.next >= self.candidates {
			self.exhausted = true;
			return Step::Exhausted;
		}
		let index: usize = self.next;
		self.next += 1;
		self.started += 1;
		// THE FINAL CANDIDATE HAS NO CAP, because there is nothing left to fall back to.
		self.deadline_ms = (index + 1 < self.candidates).then_some(now_ms + NONFINAL_MS);
		self.active = Some(index);
		Step::Start { index, deadline_ms: self.deadline_ms }
	}

	/// Candidate `index` connected.
	///
	/// A LATE SUCCESS FROM A RETIRED ATTEMPT IS NOT A WINNER. It arrives after its successor started,
	/// and acting on it would hand the caller a socket to a peer it had already given up on.
	pub fn on_connected(&mut self, index: usize) -> bool {
		if self.active != Some(index) || self.winner.is_some() {
			return false;
		}
		self.winner = Some(index);
		self.active = None;
		self.deadline_ms = None;
		true
	}

	/// Candidate `index` failed - refused, unreachable, or out of schedule.
	///
	/// AN EXPLICIT REFUSAL NEED NOT WAIT FOR A TIMEOUT: a candidate with no usable path or an
	/// incompatible explicit source fails with its typed reason and advances immediately.
	pub fn on_failed(&mut self, index: usize, now_ms: u64) -> Step {
		if self.active != Some(index) {
			return Step::Waiting;
		}
		self.active = None;
		self.deadline_ms = None;
		self.begin(now_ms)
	}

	/// Time passed. A nonfinal candidate whose deadline has arrived is retired and the next starts.
	///
	/// EXPIRY TAKES PRECEDENCE OVER A RETRANSMISSION DUE AT THE SAME INSTANT: at the cap the service
	/// retires this candidate and starts the next rather than sending another SYN.
	pub fn on_timer(&mut self, now_ms: u64) -> Step {
		let Some(index) = self.active else {
			return self.begin(now_ms);
		};
		match self.deadline_ms {
			Some(deadline) if now_ms >= deadline => self.on_failed(index, now_ms),
			_ => Step::Waiting,
		}
	}

	/// How long this open may take in total, in milliseconds.
	///
	/// DERIVED FROM THE SAME PARAMETERS AS THE SCHEDULE ITSELF: every nonfinal candidate costs its
	/// cap, and the last costs the full SYN schedule. Two candidates are 3000 + 183000 = 186000, and
	/// a singleton is 183000 - which is the assertion that tells this rule apart from one that capped
	/// the last candidate too.
	pub fn total_deadline_ms(&self) -> u64 {
		let nonfinal: u64 = (self.candidates.saturating_sub(1)) as u64 * NONFINAL_MS;
		nonfinal + u64::from(crate::tcp_rto::syn_schedule_ms())
	}
}

#[cfg(test)]
mod tests;

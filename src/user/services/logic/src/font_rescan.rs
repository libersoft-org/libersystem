//! When a RECOVERY rescan may run, and when it is refused.
//!
//! RESCAN IS NOT ON THE CLIENT INTERFACE. The normal trigger for re-reading the font directory is
//! the volume watch; an explicit rescan exists only as RECOVERY, after a dropped hint, and it is
//! held by an operator tool rather than by every font client. Authority to READ a font must not
//! become authority to make the machine work: a full directory read, digest and metadata pass is
//! real CPU and I/O, and the live-endpoint ceiling bounds ENDPOINTS rather than requests on one.
//!
//! TWO BOUNDS, BECAUSE ONE OF THEM LEFT A HOLE:
//!
//! ```text
//!   HOW OFTEN A SCAN MAY RUN    at most one in flight, and every COMPLETED scan - published,
//!                                 unchanged or failed - re-arms a bounded delay. A request inside
//!                                 the delay is refused with `try-again-at`. This is the WORK bound
//!                                 and it does not care what the scan found
//!   HOW MANY MAY PUBLISH        one per published generation: a scan that publishes spends the
//!                                 allowance for the generation it replaced, and one that does not
//!                                 publish does not. This is the CHURN bound, and it is what keeps a
//!                                 recovery scan available after a bad directory is corrected
//! ```
//!
//! THE FIRST ANSWER COUNTED ONLY SUCCESSFUL PUBLICATIONS and gave only a FAILED scan a delay - and a
//! scan of an UNCHANGED directory is neither, so an admin caller could ask for a full directory pass
//! in a loop and nothing counted it. Every completed scan re-arms the delay here, whatever it found.
//!
//! AND A FAILED SCAN MUST NOT SPEND THE ALLOWANCE, or the first recovery scan after a bad drop would
//! observe the bad directory, spend the generation's only attempt, and leave no way to scan again
//! once an operator fixed the files - the failure the operation exists for making the operation
//! unusable.

/// Why a rescan was not started.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// A scan is already running. At most one is in flight.
	InFlight,
	/// Inside the delay a completed scan re-armed. The value is when the next one may start.
	TryAgainAt(u64),
	/// This generation's publication allowance is spent.
	AlreadyPublished,
}

/// What a completed scan did, as the allowance counts it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Completion {
	/// It published a new generation, replacing the one named.
	Published { replaced: u64 },
	/// It ran and found nothing different.
	Unchanged,
	/// It ran and refused to publish - a ceiling, or a directory it could not read.
	Failed,
}

/// The allowance, as the catalogue holds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Allowance {
	/// How long a completed scan blocks the next one.
	delay: u64,
	in_flight: bool,
	next_allowed_at: u64,
	/// The generation whose publication allowance is spent, if any.
	spent_for: Option<u64>,
}

impl Allowance {
	pub const fn new(delay: u64) -> Self {
		Self { delay, in_flight: false, next_allowed_at: 0, spent_for: None }
	}

	pub const fn in_flight(&self) -> bool {
		self.in_flight
	}

	pub const fn next_allowed_at(&self) -> u64 {
		self.next_allowed_at
	}

	/// May a rescan start now, with `published` the generation currently published?
	pub fn request(&mut self, now: u64, published: u64) -> Result<(), Refusal> {
		if self.in_flight {
			return Err(Refusal::InFlight);
		}
		if now < self.next_allowed_at {
			return Err(Refusal::TryAgainAt(self.next_allowed_at));
		}
		// THE CHURN BOUND. A publication advances the generation, so in the ordinary course this
		// never bites - which is the point: it is the invariant that ONE generation cannot be
		// replaced twice, and the delay above is what actually paces an operator. It bites if a
		// publication ever leaves the generation where it was, and refusing is the right answer
		// there rather than discovering it later.
		if self.spent_for == Some(published) {
			return Err(Refusal::AlreadyPublished);
		}
		self.in_flight = true;
		Ok(())
	}

	/// A scan finished. EVERY completion re-arms the delay; only a publication spends the allowance.
	pub fn completed(&mut self, now: u64, completion: Completion) {
		self.in_flight = false;
		self.next_allowed_at = now.saturating_add(self.delay);
		if let Completion::Published { replaced } = completion {
			self.spent_for = Some(replaced);
		}
	}
}

#[cfg(test)]
#[path = "font_rescan/tests.rs"]
mod tests;

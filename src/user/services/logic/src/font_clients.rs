//! What bounds the catalogue's PER-CLIENT state, which the three installation ceilings do not.
//!
//! THE INSTALLED-FACE CEILINGS BOUND WHAT IS INSTALLED, AND NOTHING ABOUT CLIENTS. A live
//! subscription retains endpoint and subscriber state that has nothing to do with how many faces
//! exist, so N applications subscribing is N records this service holds and none of the face
//! ceilings says anything about N. This is the fixed-slot shape the device catalogue already uses.
//!
//! OVER EITHER BOUND IS A TYPED REFUSAL AT THE ASK, not a silently dropped registration: a client
//! that believes it is subscribed and never hears anything is worse than one that was told no.
//!
//! AND A CLIENT THAT GOES TAKES ITS SUBSCRIPTION WITH IT, on the disconnect event the catalogue
//! already has to handle - otherwise sixteen connect/disconnect cycles exhaust a bound nothing is
//! using.

/// Live connections.
pub const MAX_FONT_CLIENTS: usize = 16;

/// Live subscriptions - one per client at most, which is why the two numbers are the same and why a
/// second subscription on one connection is a refusal rather than a second slot.
pub const MAX_FONT_SUBSCRIBERS: usize = 16;

/// Why an ask was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Every connection slot is live.
	TooManyClients,
	/// Every subscription slot is live.
	TooManySubscribers,
	/// This connection already holds a subscription; one per client is the rule.
	AlreadySubscribed,
	/// A slot that is not live - a request on a connection that was never admitted or has gone.
	NotConnected,
}

/// The catalogue's per-client table: fixed slots, no allocation.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Endpoints {
	live: [bool; MAX_FONT_CLIENTS],
	subscribed: [bool; MAX_FONT_CLIENTS],
}

impl Endpoints {
	pub const fn new() -> Self {
		Self { live: [false; MAX_FONT_CLIENTS], subscribed: [false; MAX_FONT_CLIENTS] }
	}

	/// Admit a connection, answering the slot it took.
	pub fn connect(&mut self) -> Result<usize, Refusal> {
		match self.live.iter().position(|live| !live) {
			Some(slot) => {
				self.live[slot] = true;
				Ok(slot)
			}
			None => Err(Refusal::TooManyClients),
		}
	}

	/// Register this connection's subscription to the generation.
	pub fn subscribe(&mut self, slot: usize) -> Result<(), Refusal> {
		if !self.live.get(slot).copied().unwrap_or(false) {
			return Err(Refusal::NotConnected);
		}
		if self.subscribed[slot] {
			return Err(Refusal::AlreadySubscribed);
		}
		if self.subscribers() == MAX_FONT_SUBSCRIBERS {
			return Err(Refusal::TooManySubscribers);
		}
		self.subscribed[slot] = true;
		Ok(())
	}

	/// A connection went. Its place AND its subscription come back on the same event.
	pub fn disconnect(&mut self, slot: usize) -> Result<(), Refusal> {
		if !self.live.get(slot).copied().unwrap_or(false) {
			return Err(Refusal::NotConnected);
		}
		self.live[slot] = false;
		self.subscribed[slot] = false;
		Ok(())
	}

	pub fn clients(&self) -> usize {
		self.live.iter().filter(|live| **live).count()
	}

	pub fn subscribers(&self) -> usize {
		self.subscribed.iter().filter(|subscribed| **subscribed).count()
	}

	/// The slots to deliver a generation change to.
	pub fn subscribed_slots(&self) -> impl Iterator<Item = usize> + use<'_> {
		self.subscribed.iter().enumerate().filter(|(_, subscribed)| **subscribed).map(|(slot, _)| slot)
	}
}

#[cfg(test)]
#[path = "font_clients/tests.rs"]
mod tests;

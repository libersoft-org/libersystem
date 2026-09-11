//! What happens to a live operation when the local address or the route under it goes away.
//!
//! ONE TABLE, BECAUSE THE ANSWER DEPENDS ON WHAT HAS ALREADY BEEN OBSERVED. Nothing has left the
//! machine yet? Choosing a source again is not a change anyone can see. A request is already on the
//! wire? Then its tuple is what the peer will answer to, and quietly retrying as a different tuple
//! is a reply this host will not recognise and a request the peer may act on twice.
//!
//! AND A CALLER WHO NAMED A SOURCE IS NEVER RESELECTED FOR. Naming one has a reason - a routing
//! policy, an address the caller is deliberately not using - and silently substituting another is
//! the failure the override exists to prevent. Told the connection succeeded, the caller believes
//! it went out of the address it chose; it did not. A typed refusal lets it decide; a silent
//! re-selection does not.
//!
//! A WILDCARD LISTENER IS NOT BOUND TO AN ADDRESS AT ALL. It holds a port and a mode, so an address
//! coming and going beneath it changes nothing. A listener bound to a SPECIFIC local address is the
//! opposite case and the one a rule written as "a listener stays" got wrong: left published on an
//! address the interface no longer owns it can never accept again, while holding a port against the
//! binds that could.

use base_proto::generated::liber::base::v1::Error;

/// What an operation is doing at the moment the address or route is invalidated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OperationState {
	/// Admitted, nothing sent, and this host chose the source.
	UnsentAutomaticSource,
	/// Admitted, nothing sent, and the CALLER named the source.
	UnsentNamedSource,
	/// A request is on the wire and a reply is expected: DNS, SNTP, DHCP, a ping or a probe.
	AwaitingReply,
	/// A TCP open whose handshake has not completed.
	TcpConnecting {
		/// Whether the caller named the source for the whole open.
		named_source: bool,
		/// Whether the open still has a candidate after this one.
		candidates_left: bool,
	},
	/// An established TCP connection.
	TcpEstablished,
	/// A listener on the unspecified address.
	WildcardListener,
	/// A listener bound to one specific local address.
	SpecificListener,
}

/// What the service does about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
	/// Choose a source again and carry on, within the original deadline.
	Reselect,
	/// Fail the operation with `address-unavailable`.
	Fail,
	/// Fail and retire THIS attempt. `try_next` says whether the open may start a remaining
	/// candidate with an automatically selected source.
	RetireAttempt { try_next: bool },
	/// Flush what the peer has already acknowledged, then close with `address-unavailable`.
	Close,
	/// Nothing changes.
	Keep,
	/// Withdraw the listener, deliver `address-unavailable` on its channel, and release its port.
	Withdraw,
}

impl Outcome {
	/// The error a caller sees, or `None` when nothing is reported.
	pub fn reported(&self) -> Option<Error> {
		match self {
			Outcome::Reselect | Outcome::Keep => None,
			Outcome::Fail | Outcome::Close | Outcome::Withdraw => Some(Error::AddressUnavailable),
			// A RETIRED ATTEMPT IS NOT A REPORTED FAILURE while another candidate remains: the open
			// has not finished, and a caller told it failed would have been told twice.
			Outcome::RetireAttempt { try_next } => (!try_next).then_some(Error::AddressUnavailable),
		}
	}

	/// Does the operation keep the deadline it was admitted with?
	///
	/// A RESELECTION IS NOT A NEW OPERATION. Restarting the clock would let a link whose addresses
	/// keep changing hold a caller open indefinitely without ever sending anything.
	pub fn keeps_deadline(&self) -> bool {
		matches!(self, Outcome::Reselect | Outcome::Keep)
	}

	/// Is the port this operation held released?
	pub fn releases_port(&self) -> bool {
		matches!(self, Outcome::Withdraw)
	}
}

/// The table.
pub fn on_invalidation(state: OperationState) -> Outcome {
	match state {
		OperationState::UnsentAutomaticSource => Outcome::Reselect,
		OperationState::UnsentNamedSource => Outcome::Fail,
		// THE TUPLE IS WHAT THE PEER WILL ANSWER TO. No sent request silently changes its source or
		// route and retries as the old tuple.
		OperationState::AwaitingReply => Outcome::Fail,
		// A caller's named source fails the WHOLE open: neither fallback nor re-selection may
		// substitute another source for it.
		OperationState::TcpConnecting { named_source: true, .. } => Outcome::Fail,
		OperationState::TcpConnecting { named_source: false, candidates_left } => Outcome::RetireAttempt { try_next: candidates_left },
		// A CONNECTION IS ITS TUPLE. A new source is a new connection the peer knows nothing about.
		OperationState::TcpEstablished => Outcome::Close,
		OperationState::WildcardListener => Outcome::Keep,
		OperationState::SpecificListener => Outcome::Withdraw,
	}
}

#[cfg(test)]
mod tests;

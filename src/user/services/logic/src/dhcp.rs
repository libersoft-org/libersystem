//! Which DHCP reply this client is allowed to act on, and in which phase.
//!
//! THE ORDERING WAS WORSE THAN THE MATCHING. The old parser wrote the parsed lease into the stack
//! BEFORE its caller looked at the message type or the client's state, so a late or losing reply
//! mutated stored lease data even when the caller then ignored the event. Nothing downstream could
//! undo that, because by then it had already happened.
//!
//! AND `xid` PLUS `chaddr` CANNOT PICK AN OFFER. Every legitimate server answering the same discover
//! shares both, so matching them says only "this is an answer to my discover" - not "this is the
//! offer I chose". The client SELECTS one offer and freezes the server identifier and the requested
//! address; from then on a reply is admissible only if it comes from that server for that address.
//!
//! REBINDING IS THE ONE PHASE WHERE A NEW SERVER IS LEGITIMATE, and it is the phase that says so.

/// Where the client is in the lease conversation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
	/// Nothing held and nothing asked.
	Init,
	/// A discover is out; OFFERs and nothing else are admissible.
	Selecting,
	/// One offer was selected and requested; an ACK or NAK from THAT server is admissible.
	Requesting,
	/// A lease is held and nothing is in flight.
	Bound,
	/// Renewing with the server that granted it.
	Renewing,
	/// The renewal failed and any server may answer - which is what this phase is for.
	Rebinding,
}

/// The message types this client reads.
pub const OFFER: u8 = 2;
pub const ACK: u8 = 5;
pub const NAK: u8 = 6;

/// How a datagram reached this host.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Destination {
	Unicast,
	Broadcast,
}

/// What the client should do with a reply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admit {
	/// Select this offer: freeze the server and the address and move to REQUESTING.
	SelectOffer,
	/// Commit the lease.
	CommitLease,
	/// The server rejected this client: DISCARD the lease and the bound address, return to INIT and
	/// start again. RFC 2131's answer, and a state TRANSITION rather than a non-event.
	ClearLease,
	/// Not admissible in this phase, from this server, or at all. NOTHING CHANGES.
	Refused(Refusal),
}

/// Why a reply was not admissible.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// A different transaction.
	ForeignTransaction,
	/// A different client's hardware address.
	ForeignClient,
	/// This message type is not admissible in this phase - a NAK in SELECTING, an OFFER once one has
	/// been chosen, anything at all while BOUND.
	WrongPhase,
	/// Right type, wrong server: a competing offer, or an answer from a server this client did not
	/// select.
	ForeignServer,
	/// An ACK for an address other than the one requested.
	WrongAddress,
	/// A form the phase does not permit - an ACK that should have been unicast and was not.
	WrongDestination,
}

/// One lease conversation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transaction {
	pub phase: Phase,
	/// Drawn per exchange. The old client used a FIXED one, on the reasoning that "SLIRP is the only
	/// DHCP source".
	pub xid: u32,
	pub chaddr: [u8; 6],
	/// The server whose offer was selected, once one has been.
	pub server: Option<[u8; 4]>,
	/// The address that offer named.
	pub requested: Option<[u8; 4]>,
}

/// A reply, as far as the framing has read it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reply {
	pub message_type: u8,
	pub xid: u32,
	pub chaddr: [u8; 6],
	/// The server identifier option, when the reply carried one.
	pub server: Option<[u8; 4]>,
	pub yiaddr: [u8; 4],
	pub destination: Destination,
}

impl Transaction {
	pub fn new(xid: u32, chaddr: [u8; 6]) -> Transaction {
		Transaction { phase: Phase::Init, xid, chaddr, server: None, requested: None }
	}

	/// Decide what a reply may do. NOTHING IS WRITTEN HERE - the caller commits only what this
	/// admits, which is the property the old write-then-check order could not have.
	pub fn admit(&self, reply: &Reply) -> Admit {
		if reply.xid != self.xid {
			return Admit::Refused(Refusal::ForeignTransaction);
		}
		if reply.chaddr != self.chaddr {
			return Admit::Refused(Refusal::ForeignClient);
		}
		match reply.message_type {
			OFFER => self.admit_offer(reply),
			ACK => self.admit_ack(reply),
			NAK => self.admit_nak(reply),
			_ => Admit::Refused(Refusal::WrongPhase),
		}
	}

	fn admit_offer(&self, reply: &Reply) -> Admit {
		// SELECTING TAKES OFFERS AND NOTHING ELSE, and takes only the FIRST: a competing offer from a
		// second server arrives after one has been chosen and is refused by the phase having moved.
		if self.phase != Phase::Selecting {
			return Admit::Refused(Refusal::WrongPhase);
		}
		if reply.server.is_none() {
			return Admit::Refused(Refusal::ForeignServer);
		}
		Admit::SelectOffer
	}

	fn admit_ack(&self, reply: &Reply) -> Admit {
		match self.phase {
			Phase::Requesting | Phase::Renewing => {
				if self.server.is_some() && reply.server != self.server {
					return Admit::Refused(Refusal::ForeignServer);
				}
				if self.requested.is_some() && Some(reply.yiaddr) != self.requested {
					return Admit::Refused(Refusal::WrongAddress);
				}
				// AN ACK IS ADMITTED IN THE DESTINATION FORM ITS PHASE IMPLIES: a renewal is a
				// unicast conversation with the server that granted the lease.
				if self.phase == Phase::Renewing && reply.destination != Destination::Unicast {
					return Admit::Refused(Refusal::WrongDestination);
				}
				Admit::CommitLease
			}
			// REBINDING IS THE ONE PHASE WHERE A NEW SERVER IS LEGITIMATE.
			Phase::Rebinding => Admit::CommitLease,
			_ => Admit::Refused(Refusal::WrongPhase),
		}
	}

	fn admit_nak(&self, reply: &Reply) -> Admit {
		match self.phase {
			// A NAK IS ADMITTED BROADCAST IN EVERY PHASE, which is not a phase rule but a
			// destination-form one: RFC 2131 section 4.1 requires a server to BROADCAST every
			// DHCPNAK when `giaddr` is zero - a renewal whose REQUEST was unicast included. A client
			// that admitted only unicast replies while renewing would discard the conforming NAK for
			// a lease the server has just declared invalid and go on using it, which is the one
			// outcome this whole transaction exists to prevent.
			Phase::Requesting | Phase::Renewing => {
				if self.server.is_some() && reply.server.is_some() && reply.server != self.server {
					return Admit::Refused(Refusal::ForeignServer);
				}
				Admit::ClearLease
			}
			Phase::Rebinding => Admit::ClearLease,
			// SELECTING TAKES OFFERS AND NOTHING ELSE.
			_ => Admit::Refused(Refusal::WrongPhase),
		}
	}
}

#[cfg(test)]
mod tests;

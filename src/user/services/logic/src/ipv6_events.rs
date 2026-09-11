//! The two event paths the IPv6 layer owes its consumers, and the guarantees that make them usable.
//!
//! TWO PATHS, BECAUSE THEY FAIL DIFFERENTLY. A TABLE INVALIDATION says state a consumer is holding
//! is no longer true; losing one leaves that consumer acting on an address or route that is gone. A
//! QUOTED ERROR says a packet this host sent provoked a complaint; losing one costs a diagnosis and
//! nothing else. So the first is loss-safe and the second is advisory, and they are separate queues
//! rather than one queue with a priority field.
//!
//! WHY THE INVALIDATION QUEUE IS BOUNDED AND STILL LOSS-SAFE. A queue that grows is a queue an
//! attacker sizes. A queue that drops is a queue a consumer cannot trust. This one does neither: two
//! events for the SAME identity collapse to the later one, which is what keeps a flapping router
//! from filling it, and a new identity arriving at the cap sets a sticky RESYNC-REQUIRED flag
//! instead of growing or dropping silently. A consumer therefore either sees an event for an
//! identity or is told to re-read the tables. It is never left holding state that was invalidated
//! behind its back.
//!
//! WHAT THIS MODULE DOES NOT DO. It holds no addresses, no routes and no transport state. It is the
//! shape of the contract, not the tables behind it, which is what lets it be tested on a host.

use crate::ipv6::{Address, Interface, Prefix};
use alloc::vec::Vec;

/// How many DISTINCT identities the invalidation queue holds before it starts asking for a resync.
pub const INVALIDATION_CAPACITY: usize = 32;

/// How many quoted errors the advisory queue holds before it drops the newest.
pub const QUOTED_ERROR_CAPACITY: usize = 32;

/// WHAT changed, named the way the tables name it.
///
/// The identity is what makes coalescing meaningful: two events about the same address are one
/// event, and a consumer that acts on the identity does not have to rescan a table to find out what
/// moved. Every arm carries the interface, because the same address on a replaced NIC is a different
/// thing and a consumer that ignored the generation would act on the wrong one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Identity {
	/// One of this host's own unicast addresses.
	Address { interface: Interface, address: Address },
	/// A learned on-link prefix.
	Prefix { interface: Interface, prefix: Prefix },
	/// A route, named by its destination prefix and next hop.
	Route { interface: Interface, destination: Prefix, next_hop: Address },
	/// A default router.
	Router { interface: Interface, router: Address },
	/// A recursive DNS server learned from a router advertisement.
	Rdnss { interface: Interface, server: Address },
	/// A neighbour cache entry.
	Neighbour { interface: Interface, neighbour: Address },
	/// A path-MTU record.
	PathMtu { interface: Interface, destination: Address },
	/// The whole interface: it went away, or came back as a new generation.
	InterfaceState { interface: Interface },
}

impl Identity {
	/// The interface every identity belongs to.
	pub fn interface(&self) -> Interface {
		match self {
			Identity::Address { interface, .. } | Identity::Prefix { interface, .. } | Identity::Route { interface, .. } | Identity::Router { interface, .. } | Identity::Rdnss { interface, .. } | Identity::Neighbour { interface, .. } | Identity::PathMtu { interface, .. } | Identity::InterfaceState { interface } => *interface,
		}
	}
}

/// What happened to it.
///
/// `Changed` and `Invalidated` are not the same thing to a consumer: the first says re-read, the
/// second says stop using it. Collapsing them would make a consumer re-read a route that is gone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
	/// The record was added or its contents changed. Re-read it.
	Changed,
	/// The record is gone. Anything depending on it must stop.
	Invalidated,
}

/// One invalidation event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Invalidation {
	pub identity: Identity,
	pub change: Change,
	/// The table mutation generation this event was recorded at. A resync snapshot taken at a later
	/// generation covers it; one taken at an earlier generation does not.
	pub generation: u64,
}

/// What `record` did with an event, so a caller can count it without inspecting the queue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Recorded {
	/// A new identity took a slot.
	Queued,
	/// An identity already in the queue was updated in place. Nothing was lost.
	Coalesced,
	/// The queue was full of other identities. The resync flag is set and this event is not held.
	ResyncRequired,
}

/// The bounded, loss-safe invalidation queue.
///
/// The order events come out in is the order their identities FIRST entered, not the order they were
/// last updated. A consumer draining it sees each identity once, with the newest state, and cannot
/// be starved of an old identity by a fast-changing new one.
#[derive(Debug, Default)]
pub struct InvalidationQueue {
	entries: Vec<Invalidation>,
	resync_required: bool,
	/// The generation at which the resync flag was raised. A snapshot must be at least this new to
	/// clear it.
	resync_since: u64,
	overflow_discards: u32,
}

impl InvalidationQueue {
	pub fn new() -> InvalidationQueue {
		InvalidationQueue::default()
	}

	/// Record an event, coalescing on identity.
	pub fn record(&mut self, event: Invalidation) -> Recorded {
		if let Some(existing) = self.entries.iter_mut().find(|held| held.identity == event.identity) {
			// THE LATER ONE WINS, INCLUDING ITS CHANGE. An address that changed and then went away
			// is gone; a consumer told "changed" would re-read a record that no longer exists.
			existing.change = event.change;
			existing.generation = event.generation;
			return Recorded::Coalesced;
		}
		if self.entries.len() >= INVALIDATION_CAPACITY {
			// THE FLAG IS STICKY AND THE QUEUE DOES NOT GROW. Discarding here is safe only because
			// the flag says so: the consumer will re-read the tables rather than trust the queue.
			if !self.resync_required {
				self.resync_required = true;
				self.resync_since = event.generation;
			}
			self.overflow_discards = self.overflow_discards.saturating_add(1);
			return Recorded::ResyncRequired;
		}
		self.entries.push(event);
		Recorded::Queued
	}

	/// Take everything queued, oldest identity first, leaving the resync flag alone.
	///
	/// DRAINING IS NOT RESYNCHRONISING. A consumer that emptied the queue while the flag was set has
	/// seen every event the queue still held and none of the ones it discarded, which is exactly the
	/// case the flag exists for.
	pub fn drain(&mut self) -> Vec<Invalidation> {
		core::mem::take(&mut self.entries)
	}

	/// Is the consumer required to re-read the tables?
	pub fn resync_required(&self) -> bool {
		self.resync_required
	}

	/// How many events were discarded because the queue was full of other identities.
	pub fn overflow_discards(&self) -> u32 {
		self.overflow_discards
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	/// The consumer installed a snapshot of the tables taken at `generation`.
	///
	/// The flag clears only when that snapshot is at least as new as the mutation that raised it,
	/// AND the tables have not moved since the snapshot was taken - `current` is the table's
	/// generation now. A snapshot that raced a mutation leaves the flag up and the consumer retries;
	/// this is the one place where being conservative costs a re-read and being optimistic costs a
	/// consumer holding invalid state forever.
	pub fn snapshot_installed(&mut self, generation: u64, current: u64) -> bool {
		if !self.resync_required {
			return true;
		}
		if generation < self.resync_since || generation != current {
			return false;
		}
		// Events covered by the snapshot are the ones the consumer has just read for itself.
		self.entries.retain(|event| event.generation > generation);
		self.resync_required = false;
		self.resync_since = 0;
		true
	}
}

/// The class of ICMPv6 error, with the field its consumer needs.
///
/// TYPED, NOT A FRAME. A consumer demultiplexing these must not have to re-parse a packet the layer
/// below already validated, and must not be handed bytes whose validation it cannot see.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorClass {
	/// Type 1, with its code.
	DestinationUnreachable { code: u8 },
	/// Type 2, with the MTU the reporting router advertised.
	PacketTooBig { mtu: u32 },
	/// Type 3, with its code.
	TimeExceeded { code: u8 },
	/// Type 4, with its code and the pointer into the invoking packet.
	ParameterProblem { code: u8, pointer: u32 },
}

/// Which transport the quoted packet belonged to, recovered from the quotation.
///
/// This is the identity a consumer demultiplexes on. It is what the QUOTED bytes say, and this layer
/// does not and cannot check that the quoted packet is one this host actually sent - that is a flow
/// lookup, and this layer holds no flow state. See `QuotedError`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuotedTransport {
	/// A quoted TCP header: ports and the sequence number the consumer checks against its own
	/// unacknowledged interval.
	Tcp { source_port: u16, destination_port: u16, sequence: u32 },
	/// A quoted UDP header.
	Udp { source_port: u16, destination_port: u16 },
	/// A quoted ICMPv6 echo, by identifier and sequence.
	Icmpv6Echo { identifier: u16, sequence: u16 },
	/// A quoted packet whose upper layer this host does not demultiplex.
	Other { next_header: u8 },
}

/// A validated ICMPv6 error, ready for the consumer that owns the flow.
///
/// WHAT THE VALIDATION MEANS, EXACTLY. The type and code are known, enough of the invoking packet is
/// quoted to recover the transport identity, and the quoted SOURCE address is one this interface
/// currently holds. That last check drops a quotation naming somebody else's address, which is
/// worth having - but it is an ADDRESS check and not a FLOW check. Nothing here proves this host
/// sent the quoted packet, because proving that is a lookup in state this layer is forbidden to
/// keep. The consumer does that, and the consumer is where the durable write happens.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct QuotedError {
	pub interface: Interface,
	/// Who reported the problem.
	pub reporter: Address,
	pub class: ErrorClass,
	/// The addresses out of the quoted packet: ours, and where it was going.
	pub quoted_source: Address,
	pub quoted_destination: Address,
	pub transport: QuotedTransport,
}

/// The advisory error queue: bounded, and it drops the NEWEST on overflow.
///
/// DROPPING THE NEWEST, NOT THE OLDEST. Under a flood the newest errors are the flood; the oldest
/// are the ones that arrived before it and are more likely to be the real diagnosis. A queue that
/// evicted the oldest would let a flood erase exactly the events worth keeping.
#[derive(Debug, Default)]
pub struct QuotedErrorQueue {
	entries: Vec<QuotedError>,
	accepted: u32,
	dropped: u32,
}

impl QuotedErrorQueue {
	pub fn new() -> QuotedErrorQueue {
		QuotedErrorQueue::default()
	}

	/// Offer an error. Returns false when the queue was full and the error was dropped.
	pub fn offer(&mut self, error: QuotedError) -> bool {
		if self.entries.len() >= QUOTED_ERROR_CAPACITY {
			self.dropped = self.dropped.saturating_add(1);
			return false;
		}
		self.entries.push(error);
		self.accepted = self.accepted.saturating_add(1);
		true
	}

	/// How many errors this queue has accepted since boot. Cumulative, because `len` is emptied by
	/// every drain and a consumer asking "did anything arrive" cannot see a queue that was already
	/// read.
	pub fn accepted(&self) -> u32 {
		self.accepted
	}

	pub fn drain(&mut self) -> Vec<QuotedError> {
		core::mem::take(&mut self.entries)
	}

	/// Count an error that never reached the queue because it could not be attributed - a quotation
	/// too short for its transport identity, or one naming an address this interface does not hold.
	///
	/// ONE COUNTER FOR BOTH, deliberately. Both mean "an error arrived and no consumer will hear
	/// about it", both are things a flood produces on purpose, and a counter split by cause is a
	/// counter whose keys the flood chooses.
	pub fn note_dropped(&mut self) {
		self.dropped = self.dropped.saturating_add(1);
	}

	/// How many errors were dropped: for want of room, or because they could not be attributed.
	/// Aggregate, with no per-packet log: a flood must not be able to make this host write one line
	/// per frame.
	pub fn dropped(&self) -> u32 {
		self.dropped
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}
}

#[cfg(test)]
mod tests;

//! The hard capacities the IPv6 layer admits state under, and the pending-resolution budgets.
//!
//! THESE ARE ADMISSION LIMITS, NOT GROWTH HINTS. Every table below is a fixed size chosen so that a
//! hostile link cannot make this host allocate: a router that advertises a thousand prefixes gets
//! fifteen of them and a refusal counter, and the sixteenth does not evict a live one. That is the
//! rule that matters and it is stated once, here: a still-live record is never removed to admit an
//! unsolicited new identity. Expiry reclaims; arrival does not.
//!
//! WHY PENDING PACKETS HAVE THREE BUDGETS AND NOT ONE. A neighbour that never answers must not be
//! able to hold more than a few packets (per-neighbour), a link full of unanswering neighbours must
//! not be able to hold more than a few dozen (per-interface), and a handful of large packets must
//! not be able to hold more memory than many small ones (bytes). One budget cannot express all
//! three, and the one a packet fails is the one the caller is told about.
//!
//! CHARGE BEFORE YOU ALLOCATE. The count and the bytes are reserved before the packet is copied, so
//! a refusal costs nothing and an admission cannot discover halfway through that it had no room.

use crate::ipv6::{Address, Interface};
use alloc::vec::Vec;

/// A bounded resource the IPv6 layer admits state into.
///
/// Naming them in one enum is what lets a refusal be counted per resource without a dynamically
/// keyed map - which would itself be an unbounded allocation keyed by whatever an attacker sends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resource {
	/// One startup-selected NIC. Teardown clears its generation's state.
	Interfaces,
	/// Unicast addresses, tentative and deprecated included, one of them link-local.
	UnicastAddresses,
	/// Learned on-link prefixes, unique by prefix and length.
	Prefixes,
	/// Routes, link-local and on-link included.
	Routes,
	/// Default routers.
	DefaultRouters,
	/// Advertiser memberships per prefix.
	AdvertisersPerPrefix,
	/// Recursive DNS servers, per advertising router and server pair.
	Rdnss,
	/// Neighbour cache entries, incomplete and router next hops included.
	Neighbours,
	/// Path-MTU records.
	PathMtu,
	/// Multicast listener group records.
	MldGroups,
	/// Sources per pending multicast listener record.
	MldSourcesPerRecord,
	/// The table-invalidation queue.
	InvalidationEvents,
	/// The advisory quoted-error queue.
	QuotedErrorEvents,
	/// Retained packets waiting on one neighbour's resolution.
	PendingPerNeighbour,
	/// Retained packets waiting on resolution across the interface.
	PendingPerInterface,
	/// Retained packet bytes across the interface.
	PendingBytes,
	/// Timer actions run in one service iteration.
	DueActionsPerIteration,
}

/// Every resource, so a snapshot can walk them without a list that can drift from the enum.
pub const RESOURCES: [Resource; 17] = [
	Resource::Interfaces,
	Resource::UnicastAddresses,
	Resource::Prefixes,
	Resource::Routes,
	Resource::DefaultRouters,
	Resource::AdvertisersPerPrefix,
	Resource::Rdnss,
	Resource::Neighbours,
	Resource::PathMtu,
	Resource::MldGroups,
	Resource::MldSourcesPerRecord,
	Resource::InvalidationEvents,
	Resource::QuotedErrorEvents,
	Resource::PendingPerNeighbour,
	Resource::PendingPerInterface,
	Resource::PendingBytes,
	Resource::DueActionsPerIteration,
];

impl Resource {
	/// The hard capacity. Not configurable: a limit a remote party can raise is not a limit.
	pub const fn limit(&self) -> u32 {
		match self {
			Resource::Interfaces => 1,
			Resource::UnicastAddresses => 16,
			Resource::Prefixes => 15,
			Resource::Routes => 32,
			Resource::DefaultRouters => 8,
			Resource::AdvertisersPerPrefix => 8,
			Resource::Rdnss => 4,
			Resource::Neighbours => 64,
			Resource::PathMtu => 64,
			Resource::MldGroups => 32,
			Resource::MldSourcesPerRecord => 64,
			Resource::InvalidationEvents => 32,
			Resource::QuotedErrorEvents => 32,
			Resource::PendingPerNeighbour => 4,
			Resource::PendingPerInterface => 32,
			Resource::PendingBytes => 65536,
			Resource::DueActionsPerIteration => 16,
		}
	}

	/// A stable index, for the counter array.
	pub const fn index(&self) -> usize {
		*self as usize
	}
}

/// Saturating refusal counters, one per resource.
///
/// SATURATING RATHER THAN WRAPPING. A counter that wraps to zero under a flood reports "no refusals"
/// at exactly the moment the operator needs to see them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Refusals {
	counts: [u32; RESOURCES.len()],
}

impl Refusals {
	pub fn new() -> Refusals {
		Refusals::default()
	}

	pub fn record(&mut self, resource: Resource) {
		let slot = &mut self.counts[resource.index()];
		*slot = slot.saturating_add(1);
	}

	pub fn get(&self, resource: Resource) -> u32 {
		self.counts[resource.index()]
	}

	pub fn total(&self) -> u32 {
		self.counts.iter().fold(0u32, |sum, count| sum.saturating_add(*count))
	}
}

/// A caller's opaque handle on one retained packet.
///
/// The consumer that asked for the transmission owns it: cancelling the token removes the packet
/// that has not gone out yet and releases its charges, exactly once. A cancelled packet cannot later
/// transmit when resolution succeeds, which is the property a consumer that has given up depends on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct OperationToken(pub u64);

/// Why a packet could not be retained.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct QueueFull {
	/// Which budget refused it. The caller is told the limiting one rather than "full", because the
	/// three mean different things: retry later, use another neighbour, or send less.
	pub resource: Resource,
	pub limit: u32,
}

/// A packet retained while its neighbour resolves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
	pub token: OperationToken,
	pub neighbour: Address,
	pub interface: Interface,
	/// The whole frame as it will go out, headers included: what is charged is what is held.
	pub frame: Vec<u8>,
}

/// How a retained packet ended.
///
/// EVERY ONE OF THESE RELEASES THE CHARGES EXACTLY ONCE, which is why they are one enum rather than
/// three code paths that each have to remember.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Completion {
	/// The frame reached the driver. `at` is the monotonic time of that handoff, which is where the
	/// owning consumer starts its round-trip measurement and its reply deadline.
	Sent { token: OperationToken, at: u64 },
	/// Resolution failed, the address or route went away, or the interface was torn down.
	Failed { token: OperationToken, cause: FailureCause },
	/// The consumer cancelled it before it went out.
	Cancelled { token: OperationToken },
}

/// Why a retained packet will never be sent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FailureCause {
	/// Solicitation or probe exhaustion retired the neighbour.
	ResolutionFailed,
	/// The source address or the route it was selected against was invalidated.
	RouteInvalidated,
	/// The interface went away.
	InterfaceTorn,
}

/// The pending-resolution queues and their three budgets.
#[derive(Debug, Default)]
pub struct PendingQueue {
	entries: Vec<Pending>,
	bytes: u32,
	next_token: u64,
	refusals: Refusals,
	resolution_failures: u32,
}

impl PendingQueue {
	pub fn new() -> PendingQueue {
		PendingQueue::default()
	}

	/// Reserve room for `frame` against all three budgets and retain it.
	///
	/// The order of the checks is the order of the report: per-neighbour first, because that is the
	/// budget a single unanswering peer hits, then the interface count, then the bytes. All three
	/// must fit; nothing is charged unless every one of them does.
	pub fn admit(&mut self, interface: Interface, neighbour: Address, frame: Vec<u8>) -> Result<OperationToken, QueueFull> {
		let per_neighbour = self.entries.iter().filter(|held| held.neighbour == neighbour && held.interface == interface).count() as u32;
		if per_neighbour >= Resource::PendingPerNeighbour.limit() {
			self.refusals.record(Resource::PendingPerNeighbour);
			return Err(QueueFull { resource: Resource::PendingPerNeighbour, limit: Resource::PendingPerNeighbour.limit() });
		}
		if self.entries.len() as u32 >= Resource::PendingPerInterface.limit() {
			self.refusals.record(Resource::PendingPerInterface);
			return Err(QueueFull { resource: Resource::PendingPerInterface, limit: Resource::PendingPerInterface.limit() });
		}
		let charge = frame.len() as u32;
		if self.bytes.saturating_add(charge) > Resource::PendingBytes.limit() {
			self.refusals.record(Resource::PendingBytes);
			return Err(QueueFull { resource: Resource::PendingBytes, limit: Resource::PendingBytes.limit() });
		}
		let token = OperationToken(self.next_token);
		self.next_token += 1;
		self.bytes += charge;
		self.entries.push(Pending { token, neighbour, interface, frame });
		Ok(token)
	}

	/// Take every packet waiting on `neighbour`, oldest first, releasing their charges.
	///
	/// The caller sends them and reports `Completion::Sent` for each; the charges are already
	/// released here, so a caller that fails partway cannot leak them.
	pub fn take_for(&mut self, interface: Interface, neighbour: Address) -> Vec<Pending> {
		let mut taken = Vec::new();
		let mut kept = Vec::new();
		for entry in core::mem::take(&mut self.entries) {
			if entry.neighbour == neighbour && entry.interface == interface {
				self.bytes -= entry.frame.len() as u32;
				taken.push(entry);
			} else {
				kept.push(entry);
			}
		}
		self.entries = kept;
		taken
	}

	/// Drop the packet this token owns, if it is still here.
	///
	/// Returns the completion to report, or `None` when the token is already gone - which is what
	/// makes the release exactly-once rather than merely once per call.
	pub fn cancel(&mut self, token: OperationToken) -> Option<Completion> {
		let position = self.entries.iter().position(|held| held.token == token)?;
		let entry = self.entries.remove(position);
		self.bytes -= entry.frame.len() as u32;
		Some(Completion::Cancelled { token })
	}

	/// Fail every packet waiting on `neighbour` and release their charges.
	pub fn fail_for(&mut self, interface: Interface, neighbour: Address, cause: FailureCause) -> Vec<Completion> {
		if cause == FailureCause::ResolutionFailed {
			self.resolution_failures = self.resolution_failures.saturating_add(1);
		}
		self.take_for(interface, neighbour).into_iter().map(|entry| Completion::Failed { token: entry.token, cause }).collect()
	}

	/// Fail everything on an interface: it was torn down, or its generation moved.
	pub fn fail_interface(&mut self, interface: Interface, cause: FailureCause) -> Vec<Completion> {
		let mut completions = Vec::new();
		let mut kept = Vec::new();
		for entry in core::mem::take(&mut self.entries) {
			if entry.interface == interface {
				self.bytes -= entry.frame.len() as u32;
				completions.push(Completion::Failed { token: entry.token, cause });
			} else {
				kept.push(entry);
			}
		}
		self.entries = kept;
		completions
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	pub fn bytes(&self) -> u32 {
		self.bytes
	}

	pub fn refusals(&self) -> Refusals {
		self.refusals
	}

	pub fn resolution_failures(&self) -> u32 {
		self.resolution_failures
	}
}

/// One resource's line in the capacity snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Usage {
	pub resource: Resource,
	pub used: u32,
	pub limit: u32,
	pub refusals: u32,
}

/// What the layer will tell a host test or the guest fixture about its own bounds.
///
/// NO DYNAMICALLY KEYED COUNTERS AND NO PER-PACKET LOG. Everything here is a fixed number of fixed
/// fields, so reading it costs the same under a flood as it does on an idle link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
	pub usage: Vec<Usage>,
	pub resolution_failures: u32,
	pub quoted_errors_dropped: u32,
	pub icmp_errors_rate_limited: u32,
	pub resync_required: bool,
}

impl Snapshot {
	/// The line for one resource, for a caller that wants a single number.
	pub fn get(&self, resource: Resource) -> Option<Usage> {
		self.usage.iter().copied().find(|line| line.resource == resource)
	}

	/// Is any resource at its limit? A caller reporting health wants one answer, not seventeen.
	///
	/// THE INTERFACE COUNT IS NOT ONE OF THEM. Its limit is one and a working host uses one, so a
	/// health signal that counted it would be on from the first second of every boot - which is a
	/// signal nobody reads twice.
	pub fn any_saturated(&self) -> bool {
		self.usage.iter().filter(|line| line.resource != Resource::Interfaces).any(|line| line.used >= line.limit)
	}
}

#[cfg(test)]
mod tests;

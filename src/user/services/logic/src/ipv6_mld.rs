//! Multicast listener discovery: telling the link which groups this host wants, and keeping saying
//! it.
//!
//! WHY A HOST THAT ONLY DOES NEIGHBOUR DISCOVERY NEEDS THIS AT ALL. On a link with a snooping
//! switch, a group nobody has reported is a group the switch does not forward. Duplicate-address
//! detection sends its solicitation to the tentative address's SOLICITED-NODE group - so if that
//! group was never reported, the probe never reaches the host that already holds the address, and
//! detection passes for an address that is taken. That is the failure this exists to prevent, and it
//! is why the FIRST report is sent before detection rather than after it.
//!
//! WHICH MEANS THE FIRST REPORT HAS NO SOURCE ADDRESS TO USE. At that moment the link-local address
//! is still tentative and may not be a source, so the initial report goes out from the unspecified
//! address, which RFC 9777 requires. Once detection finishes, every joined group is reported AGAIN
//! from the real address, which is what leaves the switch's state correct for the rest of the boot.
//!
//! ALL-NODES IS NEVER REPORTED. Membership of `ff02::1` is permanent and cannot be left, so a host
//! that reported it would be announcing something it can never withdraw. MLD has excluded that
//! address since its first specification.
//!
//! AND A QUERY IS VALIDATED ON MLD'S OWN TERMS. Neighbour discovery requires hop limit 255 because
//! its messages must not have been forwarded; MLD requires hop limit ONE, because its messages must
//! not leave the link. Applying the neighbour-discovery rule here would discard every legitimate
//! query - the two protocols share an ICMPv6 header and nothing else about their envelopes.

use crate::ipv6::{ALL_MLDV2_ROUTERS, ALL_NODES, Address, Kind, UNSPECIFIED};
use crate::ipv6_budget::{Refusals, Resource};
use alloc::vec::Vec;

/// The hop limit every MLD message carries, sent and received.
pub const MLD_HOP_LIMIT: u8 = 1;

/// How many times a state-change report is retransmitted.
///
/// MLD IS UNACKNOWLEDGED, and its first packet is exactly the one whose loss costs the most: a lost
/// join report leaves the group unforwarded until the next query, which on a quiet link can be
/// minutes.
pub const ROBUSTNESS: u8 = 2;

/// The interval between state-change retransmissions, in milliseconds.
pub const UNSOLICITED_REPORT_INTERVAL_MS: u64 = 1_000;

/// How long a version-1 querier keeps this host in compatibility mode, in milliseconds.
pub const V1_QUERIER_PRESENT_MS: u64 = 260_000;

/// Which version this listener is speaking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Version {
	/// The ordinary case.
	V2,
	/// A version-1 querier was heard. Reports are v1 Reports, a leave is a v1 Done, and
	/// source-specific state is not expressible - it degrades rather than being sent as v2.
	V1 { until_ms: u64 },
}

/// Why a query was discarded.
///
/// Silently, and counted: a malformed query is either a bug on the link or somebody probing, and
/// neither is worth a log line per packet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QueryRefusal {
	/// The source is not a link-local address.
	SourceNotLinkLocal,
	/// The hop limit is not one, so the message may have crossed a router.
	HopLimitNotOne,
	/// The IPv6 Router Alert hop-by-hop option is absent.
	NoRouterAlert,
}

/// Validate a received query on MLD's terms.
pub fn validate_query(source: Address, hop_limit: u8, router_alert: bool) -> Result<(), QueryRefusal> {
	if source.kind() != Kind::LinkLocalUnicast {
		return Err(QueryRefusal::SourceNotLinkLocal);
	}
	if hop_limit != MLD_HOP_LIMIT {
		return Err(QueryRefusal::HopLimitNotOne);
	}
	if !router_alert {
		return Err(QueryRefusal::NoRouterAlert);
	}
	Ok(())
}

/// The envelope every MLD message this host emits must carry.
///
/// STATED ONCE AND USED BY ALL OF THEM. A syntactically correct membership record without this is
/// discarded by the first router or snooping switch that sees it - silently, which is the worst way
/// for it to fail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Envelope {
	pub hop_limit: u8,
	pub router_alert: bool,
	pub source: Address,
	pub destination: Address,
}

/// The envelope for a message sent to `destination`.
///
/// `link_local` is this interface's valid link-local address, or `None` while detection has not
/// finished. The unspecified source is used in that case AND IN NO OTHER: later responses,
/// state-change reports, retransmissions and version-1 messages all use the real address.
pub fn envelope(link_local: Option<Address>, destination: Address) -> Envelope {
	Envelope { hop_limit: MLD_HOP_LIMIT, router_alert: true, source: link_local.unwrap_or(UNSPECIFIED), destination }
}

/// What kind of message to emit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Emission {
	/// A version-2 membership report for one group.
	ReportV2 { group: Address },
	/// A version-1 report, in compatibility mode.
	ReportV1 { group: Address },
	/// A version-1 Done, in compatibility mode.
	DoneV1 { group: Address },
	/// A version-2 state-change report saying the group was left.
	LeaveV2 { group: Address },
}

impl Emission {
	/// The group this message is about.
	pub fn group(&self) -> Address {
		match self {
			Emission::ReportV2 { group } | Emission::ReportV1 { group } | Emission::DoneV1 { group } | Emission::LeaveV2 { group } => *group,
		}
	}

	/// Where it goes. A v2 report goes to the all-MLDv2-routers group; a v1 message goes to the
	/// group itself, except a Done, which goes to all-routers.
	pub fn destination(&self) -> Address {
		match self {
			Emission::ReportV2 { .. } | Emission::LeaveV2 { .. } => ALL_MLDV2_ROUTERS,
			Emission::ReportV1 { group } => *group,
			Emission::DoneV1 { .. } => crate::ipv6::ALL_ROUTERS,
		}
	}
}

/// A query response this host owes, and what kind it is.
///
/// AN EMPTY SOURCE LIST IS NOT "NO SOURCES", IT IS ADDRESS-SPECIFIC. The distinction is the whole
/// content of the merge rules below: a response owed about the WHOLE group cannot be narrowed by a
/// later query that names a few sources, because the broader report was already owed and dropping it
/// would answer less than was asked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Response {
	/// Empty for an address-specific or general response; otherwise the sources asked about.
	pub sources: Vec<Address>,
	/// When it goes out.
	pub due_ms: u64,
}

impl Response {
	pub fn address_specific(&self) -> bool {
		self.sources.is_empty()
	}
}

/// One group's record.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GroupRecord {
	pub group: Address,
	/// False while the record is only being kept to finish its leave retransmissions.
	pub joined: bool,
	/// A query response owed. One per record: a second query merges into it rather than adding a
	/// second answer, which is what stops a querier from making this host talk twice.
	pub response: Option<Response>,
	/// How many state-change transmissions are still owed, and when the next is due. SEPARATE from
	/// the response above: they are different obligations with different deadlines, and a listener
	/// that shared one field would let a query response cancel a join report.
	pub retransmits_left: u8,
	pub retransmit_at_ms: Option<u64>,
}

/// The bounded listener.
#[derive(Debug)]
pub struct Listener {
	groups: Vec<GroupRecord>,
	version: Version,
	refusals: Refusals,
	invalid_queries: u32,
	reported_after_dad: bool,
}

impl Default for Listener {
	fn default() -> Listener {
		Listener { groups: Vec::new(), version: Version::V2, refusals: Refusals::new(), invalid_queries: 0, reported_after_dad: false }
	}
}

impl Listener {
	pub fn new() -> Listener {
		Listener::default()
	}

	/// Join a group and emit its state-change report.
	///
	/// `ff02::1` is refused as a no-op rather than as an error: a caller that asks is not wrong, it
	/// is asking about a membership that exists and cannot be reported.
	pub fn join(&mut self, group: Address, now_ms: u64) -> Option<Emission> {
		if group == ALL_NODES || group.kind() != Kind::Multicast {
			return None;
		}
		if let Some(record) = self.groups.iter_mut().find(|held| held.group == group) {
			// A REJOIN REUSES THE RECORD IT WAS LEAVING, and RESETS its counter rather than adding
			// to it: what is owed is a fresh report, not the remainder of a Done.
			record.joined = true;
			record.retransmits_left = ROBUSTNESS.saturating_sub(1);
			record.retransmit_at_ms = (record.retransmits_left > 0).then_some(now_ms + UNSOLICITED_REPORT_INTERVAL_MS);
			return Some(self.report_kind(group));
		}
		if self.groups.len() as u32 >= Resource::MldGroups.limit() {
			self.refusals.record(Resource::MldGroups);
			return None;
		}
		self.groups.push(GroupRecord { group, joined: true, response: None, retransmits_left: ROBUSTNESS - 1, retransmit_at_ms: Some(now_ms + UNSOLICITED_REPORT_INTERVAL_MS) });
		Some(self.report_kind(group))
	}

	/// Leave a group. The record is KEPT until its transmissions finish, so a rejoin reuses it and a
	/// lost Done is repeated.
	pub fn leave(&mut self, group: Address, now_ms: u64) -> Option<Emission> {
		let version = self.version;
		let record = self.groups.iter_mut().find(|held| held.group == group && held.joined)?;
		record.joined = false;
		// A LEAVE SUPERSEDES A PENDING JOIN REPORT rather than being queued behind it. The two would
		// otherwise go out in the order the timers happened to fire, and a router that saw the Done
		// before the Report would believe the host is still a member.
		record.response = None;
		record.retransmits_left = ROBUSTNESS - 1;
		record.retransmit_at_ms = Some(now_ms + UNSOLICITED_REPORT_INTERVAL_MS);
		Some(match version {
			Version::V1 { .. } => Emission::DoneV1 { group },
			Version::V2 => Emission::LeaveV2 { group },
		})
	}

	fn report_kind(&self, group: Address) -> Emission {
		match self.version {
			Version::V1 { .. } => Emission::ReportV1 { group },
			Version::V2 => Emission::ReportV2 { group },
		}
	}

	/// Detection finished and a link-local address exists: report every joined group again.
	pub fn report_after_dad(&mut self, _now_ms: u64) -> Vec<Emission> {
		self.reported_after_dad = true;
		let groups: Vec<Address> = self.groups.iter().filter(|record| record.joined).map(|record| record.group).collect();
		for record in self.groups.iter_mut().filter(|record| record.joined) {
			record.retransmits_left = 0;
			record.retransmit_at_ms = None;
		}
		groups.into_iter().map(|group| self.report_kind(group)).collect()
	}

	pub fn reported_after_dad(&self) -> bool {
		self.reported_after_dad
	}

	/// A validated query arrived, and this is where the merge rules live.
	///
	/// `group` is `None` for a general query; `sources` narrows it to a source-specific one. The
	/// rules, each of which a plausible implementation gets wrong in a different way:
	///
	///   - the EARLIER deadline always wins, so a response already owed sooner still goes out on
	///     time and a second query never adds a second answer;
	///   - an ADDRESS-specific query merging into a pending source-specific response CLEARS the
	///     recorded sources: the broader report is now owed;
	///   - a SOURCE-specific query merging into a pending ADDRESS-specific one leaves the list
	///     EMPTY. This is the reverse ordering, and it is where a union into an empty list would
	///     silently narrow an answer that was already owed;
	///   - two source-specific queries UNION their lists, up to the cap;
	///   - past the cap the record DEGRADES to address-specific, clears the list and keeps the
	///     EARLIEST deadline already chosen. Degraded, not dropped: a querier that floods must not be
	///     able to suppress a report.
	pub fn on_query(&mut self, group: Option<Address>, sources: &[Address], max_response_ms: u64, now_ms: u64) -> usize {
		let due = now_ms + max_response_ms.max(1) / 2;
		let version = self.version;
		let cap = Resource::MldSourcesPerRecord.limit() as usize;
		let mut scheduled = 0usize;
		for record in self.groups.iter_mut().filter(|record| record.joined) {
			if group.is_some_and(|asked| asked != record.group) {
				continue;
			}
			// A version-1 listener cannot express source state, so every query is address-specific.
			let asked_sources: &[Address] = if matches!(version, Version::V1 { .. }) { &[] } else { sources };
			let merged = match record.response.take() {
				None => Response { sources: asked_sources.to_vec(), due_ms: due },
				Some(pending) => {
					let due_ms = pending.due_ms.min(due);
					if asked_sources.is_empty() || pending.address_specific() {
						// Either the new query is broader, or a broader answer was already owed.
						Response { sources: Vec::new(), due_ms }
					} else {
						let mut union = pending.sources;
						for source in asked_sources {
							if union.contains(source) {
								continue;
							}
							if union.len() >= cap {
								union.clear();
								break;
							}
							union.push(*source);
						}
						Response { sources: union, due_ms }
					}
				}
			};
			// A first source-specific query may itself exceed the cap.
			let merged = if merged.sources.len() > cap { Response { sources: Vec::new(), due_ms: merged.due_ms } } else { merged };
			record.response = Some(merged);
			scheduled += 1;
		}
		scheduled
	}

	/// A version-1 querier was heard.
	pub fn saw_v1_querier(&mut self, now_ms: u64) {
		self.version = Version::V1 { until_ms: now_ms + V1_QUERIER_PRESENT_MS };
	}

	/// Drive time forward: expire compatibility mode, then emit whatever is due.
	pub fn tick(&mut self, now_ms: u64) -> Vec<Emission> {
		if let Version::V1 { until_ms } = self.version {
			if until_ms <= now_ms {
				self.version = Version::V2;
			}
		}
		let version = self.version;
		let mut due = Vec::new();
		let mut finished = Vec::new();
		for record in self.groups.iter_mut() {
			let membership = |joined: bool, group: Address| match (version, joined) {
				(Version::V1 { .. }, true) => Emission::ReportV1 { group },
				(Version::V1 { .. }, false) => Emission::DoneV1 { group },
				(Version::V2, true) => Emission::ReportV2 { group },
				(Version::V2, false) => Emission::LeaveV2 { group },
			};
			if record.response.as_ref().is_some_and(|response| response.due_ms <= now_ms) {
				record.response = None;
				due.push(membership(record.joined, record.group));
			}
			if record.retransmit_at_ms.is_some_and(|deadline| deadline <= now_ms) {
				due.push(membership(record.joined, record.group));
				record.retransmits_left = record.retransmits_left.saturating_sub(1);
				if record.retransmits_left == 0 {
					record.retransmit_at_ms = None;
					if !record.joined {
						finished.push(record.group);
					}
				} else {
					record.retransmit_at_ms = Some(now_ms + UNSOLICITED_REPORT_INTERVAL_MS);
				}
			}
		}
		self.groups.retain(|record| !finished.contains(&record.group));
		due
	}

	pub fn version(&self) -> Version {
		self.version
	}

	/// Count a query this host discarded.
	pub fn record_invalid_query(&mut self) {
		self.invalid_queries = self.invalid_queries.saturating_add(1);
	}

	pub fn invalid_queries(&self) -> u32 {
		self.invalid_queries
	}

	pub fn joined(&self) -> Vec<Address> {
		self.groups.iter().filter(|record| record.joined).map(|record| record.group).collect()
	}

	pub fn get(&self, group: Address) -> Option<&GroupRecord> {
		self.groups.iter().find(|record| record.group == group)
	}

	pub fn len(&self) -> usize {
		self.groups.len()
	}

	pub fn is_empty(&self) -> bool {
		self.groups.is_empty()
	}

	pub fn refusals(&self) -> Refusals {
		self.refusals
	}

	/// The earliest deadline this listener has armed, over both obligations.
	pub fn next_deadline(&self) -> Option<u64> {
		self.groups.iter().flat_map(|record| [record.response.as_ref().map(|response| response.due_ms), record.retransmit_at_ms]).flatten().min()
	}
}

#[cfg(test)]
mod tests;

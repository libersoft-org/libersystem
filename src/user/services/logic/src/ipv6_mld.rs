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

/// One group's record.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GroupRecord {
	pub group: Address,
	/// False while the record is only being kept to finish its leave retransmissions.
	pub joined: bool,
	/// Sources this record names, for a source-specific answer.
	pub sources: Vec<Address>,
	/// How many state-change retransmissions are still owed.
	pub retransmits_left: u8,
	/// When the next retransmission or query answer is due.
	pub deadline_ms: Option<u64>,
}

/// The bounded listener.
#[derive(Debug)]
pub struct Listener {
	groups: Vec<GroupRecord>,
	version: Version,
	refusals: Refusals,
	invalid_queries: u32,
	/// Whether the post-detection re-report has been done.
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
	/// is simply asking about a membership that exists and cannot be reported.
	pub fn join(&mut self, group: Address, now_ms: u64) -> Option<Emission> {
		if group == ALL_NODES || group.kind() != Kind::Multicast {
			return None;
		}
		if let Some(record) = self.groups.iter_mut().find(|held| held.group == group) {
			// A REJOIN REUSES THE RECORD IT WAS LEAVING, which is what the leave path keeps it for.
			record.joined = true;
			record.retransmits_left = ROBUSTNESS;
			record.deadline_ms = Some(now_ms + UNSOLICITED_REPORT_INTERVAL_MS);
			return Some(self.report_kind(group));
		}
		if self.groups.len() as u32 >= Resource::MldGroups.limit() {
			self.refusals.record(Resource::MldGroups);
			return None;
		}
		self.groups.push(GroupRecord { group, joined: true, sources: Vec::new(), retransmits_left: ROBUSTNESS, deadline_ms: Some(now_ms + UNSOLICITED_REPORT_INTERVAL_MS) });
		Some(self.report_kind(group))
	}

	/// Leave a group. The record is KEPT until its retransmissions finish, so a rejoin reuses it and
	/// a lost Done is repeated.
	pub fn leave(&mut self, group: Address, now_ms: u64) -> Option<Emission> {
		let version = self.version;
		let record = self.groups.iter_mut().find(|held| held.group == group && held.joined)?;
		record.joined = false;
		record.sources.clear();
		record.retransmits_left = ROBUSTNESS;
		record.deadline_ms = Some(now_ms + UNSOLICITED_REPORT_INTERVAL_MS);
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
	///
	/// This is the second half of the ordering rule. A listener that never does it leaves the switch
	/// holding state learned from a report sourced from `::`, which some routers will not keep.
	pub fn report_after_dad(&mut self, now_ms: u64) -> Vec<Emission> {
		self.reported_after_dad = true;
		let groups: Vec<Address> = self.groups.iter().filter(|record| record.joined).map(|record| record.group).collect();
		for record in self.groups.iter_mut().filter(|record| record.joined) {
			record.retransmits_left = 0;
			record.deadline_ms = None;
		}
		let _ = now_ms;
		groups.into_iter().map(|group| self.report_kind(group)).collect()
	}

	/// Whether the post-detection re-report has happened.
	pub fn reported_after_dad(&self) -> bool {
		self.reported_after_dad
	}

	/// A validated query arrived.
	///
	/// `group` is `None` for a general query. `sources` narrows it to a source-specific query, which
	/// a version-1 listener cannot express and answers as address-specific instead. The response is
	/// DELAYED by a bounded amount so a link full of listeners does not answer at once.
	pub fn on_query(&mut self, group: Option<Address>, sources: &[Address], max_response_ms: u64, now_ms: u64) -> usize {
		let deadline = now_ms + max_response_ms.max(1) / 2;
		let mut scheduled = 0usize;
		for record in self.groups.iter_mut().filter(|record| record.joined) {
			if group.is_some_and(|asked| asked != record.group) {
				continue;
			}
			// A SECOND QUERY WHILE ONE IS PENDING does not add a second answer: the earlier deadline
			// wins, so the host answers once and sooner rather than twice.
			record.deadline_ms = Some(match record.deadline_ms {
				Some(existing) if existing <= deadline => existing,
				_ => deadline,
			});
			if !sources.is_empty() && self.version == Version::V2 {
				for source in sources {
					if record.sources.contains(source) {
						continue;
					}
					if record.sources.len() as u32 >= Resource::MldSourcesPerRecord.limit() {
						// DEGRADE TO ADDRESS-SPECIFIC rather than refusing the answer: an answer
						// about the whole group is correct, just less precise.
						record.sources.clear();
						break;
					}
					record.sources.push(*source);
				}
			}
			scheduled += 1;
		}
		scheduled
	}

	/// A version-1 querier was heard. Everything this host sends becomes version 1 until the timer
	/// runs out.
	pub fn saw_v1_querier(&mut self, now_ms: u64) {
		self.version = Version::V1 { until_ms: now_ms + V1_QUERIER_PRESENT_MS };
	}

	/// Drive time forward: expire compatibility mode and collect due emissions.
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
			let Some(deadline) = record.deadline_ms else {
				continue;
			};
			if deadline > now_ms {
				continue;
			}
			let emission = if record.joined {
				match version {
					Version::V1 { .. } => Emission::ReportV1 { group: record.group },
					Version::V2 => Emission::ReportV2 { group: record.group },
				}
			} else {
				match version {
					Version::V1 { .. } => Emission::DoneV1 { group: record.group },
					Version::V2 => Emission::LeaveV2 { group: record.group },
				}
			};
			due.push(emission);
			if record.retransmits_left > 0 {
				record.retransmits_left -= 1;
			}
			if record.retransmits_left == 0 {
				record.deadline_ms = None;
				if !record.joined {
					finished.push(record.group);
				}
			} else {
				record.deadline_ms = Some(now_ms + UNSOLICITED_REPORT_INTERVAL_MS);
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

	/// The earliest deadline this listener has armed, for the aggregated wait.
	pub fn next_deadline(&self) -> Option<u64> {
		self.groups.iter().filter_map(|record| record.deadline_ms).min()
	}
}

#[cfg(test)]
mod tests;

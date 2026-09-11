//! Which families this service runs, when each is ready, and what a pending operation costs.
//!
//! READINESS IS AN OBSERVATION, NOT A GATE. The service reports `online` and starts answering as soon
//! as it is listening, with every configured family still `Configuring`. Blocking the whole service
//! on a DHCP transaction - which is what it used to do - makes an IPv6-only boot wait for a
//! conversation it will never have, and makes every caller wait for a lease it may not need.
//!
//! AND READINESS CANNOT OVERRIDE A LOOKUP. Only `Disabled` is a family-wide veto; everything else is
//! decided per destination against the CURRENT sources and routes. A family that is `Configuring`
//! may still have a usable on-link path, and a family that is `Ready` may still have no route to one
//! particular destination - so a send asks the tables rather than the report.

/// Which families the service runs.
///
/// THREE VALUES AND NOT TWO FLAGS. Two independent enables are four states, one of which is "no
/// networking configured at all" - a state nobody wants and which would have to be given a meaning.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Families {
	Ipv4,
	Ipv6,
	Dual,
}

impl Families {
	/// Read the `net.families` key. An absent or unparseable value is `dual`, which is what the
	/// machine does today extended to the second family.
	pub fn parse(value: Option<&str>) -> Families {
		match value {
			Some("ipv4") => Families::Ipv4,
			Some("ipv6") => Families::Ipv6,
			_ => Families::Dual,
		}
	}

	pub fn includes_v4(&self) -> bool {
		matches!(self, Families::Ipv4 | Families::Dual)
	}

	pub fn includes_v6(&self) -> bool {
		matches!(self, Families::Ipv6 | Families::Dual)
	}

	pub fn as_text(&self) -> &'static str {
		match self {
			Families::Ipv4 => "ipv4",
			Families::Ipv6 => "ipv6",
			Families::Dual => "dual",
		}
	}
}

/// Where one family has got to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Readiness {
	/// The profile does not include it.
	Disabled,
	/// Included, and it has no usable address-and-route pair yet.
	Configuring,
	/// A valid non-tentative unicast address AND a usable route in that family on the same
	/// interface. AN ON-LINK ROUTE IS ENOUGH: a host that can reach its own link is working, and
	/// demanding a default route would report a router-less link as broken.
	Ready,
	/// No usable pair remains and configuration reported an explicit failure with no recovery
	/// pending. A REPORT, recomputed on state changes, rather than a terminal protocol state.
	Failed,
}

/// What a family has, for the readiness rule to read.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FamilyState {
	/// A valid non-tentative unicast address, preferred or deprecated.
	pub has_address: bool,
	/// Any usable route in that family on the same interface - on-link counts.
	pub has_route: bool,
	/// Configuration reported an explicit failure and nothing is retrying.
	pub failed: bool,
	/// Something is still trying: a DHCP exchange, or router solicitation.
	pub recovering: bool,
}

/// Compute one family's readiness.
///
/// SOLICITATION REACHING ITS MAXIMUM INTERVAL IS NEVER FAILURE. P02M0174 solicits indefinitely, so a
/// link with no router is a link this host keeps asking - and a family without a usable pair stays
/// `Configuring` rather than being reported broken for waiting.
pub fn readiness(included: bool, state: FamilyState) -> Readiness {
	if !included {
		return Readiness::Disabled;
	}
	if state.has_address && state.has_route {
		return Readiness::Ready;
	}
	if state.failed && !state.recovering {
		return Readiness::Failed;
	}
	Readiness::Configuring
}

/// What kind of pending operation a slot holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PendingKind {
	Dns,
	Sntp,
	/// The service's own lease work, which client work may never take capacity from.
	Dhcp,
	/// A caller's ping or probe. The two share one cap because they are one mechanism.
	Diagnostic,
}

/// The service-wide cap for each kind.
pub const DNS_CAP: u32 = 16;
pub const SNTP_CAP: u32 = 8;
pub const DHCP_RESERVED: u32 = 2;
pub const DIAGNOSTIC_CAP: u32 = 16;

/// The per-client caps, where a kind has one.
pub const DNS_PER_CLIENT: u32 = 4;
pub const DIAGNOSTIC_PER_CLIENT: u32 = 4;

/// The whole pending partition, and the scheduler beside it.
pub const PENDING_SLOTS: u32 = 128;
pub const SCHEDULER_SLOTS: u32 = 160;

/// Why an operation was refused before anything was sent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PendingRefusal {
	/// This kind is at its service-wide cap.
	Kind,
	/// This client is at its own cap for this kind.
	PerClient,
	/// The partition itself is full.
	Partition,
}

/// One kind's occupancy.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KindUse {
	pub used: u32,
	pub refusals: u32,
}

/// The pending-operation partition.
///
/// HEADROOM IS NOT PERMISSION. The caps sum to 42 of the 128 slots, and the 86 that are left are
/// unavailable to any kind: a seventeenth DNS operation is refused while the partition is two-thirds
/// empty, because the cap is what bounds one kind's share and the partition is what bounds the whole.
#[derive(Debug, Default)]
pub struct Pending {
	dns: KindUse,
	sntp: KindUse,
	dhcp: KindUse,
	diagnostic: KindUse,
	/// Per-client counts, keyed by the client's channel identity.
	clients: alloc::vec::Vec<(u64, u32, u32)>,
}

impl Pending {
	pub fn new() -> Pending {
		Pending::default()
	}

	pub fn used(&self, kind: PendingKind) -> u32 {
		self.kind_use(kind).used
	}

	pub fn refusals(&self, kind: PendingKind) -> u32 {
		self.kind_use(kind).refusals
	}

	pub fn total(&self) -> u32 {
		self.dns.used + self.sntp.used + self.dhcp.used + self.diagnostic.used
	}

	fn kind_use(&self, kind: PendingKind) -> KindUse {
		match kind {
			PendingKind::Dns => self.dns,
			PendingKind::Sntp => self.sntp,
			PendingKind::Dhcp => self.dhcp,
			PendingKind::Diagnostic => self.diagnostic,
		}
	}

	fn cap(kind: PendingKind) -> u32 {
		match kind {
			PendingKind::Dns => DNS_CAP,
			PendingKind::Sntp => SNTP_CAP,
			PendingKind::Dhcp => DHCP_RESERVED,
			PendingKind::Diagnostic => DIAGNOSTIC_CAP,
		}
	}

	fn per_client_cap(kind: PendingKind) -> Option<u32> {
		match kind {
			PendingKind::Dns => Some(DNS_PER_CLIENT),
			PendingKind::Diagnostic => Some(DIAGNOSTIC_PER_CLIENT),
			_ => None,
		}
	}

	fn client_counts(&self, client: u64) -> (u32, u32) {
		self.clients.iter().find(|(held, _, _)| *held == client).map(|(_, dns, diagnostic)| (*dns, *diagnostic)).unwrap_or((0, 0))
	}

	/// Reserve a slot before anything is transmitted.
	///
	/// RESERVED BEFORE THE SEND, which is what makes a refusal cheap: a caller is told no before a
	/// packet leaves, rather than after one has and the reply has nowhere to go.
	pub fn admit(&mut self, kind: PendingKind, client: u64) -> Result<(), PendingRefusal> {
		let refusal: Option<PendingRefusal> = if self.kind_use(kind).used >= Pending::cap(kind) {
			Some(PendingRefusal::Kind)
		} else if Pending::per_client_cap(kind).is_some_and(|cap| {
			let (dns, diagnostic) = self.client_counts(client);
			let held = match kind {
				PendingKind::Dns => dns,
				_ => diagnostic,
			};
			held >= cap
		}) {
			Some(PendingRefusal::PerClient)
		} else if self.total() >= PENDING_SLOTS {
			Some(PendingRefusal::Partition)
		} else {
			None
		};
		if let Some(refusal) = refusal {
			self.record_refusal(kind);
			return Err(refusal);
		}
		self.bump(kind, 1);
		if Pending::per_client_cap(kind).is_some() {
			match self.clients.iter_mut().find(|(held, _, _)| *held == client) {
				Some((_, dns, diagnostic)) => match kind {
					PendingKind::Dns => *dns += 1,
					_ => *diagnostic += 1,
				},
				None => self.clients.push(match kind {
					PendingKind::Dns => (client, 1, 0),
					_ => (client, 0, 1),
				}),
			}
		}
		Ok(())
	}

	/// The operation finished or expired.
	pub fn release(&mut self, kind: PendingKind, client: u64) {
		self.bump(kind, -1);
		if Pending::per_client_cap(kind).is_none() {
			return;
		}
		if let Some(index) = self.clients.iter().position(|(held, _, _)| *held == client) {
			let (_, dns, diagnostic) = &mut self.clients[index];
			match kind {
				PendingKind::Dns => *dns = dns.saturating_sub(1),
				_ => *diagnostic = diagnostic.saturating_sub(1),
			}
			if *dns == 0 && *diagnostic == 0 {
				self.clients.remove(index);
			}
		}
	}

	/// A client went away: everything it was holding is released.
	pub fn release_client(&mut self, client: u64) {
		let Some(index) = self.clients.iter().position(|(held, _, _)| *held == client) else {
			return;
		};
		let (_, dns, diagnostic) = self.clients.remove(index);
		self.dns.used = self.dns.used.saturating_sub(dns);
		self.diagnostic.used = self.diagnostic.used.saturating_sub(diagnostic);
	}

	fn bump(&mut self, kind: PendingKind, delta: i32) {
		let slot = match kind {
			PendingKind::Dns => &mut self.dns,
			PendingKind::Sntp => &mut self.sntp,
			PendingKind::Dhcp => &mut self.dhcp,
			PendingKind::Diagnostic => &mut self.diagnostic,
		};
		slot.used = match delta {
			d if d < 0 => slot.used.saturating_sub(d.unsigned_abs()),
			d => slot.used.saturating_add(d as u32),
		};
	}

	fn record_refusal(&mut self, kind: PendingKind) {
		let slot = match kind {
			PendingKind::Dns => &mut self.dns,
			PendingKind::Sntp => &mut self.sntp,
			PendingKind::Dhcp => &mut self.dhcp,
			PendingKind::Diagnostic => &mut self.diagnostic,
		};
		slot.refusals = slot.refusals.saturating_add(1);
	}
}

/// The identifier and sequence one diagnostic echo uses.
///
/// ONE PERSISTENT COUNTER AND NOT THE HANDLER'S `seq = 0`. Two probes started a millisecond apart
/// would otherwise carry the same pair, and an error quoting one would be attributed to both - which
/// is exactly the correlation M3's rules exist to make possible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EchoCounter {
	identifier: u16,
	sequence: u16,
}

impl EchoCounter {
	pub fn new(identifier: u16) -> EchoCounter {
		EchoCounter { identifier, sequence: 0 }
	}

	/// The next pair. The identifier advances when the sequence wraps, so the FULL pair is not reused
	/// within one interface generation.
	pub fn next(&mut self) -> (u16, u16) {
		let pair = (self.identifier, self.sequence);
		self.sequence = self.sequence.wrapping_add(1);
		if self.sequence == 0 {
			self.identifier = self.identifier.wrapping_add(1);
		}
		pair
	}
}

#[cfg(test)]
mod tests;

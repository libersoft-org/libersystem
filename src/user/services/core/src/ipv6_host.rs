// The IPv6 host: the state the pure layer's decisions are made against, and the frames they produce.
//
// WHERE THE LINE IS. Every rule - what an address means, when a neighbour is dead, whether a router
// advertisement may be believed, how long a report is owed - lives in `service-logic`, which builds
// on the host and is tested there. This file owns what that crate deliberately cannot: the link's
// MAC address, the frame buffers, the monotonic clock, and the queue of things to transmit. It makes
// no decisions of its own, which is what keeps the tested logic and the shipped behaviour the same.
//
// IPv4 IS UNTOUCHED. This is reached from a new EtherType arm and shares no state with the stack
// beside it, so a link with no IPv6 router behaves exactly as it did before.
//
// THE OUTBOUND QUEUE IS BOUNDED, like everything else here. A host that queued a frame per event
// would be a host an attacker sizes; past the bound the frame is dropped and counted, and the
// protocols above are all retransmitting ones that recover from it.

use alloc::vec::Vec;
use service_logic::ipv6::{self, Address, Interface, Scoped};
use service_logic::ipv6_budget::{PendingQueue, Refusals, Resource, Snapshot, Usage};
use service_logic::ipv6_events::{Change, Identity, Invalidation, InvalidationQueue, QuotedErrorQueue};
use service_logic::ipv6_icmp::{self, PathMtuCache, RateLimiter};
use service_logic::ipv6_mld::{self, Emission, Listener};
use service_logic::ipv6_nd::{self, ND_HOP_LIMIT, NdOption};
use service_logic::ipv6_neighbour::{Action, Lookup, NeighbourCache};
use service_logic::ipv6_packet::{self, ETHERTYPE_IPV6, NEXT_ICMPV6};
use service_logic::ipv6_quote;
use service_logic::ipv6_route::{PrefixTable, Route, RouteKind, RouteTable};
use service_logic::ipv6_router::{Preference, Reachability, RouterList, RouterOutcome};
use service_logic::ipv6_slaac::{AddressSet, DadOutcome, Lifetime, PrefixInformation, RdnssSet};
use service_logic::ipv6_solicit::Solicitation;
use service_logic::ipv6_timers::{Timer, TimerKind, Timers};

// How many frames may wait to be transmitted before this host starts dropping them.
const MAX_OUTBOUND: usize = 32;

// The hop limit ordinary traffic leaves with, until a router advertisement says otherwise.
const DEFAULT_HOP_LIMIT: u8 = 64;

// What arrived for the layer above.
pub struct Delivery {
	pub source: Address,
	pub destination: Address,
	pub next_header: u8,
	pub payload: Vec<u8>,
	pub hop_limit: u8,
}

// One IPv6 interface and everything this host knows about it.
/// What became of a transport packet handed to this host.
///
/// A PACKET WAITING ON ADDRESS RESOLUTION HAS NOT BEEN TRANSMITTED, and the consumer that owns the
/// flow is the only layer that can act on the difference: its sequence space is not on the wire
/// until the frame reaches the driver, so a router cannot have seen it and a quotation of it is a
/// forgery. `Held` carries the token whose completion says when that changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handoff {
	/// The frame reached the driver.
	Sent,
	/// The frame is retained while the next hop resolves; this token's completion reports its fate.
	Held(u64),
	/// Nothing was queued: no route, no room, or a packet that could not be built.
	Refused,
}

pub struct Ipv6Host {
	interface: Interface,
	mac: [u8; 6],
	mtu: u16,
	hop_limit: u8,
	link_local: Option<Address>,
	addresses: AddressSet,
	neighbours: NeighbourCache,
	routers: RouterList,
	// WHAT IS ON-LINK AND WHERE EVERYTHING ELSE GOES. These are the two tables whose capacities the
	// budget declares; keeping the declaration without the tables would have been a bound nothing
	// enforced and a snapshot that reported zero used against a real limit.
	prefixes: PrefixTable,
	routes: RouteTable,
	path_mtu: PathMtuCache,
	rdnss: RdnssSet,
	listener: Listener,
	timers: Timers,
	invalidations: InvalidationQueue,
	quoted_errors: QuotedErrorQueue,
	pending: PendingQueue,
	/// Handoff outcomes for retained packets, drained by the consumer that owns the flow.
	completions: Vec<service_logic::ipv6_budget::Completion>,
	/// How often RFC 8028's restriction found no advertiser of the source's prefix and the general
	/// default-router rules were used instead. It is an ordinary state and worth saying.
	router_fallbacks: u32,
	solicitation: Solicitation,
	limiter: RateLimiter,
	outbound: Vec<Vec<u8>>,
	outbound_dropped: u32,
	// What arrived for a transport this host does not have yet. Bounded like everything else: the
	// consumer is the next milestone's, and until it exists these are drained and reported rather
	// than accumulated.
	deliveries: Vec<Delivery>,
	generation: u64,
	// The randomness the solicitation schedule draws from, replaced per computation. Kept as a
	// simple counter-driven sequence so a boot is reproducible; the schedule's jitter exists to
	// spread a link's hosts apart, not to be unpredictable.
	jitter: u32,
	// Sixty-four bits of kernel randomness, drawn once per prefix. The policy and its stated limit
	// are written where `interface_identifier` is.
	entropy: fn() -> [u8; 8],
	// The monotonic clock, in milliseconds.
	//
	// A DEADLINE IS MEASURED FROM THE ACTION, NOT FROM THE BATCH. `bring_up` reads a timestamp once
	// and then draws randomness, builds a listener report, builds a detection probe and queues all
	// of them before it sends the first router solicitation; arming that solicitation's timer from
	// the timestamp taken before all of that makes the first retransmission interval SHORT by
	// however long the batch took. Measured on the wire: 3.563 s against a schedule whose minimum is
	// 3.6 s, every time. The host stays free of I/O - the clock is a function the caller supplies,
	// exactly as the entropy is.
	clock: fn() -> u64,
}

impl Ipv6Host {
	pub fn new(mac: [u8; 6], index: u16, generation: u32, mtu: u16, entropy: fn() -> [u8; 8], clock: fn() -> u64) -> Ipv6Host {
		Ipv6Host { interface: Interface::new(index, generation), mac, mtu, hop_limit: DEFAULT_HOP_LIMIT, link_local: None, addresses: AddressSet::new(), neighbours: NeighbourCache::new(), routers: RouterList::new(), prefixes: PrefixTable::new(), routes: RouteTable::new(), path_mtu: PathMtuCache::new(), rdnss: RdnssSet::new(), listener: Listener::new(), timers: Timers::new(), invalidations: InvalidationQueue::new(), quoted_errors: QuotedErrorQueue::new(), pending: PendingQueue::new(), completions: Vec::new(), router_fallbacks: 0, solicitation: Solicitation::new(), limiter: RateLimiter::new(ipv6_icmp::DEFAULT_ERROR_RATE), outbound: Vec::new(), outbound_dropped: 0, deliveries: Vec::new(), generation: 0, jitter: 0, entropy, clock }
	}

	// Set the rate the ICMPv6 error bucket refills at, read once from configuration.
	pub fn set_error_rate(&mut self, configured: Option<&str>) {
		self.limiter = RateLimiter::from_config(configured);
	}

	// Bring the interface up: form the link-local address, tell the link about its solicited-node
	// group BEFORE detection runs, and start asking for a router.
	//
	// THE ORDER IS THE POINT. The report goes first, sourced from the unspecified address, because
	// the detection probe is addressed to exactly that group: a switch that has not been told to
	// forward it drops the probe, and detection then passes for an address somebody else holds.
	pub fn bring_up(&mut self, now_ms: u64) -> bool {
		// THE PER-FAMILY REFUSAL, and it is why the constructor does not raise the number it was
		// given. IPv6 requires 1280 bytes on every link it runs on; below that this host does not
		// bring the family up at all, rather than bringing up one that cannot send a legal packet.
		// Everything else on the interface is unaffected: IPv4 keeps working and the frame buffers
		// stay the size the interface reported.
		if !ipv6_packet::link_carries_ipv6(self.mtu) {
			return false;
		}
		let Some(id) = self.draw_identifier() else {
			return false;
		};
		// `fe80::/10` IS ON-LINK BY DEFINITION, not by advertisement, so it is installed with the
		// family. Without it a neighbour discovery reply to a link-local peer would have no route
		// covering it and would have to be a special case in every consumer.
		let _ = self.routes.install(Route { interface: self.interface, destination: service_logic::ipv6::Prefix::link_local(), next_hop: None, kind: RouteKind::LinkLocal, expires: Lifetime::Infinite });
		let mut bytes = [0u8; 16];
		bytes[0] = 0xfe;
		bytes[1] = 0x80;
		bytes[8..].copy_from_slice(&id);
		let candidate = Address::new(bytes);

		let group = candidate.solicited_node();
		if let Some(emission) = self.listener.join(group, now_ms) {
			self.emit_mld(emission, now_ms);
		}
		if self.addresses.add_tentative(self.interface, candidate, Lifetime::Infinite, Lifetime::Infinite).is_ok() {
			self.send_dad_probe(candidate);
			self.timers.set(Timer { kind: TimerKind::DuplicateAddress, identity: Identity::Address { interface: self.interface, address: candidate }, deadline: (self.clock)() + service_logic::ipv6_slaac::DAD_RETRANS_MS });
		}
		self.start_soliciting(now_ms);
		true
	}

	// Draw an identifier, refusing the reserved forms by drawing again a bounded number of times.
	fn draw_identifier(&mut self) -> Option<[u8; 8]> {
		for _ in 0..8 {
			if let Some(id) = ipv6::interface_identifier((self.entropy)()) {
				return Some(id);
			}
		}
		None
	}

	fn next_jitter(&mut self) -> i32 {
		// A cheap, reproducible spread across the permitted band.
		self.jitter = self.jitter.wrapping_mul(1_103_515_245).wrapping_add(12_345);
		((self.jitter >> 16) % 200) as i32 - 100
	}

	fn start_soliciting(&mut self, _now_ms: u64) {
		let jitter = self.next_jitter();
		self.send_router_solicitation();
		// FROM THE SEND, not from whenever this batch began. See the note at `clock`.
		let sent_at = (self.clock)();
		let interval = self.solicitation.start(sent_at, jitter);
		self.timers.set(Timer { kind: TimerKind::RouterSolicitation, identity: Identity::InterfaceState { interface: self.interface }, deadline: sent_at + interval });
	}

	fn bump_generation(&mut self) -> u64 {
		self.generation += 1;
		self.generation
	}

	fn note(&mut self, identity: Identity, change: Change) {
		let generation = self.bump_generation();
		self.invalidations.record(Invalidation { identity, change, generation });
	}

	// Put a whole frame on the outbound queue.
	fn transmit(&mut self, frame: Vec<u8>) {
		if self.outbound.len() >= MAX_OUTBOUND {
			self.outbound_dropped = self.outbound_dropped.saturating_add(1);
			return;
		}
		self.outbound.push(frame);
	}

	// Build and queue an ICMPv6 message to a multicast destination.
	fn send_icmp_multicast(&mut self, source: Address, destination: Address, hop_limit: u8, message: Vec<u8>) {
		let Some(mac) = destination.multicast_ethernet() else {
			return;
		};
		self.send_icmp_to_mac(mac, source, destination, hop_limit, message);
	}

	fn send_icmp_to_mac(&mut self, mac: [u8; 6], source: Address, destination: Address, hop_limit: u8, message: Vec<u8>) {
		self.send_icmp_framed(mac, source, destination, hop_limit, message, false);
	}

	// The one transmit path, with or without the Hop-by-Hop Router Alert header.
	//
	// ROUTER ALERT IS NOT OPTIONAL ON A LISTENER MESSAGE. RFC 9777 requires it on every MLDv2
	// message and RFC 2710 on every v1 one, and a router or snooping switch discards a message
	// without it - silently, which is the worst way for a membership report to fail. The checksum is
	// computed over the ICMPv6 message alone, which is what the pseudo-header covers; the option
	// sits between the fixed header and the message and is not part of it.
	fn send_icmp_framed(&mut self, mac: [u8; 6], source: Address, destination: Address, hop_limit: u8, mut message: Vec<u8>, router_alert: bool) {
		let checksum = ipv6_packet::pseudo_header_checksum(source, destination, NEXT_ICMPV6, &message);
		message[2..4].copy_from_slice(&checksum.to_be_bytes());
		let extension: &[u8] = if router_alert { &ipv6_packet::ROUTER_ALERT_HEADER } else { &[] };
		let body_len = extension.len() + message.len();
		let mut frame = Vec::with_capacity(ipv6_packet::ETHERNET_HEADER_LEN + ipv6_packet::HEADER_LEN + body_len);
		frame.extend_from_slice(&mac);
		frame.extend_from_slice(&self.mac);
		frame.extend_from_slice(&ETHERTYPE_IPV6.to_be_bytes());
		frame.push(0x60);
		frame.extend_from_slice(&[0, 0, 0]);
		frame.extend_from_slice(&(body_len as u16).to_be_bytes());
		frame.push(if router_alert { ipv6_packet::NEXT_HOP_BY_HOP } else { NEXT_ICMPV6 });
		frame.push(hop_limit);
		frame.extend_from_slice(&source.octets());
		frame.extend_from_slice(&destination.octets());
		frame.extend_from_slice(extension);
		frame.extend_from_slice(&message);
		self.transmit(frame);
	}

	// A detection probe: sourced from `::`, with no source link-layer option, to the target's
	// solicited-node group.
	fn send_dad_probe(&mut self, target: Address) {
		let message = ipv6_nd::build_neighbour_solicitation(target, None);
		self.send_icmp_multicast(ipv6::UNSPECIFIED, target.solicited_node(), ND_HOP_LIMIT, message);
	}

	fn send_router_solicitation(&mut self) {
		let source = self.link_local.unwrap_or(ipv6::UNSPECIFIED);
		let option = self.link_local.map(|_| self.mac);
		let message = ipv6_nd::build_router_solicitation(option);
		self.send_icmp_multicast(source, ipv6::ALL_ROUTERS, ND_HOP_LIMIT, message);
	}

	// An MLD message, with the envelope every one of them carries.
	fn emit_mld(&mut self, emission: Emission, _now_ms: u64) {
		let envelope = ipv6_mld::envelope(self.link_local, emission.destination());
		// The report body: a minimal v2 record naming the group, or the v1 form.
		let mut message = Vec::with_capacity(28);
		match emission {
			Emission::ReportV2 { group } | Emission::LeaveV2 { group } => {
				message.push(ipv6_icmp::MLD_REPORT_V2);
				message.extend_from_slice(&[0, 0, 0, 0, 0]);
				message.extend_from_slice(&1u16.to_be_bytes());
				// One group record: type, aux length, source count, the group.
				message.push(if matches!(emission, Emission::ReportV2 { .. }) { 4 } else { 3 });
				message.extend_from_slice(&[0, 0, 0]);
				message.extend_from_slice(&group.octets());
			}
			Emission::ReportV1 { group } | Emission::DoneV1 { group } => {
				message.push(if matches!(emission, Emission::ReportV1 { .. }) { 131 } else { 132 });
				message.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0]);
				message.extend_from_slice(&group.octets());
			}
		}
		let Some(mac) = envelope.destination.multicast_ethernet() else {
			return;
		};
		self.send_icmp_framed(mac, envelope.source, envelope.destination, envelope.hop_limit, message, envelope.router_alert);
	}

	// Read one Ethernet frame. Returns what belongs to the layer above, if anything.
	pub fn on_frame(&mut self, frame: &[u8], now_ms: u64) -> Option<Delivery> {
		let parsed = ipv6_packet::parse_frame(frame).ok()?;
		let mine = self.addresses.assigned(self.interface);
		let groups = self.joined_groups();
		if !ipv6_packet::destination_is_ours(parsed.header.destination, &mine, &groups) && !self.is_detection_target(&parsed) {
			return None;
		}
		if parsed.upper != NEXT_ICMPV6 {
			return Some(Delivery { source: parsed.header.source, destination: parsed.header.destination, next_header: parsed.upper, payload: parsed.payload.to_vec(), hop_limit: parsed.header.hop_limit });
		}
		if ipv6_icmp::verify(parsed.header.source, parsed.header.destination, parsed.payload).is_err() {
			return None;
		}
		self.on_icmp(&parsed, frame, now_ms)
	}

	// A detection probe for one of OUR tentative addresses is addressed to a group we joined but
	// whose address we do not yet hold, so it has to be recognised separately.
	fn is_detection_target(&self, parsed: &ipv6_packet::Parsed<'_>) -> bool {
		parsed.header.destination.kind() == ipv6::Kind::Multicast && self.joined_groups().contains(&parsed.header.destination)
	}

	fn joined_groups(&self) -> Vec<Address> {
		let mut groups = self.listener.joined();
		groups.push(ipv6::ALL_NODES);
		groups
	}

	fn on_icmp(&mut self, parsed: &ipv6_packet::Parsed<'_>, _frame: &[u8], now_ms: u64) -> Option<Delivery> {
		let message = parsed.payload;
		let message_type = *message.first()?;
		match message_type {
			ipv6_icmp::ECHO_REQUEST => {
				// THE REPLY COMES FROM THE ADDRESS THAT WAS ASKED. RFC 4443 says so, and a peer that
				// pinged one of this host's addresses and got an answer from another cannot match it
				// to what it sent. Only a request to a MULTICAST destination has no such address, and
				// that one falls back to ordinary source selection.
				let source = match self.addresses.assigned(self.interface).contains(&parsed.header.destination) {
					true => parsed.header.destination,
					false => self.source_for(parsed.header.source)?,
				};
				let reply = ipv6_icmp::build_echo_reply(source, parsed.header.source, message).ok()?;
				self.send_unicast_icmp(parsed.header.source, source, reply, now_ms);
				None
			}
			ipv6_icmp::ECHO_REPLY => Some(Delivery { source: parsed.header.source, destination: parsed.header.destination, next_header: NEXT_ICMPV6, payload: message.to_vec(), hop_limit: parsed.header.hop_limit }),
			ipv6_icmp::NEIGHBOUR_SOLICITATION => {
				self.on_neighbour_solicitation(parsed, now_ms);
				None
			}
			ipv6_icmp::NEIGHBOUR_ADVERTISEMENT => {
				self.on_neighbour_advertisement(parsed, now_ms);
				None
			}
			ipv6_icmp::ROUTER_ADVERTISEMENT => {
				self.on_router_advertisement(parsed, now_ms);
				None
			}
			ipv6_icmp::MLD_QUERY => {
				self.on_mld_query(parsed, now_ms);
				None
			}
			_ if ipv6_icmp::is_error(message_type) => {
				self.on_icmp_error(parsed);
				None
			}
			_ => None,
		}
	}

	// The source address to answer `peer` from: a link-local peer is answered from the link-local
	// address, anything else from the first preferred address that is not link-local.
	fn source_for(&self, peer: Address) -> Option<Address> {
		let assigned = self.addresses.assigned(self.interface);
		if peer.kind() == ipv6::Kind::LinkLocalUnicast {
			return self.link_local;
		}
		assigned.iter().copied().find(|address| address.kind() == ipv6::Kind::GlobalUnicast).or(self.link_local)
	}

	// WHOSE LINK-LAYER ADDRESS TO RESOLVE, which is not the peer's as soon as the peer is not a
	// neighbour. A host that resolved an off-link destination directly would solicit an address
	// nothing on this link answers for, and the packet would sit in the pending queue until it gave
	// up - the failure looking exactly like an unreachable neighbour rather than a missing route.
	//
	// NO ROUTE AT ALL LEAVES THE PEER ITSELF, which is what this host did before it had a route table
	// and is still right for a link with no router: everything is either a neighbour or unreachable,
	// and resolution is what says which.
	// Which neighbour a packet to `peer` is handed to, given the `source` it will carry.
	//
	// RFC 8028 SECTION 3: the default router is chosen from the routers that advertise the prefix
	// THIS SOURCE was formed from, not from the whole list. Sending from an address one router
	// delegated through a different router is what makes an ISP's source-address filter drop the
	// packet - and the failure looks like a working connection that never gets a reply.
	//
	// THE ON-LINK DECISION COMES FIRST AND IS NOT FILTERED. RFC 4861 section 5.2 determines an
	// on-link next hop before default-router selection; RFC 8028 extends only the later choice, so a
	// direct route is never replaced by an advertising router.
	fn next_hop_for(&mut self, peer: Address, source: Address, now_ms: u64) -> Address {
		let Some(route) = self.route_for(peer, now_ms) else {
			return peer;
		};
		let Some(next_hop) = route.next_hop else {
			return peer;
		};
		let Some(advertisers) = self.prefixes.covering(self.interface, source, now_ms).map(|entry| entry.advertisers().to_vec()) else {
			// The source was not formed from a learned prefix - a link-local source, or a statically
			// configured address. There is no advertiser set to restrict to.
			return next_hop;
		};
		match service_logic::addr_select::default_router_for_source(&self.routers.ordered(), &advertisers) {
			Some(router) => router,
			None => {
				// EVERY ADVERTISER OF THAT PREFIX HAS GONE while the address is still valid on the
				// prefix's own lifetime. The general rules apply, and the fallback is recorded.
				self.router_fallbacks = self.router_fallbacks.saturating_add(1);
				next_hop
			}
		}
	}

	/// Send an echo request to `peer`, and say whether it was queued.
	///
	/// THE SOURCE IS SELECTED LIKE ANY OTHER OUTBOUND PACKET'S, and the identity goes on the wire so
	/// the reply can be matched to THIS probe rather than to whichever one was looked at first.
	pub fn send_echo(&mut self, peer: Address, identifier: u16, sequence: u16, hop_limit: u8, payload: &[u8], now_ms: u64) -> bool {
		let Some(source) = self.source_for(peer) else {
			return false;
		};
		let message = ipv6_icmp::build_echo_request(source, peer, identifier, sequence, payload);
		self.send_unicast_icmp_hops(peer, source, hop_limit, message, now_ms);
		true
	}

	/// This interface's default hop limit, for a caller that is not tracing.
	pub fn hop_limit(&self) -> u8 {
		self.hop_limit
	}

	/// How often the general default-router rules were used because the source's prefix had no
	/// advertiser left.
	pub fn router_fallbacks(&self) -> u32 {
		self.router_fallbacks
	}

	fn send_unicast_icmp(&mut self, peer: Address, source: Address, message: Vec<u8>, now_ms: u64) {
		self.send_unicast_icmp_hops(peer, source, self.hop_limit, message, now_ms);
	}

	// The same, with the hop limit the caller chose. A traceroute probe IS its hop limit: the whole
	// mechanism is sending one that expires and reading who said so.
	fn send_unicast_icmp_hops(&mut self, peer: Address, source: Address, hop_limit: u8, message: Vec<u8>, now_ms: u64) {
		let next_hop = self.next_hop_for(peer, source, now_ms);
		let (lookup, action) = self.neighbours.resolve(self.interface, next_hop, now_ms);
		self.perform(action, now_ms);
		match lookup {
			Lookup::Ready { link_layer } => self.send_icmp_to_mac(link_layer, source, peer, hop_limit, message),
			Lookup::Pending => {
				// Build the frame with a placeholder destination and retain it: the neighbour's
				// address is filled in when resolution completes, which is what the pending queue is
				// for. Keeping the whole frame is what the byte budget charges.
				let mut held = message;
				let checksum = ipv6_packet::pseudo_header_checksum(source, peer, NEXT_ICMPV6, &held);
				held[2..4].copy_from_slice(&checksum.to_be_bytes());
				if let Ok(packet) = ipv6_packet::build_packet(source, peer, NEXT_ICMPV6, hop_limit, &held, self.mtu) {
					let mut frame = Vec::with_capacity(ipv6_packet::ETHERNET_HEADER_LEN + packet.len());
					frame.extend_from_slice(&[0; 6]);
					frame.extend_from_slice(&self.mac);
					frame.extend_from_slice(&ETHERTYPE_IPV6.to_be_bytes());
					frame.extend_from_slice(&packet);
					let _ = self.pending.admit(self.interface, next_hop, frame);
				}
			}
			Lookup::Capacity => {}
		}
	}

	fn on_neighbour_solicitation(&mut self, parsed: &ipv6_packet::Parsed<'_>, now_ms: u64) {
		let Ok(solicitation) = ipv6_nd::decode_neighbour_solicitation(parsed.payload, parsed.header.hop_limit) else {
			return;
		};
		// A PROBE FROM `::` ABOUT AN ADDRESS WE ARE TESTING is the duplicate we are looking for.
		if parsed.header.source == ipv6::UNSPECIFIED {
			if self.addresses.on_dad_conflict(self.interface, solicitation.target) == (DadOutcome::Duplicate { address: solicitation.target }) {
				self.timers.clear_identity(Identity::Address { interface: self.interface, address: solicitation.target });
				self.note(Identity::Address { interface: self.interface, address: solicitation.target }, Change::Invalidated);
			}
			return;
		}
		for option in &solicitation.options {
			if let NdOption::SourceLinkLayer(mac) = option {
				let action = self.neighbours.on_solicitation(self.interface, parsed.header.source, *mac);
				self.perform(action, now_ms);
			}
		}
		// Answer only for an address this host actually holds.
		if !self.addresses.assigned(self.interface).contains(&solicitation.target) {
			return;
		}
		let reply = ipv6_nd::build_neighbour_advertisement(solicitation.target, self.mac, false, true, true);
		self.send_unicast_icmp_nd(parsed.header.source, solicitation.target, reply, now_ms);
	}

	// Neighbour-discovery messages leave with hop limit 255, which is the only authentication the
	// protocol has.
	fn send_unicast_icmp_nd(&mut self, peer: Address, source: Address, message: Vec<u8>, now_ms: u64) {
		let (lookup, action) = self.neighbours.resolve(self.interface, peer, now_ms);
		self.perform(action, now_ms);
		if let Lookup::Ready { link_layer } = lookup {
			self.send_icmp_to_mac(link_layer, source, peer, ND_HOP_LIMIT, message);
		}
	}

	fn on_neighbour_advertisement(&mut self, parsed: &ipv6_packet::Parsed<'_>, now_ms: u64) {
		let Ok(advertisement) = ipv6_nd::decode_neighbour_advertisement(parsed.payload, parsed.header.hop_limit) else {
			return;
		};
		// An advertisement for an address we are still testing means somebody else holds it.
		if self.addresses.on_dad_conflict(self.interface, advertisement.target) == (DadOutcome::Duplicate { address: advertisement.target }) {
			self.timers.clear_identity(Identity::Address { interface: self.interface, address: advertisement.target });
			self.note(Identity::Address { interface: self.interface, address: advertisement.target }, Change::Invalidated);
			return;
		}
		let mac = advertisement.options.iter().find_map(|option| match option {
			NdOption::TargetLinkLayer(mac) => Some(*mac),
			_ => None,
		});
		let Some(mac) = mac else {
			return;
		};
		let action = self.neighbours.on_advertisement(self.interface, advertisement.target, mac, advertisement.solicited, advertisement.override_flag, advertisement.router, now_ms);
		self.perform(action, now_ms);
		self.refresh_router_reachability(advertisement.target);
	}

	// A ROUTER THAT WENT AWAY TAKES ITS ROUTES AND ITS PREFIX MEMBERSHIPS WITH IT, AND NOTHING ELSE.
	// A prefix two routers advertised stays on-link while the other one is still saying so; only the
	// prefixes this router was alone in asserting go, and their on-link routes go with them.
	fn retire_router(&mut self, router: Address) {
		self.routes.remove_next_hop(self.interface, router);
		for gone in self.prefixes.withdraw_router(self.interface, router) {
			self.routes.remove(self.interface, gone, None);
			self.note(Identity::Prefix { interface: self.interface, prefix: gone }, Change::Invalidated);
		}
	}

	fn refresh_router_reachability(&mut self, address: Address) {
		let class = match self.neighbours.get(self.interface, address) {
			Some(entry) => Reachability::of(entry.state),
			None => Reachability::Unusable,
		};
		self.routers.set_reachability(self.interface, address, class);
	}

	fn on_router_advertisement(&mut self, parsed: &ipv6_packet::Parsed<'_>, now_ms: u64) {
		let Ok(advertisement) = ipv6_nd::decode_router_advertisement(parsed.payload, parsed.header.hop_limit) else {
			return;
		};
		// A router advertisement is only meaningful from a link-local source.
		if parsed.header.source.kind() != ipv6::Kind::LinkLocalUnicast {
			return;
		}
		if advertisement.current_hop_limit != 0 {
			self.hop_limit = advertisement.current_hop_limit;
		}
		for option in &advertisement.options {
			match option {
				NdOption::SourceLinkLayer(mac) => {
					let action = self.neighbours.on_solicitation(self.interface, parsed.header.source, *mac);
					self.perform(action, now_ms);
				}
				NdOption::Mtu(value) => {
					// The rule and its reasons are at `accept_link_mtu`, where they can be tested.
					if let Some(lowered) = ipv6_packet::accept_link_mtu(self.mtu, *value) {
						self.mtu = lowered;
					}
				}
				NdOption::Prefix(information) => self.on_prefix(information, parsed.header.source, now_ms),
				NdOption::Rdnss { lifetime_seconds, servers } => {
					// KEPT, NOT MERELY ANNOUNCED. The capacity table declares four records keyed on
					// the advertising router and the server TOGETHER, and a layer that only emitted
					// an event would have declared a bound that nothing enforced - and would have
					// let one router's withdrawal take away a server another still offers.
					for server in servers {
						if self.rdnss.advertise(self.interface, parsed.header.source, *server, *lifetime_seconds, now_ms) {
							let change = if *lifetime_seconds == 0 { Change::Invalidated } else { Change::Changed };
							self.note(Identity::Rdnss { interface: self.interface, server: *server }, change);
						}
					}
					if *lifetime_seconds != 0 {
						self.timers.set(Timer { kind: TimerKind::RdnssLifetime, identity: Identity::InterfaceState { interface: self.interface }, deadline: now_ms + u64::from(*lifetime_seconds) * 1000 });
					}
				}
				NdOption::TargetLinkLayer(_) => {}
			}
		}
		let preference = Preference::from_bits(advertisement.preference_bits);
		let outcome = self.routers.advertise(self.interface, parsed.header.source, preference, u32::from(advertisement.router_lifetime_seconds), now_ms);
		self.refresh_router_reachability(parsed.header.source);
		match outcome {
			RouterOutcome::Added | RouterOutcome::Refreshed => {
				// THE DEFAULT ROUTE IS PART OF WHAT MAKES A ROUTER USABLE, and it expires with the
				// router lifetime rather than outliving it.
				let lifetime = Lifetime::Finite(now_ms + u64::from(advertisement.router_lifetime_seconds) * 1000);
				let route = Route { interface: self.interface, destination: service_logic::ipv6::Prefix::default_route(), next_hop: Some(parsed.header.source), kind: RouteKind::Default, expires: lifetime };
				let _ = self.routes.install(route);
				self.note(Identity::Route { interface: self.interface, destination: service_logic::ipv6::Prefix::default_route(), next_hop: parsed.header.source }, Change::Changed);
				self.note(Identity::Router { interface: self.interface, router: parsed.header.source }, Change::Changed);
				self.timers.set(Timer { kind: TimerKind::RouterLifetime, identity: Identity::Router { interface: self.interface, router: parsed.header.source }, deadline: now_ms + u64::from(advertisement.router_lifetime_seconds) * 1000 });
				// ONLY AN INSTALLED DEFAULT ROUTE STOPS SOLICITATION.
				self.solicitation.default_route_installed();
				self.timers.clear(TimerKind::RouterSolicitation, Identity::InterfaceState { interface: self.interface });
			}
			RouterOutcome::Withdrawn => {
				self.retire_router(parsed.header.source);
				self.note(Identity::Router { interface: self.interface, router: parsed.header.source }, Change::Invalidated);
				if self.routers.is_empty() {
					let jitter = self.next_jitter();
					let interval = self.solicitation.router_list_empty(now_ms, jitter);
					self.send_router_solicitation();
					self.timers.set(Timer { kind: TimerKind::RouterSolicitation, identity: Identity::InterfaceState { interface: self.interface }, deadline: now_ms + interval });
				}
			}
			RouterOutcome::NotPresent | RouterOutcome::Capacity => {}
		}
	}

	fn on_prefix(&mut self, information: &PrefixInformation, router: Address, now_ms: u64) {
		let Ok(outcome) = service_logic::ipv6_slaac::evaluate_prefix(information, now_ms) else {
			return;
		};
		if let Some(valid) = outcome.on_link_route {
			// THE PREFIX AND THE ROUTE ARE RECORDED TOGETHER OR NOT AT ALL. A prefix admitted whose
			// route the table refused would leave this host believing a span is on-link with nothing
			// able to reach it, so the refusal takes the prefix back out.
			if self.prefixes.advertise(self.interface, information.prefix, router, valid).is_ok() {
				let route = Route { interface: self.interface, destination: information.prefix, next_hop: None, kind: RouteKind::OnLink, expires: valid };
				if self.routes.install(route).is_err() {
					self.prefixes.withdraw(self.interface, information.prefix, router);
					return;
				}
				self.note(Identity::Prefix { interface: self.interface, prefix: information.prefix }, Change::Changed);
				if let Lifetime::Finite(deadline) = valid {
					self.timers.set(Timer { kind: TimerKind::PrefixLifetime, identity: Identity::Prefix { interface: self.interface, prefix: information.prefix }, deadline });
				}
			}
		}
		let Some((preferred, valid)) = outcome.address else {
			return;
		};
		let Some(id) = self.draw_identifier() else {
			return;
		};
		let Some(address) = information.prefix.with_interface_id(id) else {
			return;
		};
		if self.addresses.get(self.interface, address).is_some() {
			self.addresses.refresh(self.interface, address, preferred, valid, now_ms);
			return;
		}
		let group = address.solicited_node();
		if let Some(emission) = self.listener.join(group, now_ms) {
			self.emit_mld(emission, now_ms);
		}
		if self.addresses.add_tentative(self.interface, address, preferred, valid).is_ok() {
			self.send_dad_probe(address);
			self.timers.set(Timer { kind: TimerKind::DuplicateAddress, identity: Identity::Address { interface: self.interface, address }, deadline: now_ms + service_logic::ipv6_slaac::DAD_RETRANS_MS });
		}
	}

	fn on_mld_query(&mut self, parsed: &ipv6_packet::Parsed<'_>, now_ms: u64) {
		// MLD's envelope, not neighbour discovery's: hop limit ONE and a Router Alert option.
		if ipv6_mld::validate_query(parsed.header.source, parsed.header.hop_limit, parsed.router_alert).is_err() {
			self.listener.record_invalid_query();
			return;
		}
		let message = parsed.payload;
		let max_response_ms = if message.len() >= 6 { u64::from(u16::from_be_bytes([message[4], message[5]])) } else { 10_000 };
		let group = if message.len() >= 24 {
			let mut bytes = [0u8; 16];
			bytes.copy_from_slice(&message[8..24]);
			let asked = Address::new(bytes);
			(asked != ipv6::UNSPECIFIED).then_some(asked)
		} else {
			None
		};
		self.listener.on_query(group, &[], max_response_ms, now_ms);
		if let Some(deadline) = self.listener.next_deadline() {
			self.timers.set(Timer { kind: TimerKind::MldResponseDelay, identity: Identity::InterfaceState { interface: self.interface }, deadline });
		}
	}

	fn on_icmp_error(&mut self, parsed: &ipv6_packet::Parsed<'_>) {
		// THE VALIDATION IS THE LOGIC CRATE'S, so what a consumer is handed here is exactly what the
		// host tests hand their stand-in consumers. The strongest check this layer can make is that
		// the quoted source is an address this interface holds; whether this host actually SENT the
		// quoted packet is a lookup in flow state it does not keep, and the consumer makes it before
		// anything durable is written.
		let held = self.addresses.assigned(self.interface);
		match ipv6_quote::validate(self.interface, parsed.header.source, parsed.payload, &held) {
			Ok(event) => {
				self.quoted_errors.offer(event);
			}
			// AN ERROR NOBODY CAN BE TOLD ABOUT IS STILL AN EVENT WORTH COUNTING, and counting is all
			// it gets: a flood of unattributable quotations must cost this host a counter increment
			// and not a log line.
			Err(_) => self.quoted_errors.note_dropped(),
		}
	}

	fn perform(&mut self, action: Action, now_ms: u64) {
		match action {
			Action::SolicitMulticast { target } => {
				let source = self.link_local.unwrap_or(ipv6::UNSPECIFIED);
				let option = self.link_local.map(|_| self.mac);
				let message = ipv6_nd::build_neighbour_solicitation(target, option);
				self.send_icmp_multicast(source, target.solicited_node(), ND_HOP_LIMIT, message);
				self.timers.set(Timer { kind: TimerKind::NeighbourRetry, identity: Identity::Neighbour { interface: self.interface, neighbour: target }, deadline: now_ms + service_logic::ipv6_neighbour::RETRANS_TIMER_MS });
			}
			Action::SolicitUnicast { target, link_layer } => {
				let Some(source) = self.link_local else {
					return;
				};
				let message = ipv6_nd::build_neighbour_solicitation(target, Some(self.mac));
				self.send_icmp_to_mac(link_layer, source, target, ND_HOP_LIMIT, message);
				self.timers.set(Timer { kind: TimerKind::Unreachability, identity: Identity::Neighbour { interface: self.interface, neighbour: target }, deadline: now_ms + service_logic::ipv6_neighbour::RETRANS_TIMER_MS });
			}
			Action::Retire { target } => {
				self.timers.clear_identity(Identity::Neighbour { interface: self.interface, neighbour: target });
				// THE OWNER OF EACH RETAINED PACKET IS TOLD IT NEVER WENT OUT. Discarding these left
				// the consumer believing sequence space was transmitted that no router ever saw.
				let failures = self.pending.fail_for(self.interface, target, service_logic::ipv6_budget::FailureCause::ResolutionFailed);
				self.completions.extend(failures);
				self.note(Identity::Neighbour { interface: self.interface, neighbour: target }, Change::Invalidated);
				self.routers.set_reachability(self.interface, target, Reachability::Unusable);
			}
			Action::Resolved { target, link_layer } => {
				self.timers.clear(TimerKind::NeighbourRetry, Identity::Neighbour { interface: self.interface, neighbour: target });
				for held in self.pending.take_for(self.interface, target) {
					let token = held.token;
					let mut frame = held.frame;
					frame[..6].copy_from_slice(&link_layer);
					self.transmit(frame);
					self.completions.push(service_logic::ipv6_budget::Completion::Sent { token, at: now_ms });
				}
				self.note(Identity::Neighbour { interface: self.interface, neighbour: target }, Change::Changed);
				self.refresh_router_reachability(target);
			}
			Action::Nothing => {}
		}
	}

	// Drive every timer that is due. Bounded by the seam's per-iteration limit.
	pub fn on_timer(&mut self, now_ms: u64) {
		for timer in self.timers.due(now_ms) {
			match timer.kind {
				TimerKind::DuplicateAddress => {
					let Identity::Address { address, .. } = timer.identity else {
						continue;
					};
					match self.addresses.on_dad_timeout(self.interface, address) {
						DadOutcome::Probe { address } => {
							self.send_dad_probe(address);
							self.timers.set(Timer { kind: TimerKind::DuplicateAddress, identity: timer.identity, deadline: now_ms + service_logic::ipv6_slaac::DAD_RETRANS_MS });
						}
						DadOutcome::Assigned { address } => {
							if address.kind() == ipv6::Kind::LinkLocalUnicast && self.link_local.is_none() {
								self.link_local = Some(address);
								// EVERY JOINED GROUP IS REPORTED AGAIN now that a real source exists.
								for emission in self.listener.report_after_dad(now_ms) {
									self.emit_mld(emission, now_ms);
								}
							}
							self.note(Identity::Address { interface: self.interface, address }, Change::Changed);
						}
						DadOutcome::Duplicate { .. } | DadOutcome::Nothing => {}
					}
				}
				TimerKind::RouterSolicitation => {
					let jitter = self.next_jitter();
					if self.solicitation.is_running() {
						self.send_router_solicitation();
						let sent_at = (self.clock)();
						if let Some(interval) = self.solicitation.on_timeout(sent_at, jitter) {
							self.timers.set(Timer { kind: TimerKind::RouterSolicitation, identity: timer.identity, deadline: sent_at + interval });
						}
					}
				}
				TimerKind::NeighbourRetry | TimerKind::Unreachability => {
					let Identity::Neighbour { neighbour, .. } = timer.identity else {
						continue;
					};
					let action = self.neighbours.on_timeout(self.interface, neighbour, now_ms);
					self.perform(action, now_ms);
				}
				TimerKind::RouterLifetime => {
					for gone in self.routers.expire(now_ms) {
						self.retire_router(gone);
						self.note(Identity::Router { interface: self.interface, router: gone }, Change::Invalidated);
					}
					if self.routers.is_empty() && !self.solicitation.is_running() {
						let jitter = self.next_jitter();
						let interval = self.solicitation.router_list_empty(now_ms, jitter);
						self.send_router_solicitation();
						self.timers.set(Timer { kind: TimerKind::RouterSolicitation, identity: Identity::InterfaceState { interface: self.interface }, deadline: now_ms + interval });
					}
				}
				TimerKind::AddressLifetime | TimerKind::PrefixLifetime => {
					for gone in self.addresses.tick(now_ms) {
						self.note(Identity::Address { interface: self.interface, address: gone }, Change::Invalidated);
					}
					for gone in self.prefixes.expire(now_ms) {
						self.note(Identity::Prefix { interface: self.interface, prefix: gone }, Change::Invalidated);
					}
					for gone in self.routes.expire(now_ms) {
						self.note(Identity::Route { interface: self.interface, destination: gone.destination, next_hop: gone.next_hop.unwrap_or(ipv6::UNSPECIFIED) }, Change::Invalidated);
					}
				}
				TimerKind::PathMtuExpiry => {
					self.path_mtu.expire(now_ms);
				}
				TimerKind::MldResponseDelay | TimerKind::MldStateChangeRetransmit => {
					for emission in self.listener.tick(now_ms) {
						self.emit_mld(emission, now_ms);
					}
					if let Some(deadline) = self.listener.next_deadline() {
						self.timers.set(Timer { kind: TimerKind::MldStateChangeRetransmit, identity: Identity::InterfaceState { interface: self.interface }, deadline });
					}
				}
				TimerKind::RdnssLifetime => {
					for gone in self.rdnss.expire(now_ms) {
						self.note(Identity::Rdnss { interface: self.interface, server: gone }, Change::Invalidated);
					}
				}
			}
		}
		// The address lifetimes are checked every pass, not only when a timer names them: a
		// deprecation that nobody armed a timer for is still a deprecation.
		for gone in self.addresses.tick(now_ms) {
			self.note(Identity::Address { interface: self.interface, address: gone }, Change::Invalidated);
		}
		for emission in self.listener.tick(now_ms) {
			self.emit_mld(emission, now_ms);
		}
	}

	// The next moment this host has something to do, for the loop's aggregated wait.
	pub fn next_deadline(&self) -> Option<u64> {
		match (self.timers.next_deadline(), self.listener.next_deadline()) {
			(Some(left), Some(right)) => Some(left.min(right)),
			(left, right) => left.or(right),
		}
	}

	// Frames waiting to go out.
	pub fn take_outbound(&mut self) -> Vec<Vec<u8>> {
		core::mem::take(&mut self.outbound)
	}

	/// How many frames are waiting to go to the driver.
	///
	/// A CONSUMER DRIVING A TRANSPORT NEEDS THIS, because a segment handed to this host is not
	/// written into the caller's frame buffer: "the buffer is empty" and "nothing was sent" are the
	/// same answer for IPv4 and different answers here.
	pub fn outbound_len(&self) -> usize {
		self.outbound.len()
	}

	pub fn link_local(&self) -> Option<Address> {
		self.link_local
	}

	// The recursive DNS servers this host has been offered, once each. Two routers offering the same
	// server is one server to ask.
	pub fn resolvers(&self) -> Vec<Address> {
		self.rdnss.servers()
	}

	/// The neighbour-discovery state this host holds for an address, if any.
	///
	/// THE ROUTER LIST KEEPS A CLASS AND NOT A STATE, on purpose: its ordering depends on whether a
	/// router is usable and must not change when one moves between two usable states. A REPORT wants
	/// the state itself, which is why it asks the neighbour cache rather than the router list.
	pub fn neighbour_state(&self, address: Address) -> Option<service_logic::ipv6_neighbour::NeighbourState> {
		self.neighbours.get(self.interface, address).map(|entry| entry.state)
	}

	/// This interface's identity: the index and the generation a scoped value is checked against.
	pub fn interface(&self) -> Interface {
		self.interface
	}

	/// Every address this interface holds, WITH its state and lifetimes - not only the assigned
	/// ones. A report that showed only what is usable could not say why an address is not.
	pub fn configured(&self) -> Vec<service_logic::ipv6_slaac::ConfiguredAddress> {
		self.addresses.configured(self.interface)
	}

	/// The prefix length an address of this host's was formed under.
	///
	/// SIXTY-FOUR FOR EVERY ONE OF THEM, and that is a fact rather than a simplification: SLAAC forms
	/// an address only from a /64, and the link-local address is a /64 of `fe80::/10` by the same
	/// construction. The number a host reports for its OWN address is the length of the prefix it was
	/// formed under, not the length of the range that prefix came from.
	pub fn prefix_len(&self) -> u8 {
		64
	}

	pub fn addresses(&self) -> Vec<Address> {
		self.addresses.assigned(self.interface)
	}

	pub fn routers(&self) -> Vec<service_logic::ipv6_router::Router> {
		self.routers.ordered()
	}

	pub fn scoped(&self, address: Address) -> Option<Scoped> {
		Scoped::new(address, Some(self.interface))
	}

	/// The routes this interface holds, and the prefixes it believes are on-link.
	///
	/// The consumer that will SELECT one of these is the next milestone's; what this milestone owes
	/// is the table, its bound, and the ability to read it.
	pub fn routes(&self) -> &[Route] {
		self.routes.routes()
	}

	/// The route that covers a destination, longest prefix first.
	pub fn route_for(&self, destination: Address, now_ms: u64) -> Option<&Route> {
		self.routes.lookup(self.interface, destination, now_ms)
	}

	pub fn prefixes(&self) -> usize {
		self.prefixes.len()
	}

	pub fn mtu(&self) -> u16 {
		self.mtu
	}

	pub fn drain_invalidations(&mut self) -> Vec<Invalidation> {
		self.invalidations.drain()
	}

	pub fn resync_required(&self) -> bool {
		self.invalidations.resync_required()
	}

	/// How many quoted errors were validated, and how many arrived and could not be.
	pub fn quoted_error_counts(&self) -> (u32, u32) {
		(self.quoted_errors.accepted(), self.quoted_errors.dropped())
	}

	pub fn drain_quoted_errors(&mut self) -> Vec<service_logic::ipv6_events::QuotedError> {
		self.quoted_errors.drain()
	}

	// The capacity snapshot: used, limit and refusals for every bounded resource.
	pub fn snapshot(&self) -> Snapshot {
		let refusals: Refusals = self.pending.refusals();
		let usage = service_logic::ipv6_budget::RESOURCES
			.iter()
			.map(|resource| {
				let used = match resource {
					Resource::Interfaces => 1,
					Resource::UnicastAddresses => self.addresses.len() as u32,
					Resource::DefaultRouters => self.routers.len() as u32,
					Resource::Prefixes => self.prefixes.len() as u32,
					Resource::Routes => self.routes.len() as u32,
					// A PER-PREFIX BOUND HAS NO SINGLE "USED", so this reports the fullest set: the
					// one that will refuse first, which is what a consumer watching a ceiling needs.
					Resource::AdvertisersPerPrefix => self.prefixes.widest_advertiser_set() as u32,
					Resource::Neighbours => self.neighbours.len() as u32,
					Resource::PathMtu => self.path_mtu.len() as u32,
					Resource::MldGroups => self.listener.len() as u32,
					Resource::Rdnss => self.rdnss.len() as u32,
					Resource::InvalidationEvents => self.invalidations.len() as u32,
					Resource::QuotedErrorEvents => self.quoted_errors.len() as u32,
					Resource::PendingPerInterface => self.pending.len() as u32,
					Resource::PendingBytes => self.pending.bytes(),
					_ => 0,
				};
				let counted = refusals.get(*resource) + self.prefixes.refusals().get(*resource) + self.routes.refusals().get(*resource) + self.neighbours.refusals().get(*resource) + self.routers.refusals().get(*resource) + self.listener.refusals().get(*resource) + self.rdnss.refusals().get(*resource) + self.addresses.refusals().get(*resource);
				Usage { resource: *resource, used, limit: resource.limit(), refusals: counted }
			})
			.collect();
		Snapshot { usage, resolution_failures: self.pending.resolution_failures(), quoted_errors_dropped: self.quoted_errors.dropped(), icmp_errors_rate_limited: self.limiter.limited(), resync_required: self.invalidations.resync_required() }
	}

	// How many outbound frames were dropped for want of room.
	pub fn outbound_dropped(&self) -> u32 {
		self.outbound_dropped
	}

	// Read one frame and retain anything that belongs above this layer.
	//
	// SEPARATE FROM `on_frame` so the caller cannot forget the retention: the ingress path has one
	// entry point, and what it produces is taken by `take_deliveries` rather than returned into a
	// value the caller may drop.
	pub fn receive(&mut self, frame: &[u8], now_ms: u64) {
		if let Some(delivery) = self.on_frame(frame, now_ms) {
			// The bound is the invalidation queue's, reused: a layer with no consumer must not be
			// able to hold more than a bounded amount on its behalf.
			if self.deliveries.len() < Resource::InvalidationEvents.limit() as usize {
				self.deliveries.push(delivery);
			}
		}
	}

	/// Send a transport payload to `peer`, resolving the next hop and building the IPv6 header.
	///
	/// THE TRANSPORT OWNS ITS CHECKSUM AND THIS LAYER OWNS EVERYTHING BELOW IT. A transport cannot
	/// compute its pseudo-header checksum without knowing the source address, and the source address
	/// is selected here - so the caller asks for one with `source_for`, builds its segment, and hands
	/// the finished bytes over. The alternative is this layer reaching into a transport header to
	/// patch a field, which is how a second checksum implementation appears.
	pub fn send_transport(&mut self, peer: Address, source: Address, next_header: u8, payload: Vec<u8>, now_ms: u64) -> Handoff {
		let next_hop: Address = self.next_hop_for(peer, source, now_ms);
		let (lookup, action) = self.neighbours.resolve(self.interface, next_hop, now_ms);
		self.perform(action, now_ms);
		match lookup {
			Lookup::Ready { link_layer } => {
				let Ok(packet) = ipv6_packet::build_packet(source, peer, next_header, self.hop_limit, &payload, self.mtu) else {
					return Handoff::Refused;
				};
				let mut frame = Vec::with_capacity(ipv6_packet::ETHERNET_HEADER_LEN + packet.len());
				frame.extend_from_slice(&link_layer);
				frame.extend_from_slice(&self.mac);
				frame.extend_from_slice(&ETHERTYPE_IPV6.to_be_bytes());
				frame.extend_from_slice(&packet);
				self.transmit(frame);
				Handoff::Sent
			}
			Lookup::Pending => {
				// RETAINED RATHER THAN DROPPED. The neighbour is being resolved; the packet waits in
				// the bounded queue and goes out when it answers, which is what the queue is for.
				let Ok(packet) = ipv6_packet::build_packet(source, peer, next_header, self.hop_limit, &payload, self.mtu) else {
					return Handoff::Refused;
				};
				let mut frame = Vec::with_capacity(ipv6_packet::ETHERNET_HEADER_LEN + packet.len());
				frame.extend_from_slice(&[0; 6]);
				frame.extend_from_slice(&self.mac);
				frame.extend_from_slice(&ETHERTYPE_IPV6.to_be_bytes());
				frame.extend_from_slice(&packet);
				match self.pending.admit(self.interface, next_hop, frame) {
					Ok(token) => Handoff::Held(token.0),
					Err(_) => Handoff::Refused,
				}
			}
			Lookup::Capacity => Handoff::Refused,
		}
	}

	/// Record a lowering the consumer that owns the flow has validated.
	///
	/// Take every handoff outcome recorded since the last call.
	///
	/// THE CONSUMER DRAINS THEM, because it is the layer that knows which flow each token belongs to.
	pub fn take_completions(&mut self) -> Vec<service_logic::ipv6_budget::Completion> {
		core::mem::take(&mut self.completions)
	}

	/// THE ONLY WRITE TO THE PMTU CACHE, and it happens here because only the consumer could make the
	/// check that justifies it. A full table answers `Capacity` rather than claiming it recorded the
	/// lowering; the caller keeps the smaller limit it validated either way, which is what stops
	/// cache exhaustion from restoring a limit the path has already refused.
	pub fn record_path_mtu(&mut self, destination: Address, mtu: u32, now_ms: u64) -> service_logic::ipv6_icmp::MtuOutcome {
		self.path_mtu.record(self.interface, destination, mtu, now_ms)
	}

	/// The address this host would send to `peer` from, or `None` when it has none it may use.
	pub fn source_address(&self, peer: Address) -> Option<Address> {
		self.source_for(peer)
	}

	// Take what arrived for the layer above.
	pub fn take_deliveries(&mut self) -> Vec<Delivery> {
		core::mem::take(&mut self.deliveries)
	}
}

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
use service_logic::ipv6_events::{Change, ErrorClass, Identity, Invalidation, InvalidationQueue, QuotedErrorQueue};
use service_logic::ipv6_icmp::{self, PathMtuCache, RateLimiter};
use service_logic::ipv6_mld::{self, Emission, Listener};
use service_logic::ipv6_nd::{self, ND_HOP_LIMIT, NdOption};
use service_logic::ipv6_neighbour::{Action, Lookup, NeighbourCache};
use service_logic::ipv6_packet::{self, ETHERTYPE_IPV6, NEXT_ICMPV6};
use service_logic::ipv6_router::{Preference, Reachability, RouterList, RouterOutcome};
use service_logic::ipv6_slaac::{AddressSet, DadOutcome, Lifetime, PrefixInformation};
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
pub struct Ipv6Host {
	interface: Interface,
	mac: [u8; 6],
	mtu: u16,
	hop_limit: u8,
	link_local: Option<Address>,
	addresses: AddressSet,
	neighbours: NeighbourCache,
	routers: RouterList,
	path_mtu: PathMtuCache,
	listener: Listener,
	timers: Timers,
	invalidations: InvalidationQueue,
	quoted_errors: QuotedErrorQueue,
	pending: PendingQueue,
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
}

impl Ipv6Host {
	pub fn new(mac: [u8; 6], index: u16, generation: u32, mtu: u16, entropy: fn() -> [u8; 8]) -> Ipv6Host {
		Ipv6Host { interface: Interface::new(index, generation), mac, mtu: mtu.max(ipv6_packet::MIN_MTU), hop_limit: DEFAULT_HOP_LIMIT, link_local: None, addresses: AddressSet::new(), neighbours: NeighbourCache::new(), routers: RouterList::new(), path_mtu: PathMtuCache::new(), listener: Listener::new(), timers: Timers::new(), invalidations: InvalidationQueue::new(), quoted_errors: QuotedErrorQueue::new(), pending: PendingQueue::new(), solicitation: Solicitation::new(), limiter: RateLimiter::new(ipv6_icmp::DEFAULT_ERROR_RATE), outbound: Vec::new(), outbound_dropped: 0, deliveries: Vec::new(), generation: 0, jitter: 0, entropy }
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
	pub fn bring_up(&mut self, now_ms: u64) {
		let Some(id) = self.draw_identifier() else {
			return;
		};
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
			self.timers.set(Timer { kind: TimerKind::DuplicateAddress, identity: Identity::Address { interface: self.interface, address: candidate }, deadline: now_ms + service_logic::ipv6_slaac::DAD_RETRANS_MS });
		}
		self.start_soliciting(now_ms);
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

	fn start_soliciting(&mut self, now_ms: u64) {
		let jitter = self.next_jitter();
		let interval = self.solicitation.start(now_ms, jitter);
		self.send_router_solicitation();
		self.timers.set(Timer { kind: TimerKind::RouterSolicitation, identity: Identity::InterfaceState { interface: self.interface }, deadline: now_ms + interval });
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

	fn send_icmp_to_mac(&mut self, mac: [u8; 6], source: Address, destination: Address, hop_limit: u8, mut message: Vec<u8>) {
		let checksum = ipv6_packet::pseudo_header_checksum(source, destination, NEXT_ICMPV6, &message);
		message[2..4].copy_from_slice(&checksum.to_be_bytes());
		let mut frame = Vec::with_capacity(ipv6_packet::ETHERNET_HEADER_LEN + ipv6_packet::HEADER_LEN + message.len());
		frame.extend_from_slice(&mac);
		frame.extend_from_slice(&self.mac);
		frame.extend_from_slice(&ETHERTYPE_IPV6.to_be_bytes());
		frame.push(0x60);
		frame.extend_from_slice(&[0, 0, 0]);
		frame.extend_from_slice(&(message.len() as u16).to_be_bytes());
		frame.push(NEXT_ICMPV6);
		frame.push(hop_limit);
		frame.extend_from_slice(&source.octets());
		frame.extend_from_slice(&destination.octets());
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
		self.send_icmp_multicast(envelope.source, envelope.destination, envelope.hop_limit, message);
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
				let source = self.source_for(parsed.header.source)?;
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

	fn send_unicast_icmp(&mut self, peer: Address, source: Address, message: Vec<u8>, now_ms: u64) {
		let (lookup, action) = self.neighbours.resolve(self.interface, peer, now_ms);
		self.perform(action, now_ms);
		match lookup {
			Lookup::Ready { link_layer } => self.send_icmp_to_mac(link_layer, source, peer, self.hop_limit, message),
			Lookup::Pending => {
				// Build the frame with a placeholder destination and retain it: the neighbour's
				// address is filled in when resolution completes, which is what the pending queue is
				// for. Keeping the whole frame is what the byte budget charges.
				let mut held = message;
				let checksum = ipv6_packet::pseudo_header_checksum(source, peer, NEXT_ICMPV6, &held);
				held[2..4].copy_from_slice(&checksum.to_be_bytes());
				if let Ok(packet) = ipv6_packet::build_packet(source, peer, NEXT_ICMPV6, self.hop_limit, &held, self.mtu) {
					let mut frame = Vec::with_capacity(ipv6_packet::ETHERNET_HEADER_LEN + packet.len());
					frame.extend_from_slice(&[0; 6]);
					frame.extend_from_slice(&self.mac);
					frame.extend_from_slice(&ETHERTYPE_IPV6.to_be_bytes());
					frame.extend_from_slice(&packet);
					let _ = self.pending.admit(self.interface, peer, frame);
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
					// AN ADVERTISEMENT MAY LOWER THE MTU AND NEVER RAISE IT. The frame buffers were
					// sized at boot, and a router that asked for more than the link can carry would
					// be asking this host to write past them.
					if *value >= u32::from(ipv6_packet::MIN_MTU) && *value < u32::from(self.mtu) {
						self.mtu = *value as u16;
					}
				}
				NdOption::Prefix(information) => self.on_prefix(information, now_ms),
				NdOption::Rdnss { lifetime_seconds, servers } => {
					for server in servers.iter().take(Resource::Rdnss.limit() as usize) {
						let identity = Identity::Rdnss { interface: self.interface, server: *server };
						let change = if *lifetime_seconds == 0 { Change::Invalidated } else { Change::Changed };
						self.note(identity, change);
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
				self.note(Identity::Router { interface: self.interface, router: parsed.header.source }, Change::Changed);
				self.timers.set(Timer { kind: TimerKind::RouterLifetime, identity: Identity::Router { interface: self.interface, router: parsed.header.source }, deadline: now_ms + u64::from(advertisement.router_lifetime_seconds) * 1000 });
				// ONLY AN INSTALLED DEFAULT ROUTE STOPS SOLICITATION.
				self.solicitation.default_route_installed();
				self.timers.clear(TimerKind::RouterSolicitation, Identity::InterfaceState { interface: self.interface });
			}
			RouterOutcome::Withdrawn => {
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

	fn on_prefix(&mut self, information: &PrefixInformation, now_ms: u64) {
		let Ok(outcome) = service_logic::ipv6_slaac::evaluate_prefix(information, now_ms) else {
			return;
		};
		if let Some(valid) = outcome.on_link_route {
			self.note(Identity::Prefix { interface: self.interface, prefix: information.prefix }, Change::Changed);
			if let Lifetime::Finite(deadline) = valid {
				self.timers.set(Timer { kind: TimerKind::PrefixLifetime, identity: Identity::Prefix { interface: self.interface, prefix: information.prefix }, deadline });
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
		let router_alert = parsed.extension_bytes > 0;
		if ipv6_mld::validate_query(parsed.header.source, parsed.header.hop_limit, router_alert).is_err() {
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
		let message = parsed.payload;
		if message.len() < 8 + ipv6_packet::HEADER_LEN {
			return;
		}
		let quoted = &message[8..];
		let Ok(header) = ipv6_packet::header(quoted) else {
			return;
		};
		// THE STRONGEST CHECK THIS LAYER CAN MAKE: the quoted source is an address this interface
		// holds. It is an address check and not a flow check, and the consumer above makes the
		// second one before anything durable is written.
		if !self.addresses.assigned(self.interface).contains(&header.source) {
			return;
		}
		let class = match message[0] {
			ipv6_icmp::DESTINATION_UNREACHABLE => ErrorClass::DestinationUnreachable { code: message[1] },
			ipv6_icmp::PACKET_TOO_BIG => ErrorClass::PacketTooBig { mtu: u32::from_be_bytes([message[4], message[5], message[6], message[7]]) },
			ipv6_icmp::TIME_EXCEEDED => ErrorClass::TimeExceeded { code: message[1] },
			ipv6_icmp::PARAMETER_PROBLEM => ErrorClass::ParameterProblem { code: message[1], pointer: u32::from_be_bytes([message[4], message[5], message[6], message[7]]) },
			_ => return,
		};
		let transport = transport_of(quoted, header.next_header);
		self.quoted_errors.offer(service_logic::ipv6_events::QuotedError { interface: self.interface, reporter: parsed.header.source, class, quoted_source: header.source, quoted_destination: header.destination, transport });
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
				let completions = self.pending.fail_for(self.interface, target, service_logic::ipv6_budget::FailureCause::ResolutionFailed);
				let _ = completions;
				self.note(Identity::Neighbour { interface: self.interface, neighbour: target }, Change::Invalidated);
				self.routers.set_reachability(self.interface, target, Reachability::Unusable);
			}
			Action::Resolved { target, link_layer } => {
				self.timers.clear(TimerKind::NeighbourRetry, Identity::Neighbour { interface: self.interface, neighbour: target });
				for held in self.pending.take_for(self.interface, target) {
					let mut frame = held.frame;
					frame[..6].copy_from_slice(&link_layer);
					self.transmit(frame);
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
					if let Some(interval) = self.solicitation.on_timeout(now_ms, jitter) {
						self.send_router_solicitation();
						self.timers.set(Timer { kind: TimerKind::RouterSolicitation, identity: timer.identity, deadline: now_ms + interval });
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
				TimerKind::RdnssLifetime => {}
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

	pub fn link_local(&self) -> Option<Address> {
		self.link_local
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

	pub fn mtu(&self) -> u16 {
		self.mtu
	}

	pub fn drain_invalidations(&mut self) -> Vec<Invalidation> {
		self.invalidations.drain()
	}

	pub fn resync_required(&self) -> bool {
		self.invalidations.resync_required()
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
					Resource::Neighbours => self.neighbours.len() as u32,
					Resource::PathMtu => self.path_mtu.len() as u32,
					Resource::MldGroups => self.listener.len() as u32,
					Resource::InvalidationEvents => self.invalidations.len() as u32,
					Resource::QuotedErrorEvents => self.quoted_errors.len() as u32,
					Resource::PendingPerInterface => self.pending.len() as u32,
					Resource::PendingBytes => self.pending.bytes(),
					_ => 0,
				};
				let counted = refusals.get(*resource) + self.neighbours.refusals().get(*resource) + self.routers.refusals().get(*resource) + self.listener.refusals().get(*resource);
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

	// Take what arrived for the layer above.
	pub fn take_deliveries(&mut self) -> Vec<Delivery> {
		core::mem::take(&mut self.deliveries)
	}
}

// Recover the transport identity out of a quoted packet, as far as the quotation goes.
fn transport_of(quoted: &[u8], next_header: u8) -> service_logic::ipv6_events::QuotedTransport {
	use service_logic::ipv6_events::QuotedTransport;
	let body = &quoted[ipv6_packet::HEADER_LEN.min(quoted.len())..];
	match next_header {
		ipv6_packet::NEXT_TCP if body.len() >= 8 => QuotedTransport::Tcp { source_port: u16::from_be_bytes([body[0], body[1]]), destination_port: u16::from_be_bytes([body[2], body[3]]), sequence: u32::from_be_bytes([body[4], body[5], body[6], body[7]]) },
		ipv6_packet::NEXT_UDP if body.len() >= 4 => QuotedTransport::Udp { source_port: u16::from_be_bytes([body[0], body[1]]), destination_port: u16::from_be_bytes([body[2], body[3]]) },
		NEXT_ICMPV6 if body.len() >= 8 => QuotedTransport::Icmpv6Echo { identifier: u16::from_be_bytes([body[4], body[5]]), sequence: u16::from_be_bytes([body[6], body[7]]) },
		other => QuotedTransport::Other { next_header: other },
	}
}

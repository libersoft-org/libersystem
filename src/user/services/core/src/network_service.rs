// NetworkService - the standing userspace network service.
//
// The L2/L3 stack used to live inside driver.virtio-net; it is now extracted here. The
// driver is now a pure frame-mover: it owns the NIC and the virtqueues and, over a
// single channel, forwards each received Ethernet frame to this service and
// transmits each frame this service hands back. NetworkService owns the stack
// (`net`): it learns the NIC's MAC from the driver, answers ARP and ICMP, and
// serves clients (the shell, later the net tools) the typed `network` interface -
// `info` / `resolve` / `ping` / `fetch` over generated `liber:system` bindings, so
// the network is reachable through the typed API like every other service. It
// stands on the driver's frame channel and its client channel at once with
// `wait_any`, so an inbound frame and a client request never block each other.
// Sockets handed out as capabilities and a received-data stream<T> land here next.

#![no_std]
#![no_main]

extern crate alloc;

mod ipv6_host;
mod net;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use rt::*;

use crate::net::{DNS_PORT, Event, IP_PROTO_TCP, IP_PROTO_UDP, Ipv4Addr, MacAddr, NEIGH_MAX, NTP_PORT, SockEntry, SockEntryState, Stack};
use proto::codec::{Buffer, Handles};
use proto::system::{AcceptResult, AddressState, BindMode, Chunk, DnsServer, Error, FamilyReadiness, FetchChunk, FetchOutcome, HopStatus, InterfaceAddress, InterfaceId, IpAddress, Ipv4Addr as WireIp, Ipv6Addr as WireIpv6, LinkAttachment, LinkInstalled, ListenRequest, ListenResult, MacAddr as WireMac, Neighbor, NetCapacity, NetInfo, NextHop, OpenTarget, PingReply, PingStatus, ProviderInfo, ProviderKind, Reachability, RouteEntry, RoutePreference, RouterEntry, ScopedAddress, ScopedEndpoint, SockInfo, SockState, TcpRequest, TraceHop, config, listener, network, network_link_admin, provider_catalogue, socket};
use service_logic::addr_select;
use service_logic::dhcp;
use service_logic::dns;
use service_logic::net_profile::{Families, Pending, PendingKind, Readiness, readiness};
use service_logic::sntp;
use service_logic::tcp_bind::Binding;
use service_logic::uplink;

// Static addressing for the QEMU user-mode (SLIRP) network: the guest is
// 10.0.2.15/24, the gateway/host is 10.0.2.2, and the DNS relay is 10.0.2.3. A DHCP
// client later replaces this static configuration.
const OUR_IP: Ipv4Addr = Ipv4Addr([10, 0, 2, 15]);
const OUR_MASK: Ipv4Addr = Ipv4Addr([255, 255, 255, 0]);
const GATEWAY_IP: Ipv4Addr = Ipv4Addr([10, 0, 2, 2]);
const DNS_SERVER: Ipv4Addr = Ipv4Addr([10, 0, 2, 3]);
// The UDP source port we send DNS queries from.
// How many times a drawn DNS identity is redrawn before the service gives up on finding a free one.
// A handful is enough: it collides only against the queries actually in flight, which are bounded.
const DNS_DRAW_ATTEMPTS: usize = 8;
// The UDP source port we send SNTP queries from.
const NTP_SRC_PORT: u16 = 0x7b7b;

// How long a `ping` waits for its reply, and DNS for its response (100 Hz ticks).
const PING_TIMEOUT_TICKS: u64 = 50;
const DNS_TIMEOUT_TICKS: u64 = 300;
// How long an SNTP query waits for its reply (100 Hz ticks).
const NTP_TIMEOUT_TICKS: u64 = 300;
// How long DHCP waits for each of the OFFER and the ACK before falling back to the
// static configuration (100 Hz ticks).
const DHCP_TIMEOUT_TICKS: u64 = 200;
// The scheduler tick rate the lease clock converts the DHCP seconds with.
const TICKS_PER_SEC: u64 = 100;
// How often an unanswered lease-extension REQUEST (or a failed re-acquisition
// after expiry) is retried, at most - the clamp on the halved-remaining-time pace.
const DHCP_RETRY_MAX_TICKS: u64 = 60 * TICKS_PER_SEC;
// The floor on the retry pace (RFC 2131 names one minute; a short test lease
// still retries within it, so the floor is what keeps retries bounded, not dead).
const DHCP_RETRY_MIN_TICKS: u64 = TICKS_PER_SEC / 2;
// TCP: how long a teardown is pumped for, and how long a one-shot fetch reads before giving up. The
// SYN and data retransmission schedules are NOT here - they are derived from the RTO profile in
// `service-logic`, so the interval, the backoff and the retry limit are one set of numbers rather
// than one set here and another there.
const TCP_RETX_TICKS: u64 = 50;
const TCP_RECV_TIMEOUT_TICKS: u64 = 300;
// The base ephemeral local port for outgoing connections.
const TCP_LOCAL_PORT_BASE: u16 = 0xc000;

// The default MTU: standard Ethernet, when neither the driver nor the config tree
// says otherwise. The effective MTU - the smaller of the link's report and the
// `net.mtu` config knob - sizes every frame buffer at start; there is no
// compile-time frame cap.
const DEFAULT_MTU: usize = 1500;
// The typed request and reply buffers for one client call.
//
// THE FRAMING FOLLOWS THE BOUNDS, NOT THE OTHER WAY ROUND. The wire may not advertise a request this
// service cannot receive or a reply it cannot send, so these two numbers are derived from the worst
// case the contract now permits rather than chosen for comfort:
//
//   REQUEST   an open-target of eight scoped destinations plus 1024 request bytes. Eight IPv6
//               addresses with their scopes are 8 x (1 + 16 + 1 + 12) = 240 bytes before the
//               request payload, and a DNS name may be 253; 8192 holds either with room for
//               framing that never has to be recomputed when a record gains a field.
//   REPLY     the two that can actually be large. A full `net-info` is 17 addresses, 34 routes,
//               9 routers, 5 servers and 1088 neighbours - the neighbours alone are 1088 x 26
//               bytes - and a socket list is 256 rows of two scoped endpoints. Both exceed 4096,
//               which is why that number could not survive this contract.
//
// Replies that grow without a stated bound (`fetch`'s stream, socket recv chunks) are built in Vecs
// and received exactly-sized on the client, so these bound the fixed-shape ops.
const REQ_MAX: usize = proto::net_limits::REQUEST_BYTES;
const REPLY_MAX: usize = proto::net_limits::REPLY_BYTES;
// The initial sizes of the client / socket / listener sets. Each set grows on
// demand (a slot is reused when free, a new one pushed otherwise), so these are
// size hints, never caps - the kernel's wait_any bound (64 handles) is the only
// ceiling on how many channels the serve loop can stand on at once.
const MAX_CLIENTS: usize = 4;
const MAX_SOCKS: usize = 4;
const MAX_LISTEN: usize = 2;

// The neighbor-cache size and the MTU knob from the config tree (the
// `net.arp-cache` and `net.mtu` keys), read once at start over the supervisor-minted
// ConfigService client and the client closed - both feed allocations made with the
// stack, so a later `set` applies at the next boot. The defaults stand in when no
// config tree serves this boot (handle 0, a test scenario) or a key does not parse.
fn net_policy(config: u64) -> (usize, usize, Option<alloc::string::String>, Families) {
	if config == 0 {
		return (NEIGH_MAX, DEFAULT_MTU, None, Families::Dual);
	}
	let mut client = config::Client::new(ChannelTransport { chan: config });
	let neigh: usize = match client.get("net.arp-cache") {
		Some(Ok(value)) => value.parse::<usize>().ok().filter(|&n| n > 0).unwrap_or(NEIGH_MAX),
		_ => NEIGH_MAX,
	};
	let mtu: usize = match client.get("net.mtu") {
		// BOUNDED AT BOTH ENDS. The lower bound is IPv4's minimum reassembly buffer; the upper one is
		// what the field this number ends up in can hold, so every later conversion is total.
		Some(Ok(value)) => value.parse::<usize>().ok().filter(|&n| (576..=u16::MAX as usize).contains(&n)).unwrap_or(DEFAULT_MTU),
		_ => DEFAULT_MTU,
	};
	// READ ONCE, HERE. The ICMPv6 error bucket's rate is a boot-time decision: a value that could
	// change under a running host would be a limit whose current value nothing could state.
	let icmpv6_rate: Option<alloc::string::String> = match client.get("net.icmpv6-error-rate") {
		Some(Ok(value)) => Some(value),
		_ => None,
	};
	// WHICH FAMILIES THIS BOOT RUNS, read here with the others and READ ONLY HERE. A change takes
	// effect at the next start of the service: ConfigService has no subscription operation, so a
	// promise to notice a later change would be a promise nothing can keep.
	let families: Families = Families::parse(match client.get("net.families") {
		Some(Ok(ref value)) => Some(value.as_str()),
		_ => None,
	});
	close(config);
	(neigh, mtu, icmpv6_rate, families)
}

// What the config tree decided for this boot, read once at start and KEPT: every link this service
// brings up - the NIC found at boot, a late one, one reopened after a modem, a modem context - is
// sized and configured by the same policy, and the ConfigService client is closed after the read.
struct Policy {
	neigh_cap: usize,
	mtu_knob: usize,
	icmpv6_rate: Option<alloc::string::String>,
	families: Families,
}

// The interface indices the two media are reported under. One link is selected at a time, so the
// index says WHICH KIND of link a scope names and the generation says which one.
const ETHERNET_INDEX: u32 = 0;
const RAW_INDEX: u32 = 1;
// How long an opened NIC has to lead with its MAC before it is failed and passed over.
const GREETING_TICKS: u64 = 200;
// The buffers a link-admin request and reply are read into. The largest request is `install`, a
// fixed-shape record with two DNS servers; a kilobyte holds it with room to spare.
const LINK_ADMIN_BYTES: usize = 1024;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	// 1. receive the ConfigService client the supervisor minted for us (handle 0 when no config
	//    tree serves this boot - a test scenario), the client channel the shell reaches us on (the
	//    `ip` / `ping` / `nslookup` control protocol), the provider catalogue and the private link
	//    administration.
	let config: u64 = match recv_blocking(bootstrap, &mut buf) {
		Received::Message { len, handle } if len >= 6 && &buf[..6] == b"CONFIG" => handle,
		_ => fail_bootstrap(bootstrap, b"config", b"config client not delivered"),
	};
	let client: u64 = recv_tagged(bootstrap, &mut buf, b"SERVE").unwrap_or_else(|| fail_bootstrap(bootstrap, b"serve", b"missing serve channel"));
	// THE NIC IS DISCOVERED, NOT HANDED OVER.
	//
	// This service used to be given the network driver's frame channel under `FRAMES`, taken by
	// DeviceManager into a slot of its own and routed down the boot chain - the per-kind
	// injection the provider catalogue replaces. What arrives now is a connection to the
	// CATALOGUE, and this service asks it for the network kind: a NIC bound after this service
	// started reaches it, and a machine with two of them has a second entry to offer rather than
	// a slot that is already full.
	//
	// Read POSITIONALLY, like every role: the bootstrap is read in order at every hop.
	let catalogue: u64 = recv_tagged(bootstrap, &mut buf, b"CATALOGUE").unwrap_or(0);
	// THE PRIVATE LINK ADMINISTRATION, LAST, for the same positional reason. ServiceManager keeps this
	// root and mints ModemService's connection from it; no ordinary client is ever minted here, so
	// nothing a `network` client holds reaches an installation. Zero where no such root is served - a
	// test harness - which leaves this service Ethernet-only.
	let link_root: u64 = recv_tagged(bootstrap, &mut buf, b"LINKADMIN").unwrap_or(0);
	let (neigh_cap, mtu_knob, icmpv6_rate, families): (usize, usize, Option<alloc::string::String>, Families) = net_policy(config);
	let mut runtime: Runtime = Runtime::new(bootstrap, client, catalogue, link_root, Policy { neigh_cap, mtu_knob, icmpv6_rate, families });
	// 2. SUBSCRIBE ONCE, FOR THE LIFE OF THE SERVICE. The snapshot is already in the channel when
	//    `subscribe` answers, and every NIC in it is taken before anything is chosen - so the lowest
	//    publication identity is the one brought up, whatever order the catalogue listed them in.
	//    Bringing it up says we are online at the right moment: after the stack exists and before
	//    either family has started.
	runtime.subscribe();
	runtime.on_publications();
	if !runtime.announced {
		// NO NETWORK PROVIDER ON THIS BOOT, AND THE SERVICE COMES UP ANYWAY - WITHOUT A LINK.
		//
		// This used to fail the bootstrap, and a failed NetworkService is not "no network": the
		// time service, PermissionManager, ConsoleService, SystemGraphService and the shell all
		// depend on this service by manifest, so ServiceManager never started any of them and
		// the machine came up with no shell at all. A boot whose DMA mode refuses the network
		// driver by policy - a `no-iommu` boot, where `virtio_net` declares that it requires
		// translation - is a machine with every OTHER driver and no NIC, and that is a state
		// this service has to be able to stand in: online, answering every link-bound
		// operation with a typed refusal, minting a fresh connection for every caller that
		// asks for one, and holding no stack, no lease and no frame buffers - and STILL
		// SUBSCRIBED, so a NIC published later, or a modem installed, is taken when it comes.
		//
		// Said on the console in the same breath as the DHCP report would have been, so a
		// reader of the boot log sees WHY there is no address rather than a service that went
		// quiet.
		print(b"network: no network provider on this boot - NetworkService is up without a link\n");
		send_blocking(bootstrap, b"NetworkService: online", 0);
		runtime.announced = true;
	}
	// 3. serve the link, the clients, the catalogue and the link administration at once.
	serve(runtime);
}

// A NIC publication this service has seen, and what it takes to open it again: the fallback after a
// modem, or after the selected NIC fails, reopens one of these through the catalogue.
struct Published {
	identity: uplink::Publication,
	info: ProviderInfo,
}

// THE LINK THE SERVICE RUNS ON: the channel its frames - or, on a raw-IP link, its datagrams - travel
// on, the stack built for it, its DHCP clock and the buffers sized from its MTU. All of it is dropped
// together when the link ends.
struct Link {
	frames: u64,
	stack: Stack,
	lease: LeaseClock,
	rx: Vec<u8>,
	tx: Vec<u8>,
}

// Everything the serve loop stands on.
struct Runtime {
	bootstrap: u64,
	// Whether "online" has been said. It is said once, by whichever comes first: the first link, or
	// the finding that there is none.
	announced: bool,
	catalogue: u64,
	// The persistent network-kind subscription, or 0 when there is none or it closed.
	providers: u64,
	published: Vec<Published>,
	uplinks: uplink::Uplinks,
	link: Option<Link>,
	policy: Policy,
	// The link-admin root ServiceManager keeps, and the connections minted from it.
	link_root: u64,
	link_admins: Vec<u64>,
	// The link-admin connection holding the one reservation, and which reservation it is. Only that
	// connection may install or release under it.
	holder: Option<(u64, u64)>,
	// The client channels we serve the `network` interface on: the shell's (clients[0]) plus any
	// minted by `network.open` for a spawned net tool. Each set below reuses free slots and grows on
	// demand - never a fixed cap.
	clients: Vec<u64>,
	// The active sockets (chan 0 = empty slot). Each is handed out by `network.connect` (or
	// `listener.accept`): its channel, the stack connection index it drives, and its received-data
	// stream producer (0 = none) plus that stream's frame sequence.
	socks: Vec<SockSlot>,
	// The active listeners (chan 0 = empty), each from `network.listen`: its channel, the port it
	// accepts on, and a deferred `accept`.
	listeners: Vec<Listener>,
	// The DNS identities in flight across every client this loop serves: one table, because two
	// clients drawing the same identity would each accept the other's answer.
	dns_flight: dns::InFlight,
	// The pending-operation partition, and the counter every diagnostic echo takes its identity from.
	pending: Pending,
	echo: service_logic::net_profile::EchoCounter,
	// ON THE HEAP, LIKE EVERY OTHER BUFFER HERE. The request buffer grew to 8192 bytes with the
	// open-target it now has to hold, and the user stack is 16 kB with a deep connect handshake
	// nested on top of this frame - an array that size is half the stack before the first call.
	req: Vec<u8>,
	out: Vec<u8>,
}

fn identity_of(info: &ProviderInfo) -> uplink::Publication {
	uplink::Publication { slot: info.slot, generation: info.provider_generation, binding_generation: info.binding_generation }
}

impl Runtime {
	fn new(bootstrap: u64, client: u64, catalogue: u64, link_root: u64, policy: Policy) -> Runtime {
		let mut clients: Vec<u64> = Vec::with_capacity(MAX_CLIENTS);
		clients.push(client);
		Runtime { bootstrap, announced: false, catalogue, providers: 0, published: Vec::new(), uplinks: uplink::Uplinks::new(), link: None, policy, link_root, link_admins: Vec::new(), holder: None, clients, socks: Vec::with_capacity(MAX_SOCKS), listeners: Vec::with_capacity(MAX_LISTEN), dns_flight: dns::InFlight::new(), pending: Pending::new(), echo: service_logic::net_profile::EchoCounter::new(random_u16()), req: alloc::vec![0u8; REQ_MAX], out: alloc::vec![0u8; REPLY_MAX] }
	}

	// THE NETWORK-KIND SUBSCRIPTION, TAKEN ONCE AND HELD. It used to be given back after the first NIC
	// was opened, which left a service that could not see a NIC published after boot, a withdrawal, or
	// a second NIC to fall back to.
	fn subscribe(&mut self) {
		if self.catalogue == 0 {
			print(b"NetworkService: no provider catalogue - this instance has no way to find a NIC\n");
			return;
		}
		match provider_catalogue::Client::new(ChannelTransport { chan: self.catalogue }).subscribe(&ProviderKind::Net) {
			Some(subscription) => self.providers = subscription,
			None => print(b"NetworkService: the catalogue refused a network subscription\n"),
		}
	}

	// Every frame the subscription has queued, decoded. POLLED, NEVER BLOCKED. A closed subscription is
	// given up: the NICs already known stay known, and no new one will be seen.
	fn take_publications(&mut self) -> Vec<ProviderInfo> {
		let mut taken: Vec<ProviderInfo> = Vec::new();
		if self.providers == 0 {
			return taken;
		}
		let mut buf: [u8; 256] = [0; 256];
		loop {
			match try_recv_caps(self.providers, &mut buf) {
				PolledCaps::Message { len, handles } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
					let mut frame_handles = wire::Handles::new();
					match provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) {
						Some(info) if info.kind == ProviderKind::Net => taken.push(info),
						Some(_) => {}
						None => print(b"NetworkService: a provider frame did not decode\n"),
					}
				}
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					print(b"NetworkService: the catalogue closed the network subscription - no further NIC will be seen\n");
					close(self.providers);
					self.providers = 0;
					break;
				}
			}
		}
		taken
	}

	// NICs appeared or went. Taken in identity order, so a batch - the boot snapshot above all - selects
	// its lowest member rather than whichever the catalogue happened to list first.
	fn on_publications(&mut self) {
		let mut infos: Vec<ProviderInfo> = self.take_publications();
		infos.sort_by_key(identity_of);
		for info in infos {
			let nic: uplink::Publication = identity_of(&info);
			let decision: uplink::Decision = if info.live {
				match self.uplinks.published(nic) {
					Ok(decision) => {
						if !self.published.iter().any(|held| held.identity == nic) {
							self.published.push(Published { identity: nic, info });
						}
						decision
					}
					Err(_) => {
						print(b"network: a NIC publication past the sixteen this service tracks is ignored\n");
						continue;
					}
				}
			} else {
				self.published.retain(|held| held.identity != nic);
				self.uplinks.withdrawn(nic)
			};
			self.apply(decision);
		}
	}

	// ACT ON A DECISION: end the current link and bring up the one selected. A NIC that cannot be
	// opened, or that does not report itself, is failed and the choice is made again - which ends,
	// because every failure takes one publication out of selection.
	fn apply(&mut self, decision: uplink::Decision) {
		let mut decision: uplink::Decision = decision;
		loop {
			let uplink::Decision::Switch { to, generation } = decision else {
				return;
			};
			self.tear_down();
			match to {
				uplink::Selected::None => {
					print(b"network: no usable link - serving without one\n");
					return;
				}
				uplink::Selected::Nic(nic) => {
					if self.bring_up_nic(nic, generation) {
						return;
					}
					decision = self.uplinks.failed(nic);
				}
				// Only an installation selects a modem, and it brings its own link up.
				uplink::Selected::Modem(_) => return,
			}
		}
	}

	// END THE CURRENT LINK AND EVERYTHING BOUND TO IT. No TCP connection is carried to the next link:
	// every socket and listener that belonged to this one answers `link-changed` from now on, the
	// receive streams it fed are ended, and its addresses, routes, resolvers and traffic go with the
	// stack.
	fn tear_down(&mut self) {
		let Some(link) = self.link.take() else {
			return;
		};
		for slot in self.socks.iter_mut().filter(|slot| slot.chan != 0 && !slot.dead) {
			if slot.stream_prod != 0 {
				close(slot.stream_prod);
				slot.stream_prod = 0;
			}
			slot.dead = true;
		}
		for listener in self.listeners.iter_mut().filter(|listener| listener.chan != 0 && !listener.dead) {
			if listener.pending {
				refuse_accept(listener.chan, listener.pending_corr);
				listener.pending = false;
			}
			listener.dead = true;
		}
		close(link.frames);
		print(b"network: link down - what was bound to it ends with link-changed\n");
	}

	// Open a NIC through the catalogue, read its greeting, and build and configure a stack on it.
	fn bring_up_nic(&mut self, nic: uplink::Publication, generation: u64) -> bool {
		let Some(info) = self.published.iter().find(|held| held.identity == nic).map(|held| held.info.clone()) else {
			return false;
		};
		let frames: u64 = match provider_catalogue::Client::new(ChannelTransport { chan: self.catalogue }).open(&info) {
			Some(Ok(handle)) => handle,
			Some(Err(_)) => {
				print(b"NetworkService: the catalogue refused a connection to the network provider it published\n");
				return false;
			}
			None => {
				print(b"NetworkService: the catalogue did not answer the connection it published\n");
				return false;
			}
		};
		// The frame-mover driver leads with our NIC's MAC and the link's MTU over the frame channel
		// (it owns the device; we own the protocol), so we can build the stack - its neighbor-cache
		// sized by the config tree's `net.arp-cache` policy, its MTU the smaller of the link's report
		// and the `net.mtu` knob. BOUNDED: a NIC that never says what it is, is not used.
		let mut buf: [u8; 64] = [0u8; 64];
		let greeting: Received = if wait(frames, clock() + GREETING_TICKS) == 0 { recv_blocking(frames, &mut buf) } else { Received::Closed };
		let (mac, link_mtu): (MacAddr, usize) = match greeting {
			Received::Message { len, .. } if len >= 9 && &buf[..3] == b"MAC" => {
				let link: usize = if len >= 11 { u16::from_le_bytes([buf[9], buf[10]]) as usize } else { DEFAULT_MTU };
				(MacAddr([buf[3], buf[4], buf[5], buf[6], buf[7], buf[8]]), if link == 0 { DEFAULT_MTU } else { link })
			}
			_ => {
				print(b"network: the NIC did not report its MAC - it is not used\n");
				close(frames);
				return false;
			}
		};
		let families: Families = self.policy.families;
		let mtu: usize = service_logic::ipv6_packet::effective_link_mtu(self.policy.mtu_knob as u16, link_mtu as u16) as usize;
		let frame_max: usize = mtu + 14;
		let mut stack: Stack = Stack::new(mac, OUR_IP, OUR_MASK, GATEWAY_IP, DNS_SERVER, self.policy.neigh_cap, mtu as u16);
		stack.set_interface(ETHERNET_INDEX, generation);
		// ONLINE BEFORE ANY FAMILY IS READY, AND BEFORE EITHER IS EVEN STARTED. That is the whole of
		// "start serving immediately": the service used to block on the DHCP transaction before saying
		// this, so an IPv6-only boot waited for a conversation it would never have and every caller
		// waited for a lease it might not need. Readiness is a per-family fact a caller reads and acts
		// on, never a gate on answering at all.
		//
		// AND IT IS SAID HERE RATHER THAN AFTER THE FAMILIES ARE STARTED, because the console is slow:
		// two lines written between queueing the first IPv6 frames and yielding to the driver delayed
		// them by tens of milliseconds, which the solicitation-schedule gate measures on the wire.
		if !self.announced {
			print(b"network: families=");
			print(families.as_text().as_bytes());
			print(b", online before any family is ready\n");
			send_blocking(self.bootstrap, b"NetworkService: online", 0);
			self.announced = true;
		}
		// THE IPv6 HOST BESIDE IT, on the same link and the same MAC, with its own state and its own
		// deadline, and the same interface generation the link was selected under. THE PROFILE DECIDES
		// WHETHER IPv6 IS BROUGHT UP AT ALL: under `ipv4` the host is never attached, so the family is
		// `disabled` rather than configured-and-unused.
		if families.includes_v6() {
			let mut host: ipv6_host::Ipv6Host = ipv6_host::Ipv6Host::new(mac.0, ETHERNET_INDEX as u16, u32::try_from(generation).unwrap_or(u32::MAX), mtu as u16, ipv6_entropy, now_ms);
			host.set_error_rate(self.policy.icmpv6_rate.as_deref());
			// A LINK THAT CANNOT CARRY IPv6 LEAVES IT REFUSED, and says so once rather than silently.
			if !host.bring_up(now_ms()) {
				print(b"ipv6: refused on this link - its effective MTU is below the 1280 bytes IPv6 requires; IPv4 is unaffected\n");
			}
			stack.attach_ipv6(host);
			// AT ONCE, NOT BEHIND DHCP. The listener report has to precede the detection probe on the
			// WIRE, and both have to precede anything that uses the address.
			drain_ipv6(frames, &mut stack);
		}
		// Learn our address / mask / gateway / DNS from DHCP, falling back to the static config if no
		// server answers. Under `ipv6` neither runs at all.
		let mut rx: Vec<u8> = alloc::vec![0u8; frame_max];
		let mut tx: Vec<u8> = alloc::vec![0u8; frame_max];
		let mut lease: LeaseClock = LeaseClock::none();
		if families.includes_v4() {
			// WHAT IT GOT, NOT ONLY THAT IT GOT SOMETHING.
			if do_dhcp(frames, &mut stack, &mut rx, &mut tx) {
				print(b"network: configured via DHCP - ");
				print_address(&stack);
				print(b"\n");
				lease = LeaseClock::bound(&stack);
			} else {
				print(b"network: DHCP unanswered, using static config - ");
				print_address(&stack);
				print(b"\n");
			}
		} else {
			// UNDER `ipv6` THERE IS NO IPv4 ADDRESS AT ALL, and the static fallback is not applied
			// either: a family outside the profile is disabled, not quietly configured.
			stack.clear_ipv4();
			print(b"network: ipv4 disabled by profile\n");
		}
		// Announce us on the link.
		let arp: usize = stack.build_arp_request(GATEWAY_IP, &mut tx);
		send_frame(frames, &tx[..arp]);
		self.link = Some(Link { frames, stack, lease, rx, tx });
		true
	}

	// INSTALL A RAW-IP LINK, whose configuration was validated before anything was torn down. Nothing
	// here waits: no MAC, no ARP, no DHCP and no static fallback - the address, route and resolvers are
	// the attachment's, and the MTU is the smaller of its own and the `net.mtu` knob.
	fn bring_up_raw(&mut self, attachment: &LinkAttachment, generation: u64) -> LinkInstalled {
		let mtu: u16 = attachment.mtu.min(self.policy.mtu_knob.min(usize::from(u16::MAX)) as u16);
		let octets = |ip: &WireIp| Ipv4Addr([ip.a, ip.b, ip.c, ip.d]);
		let mask: u32 = if attachment.prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(attachment.prefix.min(32))) };
		let gateway: Ipv4Addr = attachment.gateway.as_ref().map(octets).unwrap_or(Ipv4Addr([0; 4]));
		let dns: [Ipv4Addr; 2] = [attachment.dns.first().map(octets).unwrap_or(Ipv4Addr([0; 4])), attachment.dns.get(1).map(octets).unwrap_or(Ipv4Addr([0; 4]))];
		let mut stack: Stack = Stack::new_raw(octets(&attachment.address), Ipv4Addr(mask.to_be_bytes()), gateway, dns, mtu);
		stack.set_interface(RAW_INDEX, generation);
		print(b"network: modem link installed - ");
		print_address(&stack);
		print(b"\n");
		let frame_max: usize = usize::from(mtu) + 14;
		self.link = Some(Link { frames: attachment.packets, stack, lease: LeaseClock::none(), rx: alloc::vec![0u8; frame_max], tx: alloc::vec![0u8; frame_max] });
		LinkInstalled { interface: InterfaceId { index: RAW_INDEX, generation }, mtu }
	}

	// A LINK WHOSE CHANNEL CLOSED IS OVER. A NIC's is failed and not chosen again until it is published
	// again; a modem's is released, which brings back what it replaced.
	fn check_link(&mut self) {
		if !self.link.as_ref().is_some_and(|link| link.stack.channel_closed()) {
			return;
		}
		let decision: uplink::Decision = match self.uplinks.selected() {
			uplink::Selected::Nic(nic) => {
				print(b"network: the NIC's channel closed - its link is torn down\n");
				self.uplinks.failed(nic)
			}
			uplink::Selected::Modem(reservation) => {
				print(b"network: the modem link's channel closed - the link is removed\n");
				self.holder = None;
				self.uplinks.release(reservation)
			}
			uplink::Selected::None => uplink::Decision::Keep,
		};
		match decision {
			uplink::Decision::Keep => self.tear_down(),
			switch => self.apply(switch),
		}
	}

	// Frames arrived on the link: run one through the stack, then feed the receive streams and answer
	// any deferred `accept` the frame may have completed.
	fn on_frames(&mut self) {
		let Runtime { link, socks, listeners, .. } = self;
		let Some(Link { frames, stack, lease, rx, tx }) = link.as_mut() else {
			return;
		};
		let frames: u64 = *frames;
		if let Event::DhcpReply(reply) = pump(frames, stack, rx, tx) {
			lease.on_reply(&reply, stack);
		}
		// Feed any newly received bytes to each active recv stream, closing the producer (end of
		// stream) once that connection's peer closes or resets.
		for slot in socks.iter_mut().filter(|slot| slot.chan != 0 && !slot.dead && slot.stream_prod != 0) {
			let ci: usize = slot.ci;
			let prod: u64 = stream_pump(ci, frames, stack, tx, slot.stream_prod, &mut slot.stream_seq);
			slot.stream_prod = prod;
			if prod != 0 && (stack.tcp_peer_fin(ci) || stack.tcp_aborted(ci)) {
				close(prod);
				slot.stream_prod = 0;
			}
		}
		// Answer any deferred `accept`: a frame may have completed an inbound handshake, so a listener
		// that was waiting for a connection gets one now.
		for listener in listeners.iter_mut().filter(|listener| listener.chan != 0 && !listener.dead && listener.pending) {
			if let Some(ci) = stack.take_accepted(listener.binding.port) {
				if accept_handoff(listener.chan, listener.pending_corr, ci, socks, stack) {
					listener.pending = false;
				}
			}
		}
	}

	// The periodic wake: the IPv6 layer's timers and the DHCP lease clock.
	fn on_timer(&mut self) {
		let Some(Link { frames, stack, lease, rx, tx }) = self.link.as_mut() else {
			return;
		};
		if let Some(host) = stack.ipv6() {
			host.on_timer(now_ms());
		}
		drain_ipv6(*frames, stack);
		report_ipv6(stack);
		lease_due(*frames, stack, lease, rx, tx);
	}
}

// Send a built frame to the driver to transmit. A zero-length frame (the stack
// produced no reply) sends nothing.
fn send_frame(frames: u64, frame: &[u8]) {
	if !frame.is_empty() {
		send_blocking(frames, frame, 0);
	}
}

// Receive one frame from the link, run it through the stack, send any reply frame
// back, and return the stack event it produced (an echo / DNS reply an in-flight
// `ping` / `nslookup` is waiting for, or `None`). The channel closing means the link
// is gone: the stack is marked, every wait on it stops at once rather than spinning
// on a channel that is forever ready, and the serve loop tears the link down and
// moves on to the next one.
fn pump(frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Event {
	match recv_blocking(frames, rx) {
		Received::Message { len, .. } => {
			// AN IPv6 FRAME GOES TO THE IPv6 HOST AND NOWHERE ELSE. The two stacks share the link
			// and nothing else, so neither can be made to misbehave by the other's traffic. A raw-IP
			// link carries no ethertype: bytes 12 and 13 of its datagrams are a source address.
			if !stack.is_raw() && len > 13 && u16::from_be_bytes([rx[12], rx[13]]) == 0x86dd {
				if let Some(host) = stack.ipv6() {
					host.receive(&rx[..len], now_ms());
				}
				drain_ipv6(frames, stack);
				// A TRANSPORT EVENT CAN ARRIVE OVER EITHER FAMILY, and every caller of `pump` reads
				// one from here - so an IPv6 answer must come back the same way an IPv4 one does or
				// the waiting operation never learns it arrived.
				let events: Vec<Event> = deliver_ipv6(frames, stack);
				report_ipv6(stack);
				return events.into_iter().next().unwrap_or(Event::None);
			}
			let outcome: net::Outcome = stack.on_frame(&rx[..len], tx);
			send_frame(frames, &tx[..outcome.reply_len]);
			outcome.event
		}
		Received::Closed => {
			stack.mark_channel_closed();
			Event::None
		}
	}
}

// The monotonic clock in milliseconds, which is what the IPv6 layer's deadlines are in.
fn now_ms() -> u64 {
	clock_ns() / 1_000_000
}

// An IPv6 deadline, in milliseconds, expressed as the tick the scheduler waits on.
//
// ROUNDED UP, not down: a deadline rounded down fires early and the layer above computes the same
// deadline again, which is a busy loop at one tick per iteration.
fn ticks_from_ms(deadline_ms: u64) -> u64 {
	let now_tick = clock();
	let now = now_ms();
	let remaining_ms = deadline_ms.saturating_sub(now);
	now_tick + remaining_ms.div_ceil(1000 / TICKS_PER_SEC)
}

// Sixty-four bits of kernel randomness for one prefix's interface identifier.
//
// THE KERNEL'S, NOT THIS SERVICE'S. The identifier must not be derived from anything the NIC knows,
// and the one source of randomness a userspace program has that nothing on the link can predict is
// the syscall. The policy this feeds, and the reboot limit it carries, are written at
// `service_logic::ipv6::interface_identifier`.
// SIXTEEN BITS OF THE SAME RANDOMNESS the IPv6 identifier and the SNTP and DHCP items draw from.
//
// WHERE IT COMES FROM IS THE PROFILE'S. On a profile whose kernel has a secure source these are
// unpredictable and the off-path defence is real; on one without, the correlation contract still
// holds - the fields compared, the redraw on collision, the retirement on completion - because those
// are about matching rather than about entropy.
fn random_u16() -> u16 {
	let mut drawn = [0u8; 2];
	random_get(&mut drawn);
	u16::from_be_bytes(drawn)
}

fn random_u32() -> u32 {
	let mut drawn = [0u8; 4];
	random_get(&mut drawn);
	u32::from_be_bytes(drawn)
}

fn random_u64() -> u64 {
	let mut drawn = [0u8; 8];
	random_get(&mut drawn);
	u64::from_be_bytes(drawn)
}

fn ipv6_entropy() -> [u8; 8] {
	let mut drawn = [0u8; 8];
	random_get(&mut drawn);
	drawn
}

// Say what the IPv6 layer has, once per change.
//
// THE GUEST LOG IS THE ORACLE for a layer with no transport above it yet. Address configuration,
// router discovery and the seam's own health are things a booted system can be asked about only if
// it says them, and the transport that will consume them is the next milestone's.
fn report_ipv6(stack: &mut Stack) {
	let Some(host) = stack.ipv6() else {
		return;
	};
	let invalidations = host.drain_invalidations();
	let errors = host.drain_quoted_errors();
	// AN ADDRESS GOING AWAY IS ACTED ON, not only counted. A listener left published on an address
	// the interface no longer owns can never accept again while holding its port, and a connection
	// cannot complete a handshake from an address this machine does not have.
	let mut withdrawn: usize = 0;
	let mut closed: usize = 0;
	for event in &invalidations {
		if event.change != service_logic::ipv6_events::Change::Invalidated {
			continue;
		}
		let service_logic::ipv6_events::Identity::Address { address, .. } = event.identity else {
			continue;
		};
		let applied = stack.on_address_invalidated(address.octets());
		withdrawn += applied.listeners_withdrawn;
		closed += applied.connections_closed + applied.attempts_retired;
	}
	// EVERY ERROR REACHES THE FLOW THAT OWNS IT BEFORE IT IS COUNTED. The layer below validated the
	// quotation as far as a layer holding no flow state can; the consumer that owns the send queue
	// makes the second check and is the only thing that may act.
	for error in &errors {
		stack.on_ipv6_error(error);
	}
	let Some(host) = stack.ipv6() else {
		return;
	};
	for delivery in host.take_deliveries() {
		print(b"ipv6: delivered ");
		print_u64(u64::from(delivery.next_header));
		print(b" bytes=");
		print_u64(delivery.payload.len() as u64);
		print(b" hops=");
		print_u64(u64::from(delivery.hop_limit));
		print(b" from ");
		print_ipv6_address(delivery.source);
		print(b" to ");
		print_ipv6_address(delivery.destination);
		print(b"\n");
	}
	if invalidations.is_empty() && errors.is_empty() {
		return;
	}
	let snapshot = host.snapshot();
	// ONE WRITE, NOT TWENTY. Every other process on this system prints to the same console, and a
	// report assembled from twenty separate writes comes out with another subsystem's line spliced
	// through the middle of it - which is exactly what happened to this one. A report that cannot be
	// read is not evidence, so the whole line is built first and written once.
	let scope = match host.link_local().and_then(|address| host.scoped(address)).and_then(|scoped| scoped.interface()) {
		// A LINK-LOCAL ADDRESS WITHOUT ITS LINK IS NOT AN ADDRESS, so the line carries the scope
		// rather than leaving a reader to assume there is only ever one interface.
		Some(interface) => alloc::format!("%if{}.{}", interface.index, interface.generation),
		None => alloc::string::String::new(),
	};
	let where_it_is = match host.link_local() {
		Some(address) => alloc::format!("link-local {address:?}{scope}"),
		None => alloc::string::String::from("link-local pending"),
	};
	let (seen, unattributable) = host.quoted_error_counts();
	let mut line = alloc::format!("ipv6: {where_it_is}, addresses={}, routers={}, prefixes={}, routes={}, resolvers={}, mtu={}, events={}, errors={}, errors-seen={seen}, errors-dropped={unattributable}, router-fallbacks={}, withdrawn={withdrawn}, closed={closed}, dropped={}", host.addresses().len(), host.routers().len(), host.prefixes(), host.routes().len(), host.resolvers().len(), host.mtu(), invalidations.len(), errors.len(), host.router_fallbacks(), host.outbound_dropped(),);
	if host.resync_required() {
		line.push_str(", RESYNC REQUIRED");
	}
	if snapshot.any_saturated() {
		line.push_str(", a bound is saturated");
	}
	line.push('\n');
	print(line.as_bytes());
}

// The canonical text form of an address (RFC 5952), which the value type already knows how to
// write. Printing all eight groups with their leading zeros would be correct and unreadable.
fn print_ipv6_address(address: service_logic::ipv6::Address) {
	print(alloc::format!("{address:?}").as_bytes());
}

fn print_u64(mut value: u64) {
	let mut buffer = [0u8; 20];
	let mut index = buffer.len();
	loop {
		index -= 1;
		buffer[index] = b'0' + (value % 10) as u8;
		value /= 10;
		if value == 0 {
			break;
		}
	}
	print(&buffer[index..]);
}

// Wait for a frame, never past the IPv6 layer's next deadline, and drive that layer on the way back.
//
// EVERY BLOCKING WAIT IN THIS SERVICE GOES THROUGH HERE, and that is what the aggregated deadline
// means in practice. A DHCP retry, a DNS lookup or a TCP connect blocks for seconds at a time; while
// one of them waits, duplicate-address detection, router solicitation, neighbour retries and
// listener reports all have deadlines of their own, and a wait bounded only by the caller's own
// deadline starves every one of them. Measured before this existed: the first router solicitation
// left the host fifteen seconds late, behind an unanswered DHCP phase, and the interval to the
// second one was nothing like the schedule says.
//
// The caller's contract is unchanged. A wake for the IPv6 deadline is not reported to the caller as
// a timeout: the loop goes round again with the caller's own deadline still in force, so `!= 0`
// still means what it meant.
fn wait_frames(frames: u64, stack: &mut Stack, deadline: u64) -> i64 {
	loop {
		// A LINK WHOSE CHANNEL CLOSED HAS NOTHING TO WAIT FOR, and every caller stops at a non-zero
		// answer.
		if stack.channel_closed() {
			return ERR_PEER_CLOSED;
		}
		// EVERY TIMER THE STACK OWNS BOUNDS THIS WAIT. The IPv6 layer's deadlines and TCP's
		// retransmission, persist and TIME-WAIT deadlines are all reasons to wake, and a wait that
		// knew about only one of them would leave the others firing whenever unrelated traffic
		// happened to arrive - which is a retransmission schedule decided by somebody else.
		let mut bounded: u64 = deadline;
		for candidate in [stack.ipv6_deadline(), stack.tcp_deadline_any()] {
			if let Some(at) = candidate.map(ticks_from_ms) {
				bounded = bounded.min(at);
			}
		}
		let outcome: i64 = wait(frames, bounded);
		if let Some(host) = stack.ipv6() {
			host.on_timer(now_ms());
		}
		drain_ipv6(frames, stack);
		let _ipv6_events: Vec<Event> = deliver_ipv6(frames, stack);
		drive_tcp_timers(frames, stack);
		// REPORTED WHERE IT CHANGES. The line is written only when something is actually pending, so
		// this costs nothing on a quiet link - and without it the layer's state was said once, at
		// whatever moment the first event happened to land, and never again.
		report_ipv6(stack);
		if outcome == 0 || clock() >= deadline {
			return outcome;
		}
	}
}

// Fire every TCP timer that is due, and put back on the wire whatever that produced.
//
// THIS IS WHERE A RETRANSMISSION ACTUALLY HAPPENS. The queue holds the bytes and the estimator holds
// the timeout; without something driving them on a deadline the connection would retransmit only
// when a frame from somebody else woke the loop.
fn drive_tcp_timers(frames: u64, stack: &mut Stack) {
	let now: u64 = now_ms();
	// SIZED FROM THE LINK, not from a guess. `tcp_pump` builds nothing into a buffer too small for
	// the segment it wanted, so a short buffer here would drop a retransmission silently - which is
	// the failure this whole item exists to remove.
	let mut frame: Vec<u8> = alloc::vec![0u8; usize::from(stack.mtu()) + 14];
	for ci in stack.tcp_due(now) {
		if stack.tcp_on_timer(ci) {
			drain_tx(frames, ci, stack, &mut frame);
		}
	}
}

// Route what the IPv6 host delivered into the transports that own it.
//
// THE HOST OWNS L3 AND STOPS THERE. Everything above the IPv6 header - a TCP segment, a UDP datagram -
// belongs to the same machinery the other family uses, keyed by an address wide enough to tell the
// two apart. Anything this host has no transport for is reported and dropped rather than queued.
fn deliver_ipv6(frames: u64, stack: &mut Stack) -> Vec<Event> {
	let deliveries: Vec<crate::ipv6_host::Delivery> = match stack.ipv6() {
		Some(host) => host.take_deliveries(),
		None => return Vec::new(),
	};
	if deliveries.is_empty() {
		return Vec::new();
	}
	let mut out: Vec<u8> = alloc::vec![0u8; usize::from(stack.mtu()) + 14];
	let mut pending: Vec<Event> = Vec::new();
	for delivery in deliveries {
		stack.set_clock(now_ms());
		match delivery.next_header {
			IP_PROTO_TCP => {
				stack.on_tcp6(delivery.source.octets(), delivery.destination.octets(), &delivery.payload, &mut out);
			}
			IP_PROTO_UDP => {
				let event = stack.on_udp6(delivery.source.octets(), delivery.destination.octets(), &delivery.payload);
				if !matches!(event, Event::None) {
					pending.push(event);
				}
			}
			// AN ECHO REPLY IS THE ANSWER A LIVE DIAGNOSTIC IS WAITING FOR, and it reaches the
			// waiting operation the same way an IPv4 one does. Dropping it here left `ping -6`
			// unable to observe a peer that answered.
			service_logic::ipv6_packet::NEXT_ICMPV6 => {
				if let Ok(echo) = service_logic::ipv6_icmp::parse_echo(&delivery.payload)
					&& delivery.payload.first() == Some(&service_logic::ipv6_icmp::ECHO_REPLY)
				{
					pending.push(Event::EchoReply6(delivery.source.octets(), delivery.hop_limit, echo.identifier, echo.sequence));
				}
			}
			_ => {}
		}
	}
	// The replies an IPv6 segment provokes are queued on the host, not written here, so they leave
	// with everything else it has pending.
	drain_ipv6(frames, stack);
	pending
}

// Send everything the IPv6 host has queued, and tell the transports what became of what it held.
//
// BOTH HAPPEN HERE BECAUSE BOTH ARE "THE HOST FINISHED WITH A FRAME". A resolution that completed
// released the packets waiting on it; the flow that owns each one learns here that its sequence
// space is finally on the wire, or that it never will be.
fn drain_ipv6(frames: u64, stack: &mut Stack) {
	stack.apply_ipv6_completions();
	let Some(host) = stack.ipv6() else {
		return;
	};
	for frame in host.take_outbound() {
		send_frame(frames, &frame);
	}
}

// Place a client channel in the set: reuse a free slot (0), or grow the set.
fn place_client(clients: &mut Vec<u64>, chan: u64) {
	for slot in clients.iter_mut() {
		if *slot == 0 {
			*slot = chan;
			return;
		}
	}
	clients.push(chan);
}

// THE SERVICE WITHOUT A LINK: every operation that would need one answers `Io` - the device this
// service moves frames through is not there. What does work is exactly what does not need a link: the
// reserved connect request and `open` mint fresh connections, so PermissionManager can still grant the
// network capability and a tool launched under it gets a typed refusal rather than a hang; `capacity`
// counts the clients; `sockets` is empty.
struct Unlinked<'a> {
	new_client: &'a mut u64,
	clients_used: u32,
}

// The one answer for an operation that needs the NIC this boot does not have. `Io` rather than
// `not-found` or `unsupported`: the request is understood and this implementation would serve it,
// and what is missing is the device underneath - which is what `io` means - and `resolve` already
// answers `not-found` for a name that does not resolve, so the two must not read alike.
const NO_LINK: Error = Error::Io;

impl network::Service for Unlinked<'_> {
	fn info(&mut self) -> Result<NetInfo, Error> {
		Err(NO_LINK)
	}

	fn capacity(&mut self) -> Result<NetCapacity, Error> {
		// AN UNLINKED SERVICE HAS NO FAMILY CONFIGURED AT ALL, which is what `disabled` says: there
		// is no NIC for either of them to be configuring on.
		Ok(NetCapacity { clients: self.clients_used, sockets: 0, listeners: 0, connections: 0, ipv4: FamilyReadiness::Disabled, ipv6: FamilyReadiness::Disabled, diagnostic_used: 0, diagnostic_limit: service_logic::net_profile::DIAGNOSTIC_CAP, diagnostic_refusals: 0 })
	}

	fn resolve(&mut self, _name: String) -> Result<Vec<IpAddress>, Error> {
		Err(NO_LINK)
	}

	fn ping(&mut self, _addr: ScopedAddress) -> Result<PingReply, Error> {
		Err(NO_LINK)
	}

	fn probe(&mut self, _addr: ScopedAddress, _ttl: u8) -> Result<TraceHop, Error> {
		Err(NO_LINK)
	}

	fn fetch(&mut self, _req: TcpRequest) -> Result<Vec<FetchChunk>, Error> {
		Err(NO_LINK)
	}

	fn connect(&mut self, _target: OpenTarget) -> Result<u64, Error> {
		Err(NO_LINK)
	}

	// A fresh client channel, exactly as the linked service mints one: a caller granted the
	// network capability holds a connection of its own, and what it learns on it is the refusal.
	fn open(&mut self) -> Result<u64, Error> {
		match channel() {
			Some((server, peer)) => {
				*self.new_client = server;
				Ok(peer)
			}
			None => Err(Error::Again),
		}
	}

	fn listen(&mut self, _req: ListenRequest) -> Result<ListenResult, Error> {
		Err(NO_LINK)
	}

	fn sockets(&mut self) -> Result<Vec<SockInfo>, Error> {
		Ok(Vec::new())
	}

	fn sntp(&mut self, _server: ScopedAddress) -> Result<u64, Error> {
		Err(NO_LINK)
	}
}

// What a ready handle in the serve loop's wait set is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ready {
	Frames,
	Providers,
	LinkAdmin,
	Client,
	Socket,
	Listener,
}

// ONE LOOP, LINKED OR NOT. Stand on the link's channel when there is a link, the catalogue
// subscription, the link administration, every client's typed request channel, and every socket and
// listener at once (wait_any): a frame from the link is parsed (answering ARP / ICMP, any reply sent
// back to transmit); a publication may switch the link; a client request is decoded, dispatched to the
// generated `network` server - or, with no link, to the one that refuses what needs a link - and
// answered; a socket request (send / recv / close) is dispatched to the `socket` server for its
// connection. `network.open` mints a fresh client channel (one per spawned net tool) added to the
// client set; a client channel closing drops it from the set, a socket channel closing tears its
// connection down. While a DHCP lease is held, its clock arms the wait's deadline (a periodic
// housekeeping wake) and `lease_due` extends the lease when a threshold comes due.
fn serve(mut runtime: Runtime) -> ! {
	loop {
		let capacity: usize = 3 + runtime.link_admins.len() + runtime.clients.len() + runtime.socks.len() + runtime.listeners.len();
		let mut waits: Vec<u64> = Vec::with_capacity(capacity);
		let mut kind: Vec<Ready> = Vec::with_capacity(capacity);
		let mut slot_of: Vec<usize> = Vec::with_capacity(capacity);
		let mut stand = |handle: u64, ready: Ready, slot: usize| {
			if handle != 0 {
				waits.push(handle);
				kind.push(ready);
				slot_of.push(slot);
			}
		};
		if let Some(link) = runtime.link.as_ref() {
			stand(link.frames, Ready::Frames, 0);
		}
		stand(runtime.providers, Ready::Providers, 0);
		stand(runtime.link_root, Ready::LinkAdmin, 0);
		for &chan in &runtime.link_admins {
			stand(chan, Ready::LinkAdmin, 0);
		}
		for (slot, &chan) in runtime.clients.iter().enumerate() {
			stand(chan, Ready::Client, slot);
		}
		for (slot, sock) in runtime.socks.iter().enumerate() {
			stand(sock.chan, Ready::Socket, slot);
		}
		for (slot, listener) in runtime.listeners.iter().enumerate() {
			stand(listener.chan, Ready::Listener, slot);
		}
		// Nothing left to serve at all: the service ends, which is what `serve_multi` does for every
		// other service whose serve root closes.
		if waits.is_empty() {
			exit();
		}
		// ONE AGGREGATED DEADLINE. Whatever this loop is blocked on, it must not block past the next
		// thing that has to happen - the lease clock, and after it the IPv6 layer's own next timer: a
		// detection probe, a router solicitation, a neighbour retry or a listener report. Without
		// this, a blocking client request starves every one of them. No link, no deadline.
		let next: Option<u64> = runtime.link.as_ref().and_then(|link| {
			let ipv6_due: Option<u64> = link.stack.ipv6_deadline().map(ticks_from_ms);
			match (link.lease.next_due(), ipv6_due) {
				(Some(left), Some(right)) => Some(left.min(right)),
				(left, right) => left.or(right),
			}
		});
		let ready_raw: i64 = match next {
			Some(deadline) => wait_any_periodic(&waits, deadline),
			None => wait_any(&waits, 0),
		};
		if ready_raw == ERR_TIMED_OUT {
			runtime.on_timer();
		} else if ready_raw >= 0 {
			let ready: usize = ready_raw as usize;
			match kind[ready] {
				Ready::Frames => runtime.on_frames(),
				Ready::Providers => runtime.on_publications(),
				Ready::LinkAdmin => runtime.on_link_admin(waits[ready]),
				Ready::Client => runtime.on_client(slot_of[ready]),
				Ready::Socket => runtime.on_socket(slot_of[ready]),
				Ready::Listener => runtime.on_listener(slot_of[ready]),
			}
		}
		// A link whose channel closed during any of the above ends here, before the next wait.
		runtime.check_link();
	}
}

impl Runtime {
	// A client request on clients[slot]: dispatch the `network` interface. `open` may mint another
	// client channel, `connect` may open a socket, `listen` a listener; a closed channel is dropped
	// from the set.
	fn on_client(&mut self, slot: usize) {
		let Runtime { link, clients, socks, listeners, dns_flight, pending, echo, req, out, policy, .. } = self;
		let chan: u64 = clients[slot];
		match recv_caps_blocking(chan, req) {
			ReceivedCaps::Message { len, handles: caps } => {
				// A FRESH CONNECTION PER CALLER, answered here because it cannot be answered
				// generically.
				//
				// This service is not built on `serve_multi` - it stands on the link, every client,
				// every socket and every listener at once - so the reserved connect request has to be
				// handled by hand, as InputService, DisplayService and the audio engine already do.
				// Without it `service_connect` waits forever against this service and no other, and a
				// `factory` role in the bootstrap plan would mean one thing here and another
				// everywhere else.
				if len >= 2 && u16::from_le_bytes([req[0], req[1]]) == CONNECT_OP {
					for &unclaimed in caps.as_slice() {
						close(unclaimed);
					}
					match channel() {
						Some((mine, theirs)) => {
							place_client(clients, mine);
							send_blocking(chan, &[], theirs);
						}
						// Refused by replying with no capability: a caller that gets none knows it has
						// no connection, which is better than a channel nobody is waiting on.
						None => {
							send_blocking(chan, &[], 0);
						}
					}
					return;
				}
				// EVERY CAPABILITY THE MESSAGE CARRIED. This was `Handles::from_slice(&[handle])` over
				// the single-handle receive, which keeps the first and drops the rest - so a client
				// sending stdin, stdout and stderr had two destroyed before dispatch.
				let mut handle = caps;
				let mut new_sock: u64 = 0;
				let mut new_sock_ci: usize = 0;
				let mut new_client: u64 = 0;
				let mut new_listener: u64 = 0;
				let mut new_listener_id: u32 = 0;
				let mut new_listener_binding: Binding = Binding { mode: service_logic::tcp_bind::BindMode::Ipv4Only, address: service_logic::tcp_bind::Local::V4([0; 4]), port: 0 };
				// the live pool utilization, for the `capacity` reply (observability).
				let clients_used: u32 = clients.iter().filter(|&&c| c != 0).count() as u32;
				let sockets_used: u32 = socks.iter().filter(|s| s.chan != 0).count() as u32;
				let listeners_used: u32 = listeners.iter().filter(|l| l.chan != 0).count() as u32;
				let mut reply_handle = proto::codec::Handles::new();
				let answered: Option<usize> = match link.as_mut() {
					Some(Link { frames, stack, rx, tx, .. }) => {
						// every set grows on demand, so there is always room.
						let mut svc: Net = Net { frames: *frames, seq: 0, flight: dns_flight, pending, echo, families: policy.families, client: chan, stack, rx: &mut rx[..], tx: &mut tx[..], new_sock: &mut new_sock, new_sock_ci: &mut new_sock_ci, new_client: &mut new_client, new_listener: &mut new_listener, new_listener_binding: &mut new_listener_binding, new_listener_id: &mut new_listener_id, sock_room: true, client_room: true, listener_room: true, clients_used, sockets_used, listeners_used };
						network::dispatch(&mut svc, &req[..len], &mut handle, out, &mut reply_handle)
					}
					None => {
						let mut svc: Unlinked = Unlinked { new_client: &mut new_client, clients_used };
						network::dispatch(&mut svc, &req[..len], &mut handle, out, &mut reply_handle)
					}
				};
				if let Some(n2) = answered {
					if !send_caps_blocking(chan, &out[..n2], reply_handle.as_slice()) {
						for &leftover in reply_handle.as_slice() {
							close(leftover);
						}
					}
				}
				for &unclaimed in handle.as_slice() {
					close(unclaimed);
				}
				if new_sock != 0 {
					place_sock(socks, SockSlot { chan: new_sock, ci: new_sock_ci, stream_prod: 0, stream_seq: 0, dead: false });
				}
				if new_listener != 0 {
					place_listener(listeners, Listener { chan: new_listener, id: new_listener_id, binding: new_listener_binding, pending_corr: 0, pending: false, dead: false });
				}
				if new_client != 0 {
					place_client(clients, new_client);
				}
			}
			ReceivedCaps::Closed => {
				close(chan);
				clients[slot] = 0;
			}
		}
	}

	fn on_socket(&mut self, slot: usize) {
		let Runtime { link, socks, req, out, .. } = self;
		let sock: &mut SockSlot = &mut socks[slot];
		match link.as_mut() {
			Some(Link { frames, stack, rx, tx, .. }) if !sock.dead => serve_socket(sock, *frames, stack, rx, tx, out, req),
			_ => serve_dead_socket(sock, out, req),
		}
	}

	fn on_listener(&mut self, slot: usize) {
		let Runtime { link, socks, listeners, req, .. } = self;
		let listener: &mut Listener = &mut listeners[slot];
		match link.as_mut() {
			Some(link) if !listener.dead => serve_listener(listener, socks, &mut link.stack, req),
			_ => serve_dead_listener(listener, req),
		}
	}

	// A request on the link-admin root or on a connection minted from it.
	fn on_link_admin(&mut self, chan: u64) {
		let mut req: Vec<u8> = alloc::vec![0u8; LINK_ADMIN_BYTES];
		match recv_caps_blocking(chan, &mut req) {
			ReceivedCaps::Message { len, handles } => {
				// A CONNECTION PER CALLER, minted here like every other root of this service mints one.
				if len >= 2 && u16::from_le_bytes([req[0], req[1]]) == CONNECT_OP {
					for &unclaimed in handles.as_slice() {
						close(unclaimed);
					}
					match channel() {
						Some((mine, theirs)) => {
							place_client(&mut self.link_admins, mine);
							send_blocking(chan, &[], theirs);
						}
						None => {
							send_blocking(chan, &[], 0);
						}
					}
					return;
				}
				let mut handles = handles;
				let mut out: Vec<u8> = alloc::vec![0u8; LINK_ADMIN_BYTES];
				let mut reply_handles = proto::codec::Handles::new();
				let mut deferred: uplink::Decision = uplink::Decision::Keep;
				let answered: Option<usize> = {
					let mut call: LinkAdmin = LinkAdmin { runtime: self, chan, deferred: &mut deferred };
					network_link_admin::dispatch(&mut call, &req[..len], &mut handles, &mut out, &mut reply_handles)
				};
				if let Some(n) = answered {
					if !send_caps_blocking(chan, &out[..n], reply_handles.as_slice()) {
						for &leftover in reply_handles.as_slice() {
							close(leftover);
						}
					}
				}
				for &unclaimed in handles.as_slice() {
					close(unclaimed);
				}
				// THE ANSWER FIRST, THE FALLBACK AFTER. A release that brings a NIC back runs DHCP, and
				// the caller has already been told its link is gone.
				self.apply(deferred);
			}
			ReceivedCaps::Closed => {
				close(chan);
				if chan == self.link_root {
					self.link_root = 0;
				}
				for held in self.link_admins.iter_mut().filter(|held| **held == chan) {
					*held = 0;
				}
				// THE HOLDER GONE IS ITS RESERVATION GONE: a modem service that ended cannot keep a link
				// installed, or a reservation open, on its behalf.
				if let Some((holder, reservation)) = self.holder
					&& holder == chan
				{
					self.holder = None;
					let decision: uplink::Decision = self.uplinks.release(reservation);
					self.apply(decision);
				}
			}
		}
	}
}

// THE PRIVATE LINK ADMINISTRATION, for one connection. A reservation belongs to the connection that
// made it: another connection's install or release names nothing it holds.
struct LinkAdmin<'a> {
	runtime: &'a mut Runtime,
	chan: u64,
	// A release's fallback, applied after the reply has gone.
	deferred: &'a mut uplink::Decision,
}

fn octets_of(ip: &WireIp) -> [u8; 4] {
	[ip.a, ip.b, ip.c, ip.d]
}

impl network_link_admin::Service for LinkAdmin<'_> {
	// ADMISSION, BEFORE THE MODEM IS ASKED TO DO ANYTHING. `again` when another link is selected and
	// the caller may not replace it, or a modem link already exists; the current link is untouched.
	fn reserve(&mut self, replace_uplink: bool) -> Result<u64, Error> {
		match self.runtime.uplinks.reserve(replace_uplink) {
			Ok(reservation) => {
				self.runtime.holder = Some((self.chan, reservation));
				Ok(reservation)
			}
			Err(uplink::Refusal::Busy) => Err(Error::Again),
			Err(_) => Err(Error::Invalid),
		}
	}

	// COMMIT ATOMICALLY OR NOT AT ALL. Everything is validated before the current link is touched, and
	// a refusal leaves it exactly as it was and gives the packet channel back.
	fn install(&mut self, reservation: u64, attachment: LinkAttachment) -> Result<LinkInstalled, Error> {
		let refuse = |error: Error| -> Result<LinkInstalled, Error> {
			if attachment.packets != 0 {
				close(attachment.packets);
			}
			Err(error)
		};
		if self.runtime.holder != Some((self.chan, reservation)) {
			return refuse(Error::NotFound);
		}
		let checked: uplink::Attachment = uplink::Attachment { family: attachment.family as u8, address: octets_of(&attachment.address), prefix: attachment.prefix, gateway: attachment.gateway.as_ref().map(octets_of), dns: [attachment.dns.first().map(octets_of), attachment.dns.get(1).map(octets_of)], mtu: attachment.mtu };
		if let Err(refusal) = uplink::validate(&checked) {
			return refuse(if refusal == uplink::Refusal::Unsupported { Error::Unsupported } else { Error::Invalid });
		}
		// A FAMILY THIS BOOT DOES NOT RUN IS NOT INSTALLED. Under the `ipv6` profile there is no IPv4
		// at all, on any link.
		if !self.runtime.policy.families.includes_v4() {
			return refuse(Error::Unsupported);
		}
		if attachment.packets == 0 || attachment.dns.len() > 2 {
			return refuse(Error::Invalid);
		}
		match self.runtime.uplinks.install(reservation) {
			Ok(uplink::Decision::Switch { to: uplink::Selected::Modem(_), generation }) => {
				self.runtime.tear_down();
				Ok(self.runtime.bring_up_raw(&attachment, generation))
			}
			// A NIC appeared after the reservation and this caller may not replace it: the reservation
			// was given back with the refusal.
			Err(uplink::Refusal::Busy) => {
				self.runtime.holder = None;
				refuse(Error::Again)
			}
			Ok(_) | Err(_) => refuse(Error::NotFound),
		}
	}

	// Remove the link installed under the reservation, or give an unused one back. What it replaced
	// comes back after the reply.
	fn release(&mut self, reservation: u64) -> Result<(), Error> {
		if self.runtime.holder != Some((self.chan, reservation)) {
			return Err(Error::NotFound);
		}
		self.runtime.holder = None;
		*self.deferred = self.runtime.uplinks.release(reservation);
		Ok(())
	}
}

// A SOCKET FROM A LINK THAT IS GONE. Every operation answers `link-changed` - the connection was not
// carried to the new link and never will be - and a receive stream opens already ended. Closing it, or
// dropping it, frees the slot; nothing is sent, because the link it would be sent on is gone.
fn serve_dead_socket(slot: &mut SockSlot, out: &mut [u8], req: &mut [u8]) {
	match recv_caps_blocking(slot.chan, req) {
		ReceivedCaps::Message { len, handles } => {
			let mut handle = handles;
			let op: u16 = if len >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
			let mut closing: bool = false;
			{
				let mut svc: DeadSock = DeadSock { closing: &mut closing };
				if op == socket::OP_RECV {
					if let Some((corr, _)) = socket::recv_open(&mut svc, &req[..len], &mut handle)
						&& let Some((producer, consumer)) = channel()
					{
						send_blocking(slot.chan, &corr.to_le_bytes(), consumer);
						close(producer);
					}
				} else {
					let mut reply_handle = proto::codec::Handles::new();
					if let Some(n2) = socket::dispatch(&mut svc, &req[..len], &mut handle, out, &mut reply_handle)
						&& !send_caps_blocking(slot.chan, &out[..n2], reply_handle.as_slice())
					{
						for &leftover in reply_handle.as_slice() {
							close(leftover);
						}
					}
				}
			}
			for &unclaimed in handle.as_slice() {
				close(unclaimed);
			}
			if closing {
				close(slot.chan);
				*slot = SockSlot { chan: 0, ci: 0, stream_prod: 0, stream_seq: 0, dead: false };
			}
		}
		ReceivedCaps::Closed => {
			close(slot.chan);
			*slot = SockSlot { chan: 0, ci: 0, stream_prod: 0, stream_seq: 0, dead: false };
		}
	}
}

struct DeadSock<'a> {
	closing: &'a mut bool,
}

impl socket::Service for DeadSock<'_> {
	fn send(&mut self, data: Buffer) -> Result<u32, Error> {
		close(data.handle);
		Err(Error::LinkChanged)
	}

	fn recv(&mut self) -> Vec<Chunk> {
		Vec::new()
	}

	// Closed, and said to have been cut rather than closed in order.
	fn close(&mut self) -> Result<(), Error> {
		*self.closing = true;
		Err(Error::LinkChanged)
	}
}

// A listener from a link that is gone: every `accept` answers `link-changed`, and closing it frees
// the slot. Its port belonged to the old stack and went with it.
fn serve_dead_listener(listener: &mut Listener, req: &mut [u8]) {
	match recv_blocking(listener.chan, req) {
		Received::Message { len, .. } => {
			if len >= 6 && u16::from_le_bytes([req[0], req[1]]) == listener::OP_ACCEPT {
				refuse_accept(listener.chan, u32::from_le_bytes([req[2], req[3], req[4], req[5]]));
			}
		}
		Received::Closed => {
			close(listener.chan);
			listener.chan = 0;
			listener.pending = false;
			listener.dead = false;
		}
	}
}

// Answer an `accept` with `link-changed`: the correlation, the `Err` discriminant and the error, which
// is the generated encoding of that result.
fn refuse_accept(chan: u64, corr: u32) {
	let mut reply: Vec<u8> = Vec::with_capacity(6);
	reply.extend_from_slice(&corr.to_le_bytes());
	reply.push(0);
	reply.extend_from_slice(&Error::LinkChanged.encode_vec().unwrap_or_default());
	send_blocking(chan, &reply, 0);
}

// One active socket the serve loop multiplexes: the channel the `socket` interface is
// served on, the stack connection index it drives, and its received-data stream (the
// producer end, 0 = none, plus that stream's frame sequence).
#[derive(Clone, Copy)]
struct SockSlot {
	chan: u64,
	ci: usize,
	stream_prod: u64,
	stream_seq: u32,
	// Its link is gone: every operation answers `link-changed` until the client lets it go.
	dead: bool,
}

// Place a socket in the set: reuse a free slot (chan 0), or grow the set.
fn place_sock(socks: &mut Vec<SockSlot>, sock: SockSlot) {
	for slot in socks.iter_mut() {
		if slot.chan == 0 {
			*slot = sock;
			return;
		}
	}
	socks.push(sock);
}

// Service one ready socket slot: decode the request and dispatch it to the `socket`
// server for this slot's connection, or tear the slot down on close / peer-drop.
// OP_RECV opens the received-data stream out of band (a fresh sub-channel handed back
// with the correlation id, then any already-buffered bytes framed onto the producer;
// the serve loop streams everything that arrives afterwards).
fn serve_socket(slot: &mut SockSlot, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8], out: &mut [u8], req: &mut [u8]) {
	let chan: u64 = slot.chan;
	let ci: usize = slot.ci;
	match recv_caps_blocking(chan, req) {
		ReceivedCaps::Message { len, handles: caps } => {
			// EVERY CAPABILITY THE MESSAGE CARRIED. This was `Handles::from_slice(&[handle])`
			// over the single-handle receive, which keeps the first and drops the rest - so a
			// client sending stdin, stdout and stderr had two destroyed before dispatch.
			let mut handle = caps;
			let op: u16 = if len >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
			let mut closing: bool = false;
			{
				let mut svc: Sock = Sock { ci, frames, stack: &mut *stack, tx, closing: &mut closing };
				if op == socket::OP_RECV {
					if let Some((corr, items)) = socket::recv_open(&mut svc, &req[..len], &mut handle) {
						if slot.stream_prod != 0 {
							close(slot.stream_prod);
							slot.stream_prod = 0;
						}
						if let Some((producer, consumer)) = channel() {
							send_blocking(chan, &corr.to_le_bytes(), consumer);
							slot.stream_seq = 0;
							for item in &items {
								let mut frame_handles = Handles::new();
								if let Some(fl) = socket::recv_frame(slot.stream_seq, item, out, &mut frame_handles) {
									if !send_caps_blocking(producer, &out[..fl], frame_handles.as_slice()) {
										for handle in frame_handles.as_slice() {
											close(*handle);
										}
									}
									slot.stream_seq += 1;
								} else {
									for handle in frame_handles.as_slice() {
										close(*handle);
									}
								}
							}
							slot.stream_prod = producer;
						}
					}
				} else {
					let mut reply_handle = proto::codec::Handles::new();
					if let Some(n2) = socket::dispatch(&mut svc, &req[..len], &mut handle, out, &mut reply_handle) {
						if !send_caps_blocking(chan, &out[..n2], reply_handle.as_slice()) {
							for &leftover in reply_handle.as_slice() {
								close(leftover);
							}
						}
					}
				}
				for &unclaimed in handle.as_slice() {
					close(unclaimed);
				}
			}
			if closing {
				teardown_socket(slot, frames, stack, rx, tx);
			}
		}
		// The client dropped its socket without calling close(): tear it down.
		ReceivedCaps::Closed => {
			teardown_socket(slot, frames, stack, rx, tx);
		}
	}
}

// Tear a socket slot down: close its recv stream, send the connection's FIN, free the
// stack connection, close the channel, and empty the slot.
fn teardown_socket(slot: &mut SockSlot, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) {
	if slot.stream_prod != 0 {
		close(slot.stream_prod);
		slot.stream_prod = 0;
	}
	socket_teardown(slot.ci, frames, stack, rx, tx);
	stack.tcp_free(slot.ci);
	close(slot.chan);
	slot.chan = 0;
}

// One active listening socket the serve loop multiplexes: the channel the `listener`
// interface is served on, the port it accepts inbound connections on, and a deferred
// `accept` (the correlation id to answer once a connection completes, when `accept`
// was called with none pending).
#[derive(Clone, Copy)]
struct Listener {
	chan: u64,
	// The identity the inbound accounting is keyed by.
	id: u32,
	// WHAT IT CLAIMED, not only which port: releasing a claim needs the mode and the address too,
	// because two listeners may legitimately hold the same port in two families.
	binding: Binding,
	pending_corr: u32,
	pending: bool,
	// Its link is gone - see `SockSlot::dead`.
	dead: bool,
}

// Place a listener in the set: reuse a free slot (chan 0), or grow the set.
fn place_listener(listeners: &mut Vec<Listener>, listener: Listener) {
	for slot in listeners.iter_mut() {
		if slot.chan == 0 {
			*slot = listener;
			return;
		}
	}
	listeners.push(listener);
}

// Service one ready listener: an `accept` request is answered now (an inbound
// connection has completed its handshake) or deferred (its correlation id remembered)
// to be answered when one does. A closed listener channel stops listening on its port.
fn serve_listener(listener: &mut Listener, socks: &mut Vec<SockSlot>, stack: &mut Stack, req: &mut [u8]) {
	match recv_blocking(listener.chan, req) {
		Received::Message { len, .. } => {
			// The request frames as [op u16][corr u32]; OP_ACCEPT is the only op.
			if len >= 6 {
				let op: u16 = u16::from_le_bytes([req[0], req[1]]);
				let corr: u32 = u32::from_le_bytes([req[2], req[3], req[4], req[5]]);
				if op == listener::OP_ACCEPT {
					match stack.take_accepted(listener.binding.port) {
						Some(ci) if accept_handoff(listener.chan, corr, ci, socks, stack) => {}
						_ => {
							listener.pending_corr = corr;
							listener.pending = true;
						}
					}
				}
			}
		}
		Received::Closed => {
			stack.unlisten(listener.id);
			close(listener.chan);
			listener.chan = 0;
		}
	}
}

// Hand accepted connection `ci` to the listener as a socket: mint the socket channel,
// place it in the socket set (which grows on demand), and reply to the pending
// `accept` (correlation `corr`) with the client end out of band. The reply frames a
// `result<handle<channel>, error>` Ok: [corr u32][tag 1][u32 0] inline, the channel
// out of band. The connection is dropped if the channel cannot be minted.
fn accept_handoff(listener_chan: u64, corr: u32, ci: usize, socks: &mut Vec<SockSlot>, stack: &mut Stack) -> bool {
	match channel() {
		Some((server, peer)) => {
			// THE REPLY IS THE GENERATED ENCODING, not a hand-cut one. `accept` now answers with both
			// endpoints beside the capability, and a reply assembled by hand would be a second
			// encoder of the same record - which is how the two come to disagree.
			let (local_port, remote_ip, remote_port) = stack.conn_endpoints(ci).unwrap_or((0, Ipv4Addr([0; 4]), 0));
			let accepted = AcceptResult { socket: peer, local: endpoint_v4(stack.ip(), local_port), remote: endpoint_v4(remote_ip, remote_port) };
			let Some((body, handles)) = accepted.encode_message() else {
				stack.tcp_free(ci);
				return false;
			};
			let mut reply: Vec<u8> = Vec::with_capacity(5 + body.len());
			reply.extend_from_slice(&corr.to_le_bytes());
			reply.push(1);
			reply.extend_from_slice(&body);
			place_sock(socks, SockSlot { chan: server, ci, stream_prod: 0, stream_seq: 0, dead: false });
			send_blocking(listener_chan, &reply, handles.first());
			// THE SLOT IS RELEASED ONLY NOW. Removing a queue entry before a fallible handoff is not
			// release: a failed channel allocation above leaves the connection queued and charged,
			// and the listener stays live.
			stack.accepted(ci);
			true
		}
		None => {
			stack.tcp_free(ci);
			false
		}
	}
}

// The state the typed `network` service operates on for one client request: the
// driver frame channel, an ICMP/DNS sequence counter, the stack, and the frame
// scratch buffers - the stack and buffers are borrowed from the serve loop, so the
// connection state and the 2 kB frame buffers are not duplicated on the stack.
// `connect` parks the server end of the socket it opens in `new_sock` (and its stack
// connection index in `new_sock_ci`) for the serve loop to pick up. The room flags
// are always true now that every set grows on demand; they remain so a future
// resource policy can gate admission again.
struct Net<'a> {
	frames: u64,
	seq: u16,
	// The DNS identities currently in flight, so a drawn one is unique and a finished one matches
	// nothing.
	flight: &'a mut dns::InFlight,
	pending: &'a mut Pending,
	echo: &'a mut service_logic::net_profile::EchoCounter,
	families: Families,
	// Which client this request came in on, for the per-client caps.
	client: u64,
	stack: &'a mut Stack,
	rx: &'a mut [u8],
	tx: &'a mut [u8],
	new_sock: &'a mut u64,
	new_sock_ci: &'a mut usize,
	// Where `open` parks the server end of a freshly minted client channel, and `listen`
	// the server end of a listener channel (with its port), for the serve loop to pick
	// up; plus whether the client / socket / listener sets have room.
	new_client: &'a mut u64,
	new_listener: &'a mut u64,
	new_listener_binding: &'a mut Binding,
	new_listener_id: &'a mut u32,
	sock_room: bool,
	client_room: bool,
	listener_room: bool,
	// The live pool utilization the serve loop snapshots before each request: how many
	// client, socket and listener channels it currently stands on. Reported by
	// `capacity` (the TCP connection count comes from the stack).
	clients_used: u32,
	sockets_used: u32,
	listeners_used: u32,
}

// Convert the stack's internal address to the wire (canonical) form, and back.
fn to_wire(ip: Ipv4Addr) -> WireIp {
	WireIp { a: ip.0[0], b: ip.0[1], c: ip.0[2], d: ip.0[3] }
}

// An address of either family in the form every record now carries.
fn wire_v4(ip: Ipv4Addr) -> IpAddress {
	IpAddress::V4(to_wire(ip))
}

fn wire_v6(addr: service_logic::ipv6::Address) -> IpAddress {
	IpAddress::V6(WireIpv6::from_octets(addr.octets()))
}

// This service's interface identity: the one link selected, and the generation it was selected under
// - the IPv6 host is brought up with the same one - so a scoped value cannot outlive a replacement.
fn interface_of(stack: &Stack) -> InterfaceId {
	let (index, generation) = stack.interface();
	InterfaceId { index, generation }
}

fn endpoint_v4(ip: Ipv4Addr, port: u16) -> ScopedEndpoint {
	ScopedEndpoint { addr: ScopedAddress { addr: wire_v4(ip), scope: None }, port }
}

fn wire_readiness(state: Readiness) -> FamilyReadiness {
	match state {
		Readiness::Disabled => FamilyReadiness::Disabled,
		Readiness::Configuring => FamilyReadiness::Configuring,
		Readiness::Ready => FamilyReadiness::Ready,
		Readiness::Failed => FamilyReadiness::Failed,
	}
}

// A lifetime in seconds as the wire carries it, with `u32::MAX` reserved for infinity - the value
// the protocols themselves use, so nothing has to be translated on the way out.
const INFINITE_LIFETIME: u32 = u32::MAX;

// The deepest accept queue this stack's listen table can actually hold. A caller asking for more is

// The most bytes one fetch delivers, and the chunk the stream carries them in. Both live beside the
// generated types, where the clients that have to agree with them can see them.
use proto::net_limits::{FETCH_CHUNK_BYTES, MAX_FETCH_BODY_BYTES, MAX_OPEN_DESTINATIONS};

// THE FIRST DESTINATION THIS SERVICE CAN REACH, after the whole target has been validated.
//
// VALIDATION BEFORE ADMISSION, not as the attempts go along: an empty list, a list past the bound or
// a malformed scope is a request that was wrong when it arrived, and finding that out halfway
// through a sequence of attempts would mean some of them already happened.
// Where an open is going, in whichever family the caller named.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Destination {
	V4(Ipv4Addr),
	V6([u8; 16]),
}

// THE SOURCE THIS HOST WOULD SEND A DESTINATION FROM, or `None` when it has none.
//
// Factored out because two callers need the same answer and a second copy is how they come to
// disagree: the resolver's answer and an open's candidate list are the SAME question about the same
// host, and a name that resolves to a destination this host cannot reach has to rank the same way in
// both.
fn own_source(destination: Destination, stack: &Stack) -> Option<addr_select::Source> {
	match destination {
		Destination::V4(_) => Some(addr_select::Source { address: addr_select::mapped_v4(stack.ip().0), interface_index: 0, interface_generation: 1, deprecated: false, prefix_len: 32 }),
		Destination::V6(octets) => stack.ipv6_ref().and_then(|host| host.source_address(service_logic::ipv6::Address::new(octets))).map(|address| addr_select::Source { address: address.octets(), interface_index: 0, interface_generation: 1, deprecated: false, prefix_len: 64 }),
	}
}

// RFC 6724 section 6 over a plain list of destinations: the order this host will try them in.
//
// THIS IS WHAT `resolve` OWED ITS CALLERS AND DID NOT PAY. The resolver asked for `AAAA` and then
// `A` and returned them in that order, under a comment claiming the order was RFC 6724's - and the
// module that implements RFC 6724 opens by saying that "IPv6 first" is NOT an algorithm. It has no
// answer for a host whose only IPv6 address is a unique-local one and whose destination is a global
// one: rule 5 wants the labels to match, they do not, and the IPv4 pair matches at label 4. So the
// algorithm says IPv4 and the fixed order said IPv6.
//
// WHAT THAT COST, MEASURED ON A MACHINE THAT HAS THE SHAPE: a host behind a NAT that offers a
// site-local IPv6 prefix and no route out of it. `ping google.com` took the first of the list, sent
// every echo into the prefix, and reported a hundred percent loss - on a link where the IPv4 next to
// it answered every time. Nothing in the output named the family, because nothing had chosen it.
fn attempt_order_for(addresses: &[Destination], stack: &Stack) -> Vec<usize> {
	let candidates: Vec<addr_select::Destination> = addresses
		.iter()
		.enumerate()
		.map(|(index, &destination)| {
			let wide: [u8; 16] = match destination {
				Destination::V4(ip) => addr_select::mapped_v4(ip.0),
				Destination::V6(octets) => octets,
			};
			addr_select::Destination { address: wide, source: own_source(destination, stack), supplied: index }
		})
		.collect();
	addr_select::attempt_order(&candidates)
}

// The candidates a caller supplied, in the order this host will try them.
//
// THE ORDER IS RFC 6724'S AND IS DECIDED ONCE, AT ADMISSION. It is retained for this open: a
// candidate is revalidated before it starts, but the name is never re-resolved, nothing is added,
// and a failed candidate is never revisited. That is what makes a fallback a sequence rather than a
// race.
fn ordered_destinations(target: &OpenTarget, stack: &Stack) -> Result<Vec<Destination>, Error> {
	let first: Destination = first_destination(target)?;
	let named: Option<Destination> = named_source(target)?;
	// AN ADDRESS THE MACHINE NO LONGER HOLDS FAILS THE WHOLE OPEN, and says which failure it is. No
	// candidate can be opened from it, and substituting another source is the one thing this field
	// forbids.
	if let Some(source) = named
		&& !stack.holds_local(local_of(source))
	{
		return Err(Error::AddressUnavailable);
	}
	let mut candidates: Vec<addr_select::Destination> = Vec::new();
	let mut addresses: Vec<Destination> = Vec::new();
	let mut seen: Vec<ScopedAddress> = Vec::new();
	for candidate in &target.destinations {
		if seen.iter().any(|held| held == candidate) {
			continue;
		}
		seen.push(candidate.clone());
		let Ok(destination) = open_destination(candidate) else {
			continue;
		};
		// A source that cannot serve THIS destination fails this destination only; the rest of the
		// list is still tried with the same source.
		if named.is_some_and(|source| !source_serves(source, destination)) {
			continue;
		}
		let wide: [u8; 16] = match destination {
			Destination::V4(ip) => addr_select::mapped_v4(ip.0),
			Destination::V6(octets) => octets,
		};
		// THE SOURCE IS PART OF THE DESTINATION'S RANK. A destination this host has no source for is
		// unusable, and RFC 6724 section 6's first rule puts it last rather than dropping it.
		let source: Option<addr_select::Source> = match named {
			// THE CALLER'S CHOICE IS THE SOURCE, so it is also the one the ordering ranks against:
			// ranking by a source that will not be used answers a question nobody asked.
			Some(Destination::V4(ip)) => Some(addr_select::Source { address: addr_select::mapped_v4(ip.0), interface_index: 0, interface_generation: 1, deprecated: false, prefix_len: 32 }),
			Some(Destination::V6(octets)) => Some(addr_select::Source { address: octets, interface_index: 0, interface_generation: 1, deprecated: false, prefix_len: 64 }),
			// THE SAME DERIVATION `resolve` USES, from one place - see `own_source`.
			None => own_source(destination, stack),
		};
		candidates.push(addr_select::Destination { address: wide, source, supplied: addresses.len() });
		addresses.push(destination);
	}
	if addresses.is_empty() {
		// A NAMED SOURCE THAT SERVES NONE OF THEM IS A REFUSAL. Falling back to the first candidate
		// here would open it from a source the caller did not choose.
		if named.is_some() {
			return Err(Error::Invalid);
		}
		return Ok(alloc::vec![first]);
	}
	Ok(addr_select::attempt_order(&candidates).into_iter().map(|index| addresses[index]).collect())
}

// A destination in the form the stack's address table is keyed by.
fn local_of(destination: Destination) -> service_logic::tcp_bind::Local {
	match destination {
		Destination::V4(ip) => service_logic::tcp_bind::Local::V4(ip.0),
		Destination::V6(octets) => service_logic::tcp_bind::Local::V6(octets),
	}
}

// The source the caller named, if any, checked for shape before anything is admitted.
//
// SHAPE ONLY. Whether this host HOLDS the address is a separate question with a separate answer:
// a malformed source is the caller's mistake, and an address that went away is the machine's state.
fn named_source(target: &OpenTarget) -> Result<Option<Destination>, Error> {
	match &target.source {
		None => Ok(None),
		Some(scoped) => open_destination(scoped).map(Some),
	}
}

// Is the named source usable for this destination at all?
//
// THE FAMILIES MUST MATCH. An IPv6 source cannot carry an IPv4 datagram, so a mismatched pair is
// not a connection this host can make - and per M1 it fails THAT destination rather than the open:
// another candidate may be reachable from the same source.
fn source_serves(source: Destination, destination: Destination) -> bool {
	matches!((source, destination), (Destination::V4(_), Destination::V4(_)) | (Destination::V6(_), Destination::V6(_)))
}

fn first_destination(target: &OpenTarget) -> Result<Destination, Error> {
	if target.destinations.is_empty() || target.destinations.len() > MAX_OPEN_DESTINATIONS {
		return Err(Error::Invalid);
	}
	// A CALLER-CHOSEN SOURCE IS HONOURED OR REFUSED, never ignored. Naming one and silently getting
	// another is exactly the failure the field exists to prevent.
	named_source(target)?;
	// Duplicates collapse to their first occurrence; the order is the caller's and is preserved.
	let mut seen: Vec<ScopedAddress> = Vec::new();
	for candidate in &target.destinations {
		if !seen.iter().any(|held| held == candidate) {
			seen.push(candidate.clone());
		}
	}
	// Every candidate is checked, so a malformed one refuses the request even when an earlier
	// candidate would have been usable.
	let mut first: Option<Destination> = None;
	let mut carried: Option<Error> = None;
	for candidate in &seen {
		match open_destination(candidate) {
			Ok(addr) if first.is_none() => first = Some(addr),
			Ok(_) => {}
			Err(Error::Invalid) => return Err(Error::Invalid),
			Err(error) => carried = Some(error),
		}
	}
	match first {
		Some(addr) => Ok(addr),
		None => Err(carried.unwrap_or(Error::Invalid)),
	}
}

// Is this family one this boot runs at all?
//
// ONLY `Disabled` IS A FAMILY-WIDE VETO. Everything else is decided per destination against the
// current addresses and routes: a family that is still `Configuring` may have a usable on-link path,
// and one that is `Ready` may have no route to one particular destination. Readiness is
// observability and cannot override that lookup.
fn family_admits(families: Families, destination: &Destination) -> Result<(), Error> {
	let included: bool = match destination {
		Destination::V4(_) => families.includes_v4(),
		Destination::V6(_) => families.includes_v6(),
	};
	match included {
		true => Ok(()),
		// `unsupported` rather than `invalid`: the address is well formed and this boot does not
		// serve its family, which is what the readiness report names as `disabled`.
		false => Err(Error::Unsupported),
	}
}

// One candidate, validated into the form the transports open with.
// THE ONE PLACE A CALLER'S ADDRESS BECOMES SOMETHING THIS SERVICE CAN SEND TO.
//
// The contract carries either family and so does everything below it, so the job here is shape: a
// scope where a scope selects nothing, and a second spelling of an address that is already
// expressible, are both the caller's mistake and are named as such.
fn open_destination(addr: &ScopedAddress) -> Result<Destination, Error> {
	match &addr.addr {
		// A SCOPE ON AN IPv4 ADDRESS IS REFUSED RATHER THAN IGNORED. There is one interface, so the
		// scope selects nothing, and ignoring it would let a caller believe it had.
		IpAddress::V4(_) if addr.scope.is_some() => Err(Error::Invalid),
		IpAddress::V4(v4) => Ok(Destination::V4(Ipv4Addr([v4.a, v4.b, v4.c, v4.d]))),
		// AN IPv4-MAPPED ADDRESS IS NOT AN IPv4 DESTINATION, by the same rule that refuses it as a
		// listen address: the direct spelling exists, and a second one is a check every consumer has
		// to remember.
		IpAddress::V6(v6) if v6.is_ipv4_mapped() => Err(Error::Invalid),
		IpAddress::V6(v6) => Ok(Destination::V6(v6.octets())),
	}
}

// THE BIND MATRIX, IN ONE PLACE. Which mode may name which address is a frozen contract, and a
// listener that validated it anywhere else would let two callers disagree about what `::` means.
fn validate_bind(req: &ListenRequest) -> Result<Binding, Error> {
	// A scope on a listen address is refused: this service has one interface, so it selects nothing.
	if req.local.addr.scope.is_some() {
		return Err(Error::Invalid);
	}
	let address: service_logic::tcp_bind::Local = match &req.local.addr.addr {
		IpAddress::V4(addr) => service_logic::tcp_bind::Local::V4([addr.a, addr.b, addr.c, addr.d]),
		IpAddress::V6(addr) => service_logic::tcp_bind::Local::V6(addr.octets()),
	};
	let mode = match req.mode {
		BindMode::Ipv4Only => service_logic::tcp_bind::BindMode::Ipv4Only,
		BindMode::Ipv6Only => service_logic::tcp_bind::BindMode::Ipv6Only,
		BindMode::DualStack => service_logic::tcp_bind::BindMode::DualStack,
	};
	let binding = Binding { mode, address, port: req.local.port };
	// THE MATRIX IS ONE FUNCTION AND IT LIVES IN `service-logic`, where a host test can drive every
	// row of it. This is the translation into it, not a second copy of it.
	binding.well_formed().map_err(|_| Error::Invalid)?;
	Ok(binding)
}

// Cut a fetched body into the stream's chunks, with the outcome on the last one.
//
// COMPLETE, BECAUSE THIS PATH READS UNTIL THE PEER CLOSES. `do_tcp` returns success only on an
// orderly end of stream, so a body that got here is a whole body - and a body that reached the
// cumulative cap is TRUNCATED, which is the one case this shape can already tell apart.
fn fetch_stream(mut data: Vec<u8>) -> Vec<FetchChunk> {
	let truncated: bool = data.len() > MAX_FETCH_BODY_BYTES;
	data.truncate(MAX_FETCH_BODY_BYTES);
	let mut chunks: Vec<FetchChunk> = Vec::new();
	for piece in data.chunks(FETCH_CHUNK_BYTES) {
		chunks.push(FetchChunk { data: piece.to_vec(), outcome: None });
	}
	// The terminal element carries the outcome and no body, so a caller reads the ending from the
	// same place whether the body was empty or a quarter of a megabyte.
	chunks.push(FetchChunk { data: Vec::new(), outcome: Some(if truncated { FetchOutcome::Truncated } else { FetchOutcome::Complete }) });
	chunks
}

// The interface snapshot, built from the stack alone.
//
// A FREE FUNCTION AND NOT A METHOD ON THE SERVICE, so the same value can be produced without a
// client asking for it - which is what lets the boot check it rather than waiting for a tool to
// discover it cannot be encoded.
fn snapshot(stack: &Stack) -> NetInfo {
	let scope: InterfaceId = interface_of(stack);
	let mut addresses: Vec<InterfaceAddress> = Vec::new();
	let mut routes: Vec<RouteEntry> = Vec::new();
	let mut routers: Vec<RouterEntry> = Vec::new();
	let mut dns: Vec<DnsServer> = Vec::new();
	let mut neighbors: Vec<Neighbor> = Vec::new();

	// IPv4 first: the address, its prefix, the on-link route it implies, the default route
	// through the gateway, and the resolver DHCP gave us.
	let ip: Ipv4Addr = stack.ip();
	let prefix_len: u8 = stack.mask().0.iter().map(|byte| byte.count_ones()).sum::<u32>() as u8;
	addresses.push(InterfaceAddress { addr: wire_v4(ip), prefix_len, state: AddressState::Preferred, preferred_seconds: INFINITE_LIFETIME, valid_seconds: INFINITE_LIFETIME });
	let network: Ipv4Addr = Ipv4Addr([ip.0[0] & stack.mask().0[0], ip.0[1] & stack.mask().0[1], ip.0[2] & stack.mask().0[2], ip.0[3] & stack.mask().0[3]]);
	routes.push(RouteEntry { destination: wire_v4(network), prefix_len, scope: scope.clone(), preference: RoutePreference::Medium, lifetime_seconds: INFINITE_LIFETIME, hop: NextHop::Direct });
	// A POINT-TO-POINT LINK MAY HAVE NO GATEWAY AT ALL: its default route is the link itself, and there
	// is no router to report.
	if stack.is_raw() && stack.gateway().0 == [0; 4] {
		routes.push(RouteEntry { destination: wire_v4(Ipv4Addr([0, 0, 0, 0])), prefix_len: 0, scope: scope.clone(), preference: RoutePreference::Medium, lifetime_seconds: INFINITE_LIFETIME, hop: NextHop::Direct });
	} else {
		routes.push(RouteEntry { destination: wire_v4(Ipv4Addr([0, 0, 0, 0])), prefix_len: 0, scope: scope.clone(), preference: RoutePreference::Medium, lifetime_seconds: INFINITE_LIFETIME, hop: NextHop::Via(wire_v4(stack.gateway())) });
		routers.push(RouterEntry { addr: wire_v4(stack.gateway()), scope: scope.clone(), preference: RoutePreference::Medium, state: Reachability::Reachable, lifetime_seconds: INFINITE_LIFETIME });
	}
	dns.push(DnsServer { addr: wire_v4(stack.dns()), scope: scope.clone() });
	if let Some(second) = stack.dns_secondary() {
		dns.push(DnsServer { addr: wire_v4(second), scope: scope.clone() });
	}
	let mut i: usize = 0;
	while let Some((nip, nmac)) = stack.neigh_at(i) {
		neighbors.push(Neighbor { addr: wire_v4(nip), mac: WireMac::from_octets(nmac.0), scope: scope.clone() });
		i += 1;
	}

	// Then IPv6, from the tables the layer keeps. A boot whose link refused the family has none
	// of this and the snapshot is simply the IPv4 half.
	if let Some(host) = stack.ipv6_ref() {
		let now: u64 = now_ms();
		for configured in host.configured() {
			addresses.push(InterfaceAddress { addr: wire_v6(configured.address), prefix_len: host.prefix_len(), state: address_state(&configured.state), preferred_seconds: lifetime_seconds(&configured.preferred, now), valid_seconds: lifetime_seconds(&configured.valid, now) });
		}
		for route in host.routes() {
			routes.push(RouteEntry {
				destination: wire_v6(route.destination.base()),
				prefix_len: route.destination.len(),
				scope: scope.clone(),
				preference: RoutePreference::Medium,
				lifetime_seconds: lifetime_seconds(&route.expires, now),
				hop: match route.next_hop {
					Some(next) => NextHop::Via(wire_v6(next)),
					None => NextHop::Direct,
				},
			});
		}
		for router in host.routers() {
			let service_logic::ipv6_router::RouterLifetime::Until(deadline) = router.lifetime;
			routers.push(RouterEntry { addr: wire_v6(router.address), scope: scope.clone(), preference: preference_of(&router.preference), state: reachability_of(host.neighbour_state(router.address)), lifetime_seconds: seconds_until(deadline, now) });
		}
		for server in host.resolvers() {
			dns.push(DnsServer { addr: wire_v6(server), scope: scope.clone() });
		}
	}
	// THE LINK'S OWN NAME: `net0` for the Ethernet NIC, `wwan0` for a modem context, which has no MAC.
	let name: &str = if stack.is_raw() { "wwan0" } else { "net0" };
	NetInfo { scope, name: alloc::string::String::from(name), mac: WireMac::from_octets(stack.mac().0), mtu: stack.mtu(), addresses, routes, routers, dns, neighbors }
}

// A lifetime as the wire carries it: seconds remaining, or infinity.
fn lifetime_seconds(lifetime: &service_logic::ipv6_slaac::Lifetime, now_ms: u64) -> u32 {
	match lifetime {
		service_logic::ipv6_slaac::Lifetime::Infinite => INFINITE_LIFETIME,
		service_logic::ipv6_slaac::Lifetime::Finite(deadline) => seconds_until(*deadline, now_ms),
	}
}

// SECONDS, ROUNDED DOWN, AND NEVER THE INFINITY VALUE BY ACCIDENT. A deadline far enough out to
// exceed `u32` seconds saturates one below infinity rather than becoming it: a lifetime that expires
// in 136 years is not the same statement as one that never expires.
fn seconds_until(deadline_ms: u64, now_ms: u64) -> u32 {
	let remaining: u64 = deadline_ms.saturating_sub(now_ms) / 1000;
	remaining.min(u64::from(INFINITE_LIFETIME) - 1) as u32
}

fn address_state(state: &service_logic::ipv6_slaac::AddressState) -> AddressState {
	match state {
		service_logic::ipv6_slaac::AddressState::Tentative { .. } => AddressState::Tentative,
		service_logic::ipv6_slaac::AddressState::Preferred => AddressState::Preferred,
		service_logic::ipv6_slaac::AddressState::Deprecated => AddressState::Deprecated,
		// A DUPLICATE IS INVALID AND NOT MERELY DEPRECATED. Detection found somebody else holding it,
		// so it is not this host's address at all - a deprecated address still is.
		service_logic::ipv6_slaac::AddressState::Duplicate => AddressState::Invalid,
	}
}

fn preference_of(preference: &service_logic::ipv6_router::Preference) -> RoutePreference {
	match preference {
		service_logic::ipv6_router::Preference::Low => RoutePreference::Low,
		service_logic::ipv6_router::Preference::Medium => RoutePreference::Medium,
		service_logic::ipv6_router::Preference::High => RoutePreference::High,
	}
}

// THE STATE THE CACHE HOLDS, NOT THE CLASS THE ROUTER LIST KEEPS. A report that showed `reachable`
// for every usable router would hide exactly the distinction a reader is looking for - a router in
// `probe` is one this host is about to give up on.
fn reachability_of(state: Option<service_logic::ipv6_neighbour::NeighbourState>) -> Reachability {
	match state {
		Some(service_logic::ipv6_neighbour::NeighbourState::Incomplete) => Reachability::Incomplete,
		Some(service_logic::ipv6_neighbour::NeighbourState::Reachable) => Reachability::Reachable,
		Some(service_logic::ipv6_neighbour::NeighbourState::Stale) => Reachability::Stale,
		Some(service_logic::ipv6_neighbour::NeighbourState::Delay) => Reachability::Delay,
		Some(service_logic::ipv6_neighbour::NeighbourState::Probe) => Reachability::Probe,
		// No entry at all: this host has retired it, or never resolved it. Either way it cannot be
		// reached right now, and that is what the terminal state says.
		None => Reachability::Unreachable,
	}
}

// Map a stack socket snapshot to the typed `sock-info` the `ss` tool renders.
fn to_sock_info(local: Ipv4Addr, s: SockEntry) -> SockInfo {
	let state: SockState = match s.state {
		SockEntryState::Closed => SockState::Closed,
		SockEntryState::SynSent => SockState::SynSent,
		SockEntryState::SynRcvd => SockState::SynRcvd,
		SockEntryState::Established => SockState::Established,
		SockEntryState::FinWait => SockState::FinWait,
		SockEntryState::Listen => SockState::Listen,
	};
	// BOTH ENDPOINTS, because a row that named only a port could not say which of this host's
	// addresses a connection runs from - which is the whole question once there is more than one.
	SockInfo { local: endpoint_v4(local, s.local_port), remote: endpoint_v4(s.remote_ip, s.remote_port), state }
}

impl network::Service for Net<'_> {
	// The interface state: one COMBINED dual-stack snapshot of the single interface this service
	// owns - every address it holds, every route, router and recursive server it knows, and its
	// neighbour cache.
	//
	// THE TWO FAMILIES ARE ONE VIEW, not two reports a caller has to join. The IPv4 configuration is
	// three scalars on the stack and the IPv6 configuration is five tables beside it; presenting them
	// separately would leave every consumer re-deriving "what does this interface actually hold", and
	// the answer would differ between them.
	fn info(&mut self) -> Result<NetInfo, Error> {
		Ok(snapshot(self.stack))
	}

	// The live pool utilization: the client, socket and listener channels the serve loop
	// stands on, and the stack's live TCP connections. Every pool grows on demand (the
	// domain's handle budget is the only ceiling), so these are observability counts -
	// what `ss` prints and the graph folds in - not a fraction of a fixed cap.
	fn capacity(&mut self) -> Result<NetCapacity, Error> {
		// THE CONNECTION COUNT IS WHAT THE ACCOUNTING HOLDS, not what the pool happens to contain: a
		// reserved half-open connection is state this service is carrying and a report that omitted
		// it would understate exactly the thing a flood fills.
		let (occupancy, _refusals) = self.stack.admission();
		// PER-FAMILY READINESS IS WHAT A CALLER ACTS ON. It is recomputed from the tables every time
		// it is asked, rather than latched at boot: a family becomes ready the moment it has an
		// address and a route, and stops being ready if it loses them.
		let (v4, v6) = self.stack.family_state();
		Ok(NetCapacity { clients: self.clients_used, sockets: self.sockets_used, listeners: self.listeners_used, connections: occupancy.control_blocks, ipv4: wire_readiness(readiness(self.families.includes_v4(), v4)), ipv6: wire_readiness(readiness(self.families.includes_v6(), v6)), diagnostic_used: self.pending.used(PendingKind::Diagnostic), diagnostic_limit: service_logic::net_profile::DIAGNOSTIC_CAP, diagnostic_refusals: self.pending.refusals(PendingKind::Diagnostic) })
	}

	// Resolve a name to the ordered candidate list.
	//
	// ONE CANDIDATE TODAY, AND A LIST ON THE WIRE. The resolver asks for an A record and gets one
	// address; the list is what lets a caller hand every candidate to `connect` and let this service
	// try them in order, which is the shape the AAAA query and the selection rules arrive into
	// without another contract change.
	fn resolve(&mut self, name: String) -> Result<Vec<IpAddress>, Error> {
		if self.pending.admit(PendingKind::Dns, self.client).is_err() {
			return Err(Error::Exhausted);
		}
		let outcome = do_dns(name.as_bytes(), self.families, self.frames, self.stack, self.flight, self.rx, self.tx);
		self.pending.release(PendingKind::Dns, self.client);
		match outcome {
			// ORDERED BEFORE IT LEAVES, because every caller of this takes the FIRST one. `ping`,
			// `traceroute` and `resolve_target` all do, and each of them asks one question of one
			// host: handing them the resolver's record order makes the family a property of what the
			// name server happened to answer first rather than of what this host can reach.
			Ok(addresses) => {
				let destinations: Vec<Destination> = addresses
					.iter()
					.map(|address| match address {
						dns::Address::V4(octets) => Destination::V4(Ipv4Addr(*octets)),
						dns::Address::V6(octets) => Destination::V6(*octets),
					})
					.collect();
				Ok(attempt_order_for(&destinations, self.stack)
					.into_iter()
					.map(|index| match destinations[index] {
						Destination::V4(ip) => wire_v4(ip),
						Destination::V6(octets) => IpAddress::V6(WireIpv6::from_octets(octets)),
					})
					.collect())
			}
			// THE CAUSES ARE KEPT APART, because a caller acts differently on each: a name that does
			// not exist is not a server that is broken, and neither is a timeout.
			Err(dns::Refusal::NameError) | Err(dns::Refusal::NoAddress) => Err(Error::NotFound),
			Err(dns::Refusal::ServerFailure) => Err(Error::Io),
			Err(_) => Err(Error::Again),
		}
	}

	// Ping an address: a reply (with its TTL and round-trip time), a timeout, or
	// unreachable (no route / no ARP).
	fn ping(&mut self, addr: ScopedAddress) -> Result<PingReply, Error> {
		// EITHER FAMILY, ONE CONTRACT. A caller asking this service to ping an address must not get
		// a different answer shape depending on which family the address turned out to be.
		let target: Destination = open_destination(&addr)?;
		family_admits(self.families, &target)?;
		// PING AND PROBE SHARE ONE CAP ACROSS BOTH FAMILIES, because they are one mechanism: a
		// per-family partition would let a caller hold twice its share by alternating.
		if self.pending.admit(PendingKind::Diagnostic, self.client).is_err() {
			return Err(Error::Exhausted);
		}
		let (identifier, sequence): (u16, u16) = self.echo.next();
		let (status, ttl, rtt_us): (u8, u8, u32) = match target {
			Destination::V4(ip) => do_ping(ip, self.frames, self.stack, &mut self.seq, self.rx, self.tx),
			Destination::V6(octets) => do_ping6(octets, identifier, sequence, self.frames, self.stack, self.rx, self.tx),
		};
		self.pending.release(PendingKind::Diagnostic, self.client);
		let status: PingStatus = match status {
			1 => PingStatus::Reply,
			2 => PingStatus::Unreachable,
			_ => PingStatus::Timeout,
		};
		Ok(PingReply { status, ttl, rtt_us })
	}

	// One traceroute probe. See the op's own comment in `network.lsidl` for why this is an
	// operation rather than a raw socket handed to a tool.
	fn probe(&mut self, addr: ScopedAddress, ttl: u8) -> Result<TraceHop, Error> {
		let target: Destination = open_destination(&addr)?;
		family_admits(self.families, &target)?;
		// PING AND PROBE SHARE ONE CAP, because they are one mechanism: a caller that could take four
		// of each would hold eight echoes against a partition sized for four.
		if self.pending.admit(PendingKind::Diagnostic, self.client).is_err() {
			return Err(Error::Exhausted);
		}
		let (identifier, sequence): (u16, u16) = self.echo.next();
		let hop: (HopStatus, IpAddress, u32) = match target {
			Destination::V4(ip) => {
				let (status, who, rtt_us) = do_probe(ip, ttl.max(1), self.frames, self.stack, &mut self.seq, self.rx, self.tx);
				(status, wire_v4(who), rtt_us)
			}
			Destination::V6(octets) => {
				let (status, who, rtt_us) = do_probe6(octets, identifier, sequence, ttl.max(1), self.frames, self.stack, self.rx, self.tx);
				// A HOP THAT DID NOT ANSWER HAS NO ADDRESS, and the unspecified address says so
				// rather than naming a router that said nothing.
				(status, IpAddress::V6(WireIpv6::from_octets(who.unwrap_or([0; 16]))), rtt_us)
			}
		};
		self.pending.release(PendingKind::Diagnostic, self.client);
		Ok(TraceHop { status: hop.0, addr: hop.1, rtt_us: hop.2 })
	}

	// A one-shot TCP exchange: connect, send the request, read the response, close.
	// Maps the connect failure modes onto the error enum. The response accumulates
	// in a Vec and rides an exactly-sized reply - its size is bounded by the peer
	// closing, never by a wire constant.
	fn fetch(&mut self, req: TcpRequest) -> Result<Vec<FetchChunk>, Error> {
		// THE OPEN IS GUARDED: everything that can refuse before a body exists refuses here, as the
		// typed error every other operation uses, rather than as an empty stream a caller would have
		// to interpret.
		let order: Vec<Destination> = ordered_destinations(&req.target, self.stack)?;
		family_admits(self.families, &order[0])?;
		let ci: usize = match self.stack.tcp_open_outbound() {
			Some(i) => i,
			None => return Err(Error::Again),
		};
		let mut data: Vec<u8> = Vec::new();
		let named: Option<Destination> = named_source(&req.target)?;
		let status: u8 = do_tcp(ci, &order, req.target.port, named, &req.request, self.frames, self.stack, self.rx, self.tx, &mut data);
		self.stack.tcp_free(ci);
		match status {
			1 => Ok(fetch_stream(data)),
			2 => Err(Error::NotFound),
			3 => Err(Error::Denied),
			_ => Err(Error::Again),
		}
	}

	// Open a TCP connection to `ep` and hand the client the socket as a capability:
	// the client end of a fresh channel on which we serve the `socket` interface. The
	// server end (and its stack connection index) are parked in `new_sock`/`new_sock_ci`
	// for the serve loop to start waiting on. Refused with `Again` when the socket pool
	// or the connection pool is full.
	fn connect(&mut self, target: OpenTarget) -> Result<u64, Error> {
		let order: Vec<Destination> = ordered_destinations(&target, self.stack)?;
		family_admits(self.families, &order[0])?;
		if !self.sock_room {
			return Err(Error::Again);
		}
		let ci: usize = match self.stack.tcp_open_outbound() {
			Some(i) => i,
			None => return Err(Error::Again),
		};
		let named: Option<Destination> = named_source(&target)?;
		match tcp_open_sequence(ci, &order, target.port, named, self.frames, self.stack, self.rx, self.tx) {
			1 => match channel() {
				Some((server, peer)) => {
					*self.new_sock = server;
					*self.new_sock_ci = ci;
					Ok(peer)
				}
				None => {
					self.stack.tcp_free(ci);
					Err(Error::Again)
				}
			},
			2 => {
				self.stack.tcp_free(ci);
				Err(Error::NotFound)
			}
			3 => {
				self.stack.tcp_free(ci);
				Err(Error::Denied)
			}
			_ => {
				self.stack.tcp_free(ci);
				Err(Error::Again)
			}
		}
	}

	// Mint a fresh client channel and hand the caller its client end; the server end
	// is parked in `new_client` for the serve loop to start serving. The shell calls
	// this to give each net tool it spawns its own NetworkService capability rather
	// than sharing one channel (a shared channel would race). Refused with `Again`
	// when the client set is full.
	fn open(&mut self) -> Result<u64, Error> {
		if !self.client_room {
			return Err(Error::Again);
		}
		match channel() {
			Some((server, peer)) => {
				*self.new_client = server;
				Ok(peer)
			}
			None => Err(Error::Again),
		}
	}

	// Open a listening socket on `port` (passive open) and hand the caller a `listener`
	// capability: the client end of a fresh channel on which we serve `accept`. The
	// server end (and its claim) are parked in `new_listener`/`new_listener_binding` for the
	// serve loop to start waiting on, and the stack starts accepting inbound connections
	// on the port. Refused with `Again` when the listener set or the listen table is
	// full.
	fn listen(&mut self, req: ListenRequest) -> Result<ListenResult, Error> {
		let binding: Binding = validate_bind(&req)?;
		if !self.listener_room {
			return Err(Error::Again);
		}
		// ZERO IS A REFUSAL AND ANYTHING ABOVE THE CAP IS CLAMPED. A listener that may hold no
		// unaccepted connection cannot work; a request above the cap succeeds AT the cap, which keeps
		// this service's own bound authoritative instead of making a portable program guess it.
		let effective: u16 = service_logic::tcp_admission::effective_backlog(req.backlog).map_err(|_| Error::Invalid)?;
		// THE MATRIX REFUSES BEFORE ANYTHING IS PUBLISHED. A port already held in a way that overlaps
		// is `denied` rather than `again`: retrying will not make the conflict go away.
		let id: u32 = match self.stack.listen(binding, effective) {
			Ok(id) => id,
			Err(refusal) => {
				return Err(match refusal {
					service_logic::tcp_bind::BindRefusal::InUse => Error::Denied,
					_ => Error::Invalid,
				});
			}
		};
		match channel() {
			Some((server, peer)) => {
				*self.new_listener = server;
				*self.new_listener_binding = binding;
				*self.new_listener_id = id;
				// THE BACKLOG IT ACTUALLY GOT, which is what `listen-result` carries beside the
				// capability so the caller learns the real cap in the same reply.
				Ok(ListenResult { listener: peer, backlog: effective })
			}
			None => {
				self.stack.unlisten(id);
				Err(Error::Again)
			}
		}
	}

	// The live sockets in the stack's table: the listening ports and every open
	// connection, with its local port, remote endpoint, and TCP state - what `ss` lists.
	fn sockets(&mut self) -> Result<Vec<SockInfo>, Error> {
		let local: Ipv4Addr = self.stack.ip();
		Ok(self.stack.sockets().into_iter().map(|entry| to_sock_info(local, entry)).collect())
	}

	// Query an NTP server for the wall-clock time, returning the Unix epoch seconds from
	// its reply. The TimeService combines this with the monotonic clock and the RTC.
	fn sntp(&mut self, server: ScopedAddress) -> Result<u64, Error> {
		let target: Destination = open_destination(&server)?;
		family_admits(self.families, &target)?;
		// RESERVED BEFORE ANYTHING IS SENT, so a caller at the cap is told no before a packet leaves
		// rather than after one has and the reply has nowhere to go.
		if self.pending.admit(PendingKind::Sntp, self.client).is_err() {
			return Err(Error::Exhausted);
		}
		let outcome = do_sntp(target, self.frames, self.stack, self.rx, self.tx);
		self.pending.release(PendingKind::Sntp, self.client);
		match outcome {
			Some(unix) => Ok(unix),
			None => Err(Error::Again),
		}
	}
}

// The state the typed `socket` service operates on for one connected socket: the
// stack connection index `ci` it drives, the frame channel, the stack, and the
// transmit buffer - all borrowed from the serve loop. `closing` is set by `close` so
// the loop tears the connection down after replying.
struct Sock<'a> {
	ci: usize,
	frames: u64,
	stack: &'a mut Stack,
	tx: &'a mut [u8],
	closing: &'a mut bool,
}

impl socket::Service for Sock<'_> {
	// Send the bytes carried by `data` (a zero-copy `buffer`: a handle to a shared
	// memory object the caller filled) as one TCP data segment; we map it, copy the
	// bytes onto the wire, then unmap and close the handle. The ack arrives via the
	// serve loop's frame pump. Closed once the connection is reset or gone.
	fn send(&mut self, data: Buffer) -> Result<u32, Error> {
		if !self.stack.tcp_established(self.ci) || self.stack.tcp_aborted(self.ci) {
			close(data.handle);
			return Err(Error::Closed);
		}
		unsafe {
			let base: u64 = match map_object(data.handle) {
				Some(b) => b,
				None => {
					close(data.handle);
					return Err(Error::Invalid);
				}
			};
			// THE WHOLE BUFFER, NOT ONE SEGMENT'S WORTH. The view used to be cut to what fits a
			// single transmit frame and the caller was told the whole length had been sent, which
			// silently lost everything past the first segment. The queue below segments it.
			let n: usize = data.len as usize;
			let bytes: &[u8] = core::slice::from_raw_parts(base as *const u8, n);
			let accepted: usize = socket_send(self.ci, bytes, self.frames, self.stack, self.tx);
			unmap_object(data.handle);
			close(data.handle);
			// ZERO ACCEPTED IS `Again`, NOT A SUCCESS CARRYING NOTHING. The queue is full and the
			// caller must offer the same bytes again once it drains.
			match accepted {
				0 => Err(Error::Again),
				taken => Ok(taken as u32),
			}
		}
	}

	// The snapshot of bytes already buffered when the recv stream opens; the serve
	// loop streams everything that arrives afterwards. One chunk - as large as the
	// connection's receive buffer holds - or empty; a non-empty drain is followed
	// by a window-update ACK (the buffer just reopened).
	fn recv(&mut self) -> Vec<Chunk> {
		let mut chunks: Vec<Chunk> = Vec::new();
		let data: Vec<u8> = self.stack.tcp_take_rx_all(self.ci);
		if !data.is_empty() {
			let w: usize = self.stack.tcp_build_window_update(self.ci, self.tx);
			send_frame(self.frames, &self.tx[..w]);
			chunks.push(Chunk { data });
		}
		chunks
	}

	// Mark the socket for teardown; the serve loop sends the FIN and drops the channel
	// after this reply.
	fn close(&mut self) -> Result<(), Error> {
		*self.closing = true;
		Ok(())
	}
}

// ARP-resolve `ip` to its MAC, sending a request and pumping received frames if it
// is not already cached. None if it does not answer in time.
fn resolve(ip: Ipv4Addr, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Option<MacAddr> {
	if stack.lookup(ip).is_none() {
		let arp: usize = stack.build_arp_request(ip, tx);
		send_frame(frames, &tx[..arp]);
		let deadline: u64 = clock() + PING_TIMEOUT_TICKS;
		while clock() < deadline && stack.lookup(ip).is_none() {
			if wait_frames(frames, stack, deadline) != 0 {
				break;
			}
			pump(frames, stack, rx, tx);
		}
	}
	stack.lookup(ip)
}

// Send an ICMP echo request to `ip` and wait for the reply, pumping received frames
// as they arrive. Returns (status, ttl, rtt_us): status 1 = reply received (ttl is
// the reply's IP TTL, rtt_us the round-trip time in microseconds), 0 = timed out,
// 2 = unresolved (ttl/rtt 0 in both).
fn do_ping(ip: Ipv4Addr, frames: u64, stack: &mut Stack, seq: &mut u16, rx: &mut [u8], tx: &mut [u8]) -> (u8, u8, u32) {
	let hop: Ipv4Addr = stack.next_hop(ip);
	let mac: MacAddr = match resolve(hop, frames, stack, rx, tx) {
		Some(m) => m,
		None => return (2, 0, 0),
	};
	*seq = seq.wrapping_add(1);
	let sent_seq: u16 = *seq;
	let echo: usize = stack.build_icmp_echo(mac, ip, 1, sent_seq, tx);
	let start: u64 = clock_ns();
	send_frame(frames, &tx[..echo]);
	let deadline: u64 = clock() + PING_TIMEOUT_TICKS;
	while clock() < deadline {
		if wait_frames(frames, stack, deadline) != 0 {
			break;
		}
		if let Event::EchoReply(reply, ttl, rseq) = pump(frames, stack, rx, tx) {
			if reply == ip && rseq == sent_seq {
				let rtt_us: u32 = (clock_ns().saturating_sub(start) / 1000).min(u32::MAX as u64) as u32;
				return (1, ttl, rtt_us);
			}
		}
	}
	(0, 0, 0)
}

// One IPv6 echo, and what answered it.
//
// THE SAME SHAPE AS THE IPv4 DRIVER, deliberately: a caller asking this service to ping something
// should not get a different contract depending on which family the address turned out to be. What
// differs is only where the next hop comes from - the IPv6 host owns its route and its neighbour
// cache, so the echo is handed down and resolution happens on the way out.
fn do_ping6(peer: [u8; 16], identifier: u16, sequence: u16, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> (u8, u8, u32) {
	let destination = service_logic::ipv6::Address::new(peer);
	// A LATE ERROR FROM A RETIRED PROBE IS NOT THIS PROBE'S ANSWER. Clearing before sending is what
	// keeps one trace's rows from inheriting the previous row's router.
	stack.clear_probe_quotes();
	stack.set_clock(now_ms());
	let hops: u8 = match stack.ipv6() {
		Some(host) => host.hop_limit(),
		None => return (2, 0, 0),
	};
	let Some(flow) = probe_flow(stack, peer) else {
		return (2, 0, 0);
	};
	let sent: bool = match stack.ipv6() {
		Some(host) => host.send_echo(destination, identifier, sequence, hops, &[], now_ms()),
		None => false,
	};
	if !sent {
		return (2, 0, 0);
	}
	drain_ipv6(frames, stack);
	let start: u64 = clock_ns();
	let deadline: u64 = clock() + PING_TIMEOUT_TICKS;
	while clock() < deadline {
		if wait_frames(frames, stack, deadline) != 0 {
			break;
		}
		let event = pump(frames, stack, rx, tx);
		let elapsed = || (clock_ns().saturating_sub(start) / 1000).min(u32::MAX as u64) as u32;
		if let Event::EchoReply6(from, hop_limit, rid, rseq) = event
			&& from == peer
			&& rid == identifier
			&& rseq == sequence
		{
			// THE HOP LIMIT THE REPLY ARRIVED WITH, which is the IPv6 field the IPv4 driver reports
			// as the TTL - not this host's own outbound value.
			return (1, hop_limit, elapsed());
		}
		if let Some(quote) = stack.take_probe_quote(&flow, identifier, sequence) {
			// An error about THIS probe. Unreachable is somebody answering to refuse; anything else
			// is not a completed ping.
			return match quote.message_type == service_logic::ipv6_icmp::DESTINATION_UNREACHABLE {
				true => (2, 0, elapsed()),
				false => (0, 0, 0),
			};
		}
	}
	(0, 0, 0)
}

// One IPv6 traceroute probe: an echo with a chosen Hop Limit, and whoever says it expired.
fn do_probe6(peer: [u8; 16], identifier: u16, sequence: u16, hops: u8, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> (HopStatus, Option<[u8; 16]>, u32) {
	let destination = service_logic::ipv6::Address::new(peer);
	stack.clear_probe_quotes();
	stack.set_clock(now_ms());
	let Some(flow) = probe_flow(stack, peer) else {
		return (HopStatus::Unreachable, None, 0);
	};
	let sent: bool = match stack.ipv6() {
		Some(host) => host.send_echo(destination, identifier, sequence, hops.max(1), &[], now_ms()),
		None => false,
	};
	if !sent {
		// This machine has no way to send at all, which is not a hop refusing us.
		return (HopStatus::Unreachable, None, 0);
	}
	drain_ipv6(frames, stack);
	let start: u64 = clock_ns();
	let deadline: u64 = clock() + PING_TIMEOUT_TICKS;
	while clock() < deadline {
		if wait_frames(frames, stack, deadline) != 0 {
			break;
		}
		let event = pump(frames, stack, rx, tx);
		let elapsed = || (clock_ns().saturating_sub(start) / 1000).min(u32::MAX as u64) as u32;
		if let Event::EchoReply6(from, _, rid, rseq) = event
			&& rid == identifier
			&& rseq == sequence
		{
			return (HopStatus::Reply, Some(from), elapsed());
		}
		// THE RESPONDER IS THE HOP THAT COMPLAINED, not the destination this probe named. Reporting
		// the destination is how a trace comes to name the same router for every row.
		if let Some(quote) = stack.take_probe_quote(&flow, identifier, sequence) {
			let service_logic::tcp_bind::Local::V6(responder) = quote.quote.responder else {
				continue;
			};
			return match quote.message_type {
				service_logic::ipv6_icmp::TIME_EXCEEDED => (HopStatus::TimeExceeded, Some(responder), elapsed()),
				service_logic::ipv6_icmp::DESTINATION_UNREACHABLE => (HopStatus::Unreachable, Some(responder), elapsed()),
				_ => continue,
			};
		}
	}
	// A hop that does not report itself is a TIMEOUT and not a failure: the conventional `* * *` row.
	(HopStatus::Timeout, None, 0)
}

// The identity an ICMPv6 error must quote to be about this probe.
fn probe_flow(stack: &Stack, peer: [u8; 16]) -> Option<service_logic::tcp_flow::FlowKey> {
	let host = stack.ipv6_ref()?;
	let source = host.source_address(service_logic::ipv6::Address::new(peer))?;
	Some(service_logic::tcp_flow::FlowKey {
		local: service_logic::tcp_bind::Local::V6(source.octets()),
		// AN ECHO HAS NO PORTS. The identifier and the sequence are its ports, and `probe_owns`
		// checks them separately.
		local_port: 0,
		remote: service_logic::tcp_bind::Local::V6(peer),
		remote_port: 0,
		interface_generation: host.interface().generation,
	})
}

// Send one echo with a chosen TTL and report what answered.
//
// EVERY ANSWER IS MATCHED BY SEQUENCE, and that is not belt-and-braces: several probes may be in
// flight, every router answers from its own address, and an ICMP error quotes the datagram that
// caused it precisely so the quote can be matched. Accepting the first error that arrives would
// attribute one hop's answer to another probe's row, which is a traceroute that draws a plausible
// route that is not the route.
//
// A hop that does not report itself is a TIMEOUT and not a failure: routers are commonly
// configured not to answer, and the conventional `* * *` row says exactly that. It is distinct
// from `unreachable`, which is somebody answering to refuse.
fn do_probe(ip: Ipv4Addr, ttl: u8, frames: u64, stack: &mut Stack, seq: &mut u16, rx: &mut [u8], tx: &mut [u8]) -> (HopStatus, Ipv4Addr, u32) {
	let hop: Ipv4Addr = stack.next_hop(ip);
	let mac: MacAddr = match resolve(hop, frames, stack, rx, tx) {
		Some(m) => m,
		// The FIRST hop cannot be resolved, which is not a hop refusing us - it is this machine
		// having no way to send at all.
		None => return (HopStatus::Unreachable, Ipv4Addr([0, 0, 0, 0]), 0),
	};
	*seq = seq.wrapping_add(1);
	let sent_seq: u16 = *seq;
	let echo: usize = stack.build_icmp_echo_ttl(mac, ip, 1, sent_seq, ttl, tx);
	let start: u64 = clock_ns();
	send_frame(frames, &tx[..echo]);
	let deadline: u64 = clock() + PING_TIMEOUT_TICKS;
	while clock() < deadline {
		if wait_frames(frames, stack, deadline) != 0 {
			break;
		}
		let elapsed = || (clock_ns().saturating_sub(start) / 1000).min(u32::MAX as u64) as u32;
		match pump(frames, stack, rx, tx) {
			Event::EchoReply(reply, _, rseq) if rseq == sent_seq => return (HopStatus::Reply, reply, elapsed()),
			Event::TimeExceeded(router, rseq) if rseq == sent_seq => return (HopStatus::TimeExceeded, router, elapsed()),
			Event::Unreachable(who, rseq) if rseq == sent_seq => return (HopStatus::Unreachable, who, elapsed()),
			_ => {}
		}
	}
	(HopStatus::Timeout, Ipv4Addr([0, 0, 0, 0]), 0)
}

// The held DHCP lease's renewal clock, ticked by the serve loop: at T1 the lease is
// extended with a REQUEST unicast to its server, from T2 by broadcast (any server),
// and at expiry the address is re-acquired from scratch. An unanswered REQUEST is
// retried at half the time remaining to the next threshold (clamped to a sane
// range), the RFC 2131 retransmission pace.
struct LeaseClock {
	phase: LeasePhase,
	// The phase the current transaction identity was drawn for, so a new phase draws a new one.
	identity_phase: LeasePhase,
	// Tick deadlines (the T1 / T2 / expiry thresholds), and the next transmission.
	t1: u64,
	t2: u64,
	expiry: u64,
	retry: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum LeasePhase {
	// No lease clock runs (static config, or a lease with no / infinite duration).
	None,
	// A lease is held; nothing to do until T1.
	Bound,
	// Past T1: extending with the holding server (unicast REQUEST, retried).
	Renewing,
	// Past T2: extending with any server (broadcast REQUEST, retried).
	Rebinding,
	// The lease ran out: re-acquiring from scratch (DISCOVER, retried).
	Expired,
}

impl LeaseClock {
	const fn none() -> LeaseClock {
		LeaseClock { phase: LeasePhase::None, identity_phase: LeasePhase::None, t1: 0, t2: 0, expiry: 0, retry: 0 }
	}

	// The clock for the lease just learned by the stack: thresholds from its T1 /
	// T2 / duration, or an idle clock when there is nothing to renew.
	fn bound(stack: &Stack) -> LeaseClock {
		match stack.dhcp_times() {
			Some((t1, t2, lease)) => {
				let now: u64 = clock();
				LeaseClock { phase: LeasePhase::Bound, identity_phase: LeasePhase::Bound, t1: now + t1 as u64 * TICKS_PER_SEC, t2: now + t2 as u64 * TICKS_PER_SEC, expiry: now + lease as u64 * TICKS_PER_SEC, retry: 0 }
			}
			None => LeaseClock::none(),
		}
	}

	// The next tick the serve loop must wake at, or None when the clock is idle.
	fn next_due(&self) -> Option<u64> {
		match self.phase {
			LeasePhase::None => None,
			LeasePhase::Bound => Some(self.t1),
			LeasePhase::Renewing => Some(self.retry.min(self.t2)),
			LeasePhase::Rebinding => Some(self.retry.min(self.expiry)),
			LeasePhase::Expired => Some(self.retry),
		}
	}

	// The retransmission pace from `now` toward `threshold`: half the remaining
	// time, clamped between the floor and the one-minute cap.
	fn pace(now: u64, threshold: u64) -> u64 {
		(threshold.saturating_sub(now) / 2).clamp(DHCP_RETRY_MIN_TICKS, DHCP_RETRY_MAX_TICKS)
	}

	// A DHCP reply arrived on the standing loop's pump path: an ACK while extending
	// is the server's lease extension - re-apply the configuration and restart the
	// clock; a NAK is a refusal - the address is forfeit, re-acquire from scratch
	// at once. Replies in any other phase belong to no exchange of ours.
	fn on_reply(&mut self, reply: &dhcp::Reply, stack: &mut Stack) {
		// THE PHASE DECIDES WHAT IS ADMISSIBLE, and a renewal is a different conversation from a
		// rebind: the first accepts an answer only from the server that granted the lease, and the
		// second accepts one from any server - which is what that phase exists for.
		let phase = match self.phase {
			LeasePhase::Renewing => dhcp::Phase::Renewing,
			LeasePhase::Rebinding => dhcp::Phase::Rebinding,
			_ => return,
		};
		let (server, address) = stack.lease_identity();
		let txn = dhcp::Transaction { phase, xid: stack.dhcp_xid(), chaddr: stack.mac().0, server: Some(server), requested: Some(address) };
		match txn.admit(reply) {
			dhcp::Admit::CommitLease => {
				stack.commit_lease();
				stack.apply_dhcp();
				*self = LeaseClock::bound(stack);
				print(b"network: DHCP lease renewed\n");
			}
			// AN ADMITTED NAK CLEARS THE LEASE. The server has declared it invalid, so going on using
			// it is the one outcome the staged transaction exists to prevent.
			dhcp::Admit::ClearLease => {
				stack.clear_lease();
				self.phase = LeasePhase::Expired;
				self.retry = clock();
				print(b"network: DHCP lease refused by the server - discarding it\n");
			}
			_ => {}
		}
	}
}

// A lease threshold came due (the serve loop's periodic wake fired): send what the
// phase calls for and advance the clock. Renewal REQUESTs go out here and their
// ACK arrives through the standing loop's pump (`LeaseClock::on_reply`); only a
// full re-acquisition after expiry runs the blocking handshake, since there is no
// held address left to serve with in the meantime.
fn lease_due(frames: u64, stack: &mut Stack, lease: &mut LeaseClock, rx: &mut [u8], tx: &mut [u8]) {
	let now: u64 = clock();
	// Cross into the later phase first, so its transmission form is used at once.
	if lease.phase == LeasePhase::Bound && now >= lease.t1 {
		lease.phase = LeasePhase::Renewing;
	}
	if lease.phase == LeasePhase::Renewing && now >= lease.t2 {
		lease.phase = LeasePhase::Rebinding;
	}
	if lease.phase == LeasePhase::Rebinding && now >= lease.expiry {
		lease.phase = LeasePhase::Expired;
		lease.retry = now;
		print(b"network: DHCP lease expired\n");
	}
	// A FRESH IDENTITY PER EXCHANGE, and a renewal is an exchange. Reusing the identity of the
	// conversation that granted the lease would let a reply to THAT conversation, held or replayed,
	// answer this one.
	if lease.phase != lease.identity_phase {
		stack.set_dhcp_xid(random_u32());
		lease.identity_phase = lease.phase;
	}
	match lease.phase {
		LeasePhase::Renewing => {
			// Unicast to the holding server when its MAC is known (the usual case -
			// the gateway answered ARP long ago); a cache miss falls back to the
			// broadcast form rather than blocking the serve loop on an ARP exchange.
			let server: Ipv4Addr = stack.dhcp_server();
			let unicast: Option<MacAddr> = stack.lookup(stack.next_hop(server));
			let renew: usize = stack.build_dhcp_renew(unicast, tx);
			send_frame(frames, &tx[..renew]);
			lease.retry = now + LeaseClock::pace(now, lease.t2);
		}
		LeasePhase::Rebinding => {
			let renew: usize = stack.build_dhcp_renew(None, tx);
			send_frame(frames, &tx[..renew]);
			lease.retry = now + LeaseClock::pace(now, lease.expiry);
		}
		LeasePhase::Expired => {
			// Re-acquire from scratch. The stale address stays applied while this
			// retries - there is no better configuration to fall back to.
			if do_dhcp(frames, stack, rx, tx) {
				*lease = LeaseClock::bound(stack);
				print(b"network: DHCP lease reacquired\n");
			} else {
				lease.retry = now + DHCP_RETRY_MAX_TICKS;
			}
		}
		_ => {}
	}
}

// Run the DHCP client handshake (DISCOVER -> OFFER -> REQUEST -> ACK), pumping
// received frames for the replies, and on success apply the learned address / mask /
// gateway / DNS to the stack. Returns whether a lease was obtained (false = the
// caller keeps the static configuration).
// `a.b.c.d/prefix via gateway`, built in a fixed buffer because a service has no formatter.
fn push(line: &mut [u8; 64], at: &mut usize, bytes: &[u8]) {
	for byte in bytes {
		if *at < line.len() {
			line[*at] = *byte;
			*at += 1;
		}
	}
}

fn push_decimal(line: &mut [u8; 64], at: &mut usize, value: u8) {
	let mut digits = [0u8; 3];
	let mut count = 0usize;
	let mut v = value;
	loop {
		digits[count] = b'0' + v % 10;
		count += 1;
		v /= 10;
		if v == 0 {
			break;
		}
	}
	while count > 0 {
		count -= 1;
		let digit = digits[count];
		push(line, at, &[digit]);
	}
}

fn push_decimal_u16(line: &mut [u8; 64], at: &mut usize, value: u16) {
	let mut digits = [0u8; 5];
	let mut count = 0usize;
	let mut v = value;
	loop {
		digits[count] = b'0' + (v % 10) as u8;
		count += 1;
		v /= 10;
		if v == 0 {
			break;
		}
	}
	while count > 0 {
		count -= 1;
		let digit = digits[count];
		push(line, at, &[digit]);
	}
}

fn push_ipv4(line: &mut [u8; 64], at: &mut usize, octets: [u8; 4]) {
	for (index, octet) in octets.iter().enumerate() {
		if index > 0 {
			push(line, at, b".");
		}
		push_decimal(line, at, *octet);
	}
}

fn print_address(stack: &Stack) {
	let mut line = [0u8; 64];
	let mut at = 0usize;
	push_ipv4(&mut line, &mut at, stack.ip().0);
	// The mask as a prefix length, which is how a reader thinks of it.
	let prefix: u32 = stack.mask().0.iter().map(|byte| byte.count_ones()).sum();
	push(&mut line, &mut at, b"/");
	push_decimal(&mut line, &mut at, prefix as u8);
	push(&mut line, &mut at, b" via ");
	push_ipv4(&mut line, &mut at, stack.gateway().0);
	// AND THE SIZE THE FRAME BUFFERS WERE CUT TO, which is the link's own report bounded by the
	// `net.mtu` knob. It belongs on this line because it is decided once, at boot, and nothing later
	// changes it - a refused IPv6 family included.
	push(&mut line, &mut at, b" mtu=");
	push_decimal_u16(&mut line, &mut at, stack.mtu());
	print(&line[..at]);
}

fn do_dhcp(frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> bool {
	// A TRANSACTION IDENTITY PER EXCHANGE. The old client used a FIXED one, on the reasoning that
	// "SLIRP is the only DHCP source" - which is a statement about one deployment rather than about
	// the protocol.
	let mut txn: dhcp::Transaction = dhcp::Transaction::new(random_u32(), stack.mac().0);
	txn.phase = dhcp::Phase::Selecting;
	stack.set_dhcp_xid(txn.xid);
	// Broadcast a DISCOVER and wait for the server's OFFER.
	let discover: usize = stack.build_dhcp_discover(tx);
	send_frame(frames, &tx[..discover]);
	let deadline: u64 = clock() + DHCP_TIMEOUT_TICKS;
	while clock() < deadline && txn.phase == dhcp::Phase::Selecting {
		if wait_frames(frames, stack, deadline) != 0 {
			break;
		}
		let Event::DhcpReply(reply) = pump(frames, stack, rx, tx) else {
			continue;
		};
		// ADMITTED FIRST, COMMITTED AFTER. A reply that is not admissible in this phase changes
		// nothing at all, which is the property the old write-then-check order could not have.
		if let dhcp::Admit::SelectOffer = txn.admit(&reply) {
			// THE CLIENT SELECTS ONE OFFER and freezes the server and the address: from here a reply
			// is admissible only if it comes from that server for that address, which `xid` and
			// `chaddr` alone cannot decide because every legitimate server answering the same
			// discover shares both.
			txn.server = reply.server;
			txn.requested = Some(reply.yiaddr);
			txn.phase = dhcp::Phase::Requesting;
			stack.commit_lease();
		}
	}
	if txn.phase != dhcp::Phase::Requesting {
		return false;
	}
	// REQUEST the offered address and wait for the server's ACK.
	let request: usize = stack.build_dhcp_request(tx);
	send_frame(frames, &tx[..request]);
	let deadline: u64 = clock() + DHCP_TIMEOUT_TICKS;
	while clock() < deadline {
		if wait_frames(frames, stack, deadline) != 0 {
			break;
		}
		let Event::DhcpReply(reply) = pump(frames, stack, rx, tx) else {
			continue;
		};
		match txn.admit(&reply) {
			dhcp::Admit::CommitLease => {
				stack.commit_lease();
				stack.apply_dhcp();
				return true;
			}
			// AN ADMISSIBLE NAK IS A STATE TRANSITION, not a non-event: the server has declared this
			// client's request invalid, so the lease and the address go and discovery starts again.
			dhcp::Admit::ClearLease => {
				stack.clear_lease();
				return false;
			}
			_ => {}
		}
	}
	false
}

// Resolve `name` to an IPv4 address via a DNS A-record query to the SLIRP DNS
// server, pumping received frames for the response. None on timeout or failure.
// Resolve `name` and return the addresses in the order this host will try them.
//
// THE IDENTITY IS DRAWN, NOT COUNTED. A fixed source port and a transaction ID incremented by one
// are the whole of what an off-path forgery has to guess, and RFC 5452 section 9.2 asks for both to
// be unpredictable and spread over their ranges. The comparison alone cannot make up for it: a
// forgery carrying the NEXT sequential identity satisfies every field a correlating resolver checks.
//
// AND THE IDENTITY IS RETIRED when the query finishes or expires, so a late answer to a question
// nobody is waiting for matches nothing.
// Resolve a name into every address of every family this boot runs.
//
// BOTH RECORD TYPES, AND A SEPARATE IDENTITY FOR EACH. A resolver that asked only for `A` could
// never reach an IPv6-only host however well the rest of the stack worked, and one that asked for
// both under a single identity would let an answer to the first complete the second.
//
// A FAILURE OF ONE FAMILY IS NOT A FAILURE OF THE NAME. A host with an `A` record and no `AAAA` is
// ordinary; so is the reverse. The name fails only when NEITHER produced an address, and the refusal
// reported is the first query's - which is the one the caller was most likely asking about.
fn do_dns(name: &[u8], families: Families, frames: u64, stack: &mut Stack, flight: &mut dns::InFlight, rx: &mut [u8], tx: &mut [u8]) -> Result<Vec<dns::Address>, dns::Refusal> {
	let normalized = dns::normalize(name);
	let mut addresses: Vec<dns::Address> = Vec::new();
	let mut refusal: Option<dns::Refusal> = None;
	// BOTH RECORD TYPES ARE ASKED FOR, AND THE ORDER THEY ARE ASKED IN IS NOT THE ORDER THEY ARE
	// RETURNED IN. This used to say AAAA came first "because RFC 6724's ordering puts a reachable
	// IPv6 destination ahead of an IPv4 one", which is the claim the module implementing RFC 6724
	// opens by refusing: "IPv6 first" is not an algorithm and has no answer for a host whose only
	// IPv6 source is a unique-local address. The ranking happens in `resolve`, over both families at
	// once, with this host's own sources - which is the only place that knows them.
	let wanted: [(u16, bool); 2] = [(dns::TYPE_AAAA, families.includes_v6()), (dns::TYPE_A, families.includes_v4())];
	for (qtype, included) in wanted {
		if !included {
			continue;
		}
		let question = dns::Question { name: normalized.clone(), qtype, qclass: dns::CLASS_IN };
		let Some(tuple) = flight.draw(|| (random_u16(), random_u16()), DNS_DRAW_ATTEMPTS) else {
			return Err(dns::Refusal::Malformed);
		};
		let outcome: Result<Vec<dns::Address>, dns::Refusal> = query_dns(&question, tuple, frames, stack, rx, tx);
		flight.retire(tuple);
		match outcome {
			Ok(found) => addresses.extend(found),
			Err(cause) => refusal = refusal.or(Some(cause)),
		}
	}
	if addresses.is_empty() {
		return Err(refusal.unwrap_or(dns::Refusal::Malformed));
	}
	Ok(addresses)
}

// Which resolver to ask, in the order to ask it.
//
// THE FAMILY OF THE RESOLVER IS NOT THE FAMILY OF THE QUESTION. An `AAAA` record is perfectly
// answerable over IPv4 and an `A` record over IPv6; what decides is which resolver this host can
// actually reach. A learned RDNSS server comes first when IPv6 is up, because a link that
// advertised one is saying that is the resolver for it.
fn dns_servers(stack: &Stack) -> Vec<Destination> {
	let mut servers: Vec<Destination> = Vec::new();
	if let Some(host) = stack.ipv6_ref() {
		for server in host.resolvers() {
			servers.push(Destination::V6(server.octets()));
		}
	}
	servers.push(Destination::V4(stack.dns()));
	if let Some(second) = stack.dns_secondary() {
		servers.push(Destination::V4(second));
	}
	servers
}

fn query_dns(question: &dns::Question, tuple: dns::Tuple, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Result<Vec<dns::Address>, dns::Refusal> {
	let mut refusal: dns::Refusal = dns::Refusal::Malformed;
	// EACH RESOLVER IN TURN, and a server that does not answer is not the name failing: a link with
	// an advertised resolver and a working IPv4 one has two chances and should use both.
	for server in dns_servers(stack) {
		match query_one_dns(question, tuple, server, frames, stack, rx, tx) {
			Ok(addresses) => return Ok(addresses),
			Err(cause) => refusal = cause,
		}
	}
	Err(refusal)
}

fn query_one_dns(question: &dns::Question, tuple: dns::Tuple, server: Destination, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Result<Vec<dns::Address>, dns::Refusal> {
	let Some(body) = dns::build_query(tuple, question) else {
		return Err(dns::Refusal::Malformed);
	};
	match server {
		Destination::V4(address) => {
			let hop: Ipv4Addr = stack.next_hop(address);
			let Some(mac) = resolve(hop, frames, stack, rx, tx) else {
				return Err(dns::Refusal::Malformed);
			};
			let query: usize = stack.build_dns_query(mac, address, &body, tuple.source_port, tx);
			if query == 0 {
				return Err(dns::Refusal::Malformed);
			}
			send_frame(frames, &tx[..query]);
		}
		// THE IPv6 HOST OWNS ITS OWN ROUTE AND NEIGHBOUR CACHE, so the datagram is handed down and
		// resolution happens on the way out rather than being done again here.
		Destination::V6(octets) => {
			stack.set_clock(now_ms());
			if !stack.send_udp6(octets, tuple.source_port, DNS_PORT, &body) {
				return Err(dns::Refusal::Malformed);
			}
			drain_ipv6(frames, stack);
		}
	}
	let deadline: u64 = clock() + DNS_TIMEOUT_TICKS;
	let mut refusal: dns::Refusal = dns::Refusal::Malformed;
	while clock() < deadline {
		if wait_frames(frames, stack, deadline) != 0 {
			break;
		}
		let Event::DnsReply(payload) = pump(frames, stack, rx, tx) else {
			continue;
		};
		match dns::parse_response(&payload, tuple, question) {
			Ok(answer) => return Ok(answer.addresses),
			// A DATAGRAM THAT IS NOT THIS ANSWER IS NOT AN ANSWER AT ALL: keep waiting rather than
			// reporting somebody else's reply as this query's outcome.
			Err(dns::Refusal::NotOurs) => continue,
			// TRUNCATION IS AN ORDINARY OUTCOME. AAAA and CNAME sets are exactly the answers that
			// overflow a datagram, and RFC 7766 section 5 asks a general-purpose stub resolver to ask
			// again over TCP rather than to report a partial answer.
			Err(dns::Refusal::Truncated) => return query_dns_over_tcp(question, tuple, server, frames, stack, rx, tx),
			Err(other) => {
				refusal = other;
				break;
			}
		}
	}
	Err(refusal)
}

// The same question over TCP, with the two-byte length framing RFC 1035 section 4.2.2 puts in front
// of it.
fn query_dns_over_tcp(question: &dns::Question, tuple: dns::Tuple, server: Destination, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Result<Vec<dns::Address>, dns::Refusal> {
	let Some(body) = dns::build_query(tuple, question) else {
		return Err(dns::Refusal::Malformed);
	};
	let Some(ci) = stack.tcp_open_outbound() else {
		return Err(dns::Refusal::Malformed);
	};
	let mut framed: Vec<u8> = Vec::with_capacity(2 + body.len());
	framed.extend_from_slice(&(body.len() as u16).to_be_bytes());
	framed.extend_from_slice(&body);
	let mut reply: Vec<u8> = Vec::new();
	// An internal query has no caller-named source: this host chooses it.
	// THE RETRY GOES TO THE SAME RESOLVER, not to whichever one is first: truncation is that
	// server's answer, and asking a different one asks a different question.
	let status: u8 = do_tcp(ci, &[server], DNS_PORT, None, &framed, frames, stack, rx, tx, &mut reply);
	stack.tcp_free(ci);
	if status != 1 || reply.len() < 2 {
		return Err(dns::Refusal::Malformed);
	}
	let declared: usize = usize::from(u16::from_be_bytes([reply[0], reply[1]]));
	let Some(message) = reply.get(2..2 + declared) else {
		return Err(dns::Refusal::Malformed);
	};
	dns::parse_response(message, tuple, question).map(|answer| answer.addresses)
}

// Send an SNTP request to `server` and return the Unix epoch seconds from its reply,
// or None on timeout / no route. A one-shot UDP query/response, mirroring do_dns.
fn do_sntp(server: Destination, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Option<u64> {
	// A PER-REQUEST TRANSMIT TIMESTAMP, WHERE THERE WAS A ZERO. The server echoes it in the reply's
	// originate field, so it is the only thing tying a reply to a request - and a constant zero ties
	// it to every request ever made, which is what let one forged datagram set the wall clock.
	let sent: u64 = random_u64() | 1;
	let request: [u8; sntp::MESSAGE_LEN] = sntp::build_request(sent);
	match server {
		Destination::V4(ip) => {
			let hop: Ipv4Addr = stack.next_hop(ip);
			let mac: MacAddr = resolve(hop, frames, stack, rx, tx)?;
			let query: usize = stack.build_sntp_request(mac, ip, NTP_SRC_PORT, &request, tx);
			if query == 0 {
				return None;
			}
			send_frame(frames, &tx[..query]);
		}
		// THE SAME REQUEST OVER THE OTHER FAMILY. The datagram is identical; what differs is the
		// pseudo-header its mandatory checksum covers and who resolves the next hop.
		Destination::V6(octets) => {
			stack.set_clock(now_ms());
			if !stack.send_udp6(octets, NTP_SRC_PORT, NTP_PORT, &request) {
				return None;
			}
			drain_ipv6(frames, stack);
		}
	}
	let deadline: u64 = clock() + NTP_TIMEOUT_TICKS;
	while clock() < deadline {
		if wait_frames(frames, stack, deadline) != 0 {
			break;
		}
		let Event::SntpReply(payload) = pump(frames, stack, rx, tx) else {
			continue;
		};
		// EVERYTHING IS CHECKED BEFORE THE CLOCK IS TOUCHED. A validation that rejects a reply after
		// TimeService has applied it is not one.
		match sntp::parse_reply(&payload, sent, None) {
			Ok(unix) => return Some(unix),
			Err(_) => continue,
		}
	}
	None
}

// Open a TCP connection to `ip`:`port` (next-hop via the gateway when off-link),
// send `request`, read the response, and close. Writes a status byte then the
// received bytes into `reply` (status 1 = connected with data, 0 = no SYN-ACK in
// time, 2 = unreachable / no ARP, 3 = refused / reset) and returns the total length.
#[allow(clippy::too_many_arguments)]
// One-shot TCP exchange driver for `fetch`: establish, send the request, then
// accumulate the response into `reply` until the peer closes or it falls quiet.
// Returns the establish status (1 = ok, 2 = unreachable, 3 = refused, 0 = timeout).
#[allow(clippy::too_many_arguments)]
fn do_tcp(ci: usize, order: &[Destination], port: u16, named: Option<Destination>, request: &[u8], frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8], reply: &mut Vec<u8>) -> u8 {
	// Establish the connection; on failure report the status and stop (the
	// establish status bytes 2 / 3 / 0 map straight onto the fetch errors).
	match tcp_open_sequence(ci, order, port, named, frames, stack, rx, tx) {
		1 => {}
		other => return other,
	}
	// Send the request, then read the response until the peer closes or it
	// falls quiet - the response grows in the Vec, never against a cap.
	if !request.is_empty() {
		// SEGMENTED AND RETAINED, NOT TRUNCATED TO ONE SEGMENT. `tcp_send` copies what it accepts and
		// `drain_tx` cuts it to whatever the congestion window, the peer's window and the path allow.
		let mut offered: &[u8] = request;
		while !offered.is_empty() {
			let accepted: usize = stack.tcp_send(ci, offered, aggregate_room(stack));
			if accepted == 0 {
				break;
			}
			offered = &offered[accepted..];
			drain_tx(frames, ci, stack, tx);
		}
	}
	let recv_deadline: u64 = clock() + TCP_RECV_TIMEOUT_TICKS;
	while clock() < recv_deadline && !stack.tcp_peer_fin(ci) && !stack.tcp_aborted(ci) {
		if wait_frames(frames, stack, recv_deadline) != 0 {
			break;
		}
		pump(frames, stack, rx, tx);
		let data: Vec<u8> = stack.tcp_take_rx_all(ci);
		if !data.is_empty() {
			// the drain reopened the receive window; tell the peer.
			let w: usize = stack.tcp_build_window_update(ci, tx);
			send_frame(frames, &tx[..w]);
		}
		reply.extend_from_slice(&data);
	}
	reply.extend_from_slice(&stack.tcp_take_rx_all(ci));
	// Close our half, and keep pumping until the closing handshake finishes or the sender gives up.
	// THE FIN IS QUEUED LIKE A BYTE and is retransmitted under the same timer, so this loop is
	// waiting for an acknowledgement rather than hoping one arrives inside a fixed window.
	stack.tcp_close_half(ci);
	drain_tx(frames, ci, stack, tx);
	let close_deadline: u64 = clock() + TCP_RETX_TICKS;
	while clock() < close_deadline && !stack.tcp_aborted(ci) && !stack.tcp_send_failed(ci) && !(stack.tcp_fully_acknowledged(ci) && stack.tcp_peer_fin(ci)) {
		if wait_frames(frames, stack, close_deadline) != 0 {
			break;
		}
		pump(frames, stack, rx, tx);
		drain_tx(frames, ci, stack, tx);
	}
	1
}

// What is left of the service-wide unacknowledged-byte budget.
//
// MEASURED ACROSS EVERY CONNECTION, not assumed per connection. The per-flow ceiling alone would let
// sixteen connections hold sixteen times it; this is the number that stops one client's transfers
// from spending the whole service's memory on retransmission queues.
fn aggregate_room(stack: &Stack) -> usize {
	service_logic::tcp_queue::MAX_UNACKED_TOTAL.saturating_sub(stack.tcp_outstanding_total())
}

// Push out everything connection `ci` owes the wire right now.
//
// ONE SEGMENT PER CALL IS THE STACK'S CONTRACT, so this is the loop that turns it into "send what is
// allowed": the queue, the congestion window and the peer's window decide when it stops, and it
// always stops - `tcp_pump` returns nothing the moment any of the three is exhausted.
// Push everything connection `ci` is holding onto the wire.
//
// A LENGTH OF ZERO IS NOT "NOTHING WENT OUT" FOR IPv6. That family's segments are queued on the
// host below rather than written into `tx`, so a loop that stopped at the first zero sent the
// handshake (which another path drains) and then silently sent no data at all: the connection
// established, the caller was told its bytes were accepted, and not one of them left the machine.
fn drain_tx(frames: u64, ci: usize, stack: &mut Stack, tx: &mut [u8]) {
	stack.set_clock(now_ms());
	loop {
		let before: usize = stack.ipv6_ref().map_or(0, |host| host.outbound_len());
		let len: usize = stack.tcp_pump(ci, tx);
		if len > 0 {
			send_frame(frames, &tx[..len]);
			continue;
		}
		let queued: bool = stack.ipv6_ref().map_or(0, |host| host.outbound_len()) > before;
		// Whatever the pump queued goes out here, and the loop asks for the next segment only when
		// this one actually reached the queue.
		drain_ipv6(frames, stack);
		if !queued {
			return;
		}
	}
}

// Establish a TCP connection to `ip`:`port` (next-hop via the gateway when off-link):
// resolve the next hop, open the connection, and send the SYN, retransmitting it
// until the handshake completes. Returns 1 = established, 2 = unreachable (no ARP),
// 3 = refused (reset), 0 = timed out. Shared by `fetch` (do_tcp) and `connect`.
// Try the ordered candidates, one at a time, and report the last one's outcome.
//
// SEQUENTIAL AND NOT A RACE. One active attempt at a time is what makes the budgets mean anything: a
// parallel open would charge every candidate at once and a caller's RPC deadline would become the
// fallback mechanism, which is a fallback nobody wrote down.
//
// A NONFINAL CANDIDATE EXPIRES AFTER THREE SECONDS, including the time spent resolving its next hop,
// and the clock is NOT restarted by a retransmission. The FINAL candidate - a singleton literal
// included - gets the complete SYN schedule instead, because there is nothing left to fall back to
// and abandoning it early would abandon the connection.
// Try the candidates in order, one at a time, until one connects or they are all spent.
//
// THE TRANSITION CORE IS `service-logic`'s AND NOT THIS FUNCTION'S. Which candidate is next, how
// long a nonfinal one gets and whether the final one is capped at all are decisions with host
// fixtures against them; a second copy of those rules here would be a second set of answers.
fn tcp_open_sequence(ci: usize, order: &[Destination], port: u16, named: Option<Destination>, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> u8 {
	use service_logic::open_sequence::{Sequence, Step};
	let mut sequence = Sequence::new(order.len());
	let mut outcome: u8 = 0;
	let mut step: Step = sequence.begin(now_ms());
	loop {
		let Step::Start { index, deadline_ms } = step else {
			return outcome;
		};
		// AN ABSOLUTE DEADLINE, converted once. `ticks_from_ms` takes the millisecond a deadline
		// falls on and answers the tick to wait until, so adding the current tick to its answer
		// pushes the cap a whole uptime into the future and the handover never happens.
		let cap: Option<u64> = deadline_ms.map(ticks_from_ms);
		outcome = tcp_establish_capped(ci, order[index], port, named, cap, frames, stack, rx, tx);
		if outcome == 1 {
			sequence.on_connected(index);
			return 1;
		}
		step = sequence.on_failed(index, now_ms());
		if matches!(step, Step::Start { .. }) {
			// The attempt is retired and its control block reset before the next one starts; nothing
			// of it is carried forward, and a failed candidate is never revisited.
			stack.tcp_reset(ci);
		}
	}
}

#[allow(clippy::too_many_arguments)]
fn tcp_establish_capped(ci: usize, destination: Destination, port: u16, named: Option<Destination>, cap: Option<u64>, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> u8 {
	let iss: u32 = clock() as u32;
	let local_port: u16 = TCP_LOCAL_PORT_BASE | (clock() as u16 & 0x0fff);
	match destination {
		Destination::V4(ip) => {
			// IPv4 RESOLVES ITS OWN NEXT HOP HERE, because the IPv4 stack owns its ARP cache.
			let hop: Ipv4Addr = stack.next_hop(ip);
			let mac: MacAddr = match resolve(hop, frames, stack, rx, tx) {
				Some(m) => m,
				None => return 2,
			};
			stack.tcp_open(ci, ip, port, mac, local_port, iss);
		}
		// IPv6 DOES NOT, because the host below owns the route and the neighbour cache: the segment
		// is handed down and resolution happens when it goes out, which is what keeps an off-link
		// connection following the route rather than a MAC frozen at open time.
		Destination::V6(octets) => {
			let source: Option<[u8; 16]> = match named {
				Some(Destination::V6(from)) => Some(from),
				_ => None,
			};
			if !stack.tcp_open6_from(ci, octets, port, local_port, iss, source) {
				return 2;
			}
		}
	}
	let syn: usize = stack.tcp_build_syn(ci, tx);
	send_frame(frames, &tx[..syn]);
	// THE SYN SCHEDULE IS THE PROFILE'S, NOT A FIXED INTERVAL REPEATED. RFC 9293 section 3.8.3
	// requires the retransmission threshold to permit at least three minutes, and a fixed 500 ms
	// retry inside a short overall deadline abandons a conforming peer behind a slow or lossy path
	// inside a fraction of the interval the base specification reserves for opening a connection.
	// The intervals are derived from the same RTO parameters the data path uses - 1, 2, 4, 8, 16, 32
	// and 60 seconds - so changing the initial RTO or the ceiling changes this too.
	let mut attempt: usize = 0;
	while !stack.tcp_established(ci) && !stack.tcp_aborted(ci) {
		// EXPIRY TAKES PRECEDENCE OVER A RETRANSMISSION DUE AT THE SAME INSTANT: at the cap the
		// service retires this candidate and starts the next rather than sending another SYN.
		if cap.is_some_and(|deadline| clock() >= deadline) {
			return 0;
		}
		let Some(interval) = service_logic::tcp_rto::syn_interval(attempt) else {
			// Every retransmission has been sent and the last interval has elapsed: the open fails
			// with a typed timeout rather than retrying for ever.
			break;
		};
		let mut until: u64 = ticks_from_ms(now_ms() + u64::from(interval));
		if let Some(deadline) = cap {
			until = until.min(deadline);
		}
		if wait_frames(frames, stack, until) != 0 {
			let s: usize = stack.tcp_build_syn(ci, tx);
			send_frame(frames, &tx[..s]);
			attempt += 1;
		} else {
			pump(frames, stack, rx, tx);
		}
	}
	// The wait on the LAST interval, which is what makes the schedule span its full duration rather
	// than ending the moment the last retransmission goes out.
	if !stack.tcp_established(ci) && !stack.tcp_aborted(ci) && cap.is_none() {
		let last: u32 = service_logic::tcp_rto::syn_interval(service_logic::tcp_rto::syn_attempts() - 1).unwrap_or(1000);
		let until: u64 = ticks_from_ms(now_ms() + u64::from(last));
		while clock() < until && !stack.tcp_established(ci) && !stack.tcp_aborted(ci) {
			if wait_frames(frames, stack, until) != 0 {
				break;
			}
			pump(frames, stack, rx, tx);
		}
	}
	if stack.tcp_aborted(ci) {
		return 3;
	}
	if !stack.tcp_established(ci) {
		return 0;
	}
	1
}

// Hand `data` to connection `ci`'s transmit queue and push out what the windows allow.
//
// RETURNS BYTES ACCEPTED, which is the contract `socket.send` reports: they are copied into the
// queue, so the caller may reuse its buffer at once, and fewer than offered is backpressure rather
// than an error. Acknowledgement arrives later through the serve loop's frame pump.
fn socket_send(ci: usize, data: &[u8], frames: u64, stack: &mut Stack, tx: &mut [u8]) -> usize {
	let accepted: usize = stack.tcp_send(ci, data, aggregate_room(stack));
	if accepted > 0 {
		drain_tx(frames, ci, stack, tx);
	}
	accepted
}

// Drain newly received bytes from the connection and frame each chunk onto the recv
// stream `producer` - one chunk per drain, as large as the connection's receive
// buffer held; each drained chunk is followed by a window-update ACK to the peer
// (the drain reopened the receive window). Returns the producer handle, or 0 if
// the consumer was dropped (in which case the producer is closed here).
fn stream_pump(ci: usize, frames: u64, stack: &mut Stack, tx: &mut [u8], producer: u64, seq: &mut u32) -> u64 {
	loop {
		let data: Vec<u8> = stack.tcp_take_rx_all(ci);
		if data.is_empty() {
			return producer;
		}
		let w: usize = stack.tcp_build_window_update(ci, tx);
		send_frame(frames, &tx[..w]);
		let chunk: Chunk = Chunk { data };
		// the frame grows with the chunk: encoded exactly, sent as one message
		// (the consumer receives it exactly-sized via the peek).
		let mut frame: Vec<u8> = alloc::vec![0u8; 8 + chunk.data.len() + 16];
		let mut frame_handles = Handles::new();
		match socket::recv_frame(*seq, &chunk, &mut frame, &mut frame_handles) {
			Some(fl) => {
				if !send_caps_blocking(producer, &frame[..fl], frame_handles.as_slice()) {
					for handle in frame_handles.as_slice() {
						close(*handle);
					}
					close(producer);
					return 0;
				}
				*seq += 1;
			}
			None => {
				for handle in frame_handles.as_slice() {
					close(*handle);
				}
				return producer;
			}
		}
	}
}

// Close our half of connection `ci`: send a FIN (unless already reset) and briefly
// pump to acknowledge the peer's FIN, so the connection winds down before its slot is
// freed.
fn socket_teardown(ci: usize, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) {
	if !stack.tcp_aborted(ci) {
		stack.tcp_close_half(ci);
		drain_tx(frames, ci, stack, tx);
	}
	let deadline: u64 = clock() + TCP_RETX_TICKS;
	while clock() < deadline && !stack.tcp_aborted(ci) && !stack.tcp_send_failed(ci) && !(stack.tcp_fully_acknowledged(ci) && stack.tcp_peer_fin(ci)) {
		if wait_frames(frames, stack, deadline) != 0 {
			break;
		}
		pump(frames, stack, rx, tx);
		drain_tx(frames, ci, stack, tx);
	}
}

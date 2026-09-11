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

use crate::net::{DHCP_ACK, DHCP_NAK, DHCP_OFFER, Event, IP_PROTO_TCP, IP_PROTO_UDP, Ipv4Addr, MacAddr, NEIGH_MAX, NTP_PORT, SockEntry, SockEntryState, Stack};
use proto::codec::{Buffer, Handles};
use proto::system::{AcceptResult, AddressState, BindMode, Chunk, DnsServer, Error, FetchChunk, FetchOutcome, HopStatus, InterfaceAddress, InterfaceId, IpAddress, Ipv4Addr as WireIp, Ipv6Addr as WireIpv6, ListenRequest, ListenResult, MacAddr as WireMac, Neighbor, NetCapacity, NetInfo, NextHop, OpenTarget, PingReply, PingStatus, ProviderKind, Reachability, RouteEntry, RoutePreference, RouterEntry, ScopedAddress, ScopedEndpoint, SockInfo, SockState, TcpRequest, TraceHop, config, listener, network, provider_catalogue, socket};
use service_logic::tcp_bind::Binding;

// Static addressing for the QEMU user-mode (SLIRP) network: the guest is
// 10.0.2.15/24, the gateway/host is 10.0.2.2, and the DNS relay is 10.0.2.3. A DHCP
// client later replaces this static configuration.
const OUR_IP: Ipv4Addr = Ipv4Addr([10, 0, 2, 15]);
const OUR_MASK: Ipv4Addr = Ipv4Addr([255, 255, 255, 0]);
const GATEWAY_IP: Ipv4Addr = Ipv4Addr([10, 0, 2, 2]);
const DNS_SERVER: Ipv4Addr = Ipv4Addr([10, 0, 2, 3]);
// The UDP source port we send DNS queries from.
const DNS_SRC_PORT: u16 = 0x9876;
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
fn net_policy(config: u64) -> (usize, usize, Option<alloc::string::String>) {
	if config == 0 {
		return (NEIGH_MAX, DEFAULT_MTU, None);
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
	close(config);
	(neigh, mtu, icmpv6_rate)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	// 1. receive the driver's frame channel (we move frames over it), the
	//    ConfigService client the supervisor minted for us (handle 0 when no
	//    config tree serves this boot - a test scenario), and the client channel
	//    the shell reaches us on (the `ip` / `ping` / `nslookup` control
	//    protocol).
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
	// LAST IN THE ROLE LIST, because the bootstrap is read POSITIONALLY at every hop.
	let catalogue: u64 = recv_tagged(bootstrap, &mut buf, b"CATALOGUE").unwrap_or(0);
	// THE SNAPSHOT IS ALREADY IN THE CHANNEL when `subscribe` answers - the catalogue registers a
	// subscriber and sends it everything published in one step - so this is a poll and never a
	// block. A machine with no NIC published has none, which is the same state a zero `FRAMES`
	// handle used to be - and is served without a link below rather than refused.
	let frames: u64 = take_published_nic(catalogue);
	if frames == 0 {
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
		// asks for one, and holding no stack, no lease and no frame buffers.
		//
		// Said on the console in the same breath as the DHCP report would have been, so a
		// reader of the boot log sees WHY there is no address rather than a service that went
		// quiet.
		print(b"network: no network provider on this boot - NetworkService is up without a link\n");
		send_blocking(bootstrap, b"NetworkService: online", 0);
		serve_unlinked(client);
	}
	// 2. the frame-mover driver leads with our NIC's MAC and the link's MTU over
	//    the frame channel (it owns the device; we own the protocol), so we can
	//    build the stack - its neighbor-cache sized by the config tree's
	//    `net.arp-cache` policy, its MTU the smaller of the link's report and the
	//    `net.mtu` knob.
	let (mac, link_mtu): (MacAddr, usize) = match recv_blocking(frames, &mut buf) {
		Received::Message { len, .. } if len >= 9 && &buf[..3] == b"MAC" => {
			let link: usize = if len >= 11 { u16::from_le_bytes([buf[9], buf[10]]) as usize } else { DEFAULT_MTU };
			(MacAddr([buf[3], buf[4], buf[5], buf[6], buf[7], buf[8]]), if link == 0 { DEFAULT_MTU } else { link })
		}
		_ => fail_bootstrap(bootstrap, b"driver", b"NIC did not report its MAC"),
	};
	let (neigh_cap, mtu_knob, icmpv6_rate): (usize, usize, Option<alloc::string::String>) = net_policy(config);
	let mtu: usize = service_logic::ipv6_packet::effective_link_mtu(mtu_knob as u16, link_mtu as u16) as usize;
	let frame_max: usize = mtu + 14;
	let mut stack: Stack = Stack::new(mac, OUR_IP, OUR_MASK, GATEWAY_IP, DNS_SERVER, neigh_cap, mtu as u16);
	// THE IPv6 HOST BESIDE IT, on the same link and the same MAC, with its own state and its own
	// deadline. It shares nothing with the IPv4 stack, so a link with no IPv6 router behaves exactly
	// as it did before: the host keeps soliciting at a widening interval and costs one packet.
	let mut host: ipv6_host::Ipv6Host = ipv6_host::Ipv6Host::new(mac.0, 0, 1, mtu as u16, ipv6_entropy, now_ms);
	host.set_error_rate(icmpv6_rate.as_deref());
	// A LINK THAT CANNOT CARRY IPv6 LEAVES IT REFUSED, and says so once rather than silently.
	if !host.bring_up(now_ms()) {
		print(b"ipv6: refused on this link - its effective MTU is below the 1280 bytes IPv6 requires; IPv4 is unaffected\n");
	}
	stack.attach_ipv6(host);
	// AT ONCE, NOT BEHIND DHCP. The listener report has to precede the detection probe on the WIRE,
	// and both have to precede anything that uses the address; leaving them in the queue until the
	// first idle moment of the serve loop put them fifteen seconds late on a link with no DHCP
	// server, which is the one case where they matter most.
	drain_ipv6(frames, &mut stack);
	// 3. learn our address / mask / gateway / DNS from DHCP, falling back to the
	//    static config above if no server answers. The frame buffers are heap Vecs
	//    scoped so they are freed before serve allocates its own.
	let mut lease: LeaseClock = LeaseClock::none();
	{
		let mut drx: Vec<u8> = alloc::vec![0u8; frame_max];
		let mut dtx: Vec<u8> = alloc::vec![0u8; frame_max];
		// WHAT IT GOT, NOT ONLY THAT IT GOT SOMETHING. Every other line in the boot report
		// carries its own fact - the frame count, the core count, the volume name, the release -
		// and this one said a transaction had completed and left the reader to go and look.
		if do_dhcp(frames, &mut stack, &mut drx, &mut dtx) {
			print(b"network: configured via DHCP - ");
			print_address(&stack);
			print(b"\n");
			lease = LeaseClock::bound(&stack);
		} else {
			print(b"network: DHCP unanswered, using static config - ");
			print_address(&stack);
			print(b"\n");
		}
	}
	// 4. report in, then serve the network and the client at once (serve announces
	//    us on the link with a gratuitous ARP first).
	send_blocking(bootstrap, b"NetworkService: online", 0);
	serve(frames, client, &mut stack, lease, frame_max);
}

// Send a built frame to the driver to transmit. A zero-length frame (the stack
// produced no reply) sends nothing.
// THE FIRST LIVE NETWORK PROVIDER THE SUBSCRIPTION HAS ALREADY QUEUED, connected to.
//
// POLLED, NEVER BLOCKED, and it stops at the first provider it CONNECTS to rather than draining the
// channel: a frame this function reads and drops is a publication nothing will see again. Zero for a
// boot that granted no catalogue connection or a machine with no NIC, which the caller serves
// without a link.
fn take_published_nic(catalogue: u64) -> u64 {
	if catalogue == 0 {
		print(b"NetworkService: no provider catalogue - this instance has no way to find a NIC\n");
		return 0;
	}
	let providers: u64 = match provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Net) {
		Some(subscription) => subscription,
		None => {
			print(b"NetworkService: the catalogue refused a network subscription\n");
			return 0;
		}
	};
	let mut buf: [u8; 256] = [0; 256];
	let mut opened: u64 = 0;
	loop {
		let PolledCaps::Message { len, handles } = try_recv_caps(providers, &mut buf) else { break };
		for &handle in handles.as_slice() {
			close(handle);
		}
		let mut frame_handles = wire::Handles::new();
		let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else {
			print(b"NetworkService: a provider frame did not decode\n");
			continue;
		};
		if !info.live {
			continue;
		}
		match provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) {
			Some(Ok(handle)) => {
				opened = handle;
				break;
			}
			Some(Err(_)) => print(b"NetworkService: the catalogue refused a connection to the network provider it published\n"),
			None => print(b"NetworkService: the catalogue did not answer the connection it published\n"),
		}
	}
	// THE SUBSCRIPTION IS GIVEN BACK. This service takes one NIC at bootstrap and does not follow
	// a replacement - its whole stack is built on the link it opened - so holding the stream open
	// would be a handle nothing reads and a slot the catalogue could not give to a consumer that
	// does follow one.
	close(providers);
	opened
}

fn send_frame(frames: u64, frame: &[u8]) {
	if !frame.is_empty() {
		send_blocking(frames, frame, 0);
	}
}

// Receive one frame from the driver, run it through the stack, send any reply frame
// back to the driver, and return the stack event it produced (an echo / DNS reply
// an in-flight `ping` / `nslookup` is waiting for, or `None`). The frame channel
// closing means the driver is gone - there is no network left to serve, and a wait
// on the closed channel would be forever-ready, so the service exits instead of
// spinning on it.
fn pump(frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Event {
	match recv_blocking(frames, rx) {
		Received::Message { len, .. } => {
			// AN IPv6 FRAME GOES TO THE IPv6 HOST AND NOWHERE ELSE. The two stacks share the link
			// and nothing else, so neither can be made to misbehave by the other's traffic.
			if len > 13 && u16::from_be_bytes([rx[12], rx[13]]) == 0x86dd {
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
		Received::Closed => exit(),
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
	let mut line = alloc::format!("ipv6: {where_it_is}, addresses={}, routers={}, prefixes={}, routes={}, resolvers={}, mtu={}, events={}, errors={}, errors-seen={seen}, errors-dropped={unattributable}, dropped={}", host.addresses().len(), host.routers().len(), host.prefixes(), host.routes().len(), host.resolvers().len(), host.mtu(), invalidations.len(), errors.len(), host.outbound_dropped(),);
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
			_ => {}
		}
	}
	// The replies an IPv6 segment provokes are queued on the host, not written here, so they leave
	// with everything else it has pending.
	drain_ipv6(frames, stack);
	pending
}

// Send everything the IPv6 host has queued.
fn drain_ipv6(frames: u64, stack: &mut Stack) {
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

// THE SERVICE WITHOUT A LINK. Stand on every client channel and nothing else: there is no frame
// channel to pump, no socket and no listener can exist, and every operation that would need the
// NIC answers `Io` - the device this service moves frames through is not there. What does work is
// exactly what does not need a link: the reserved connect request and `open` mint fresh
// connections, so PermissionManager can still grant the network capability and a tool launched
// under it gets a typed refusal rather than a hang; `capacity` counts the clients; `sockets` is
// empty. The service ends when its last client is gone, which is what `serve_multi` does for every
// other service whose serve root closes.
fn serve_unlinked(client: u64) -> ! {
	// ON THE HEAP, LIKE EVERY OTHER BUFFER HERE. The request buffer grew to 8192 bytes with the
	// open-target it now has to hold, and the user stack is 16 kB with a deep connect handshake
	// nested on top of this frame - an array that size is half the stack before the first call.
	let mut req: Vec<u8> = alloc::vec![0u8; REQ_MAX];
	let mut out: Vec<u8> = alloc::vec![0u8; REPLY_MAX];
	let mut clients: Vec<u64> = Vec::with_capacity(MAX_CLIENTS);
	clients.push(client);
	loop {
		let mut waits: Vec<u64> = Vec::with_capacity(clients.len());
		let mut slot_of: Vec<usize> = Vec::with_capacity(clients.len());
		let mut i: usize = 0;
		while i < clients.len() {
			if clients[i] != 0 {
				waits.push(clients[i]);
				slot_of.push(i);
			}
			i += 1;
		}
		if waits.is_empty() {
			exit();
		}
		let ready_raw: i64 = wait_any(&waits, 0);
		if ready_raw < 0 {
			continue;
		}
		let slot: usize = slot_of[ready_raw as usize];
		let chan: u64 = clients[slot];
		match recv_caps_blocking(chan, &mut req) {
			ReceivedCaps::Message { len, handles: caps } => {
				// A FRESH CONNECTION PER CALLER, answered by hand for the same reason the
				// linked loop answers it by hand: this service does not stand on `serve_multi`.
				if len >= 2 && u16::from_le_bytes([req[0], req[1]]) == CONNECT_OP {
					for &unclaimed in caps.as_slice() {
						close(unclaimed);
					}
					match channel() {
						Some((mine, theirs)) => {
							place_client(&mut clients, mine);
							send_blocking(chan, &[], theirs);
						}
						None => {
							send_blocking(chan, &[], 0);
						}
					}
					continue;
				}
				let mut handle = caps;
				let mut new_client: u64 = 0;
				let clients_used: u32 = clients.iter().filter(|&&c| c != 0).count() as u32;
				{
					let mut svc: Unlinked = Unlinked { new_client: &mut new_client, clients_used };
					let mut reply_handle = proto::codec::Handles::new();
					if let Some(n2) = network::dispatch(&mut svc, &req[..len], &mut handle, &mut out, &mut reply_handle) {
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
				if new_client != 0 {
					place_client(&mut clients, new_client);
				}
			}
			ReceivedCaps::Closed => {
				close(chan);
				clients[slot] = 0;
			}
		}
	}
}

// The typed `network` service with no link behind it - see `serve_unlinked`.
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
		Ok(NetCapacity { clients: self.clients_used, sockets: 0, listeners: 0, connections: 0 })
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

// Stand on the driver's frame channel, every client's typed request channel, and
// the active socket's channel at once (wait_any): a frame from the driver is parsed
// (answering ARP / ICMP, any reply sent back to transmit); a client request is
// decoded, dispatched to the generated `network` server, and answered; a socket
// request (send / recv / close) is dispatched to the `socket` server for the one
// connection `network.connect` opened. `network.open` mints a fresh client channel
// (one per spawned net tool) added to the client set; a client channel closing
// drops it from the set, the socket channel closing tears the connection down. The
// gratuitous ARP that announces us on the link goes out first. While a DHCP lease
// is held, its clock arms the wait's deadline (a periodic housekeeping wake) and
// `lease_due` extends the lease when a threshold comes due.
fn serve(frames: u64, client: u64, stack: &mut Stack, mut lease: LeaseClock, frame_max: usize) -> ! {
	// The frame and reply buffers live on the heap, not in this function's frame:
	// serve holds all of them for its whole lifetime and the connect handshake
	// nests a deep call chain on top, which would overflow the 16 kB user stack.
	let mut rx: Vec<u8> = alloc::vec![0u8; frame_max];
	let mut tx: Vec<u8> = alloc::vec![0u8; frame_max];
	// ON THE HEAP, LIKE EVERY OTHER BUFFER HERE. The request buffer grew to 8192 bytes with the
	// open-target it now has to hold, and the user stack is 16 kB with a deep connect handshake
	// nested on top of this frame - an array that size is half the stack before the first call.
	let mut req: Vec<u8> = alloc::vec![0u8; REQ_MAX];
	let mut out: Vec<u8> = alloc::vec![0u8; REPLY_MAX];
	let arp: usize = stack.build_arp_request(GATEWAY_IP, &mut tx);
	send_frame(frames, &tx[..arp]);
	// The client channels we serve the `network` interface on: the shell's
	// (clients[0]) plus any minted by `network.open` for a spawned net tool.
	// Each set below reuses free slots and grows on demand - never a fixed cap.
	let mut clients: Vec<u64> = Vec::with_capacity(MAX_CLIENTS);
	clients.push(client);
	// The active sockets (chan 0 = empty slot). Each is handed out by `network.connect`
	// (later `listener.accept`): its channel, the stack connection index it drives, and
	// its received-data stream producer (0 = none) plus that stream's frame sequence.
	// The serve loop waits on every active socket channel at once.
	let mut socks: Vec<SockSlot> = Vec::with_capacity(MAX_SOCKS);
	// The active listeners (chan 0 = empty), each from `network.listen`: its channel,
	// the port it accepts on, and a deferred `accept` (the correlation id to answer
	// once an inbound connection completes, if accept was called with none pending).
	let mut listeners: Vec<Listener> = Vec::with_capacity(MAX_LISTEN);
	loop {
		// Build the wait set: the driver frame channel always (index 0), every active
		// client channel, then every active socket channel, then every listener. `kind`
		// tags each wait index (0 = client, 1 = socket, 2 = listener) and `slot_of` maps
		// it back to its slot.
		let capacity: usize = 1 + clients.len() + socks.len() + listeners.len();
		let mut waits: Vec<u64> = Vec::with_capacity(capacity);
		let mut kind: Vec<u8> = Vec::with_capacity(capacity);
		let mut slot_of: Vec<usize> = Vec::with_capacity(capacity);
		waits.push(frames);
		kind.push(0);
		slot_of.push(usize::MAX);
		let mut i: usize = 0;
		while i < clients.len() {
			if clients[i] != 0 {
				waits.push(clients[i]);
				kind.push(0);
				slot_of.push(i);
			}
			i += 1;
		}
		let mut i: usize = 0;
		while i < socks.len() {
			if socks[i].chan != 0 {
				waits.push(socks[i].chan);
				kind.push(1);
				slot_of.push(i);
			}
			i += 1;
		}
		let mut i: usize = 0;
		while i < listeners.len() {
			if listeners[i].chan != 0 {
				waits.push(listeners[i].chan);
				kind.push(2);
				slot_of.push(i);
			}
			i += 1;
		}
		let n: usize = waits.len();
		// While a lease clock runs, its next threshold bounds the wait as a periodic
		// housekeeping wake; without one the wait has no deadline at all.
		// ONE AGGREGATED DEADLINE. Whatever this loop is blocked on, it must not block past the next
		// thing that has to happen - and after the lease clock, that is the IPv6 layer's own next
		// timer: a detection probe, a router solicitation, a neighbour retry or a listener report.
		// Without this, a blocking client request starves every one of them.
		let ipv6_due: Option<u64> = stack.ipv6_deadline().map(ticks_from_ms);
		let next: Option<u64> = match (lease.next_due(), ipv6_due) {
			(Some(left), Some(right)) => Some(left.min(right)),
			(left, right) => left.or(right),
		};
		let ready_raw: i64 = match next {
			Some(deadline) => wait_any_periodic(&waits[..n], deadline),
			None => wait_any(&waits[..n], 0),
		};
		if ready_raw == ERR_TIMED_OUT {
			if let Some(host) = stack.ipv6() {
				host.on_timer(now_ms());
			}
			drain_ipv6(frames, stack);
			report_ipv6(stack);
			lease_due(frames, stack, &mut lease, &mut rx, &mut tx);
			continue;
		}
		let ready: usize = ready_raw as usize;
		if ready == 0 {
			if let Event::DhcpReply(msg_type) = pump(frames, stack, &mut rx, &mut tx) {
				lease.on_reply(msg_type, stack);
			}
			// Feed any newly received bytes to each active recv stream, closing the
			// producer (end of stream) once that connection's peer closes or resets.
			let mut si: usize = 0;
			while si < socks.len() {
				if socks[si].chan != 0 && socks[si].stream_prod != 0 {
					let ci: usize = socks[si].ci;
					let prod: u64 = stream_pump(ci, frames, stack, &mut tx, socks[si].stream_prod, &mut socks[si].stream_seq);
					socks[si].stream_prod = prod;
					if prod != 0 && (stack.tcp_peer_fin(ci) || stack.tcp_aborted(ci)) {
						close(prod);
						socks[si].stream_prod = 0;
					}
				}
				si += 1;
			}
			// Answer any deferred `accept`: a frame may have completed an inbound
			// handshake, so a listener that was waiting for a connection gets one now.
			let mut li: usize = 0;
			while li < listeners.len() {
				if listeners[li].chan != 0 && listeners[li].pending {
					if let Some(ci) = stack.take_accepted(listeners[li].binding.port) {
						if accept_handoff(listeners[li].chan, listeners[li].pending_corr, ci, &mut socks, stack) {
							listeners[li].pending = false;
						}
					}
				}
				li += 1;
			}
		} else if kind[ready] == 1 {
			serve_socket(&mut socks[slot_of[ready]], frames, stack, &mut rx, &mut tx, &mut out, &mut req);
		} else if kind[ready] == 2 {
			serve_listener(&mut listeners[slot_of[ready]], &mut socks, stack, &mut req);
		} else {
			// A client request on clients[slot_of[ready]]: dispatch the `network`
			// interface. `open` may mint another client channel, `connect` may open a
			// socket, `listen` a listener; a closed channel is dropped from the set.
			let slot: usize = slot_of[ready];
			let chan: u64 = clients[slot];
			match recv_caps_blocking(chan, &mut req) {
				ReceivedCaps::Message { len, handles: caps } => {
					// A FRESH CONNECTION PER CALLER, answered here because it cannot be
					// answered generically.
					//
					// This service is not built on `serve_multi` - it stands on the driver's
					// frame channel, every client, every socket and every listener at once -
					// so the reserved connect request has to be handled by hand, as
					// InputService, DisplayService and the audio engine already do. Without
					// it `service_connect` waits forever against this service and no other,
					// and a `factory` role in the bootstrap plan would mean one thing here
					// and another everywhere else.
					if len >= 2 && u16::from_le_bytes([req[0], req[1]]) == CONNECT_OP {
						for &unclaimed in caps.as_slice() {
							close(unclaimed);
						}
						match channel() {
							Some((mine, theirs)) => {
								place_client(&mut clients, mine);
								send_blocking(chan, &[], theirs);
							}
							// Refused by replying with no capability: a caller that gets none
							// knows it has no connection, which is better than a channel
							// nobody is waiting on.
							None => {
								send_blocking(chan, &[], 0);
							}
						}
						continue;
					}
					// EVERY CAPABILITY THE MESSAGE CARRIED. This was `Handles::from_slice(&[handle])`
					// over the single-handle receive, which keeps the first and drops the rest - so a
					// client sending stdin, stdout and stderr had two destroyed before dispatch.
					let mut handle = caps;
					let mut new_sock: u64 = 0;
					let mut new_sock_ci: usize = 0;
					let mut new_client: u64 = 0;
					let mut new_listener: u64 = 0;
					let mut new_listener_binding: Binding = Binding { mode: service_logic::tcp_bind::BindMode::Ipv4Only, address: service_logic::tcp_bind::Local::V4([0; 4]), port: 0 };
					// every set grows on demand, so there is always room.
					let client_room: bool = true;
					let sock_room: bool = true;
					let listener_room: bool = true;
					// the live pool utilization, for the `capacity` reply (observability).
					let clients_used: u32 = clients.iter().filter(|&&c| c != 0).count() as u32;
					let sockets_used: u32 = socks.iter().filter(|s| s.chan != 0).count() as u32;
					let listeners_used: u32 = listeners.iter().filter(|l| l.chan != 0).count() as u32;
					{
						let mut svc: Net = Net { frames, seq: 0, stack: &mut *stack, rx: &mut rx[..], tx: &mut tx[..], new_sock: &mut new_sock, new_sock_ci: &mut new_sock_ci, new_client: &mut new_client, new_listener: &mut new_listener, new_listener_binding: &mut new_listener_binding, sock_room, client_room, listener_room, clients_used, sockets_used, listeners_used };
						let mut reply_handle = proto::codec::Handles::new();
						if let Some(n2) = network::dispatch(&mut svc, &req[..len], &mut handle, &mut out, &mut reply_handle) {
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
					if new_sock != 0 {
						place_sock(&mut socks, SockSlot { chan: new_sock, ci: new_sock_ci, stream_prod: 0, stream_seq: 0 });
					}
					if new_listener != 0 {
						place_listener(&mut listeners, Listener { chan: new_listener, binding: new_listener_binding, pending_corr: 0, pending: false });
					}
					if new_client != 0 {
						place_client(&mut clients, new_client);
					}
				}
				ReceivedCaps::Closed => {
					close(chan);
					clients[slot] = 0;
				}
			}
		}
	}
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
	// WHAT IT CLAIMED, not only which port: releasing a claim needs the mode and the address too,
	// because two listeners may legitimately hold the same port in two families.
	binding: Binding,
	pending_corr: u32,
	pending: bool,
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
			stack.unlisten(&listener.binding);
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
			place_sock(socks, SockSlot { chan: server, ci, stream_prod: 0, stream_seq: 0 });
			send_blocking(listener_chan, &reply, handles.first());
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

// THE ONE PLACE A CALLER'S ADDRESS BECOMES SOMETHING THIS SERVICE CAN SEND TO.
//
// The contract carries either family; the transports below carry one. A v6 destination is therefore
// `unsupported` - the request is understood and this implementation does not serve it yet - and NOT
// `invalid`, which would say the caller got the address wrong. The two read differently to a caller
// deciding whether to retry with something else.
fn ipv4_destination(addr: &ScopedAddress) -> Result<Ipv4Addr, Error> {
	match &addr.addr {
		// A SCOPE ON AN IPv4 ADDRESS IS REFUSED RATHER THAN IGNORED. There is one interface, so the
		// scope adds nothing and ignoring it would make a caller believe it selected something.
		IpAddress::V4(_) if addr.scope.is_some() => Err(Error::Invalid),
		IpAddress::V4(v4) => Ok(Ipv4Addr([v4.a, v4.b, v4.c, v4.d])),
		IpAddress::V6(_) => Err(Error::Unsupported),
	}
}

// This service's single interface identity: one NIC, and the generation the IPv6 host was brought up
// with so a scoped value cannot outlive a replacement.
fn interface_of(stack: &Stack) -> InterfaceId {
	match stack.ipv6_identity() {
		Some((index, generation)) => InterfaceId { index, generation },
		None => InterfaceId { index: 0, generation: 1 },
	}
}

fn endpoint_v4(ip: Ipv4Addr, port: u16) -> ScopedEndpoint {
	ScopedEndpoint { addr: ScopedAddress { addr: wire_v4(ip), scope: None }, port }
}

// A lifetime in seconds as the wire carries it, with `u32::MAX` reserved for infinity - the value
// the protocols themselves use, so nothing has to be translated on the way out.
const INFINITE_LIFETIME: u32 = u32::MAX;

// The deepest accept queue this stack's listen table can actually hold. A caller asking for more is
// told what it got rather than being refused: the request is reasonable and the answer is a number.
const LISTEN_BACKLOG_MAX: u16 = 32;

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

fn first_destination(target: &OpenTarget) -> Result<Destination, Error> {
	if target.destinations.is_empty() || target.destinations.len() > MAX_OPEN_DESTINATIONS {
		return Err(Error::Invalid);
	}
	// A CALLER-CHOSEN SOURCE IS REFUSED RATHER THAN IGNORED. Naming one and silently getting another
	// is exactly the failure the field exists to prevent, so until selection can honour it the
	// honest answer is that this implementation does not serve it.
	if target.source.is_some() {
		return Err(Error::Unsupported);
	}
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

// One candidate, validated into the form the transports open with.
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
	routes.push(RouteEntry { destination: wire_v4(Ipv4Addr([0, 0, 0, 0])), prefix_len: 0, scope: scope.clone(), preference: RoutePreference::Medium, lifetime_seconds: INFINITE_LIFETIME, hop: NextHop::Via(wire_v4(stack.gateway())) });
	routers.push(RouterEntry { addr: wire_v4(stack.gateway()), scope: scope.clone(), preference: RoutePreference::Medium, state: Reachability::Reachable, lifetime_seconds: INFINITE_LIFETIME });
	dns.push(DnsServer { addr: wire_v4(stack.dns()), scope: scope.clone() });
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
	NetInfo { scope, name: alloc::string::String::from("net0"), mac: WireMac::from_octets(stack.mac().0), mtu: stack.mtu(), addresses, routes, routers, dns, neighbors }
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
		Ok(NetCapacity { clients: self.clients_used, sockets: self.sockets_used, listeners: self.listeners_used, connections: self.stack.conn_used() as u32 })
	}

	// Resolve a name to the ordered candidate list.
	//
	// ONE CANDIDATE TODAY, AND A LIST ON THE WIRE. The resolver asks for an A record and gets one
	// address; the list is what lets a caller hand every candidate to `connect` and let this service
	// try them in order, which is the shape the AAAA query and the selection rules arrive into
	// without another contract change.
	fn resolve(&mut self, name: String) -> Result<Vec<IpAddress>, Error> {
		match do_dns(name.as_bytes(), self.frames, self.stack, &mut self.seq, self.rx, self.tx) {
			Some(addr) => Ok(alloc::vec![wire_v4(addr)]),
			None => Err(Error::NotFound),
		}
	}

	// Ping an address: a reply (with its TTL and round-trip time), a timeout, or
	// unreachable (no route / no ARP).
	fn ping(&mut self, addr: ScopedAddress) -> Result<PingReply, Error> {
		let target: Ipv4Addr = ipv4_destination(&addr)?;
		let (status, ttl, rtt_us): (u8, u8, u32) = do_ping(target, self.frames, self.stack, &mut self.seq, self.rx, self.tx);
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
		let target: Ipv4Addr = ipv4_destination(&addr)?;
		let (status, who, rtt_us) = do_probe(target, ttl.max(1), self.frames, self.stack, &mut self.seq, self.rx, self.tx);
		Ok(TraceHop { status, addr: wire_v4(who), rtt_us })
	}

	// A one-shot TCP exchange: connect, send the request, read the response, close.
	// Maps the connect failure modes onto the error enum. The response accumulates
	// in a Vec and rides an exactly-sized reply - its size is bounded by the peer
	// closing, never by a wire constant.
	fn fetch(&mut self, req: TcpRequest) -> Result<Vec<FetchChunk>, Error> {
		// THE OPEN IS GUARDED: everything that can refuse before a body exists refuses here, as the
		// typed error every other operation uses, rather than as an empty stream a caller would have
		// to interpret.
		let destination: Destination = first_destination(&req.target)?;
		let ci: usize = match self.stack.tcp_alloc() {
			Some(i) => i,
			None => return Err(Error::Again),
		};
		let mut data: Vec<u8> = Vec::new();
		let status: u8 = do_tcp(ci, destination, req.target.port, &req.request, self.frames, self.stack, self.rx, self.tx, &mut data);
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
		let destination: Destination = first_destination(&target)?;
		if !self.sock_room {
			return Err(Error::Again);
		}
		let ci: usize = match self.stack.tcp_alloc() {
			Some(i) => i,
			None => return Err(Error::Again),
		};
		match tcp_establish(ci, destination, target.port, self.frames, self.stack, self.rx, self.tx) {
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
		// THE MATRIX REFUSES BEFORE ANYTHING IS PUBLISHED. A port already held in a way that overlaps
		// is `denied` rather than `again`: retrying will not make the conflict go away.
		if let Err(refusal) = self.stack.listen(binding) {
			return Err(match refusal {
				service_logic::tcp_bind::BindRefusal::InUse => Error::Denied,
				_ => Error::Invalid,
			});
		}
		match channel() {
			Some((server, peer)) => {
				*self.new_listener = server;
				*self.new_listener_binding = binding;
				// THE BACKLOG IT ACTUALLY GOT. The accept queue this stack keeps is the listen table's
				// own, so what a caller asked for is reported back rather than assumed granted.
				Ok(ListenResult { listener: peer, backlog: req.backlog.min(LISTEN_BACKLOG_MAX) })
			}
			None => {
				self.stack.unlisten(&binding);
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
		match do_sntp(target, self.frames, self.stack, self.rx, self.tx) {
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
		LeaseClock { phase: LeasePhase::None, t1: 0, t2: 0, expiry: 0, retry: 0 }
	}

	// The clock for the lease just learned by the stack: thresholds from its T1 /
	// T2 / duration, or an idle clock when there is nothing to renew.
	fn bound(stack: &Stack) -> LeaseClock {
		match stack.dhcp_times() {
			Some((t1, t2, lease)) => {
				let now: u64 = clock();
				LeaseClock { phase: LeasePhase::Bound, t1: now + t1 as u64 * TICKS_PER_SEC, t2: now + t2 as u64 * TICKS_PER_SEC, expiry: now + lease as u64 * TICKS_PER_SEC, retry: 0 }
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
	fn on_reply(&mut self, msg_type: u8, stack: &mut Stack) {
		if self.phase != LeasePhase::Renewing && self.phase != LeasePhase::Rebinding {
			return;
		}
		if msg_type == DHCP_ACK {
			stack.apply_dhcp();
			*self = LeaseClock::bound(stack);
			print(b"network: DHCP lease renewed\n");
		} else if msg_type == DHCP_NAK {
			self.phase = LeasePhase::Expired;
			self.retry = clock();
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
	// Broadcast a DISCOVER and wait for the server's OFFER.
	let discover: usize = stack.build_dhcp_discover(tx);
	send_frame(frames, &tx[..discover]);
	let mut offered: bool = false;
	let deadline: u64 = clock() + DHCP_TIMEOUT_TICKS;
	while clock() < deadline && !offered {
		if wait_frames(frames, stack, deadline) != 0 {
			break;
		}
		if let Event::DhcpReply(msg_type) = pump(frames, stack, rx, tx) {
			if msg_type == DHCP_OFFER {
				offered = true;
			}
		}
	}
	if !offered {
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
		if let Event::DhcpReply(msg_type) = pump(frames, stack, rx, tx) {
			if msg_type == DHCP_ACK {
				stack.apply_dhcp();
				return true;
			}
		}
	}
	false
}

// Resolve `name` to an IPv4 address via a DNS A-record query to the SLIRP DNS
// server, pumping received frames for the response. None on timeout or failure.
fn do_dns(name: &[u8], frames: u64, stack: &mut Stack, txn: &mut u16, rx: &mut [u8], tx: &mut [u8]) -> Option<Ipv4Addr> {
	let dns: Ipv4Addr = stack.dns();
	let hop: Ipv4Addr = stack.next_hop(dns);
	let mac: MacAddr = match resolve(hop, frames, stack, rx, tx) {
		Some(m) => m,
		None => return None,
	};
	*txn = txn.wrapping_add(1);
	let query: usize = stack.build_dns_query(mac, dns, name, *txn, DNS_SRC_PORT, tx);
	if query == 0 {
		return None;
	}
	send_frame(frames, &tx[..query]);
	let deadline: u64 = clock() + DNS_TIMEOUT_TICKS;
	while clock() < deadline {
		if wait_frames(frames, stack, deadline) != 0 {
			break;
		}
		if let Event::DnsReply(addr) = pump(frames, stack, rx, tx) {
			return Some(addr);
		}
	}
	None
}

// Send an SNTP request to `server` and return the Unix epoch seconds from its reply,
// or None on timeout / no route. A one-shot UDP query/response, mirroring do_dns.
// The 48-byte SNTP client request, which is the same on either family.
//
// ONE DEFINITION, because it is the message and not the framing: `build_sntp_request` writes these
// bytes into an IPv4 frame and the IPv6 path hands them to the host, and a second copy of "LI 0, VN
// 4, Mode 3 and forty-seven zeros" is a second thing to get wrong.
fn sntp_request_body() -> Vec<u8> {
	let mut body: Vec<u8> = alloc::vec![0u8; 48];
	body[0] = 0x23;
	body
}

fn do_sntp(server: Destination, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Option<u64> {
	match server {
		Destination::V4(ip) => {
			let hop: Ipv4Addr = stack.next_hop(ip);
			let mac: MacAddr = resolve(hop, frames, stack, rx, tx)?;
			let query: usize = stack.build_sntp_request(mac, ip, NTP_SRC_PORT, tx);
			if query == 0 {
				return None;
			}
			send_frame(frames, &tx[..query]);
		}
		// THE SAME REQUEST OVER THE OTHER FAMILY. The datagram is identical; what differs is the
		// pseudo-header its mandatory checksum covers and who resolves the next hop.
		Destination::V6(octets) => {
			stack.set_clock(now_ms());
			if !stack.send_udp6(octets, NTP_SRC_PORT, NTP_PORT, &sntp_request_body()) {
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
		if let Event::SntpReply(unix) = pump(frames, stack, rx, tx) {
			return Some(unix);
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
fn do_tcp(ci: usize, destination: Destination, port: u16, request: &[u8], frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8], reply: &mut Vec<u8>) -> u8 {
	// Establish the connection; on failure report the status and stop (the
	// establish status bytes 2 / 3 / 0 map straight onto the fetch errors).
	match tcp_establish(ci, destination, port, frames, stack, rx, tx) {
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
fn drain_tx(frames: u64, ci: usize, stack: &mut Stack, tx: &mut [u8]) {
	stack.set_clock(now_ms());
	loop {
		let len: usize = stack.tcp_pump(ci, tx);
		if len == 0 {
			return;
		}
		send_frame(frames, &tx[..len]);
	}
}

// Establish a TCP connection to `ip`:`port` (next-hop via the gateway when off-link):
// resolve the next hop, open the connection, and send the SYN, retransmitting it
// until the handshake completes. Returns 1 = established, 2 = unreachable (no ARP),
// 3 = refused (reset), 0 = timed out. Shared by `fetch` (do_tcp) and `connect`.
fn tcp_establish(ci: usize, destination: Destination, port: u16, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> u8 {
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
			if !stack.tcp_open6(ci, octets, port, local_port, iss) {
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
		let Some(interval) = service_logic::tcp_rto::syn_interval(attempt) else {
			// Every retransmission has been sent and the last interval has elapsed: the open fails
			// with a typed timeout rather than retrying for ever.
			break;
		};
		let until: u64 = clock() + ticks_from_ms(u64::from(interval));
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
	if !stack.tcp_established(ci) && !stack.tcp_aborted(ci) {
		let last: u32 = service_logic::tcp_rto::syn_interval(service_logic::tcp_rto::syn_attempts() - 1).unwrap_or(1000);
		let until: u64 = clock() + ticks_from_ms(u64::from(last));
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

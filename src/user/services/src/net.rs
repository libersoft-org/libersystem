// A minimal userspace network stack: Ethernet II framing, ARP, IPv4, ICMP echo,
// and a UDP/DNS client. It is the core of NetworkService (M33): the standing
// service owns this stack and receives each Ethernet frame from the frame-mover
// `driver.virtio-net` over a channel, hands it to `Stack::on_frame` (which parses
// it, updates the neighbor cache, and writes an optional reply frame), and sends
// any reply back to the driver to transmit. It carries no device knowledge - the
// driver owns the NIC; the service owns the protocol.

#![allow(dead_code)]

use alloc::vec::Vec;

// EtherType values (the 2-byte type field of an Ethernet II frame).
const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;

// ARP fields.
const ARP_HTYPE_ETHERNET: u16 = 1;
const ARP_PTYPE_IPV4: u16 = 0x0800;
const ARP_OP_REQUEST: u16 = 1;
const ARP_OP_REPLY: u16 = 2;

// IPv4 protocol numbers.
const IP_PROTO_ICMP: u8 = 1;
const IP_PROTO_UDP: u8 = 17;
const IP_PROTO_TCP: u8 = 6;

// ICMP message types.
const ICMP_ECHO_REQUEST: u8 = 8;
const ICMP_ECHO_REPLY: u8 = 0;

// The DNS server port (UDP).
const DNS_PORT: u16 = 53;

// The NTP / SNTP server port (UDP), and the offset between the NTP epoch (1900) and
// the Unix epoch (1970) in seconds (70 years, 17 of them leap).
const NTP_PORT: u16 = 123;
const NTP_UNIX_OFFSET: u32 = 2_208_988_800;

// DHCP / BOOTP: a UDP client on port 68 talking to a server on port 67. The client
// broadcasts a DISCOVER, the server OFFERs an address, the client REQUESTs it, and
// the server ACKs - the client learning its address and the subnet mask / gateway /
// DNS server from the reply options.
const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_SERVER_PORT: u16 = 67;
const BOOTP_REQUEST: u8 = 1;
const BOOTP_REPLY: u8 = 2;
const BOOTP_HDR: usize = 236;
const DHCP_MAGIC: u32 = 0x6382_5363;
const DHCP_DISCOVER: u8 = 1;
pub const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
pub const DHCP_ACK: u8 = 5;
pub const DHCP_NAK: u8 = 6;
const DHCP_OPT_MASK: u8 = 1;
const DHCP_OPT_ROUTER: u8 = 3;
const DHCP_OPT_DNS: u8 = 6;
const DHCP_OPT_REQUESTED_IP: u8 = 50;
const DHCP_OPT_LEASE_TIME: u8 = 51;
const DHCP_OPT_MSG_TYPE: u8 = 53;
const DHCP_OPT_SERVER_ID: u8 = 54;
const DHCP_OPT_PARAM_LIST: u8 = 55;
const DHCP_OPT_T1: u8 = 58;
const DHCP_OPT_T2: u8 = 59;
const DHCP_OPT_END: u8 = 255;

// A lease-time value of all ones means the lease never expires (no renewal clock).
const DHCP_LEASE_INFINITE: u32 = 0xffff_ffff;

// The limited-broadcast IPv4 address (255.255.255.255): the DHCP server addresses
// its OFFER/ACK here when it broadcasts the reply (we have no address yet).
const IPV4_BROADCAST: Ipv4Addr = Ipv4Addr([255, 255, 255, 255]);

// Header sizes (bytes).
const ETH_HDR: usize = 14;
const ARP_LEN: usize = 28;
const IPV4_HDR: usize = 20;
const ICMP_HDR: usize = 8;
const UDP_HDR: usize = 8;
const TCP_HDR: usize = 20;

// ICMP echo payload size (bytes). 56 matches the ping default, so the on-wire
// packet is 84 bytes (20 IP + 8 ICMP + 56) and a reply reports the familiar 64
// bytes (8 ICMP + 56 payload).
const ICMP_PAYLOAD: usize = 56;

// TCP control flags.
const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;

// The receive buffer / advertised window for one TCP connection: 65535 is the most
// a TCP header's 16-bit window field can advertise without window scaling (adding
// the WS option is the future step beyond this, as autotuned buffers are).
// The buffer lives inside the heap-pooled connection state.
const TCP_RX_MAX: usize = 65535;

// The initial TCP connection pool size: outbound `connect`s and inbound accepted
// connections share the pool, which grows on demand - a size hint, never a cap.
const TCP_CONN_MAX: usize = 4;

// The initial listen-table size (passive open); the table grows on demand.
const LISTEN_MAX: usize = 2;

// A 48-bit Ethernet MAC address.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
	pub const BROADCAST: MacAddr = MacAddr([0xff; 6]);
	pub const ZERO: MacAddr = MacAddr([0; 6]);
}

// A 32-bit IPv4 address.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Addr(pub [u8; 4]);

// Read a big-endian u16 at `off` within `b` (the caller guarantees the bounds).
fn be16(b: &[u8], off: usize) -> u16 {
	((b[off] as u16) << 8) | b[off + 1] as u16
}

// Write a big-endian u16 `v` at `off` within `b`.
fn put16(b: &mut [u8], off: usize, v: u16) {
	b[off] = (v >> 8) as u8;
	b[off + 1] = v as u8;
}

// Read a big-endian u32 at `off` within `b` (the caller guarantees the bounds).
fn be32(b: &[u8], off: usize) -> u32 {
	((b[off] as u32) << 24) | ((b[off + 1] as u32) << 16) | ((b[off + 2] as u32) << 8) | b[off + 3] as u32
}

// Write a big-endian u32 `v` at `off` within `b`.
fn put32(b: &mut [u8], off: usize, v: u32) {
	b[off] = (v >> 24) as u8;
	b[off + 1] = (v >> 16) as u8;
	b[off + 2] = (v >> 8) as u8;
	b[off + 3] = v as u8;
}

// TCP serial-number comparisons (RFC 793 modular arithmetic): `a` is after `b`, or
// `a` is at or before `b`, accounting for 32-bit wraparound.
fn seq_gt(a: u32, b: u32) -> bool {
	(a.wrapping_sub(b) as i32) > 0
}

fn seq_le(a: u32, b: u32) -> bool {
	(a.wrapping_sub(b) as i32) <= 0
}

// The internet checksum (ones-complement sum of 16-bit words) of `data`.
fn checksum(data: &[u8]) -> u16 {
	let mut sum: u32 = 0;
	let mut i: usize = 0;
	while i + 1 < data.len() {
		sum += be16(data, i) as u32;
		i += 2;
	}
	if i < data.len() {
		sum += (data[i] as u32) << 8;
	}
	while sum >> 16 != 0 {
		sum = (sum & 0xffff) + (sum >> 16);
	}
	!(sum as u16)
}

// The UDP checksum over the IPv4 pseudo-header (src, dst, proto, length) plus the
// UDP header and payload `udp` (whose own checksum field must be zero). A computed
// 0 is sent as 0xffff (0 means "no checksum").
fn udp_checksum(src: Ipv4Addr, dst: Ipv4Addr, udp: &[u8]) -> u16 {
	let mut sum: u32 = be16(&src.0, 0) as u32 + be16(&src.0, 2) as u32 + be16(&dst.0, 0) as u32 + be16(&dst.0, 2) as u32 + IP_PROTO_UDP as u32 + udp.len() as u32;
	let mut i: usize = 0;
	while i + 1 < udp.len() {
		sum += be16(udp, i) as u32;
		i += 2;
	}
	if i < udp.len() {
		sum += (udp[i] as u32) << 8;
	}
	while sum >> 16 != 0 {
		sum = (sum & 0xffff) + (sum >> 16);
	}
	let c: u16 = !(sum as u16);
	if c == 0 {
		0xffff
	} else {
		c
	}
}

// The TCP checksum over the IPv4 pseudo-header (src, dst, proto, length) plus the
// TCP header and payload `seg` (whose own checksum field must be zero). Unlike UDP,
// a computed 0 is transmitted as-is (TCP always checksums).
fn tcp_checksum(src: Ipv4Addr, dst: Ipv4Addr, seg: &[u8]) -> u16 {
	let mut sum: u32 = be16(&src.0, 0) as u32 + be16(&src.0, 2) as u32 + be16(&dst.0, 0) as u32 + be16(&dst.0, 2) as u32 + IP_PROTO_TCP as u32 + seg.len() as u32;
	let mut i: usize = 0;
	while i + 1 < seg.len() {
		sum += be16(seg, i) as u32;
		i += 2;
	}
	if i < seg.len() {
		sum += (seg[i] as u32) << 8;
	}
	while sum >> 16 != 0 {
		sum = (sum & 0xffff) + (sum >> 16);
	}
	!(sum as u16)
}

// One entry of the small ARP neighbor cache (IPv4 -> MAC).
#[derive(Clone, Copy)]
struct Neigh {
	ip: Ipv4Addr,
	mac: MacAddr,
	valid: bool,
}

// The ARP neighbor cache size - a cache with eviction (the oldest slot is replaced
// when full), so it bounds memory, never which neighbors are reachable. Lives
// on the heap.
const NEIGH_MAX: usize = 1024;

// The address configuration learned from a DHCP OFFER/ACK: the offered address plus
// the mask / gateway / DNS / server-id carried in the reply options, and the lease
// clock (the lease duration with its T1 renewal / T2 rebinding thresholds, seconds;
// 0 = the server sent none).
#[derive(Clone, Copy)]
struct DhcpLease {
	yiaddr: Ipv4Addr,
	mask: Ipv4Addr,
	gateway: Ipv4Addr,
	dns: Ipv4Addr,
	server: Ipv4Addr,
	lease_secs: u32,
	t1_secs: u32,
	t2_secs: u32,
}

impl DhcpLease {
	const fn empty() -> DhcpLease {
		DhcpLease { yiaddr: Ipv4Addr([0; 4]), mask: Ipv4Addr([0; 4]), gateway: Ipv4Addr([0; 4]), dns: Ipv4Addr([0; 4]), server: Ipv4Addr([0; 4]), lease_secs: 0, t1_secs: 0, t2_secs: 0 }
	}
}

// The notable thing a received frame did, for the driver to log or react to.
#[derive(Clone, Copy)]
pub enum Event {
	None,
	// We learned a neighbor's MAC (from an ARP reply for an address we asked about).
	Learned(Ipv4Addr, MacAddr),
	// An ICMP echo reply arrived (a `ping` we sent was answered): the responder's
	// address, the reply packet's IP TTL, and the echoed sequence number.
	EchoReply(Ipv4Addr, u8, u16),
	// A DNS response resolved a name to this address.
	DnsReply(Ipv4Addr),
	// A DHCP reply arrived with this message type (OFFER or ACK); the learned lease is
	// stored in the stack.
	DhcpReply(u8),
	// An SNTP reply arrived carrying this Unix timestamp (seconds since 1970, UTC).
	SntpReply(u64),
}

// The result of feeding one frame to the stack: an optional reply to transmit
// (`reply_len` bytes written to the caller's output buffer, 0 = none) and an event.
pub struct Outcome {
	pub reply_len: usize,
	pub event: Event,
}

// The TCP connection state machine (per pooled connection, client or server side).
#[derive(Clone, Copy, PartialEq)]
enum TcpState {
	// No connection.
	Closed,
	// SYN sent (active open), awaiting the peer's SYN-ACK.
	SynSent,
	// SYN received (passive open), our SYN-ACK sent, awaiting the completing ACK.
	SynRcvd,
	// Handshake complete, data may flow.
	Established,
	// We sent a FIN and are tearing the connection down.
	FinWait,
}

// One TCP connection in the stack's pool. A slot is free when `in_use` is false; with
// our IP fixed, the (local_port, remote_ip, remote_port) tuple demuxes inbound
// segments to it.
struct TcpConn {
	// Whether this pool slot is allocated to a live connection (an outbound connect or an
	// inbound accepted connection). A free slot is reused by the next open.
	in_use: bool,
	state: TcpState,
	// The peer sent a RST (the connection was refused or reset).
	aborted: bool,
	// The peer sent a FIN (it closed its half).
	peer_fin: bool,
	// Accepted via a listener (passive open), established, and not yet handed to a
	// socket - awaiting the listener's `accept`.
	pending_accept: bool,
	local_port: u16,
	remote_ip: Ipv4Addr,
	remote_port: u16,
	remote_mac: MacAddr,
	// Send sequence: oldest unacknowledged, and the next sequence to use.
	snd_una: u32,
	snd_nxt: u32,
	// Receive sequence: the next in-order byte we expect.
	rcv_nxt: u32,
	// Received in-order data waiting to be read, and how much. Heap-allocated (the
	// 64 kB window must never sit on a 16 kB user stack).
	rx: Vec<u8>,
	rx_len: usize,
}

impl TcpConn {
	fn closed() -> TcpConn {
		TcpConn { in_use: false, state: TcpState::Closed, aborted: false, peer_fin: false, pending_accept: false, local_port: 0, remote_ip: Ipv4Addr([0; 4]), remote_port: 0, remote_mac: MacAddr::ZERO, snd_una: 0, snd_nxt: 0, rcv_nxt: 0, rx: alloc::vec![0; TCP_RX_MAX], rx_len: 0 }
	}
}

// A snapshot of one live socket for enumeration (`ss`): its local port, the remote
// endpoint it talks to (zeros for a listening socket), and a small state tag.
pub struct SockEntry {
	pub local_port: u16,
	pub remote_ip: Ipv4Addr,
	pub remote_port: u16,
	pub state: SockEntryState,
}

// The position of a socket in the connection lifecycle, for `ss` to label each row.
#[derive(Clone, Copy)]
pub enum SockEntryState {
	Closed,
	SynSent,
	SynRcvd,
	Established,
	FinWait,
	Listen,
}

// The interface's L2/L3 state: our addresses, the neighbor cache, and the pool of TCP
// connections (on the heap - each carries a kilobyte receive buffer, too large for the
// 16 kB user stack).
pub struct Stack {
	mac: MacAddr,
	ip: Ipv4Addr,
	mask: Ipv4Addr,
	gateway: Ipv4Addr,
	dns: Ipv4Addr,
	neigh: Vec<Neigh>,
	conns: Vec<TcpConn>,
	// The ports we accept inbound connections on (passive open); 0 = unused slot.
	// Grows on demand.
	listen_ports: Vec<u16>,
	// The next initial send sequence to hand a passively-opened connection (bumped per
	// accept; predictability is not a concern for this stack).
	next_iss: u32,
	dhcp: DhcpLease,
}

impl Stack {
	pub fn new(mac: MacAddr, ip: Ipv4Addr, mask: Ipv4Addr, gateway: Ipv4Addr, dns: Ipv4Addr) -> Stack {
		let mut conns: Vec<TcpConn> = Vec::with_capacity(TCP_CONN_MAX);
		for _ in 0..TCP_CONN_MAX {
			conns.push(TcpConn::closed());
		}
		Stack { mac, ip, mask, gateway, dns, neigh: alloc::vec![Neigh { ip: Ipv4Addr([0; 4]), mac: MacAddr::ZERO, valid: false }; NEIGH_MAX], conns, listen_ports: alloc::vec![0; LISTEN_MAX], next_iss: 0x1000_0000, dhcp: DhcpLease::empty() }
	}

	pub fn mac(&self) -> MacAddr {
		self.mac
	}

	pub fn ip(&self) -> Ipv4Addr {
		self.ip
	}

	pub fn gateway(&self) -> Ipv4Addr {
		self.gateway
	}

	// The DNS server address (the static fallback, or the one learned from DHCP).
	pub fn dns(&self) -> Ipv4Addr {
		self.dns
	}

	// The next-hop address for reaching `dst`: `dst` itself when it shares our subnet
	// (on-link, reached by direct ARP), otherwise the gateway (off-link, routed). The
	// L3 destination of the packet is still `dst`; only the L2 MAC we resolve changes.
	pub fn next_hop(&self, dst: Ipv4Addr) -> Ipv4Addr {
		if self.on_link(dst) {
			dst
		} else {
			self.gateway
		}
	}

	// Whether `dst` is on our local subnet (its network part matches ours under the
	// mask), so it is reachable by a direct ARP rather than through the gateway.
	fn on_link(&self, dst: Ipv4Addr) -> bool {
		let mut i: usize = 0;
		while i < 4 {
			if (dst.0[i] & self.mask.0[i]) != (self.ip.0[i] & self.mask.0[i]) {
				return false;
			}
			i += 1;
		}
		true
	}

	// Record (or refresh) a neighbor's MAC, evicting the oldest slot when full.
	fn learn(&mut self, ip: Ipv4Addr, mac: MacAddr) {
		for n in self.neigh.iter_mut() {
			if n.valid && n.ip == ip {
				n.mac = mac;
				return;
			}
		}
		for n in self.neigh.iter_mut() {
			if !n.valid {
				*n = Neigh { ip, mac, valid: true };
				return;
			}
		}
		self.neigh[0] = Neigh { ip, mac, valid: true };
	}

	// The cached MAC for `ip`, if known.
	pub fn lookup(&self, ip: Ipv4Addr) -> Option<MacAddr> {
		for n in self.neigh.iter() {
			if n.valid && n.ip == ip {
				return Some(n.mac);
			}
		}
		None
	}

	// The `idx`-th valid neighbor (address + MAC), or None past the end - the
	// iteration the typed `info` interface state is built from.
	pub fn neigh_at(&self, idx: usize) -> Option<(Ipv4Addr, MacAddr)> {
		self.neigh.iter().filter(|n: &&Neigh| n.valid).nth(idx).map(|n: &Neigh| (n.ip, n.mac))
	}

	// Snapshot the live sockets for `ss`: the listening ports first (as Listen rows),
	// then every in-use pooled connection with its local port, remote endpoint, and TCP
	// state. NetworkService maps these to the typed `sock-info` the tool renders.
	pub fn sockets(&self) -> Vec<SockEntry> {
		let mut out: Vec<SockEntry> = Vec::new();
		for &p in self.listen_ports.iter() {
			if p != 0 {
				out.push(SockEntry { local_port: p, remote_ip: Ipv4Addr([0; 4]), remote_port: 0, state: SockEntryState::Listen });
			}
		}
		for c in self.conns.iter() {
			if !c.in_use {
				continue;
			}
			let state: SockEntryState = match c.state {
				TcpState::Closed => SockEntryState::Closed,
				TcpState::SynSent => SockEntryState::SynSent,
				TcpState::SynRcvd => SockEntryState::SynRcvd,
				TcpState::Established => SockEntryState::Established,
				TcpState::FinWait => SockEntryState::FinWait,
			};
			out.push(SockEntry { local_port: c.local_port, remote_ip: c.remote_ip, remote_port: c.remote_port, state });
		}
		out
	}

	// Serialize the interface state for the `ip` / `net` command into `out`, returning
	// its length: our address (4), MAC (6), gateway (4), the neighbor count (1), then
	// that many (ip 4, mac 6) cache entries. The shell parses and renders it.
	pub fn write_state(&self, out: &mut [u8]) -> usize {
		out[0..4].copy_from_slice(&self.ip.0);
		out[4..10].copy_from_slice(&self.mac.0);
		out[10..14].copy_from_slice(&self.gateway.0);
		let mut count: u8 = 0;
		let mut off: usize = 15;
		for n in self.neigh.iter() {
			if n.valid && off + 10 <= out.len() {
				out[off..off + 4].copy_from_slice(&n.ip.0);
				out[off + 4..off + 10].copy_from_slice(&n.mac.0);
				off += 10;
				count += 1;
			}
		}
		out[14] = count;
		off
	}

	// Parse one received Ethernet frame, update the neighbor cache, and write an
	// optional reply frame to `out`. Any malformed or unhandled frame yields no reply.
	pub fn on_frame(&mut self, frame: &[u8], out: &mut [u8]) -> Outcome {
		if frame.len() < ETH_HDR {
			return Outcome { reply_len: 0, event: Event::None };
		}
		match be16(frame, 12) {
			ETHERTYPE_ARP => self.on_arp(frame, out),
			ETHERTYPE_IPV4 => self.on_ipv4(frame, out),
			_ => Outcome { reply_len: 0, event: Event::None },
		}
	}

	// Handle an ARP packet: learn the sender, reply to a request for our address, and
	// report a reply as a learned neighbor.
	fn on_arp(&mut self, frame: &[u8], out: &mut [u8]) -> Outcome {
		let a: &[u8] = &frame[ETH_HDR..];
		if a.len() < ARP_LEN || be16(a, 0) != ARP_HTYPE_ETHERNET || be16(a, 2) != ARP_PTYPE_IPV4 {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let op: u16 = be16(a, 6);
		let sender_mac: MacAddr = MacAddr([a[8], a[9], a[10], a[11], a[12], a[13]]);
		let sender_ip: Ipv4Addr = Ipv4Addr([a[14], a[15], a[16], a[17]]);
		let target_ip: Ipv4Addr = Ipv4Addr([a[24], a[25], a[26], a[27]]);
		self.learn(sender_ip, sender_mac);
		if op == ARP_OP_REQUEST && target_ip == self.ip {
			let len: usize = self.build_arp(ARP_OP_REPLY, sender_mac, sender_ip, out);
			return Outcome { reply_len: len, event: Event::None };
		}
		if op == ARP_OP_REPLY {
			return Outcome { reply_len: 0, event: Event::Learned(sender_ip, sender_mac) };
		}
		Outcome { reply_len: 0, event: Event::None }
	}

	// Build an Ethernet + ARP frame (request or reply) into `out`, returning its length.
	fn build_arp(&self, op: u16, target_mac: MacAddr, target_ip: Ipv4Addr, out: &mut [u8]) -> usize {
		let dst: MacAddr = if op == ARP_OP_REQUEST { MacAddr::BROADCAST } else { target_mac };
		out[0..6].copy_from_slice(&dst.0);
		out[6..12].copy_from_slice(&self.mac.0);
		put16(out, 12, ETHERTYPE_ARP);
		let a: &mut [u8] = &mut out[ETH_HDR..ETH_HDR + ARP_LEN];
		put16(a, 0, ARP_HTYPE_ETHERNET);
		put16(a, 2, ARP_PTYPE_IPV4);
		a[4] = 6;
		a[5] = 4;
		put16(a, 6, op);
		a[8..14].copy_from_slice(&self.mac.0);
		a[14..18].copy_from_slice(&self.ip.0);
		a[18..24].copy_from_slice(&target_mac.0);
		a[24..28].copy_from_slice(&target_ip.0);
		ETH_HDR + ARP_LEN
	}

	// Build a broadcast ARP request asking who has `target`, into `out`.
	pub fn build_arp_request(&self, target: Ipv4Addr, out: &mut [u8]) -> usize {
		self.build_arp(ARP_OP_REQUEST, MacAddr::ZERO, target, out)
	}

	// Handle an IPv4 packet addressed to us; ICMP, UDP (DNS), and TCP are processed.
	fn on_ipv4(&mut self, frame: &[u8], out: &mut [u8]) -> Outcome {
		let ip: &[u8] = &frame[ETH_HDR..];
		if ip.len() < IPV4_HDR || ip[0] >> 4 != 4 {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let ihl: usize = (ip[0] & 0x0f) as usize * 4;
		if ihl < IPV4_HDR || ip.len() < ihl {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let dst_ip: Ipv4Addr = Ipv4Addr([ip[16], ip[17], ip[18], ip[19]]);
		let proto: u8 = ip[9];
		// Accept packets addressed to us, plus limited-broadcast UDP - so the DHCP
		// server's broadcast OFFER/ACK, sent before we have an address, reach us.
		if dst_ip != self.ip && !(dst_ip == IPV4_BROADCAST && proto == IP_PROTO_UDP) {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let src_ip: Ipv4Addr = Ipv4Addr([ip[12], ip[13], ip[14], ip[15]]);
		match proto {
			IP_PROTO_ICMP => self.on_icmp(frame, ihl, src_ip, out),
			IP_PROTO_UDP => self.on_udp(frame, ihl),
			IP_PROTO_TCP => self.on_tcp(frame, ihl, src_ip, out),
			_ => Outcome { reply_len: 0, event: Event::None },
		}
	}

	// Handle an inbound UDP datagram: a DNS response (source port 53) is parsed into
	// the resolved address, a DHCP reply (source port 67) into the learned lease.
	fn on_udp(&mut self, frame: &[u8], ihl: usize) -> Outcome {
		let udp: &[u8] = &frame[ETH_HDR + ihl..];
		if udp.len() < UDP_HDR {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let src_port: u16 = be16(udp, 0);
		if src_port == DNS_PORT {
			if let Some(addr) = parse_dns_response(&udp[UDP_HDR..]) {
				return Outcome { reply_len: 0, event: Event::DnsReply(addr) };
			}
		} else if src_port == DHCP_SERVER_PORT {
			if let Some(msg_type) = self.parse_dhcp(&udp[UDP_HDR..]) {
				return Outcome { reply_len: 0, event: Event::DhcpReply(msg_type) };
			}
		} else if src_port == NTP_PORT {
			if let Some(unix) = parse_sntp(&udp[UDP_HDR..]) {
				return Outcome { reply_len: 0, event: Event::SntpReply(unix) };
			}
		}
		Outcome { reply_len: 0, event: Event::None }
	}

	// Handle an inbound TCP segment: demux it to the live connection it belongs to (by
	// its 4-tuple), complete the handshake (SYN-ACK -> ACK), accept in-order data and
	// acknowledge it, note a peer FIN, and abort on RST. Segments for no live
	// connection are ignored.
	fn on_tcp(&mut self, frame: &[u8], ihl: usize, src_ip: Ipv4Addr, out: &mut [u8]) -> Outcome {
		let tcp: &[u8] = &frame[ETH_HDR + ihl..];
		if tcp.len() < TCP_HDR {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let src_port: u16 = be16(tcp, 0);
		let dst_port: u16 = be16(tcp, 2);
		let ci: usize = match self.find_conn(src_ip, src_port, dst_port) {
			Some(i) => i,
			None => {
				// A SYN to a listening port opens a new inbound connection (passive open).
				let flags: u8 = tcp[13];
				if flags & TCP_SYN != 0 && flags & TCP_ACK == 0 && self.is_listening(dst_port) {
					let seg_seq: u32 = be32(tcp, 4);
					return self.passive_open(frame, src_ip, src_port, dst_port, seg_seq, out);
				}
				return Outcome { reply_len: 0, event: Event::None };
			}
		};
		let seg_seq: u32 = be32(tcp, 4);
		let seg_ack: u32 = be32(tcp, 8);
		let data_off: usize = ((tcp[12] >> 4) as usize) * 4;
		let flags: u8 = tcp[13];
		if data_off < TCP_HDR || data_off > tcp.len() {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let payload: &[u8] = &tcp[data_off..];
		// A reset aborts the connection (a refused connect, or a reset peer).
		if flags & TCP_RST != 0 {
			self.conns[ci].state = TcpState::Closed;
			self.conns[ci].aborted = true;
			return Outcome { reply_len: 0, event: Event::None };
		}
		// Complete the handshake: a SYN-ACK acknowledging our SYN.
		if self.conns[ci].state == TcpState::SynSent {
			if flags & TCP_SYN != 0 && flags & TCP_ACK != 0 && seg_ack == self.conns[ci].snd_nxt {
				self.conns[ci].rcv_nxt = seg_seq.wrapping_add(1);
				self.conns[ci].snd_una = seg_ack;
				self.conns[ci].state = TcpState::Established;
				let len: usize = self.build_tcp(ci, TCP_ACK, self.conns[ci].snd_nxt, self.conns[ci].rcv_nxt, &[], out);
				return Outcome { reply_len: len, event: Event::None };
			}
			return Outcome { reply_len: 0, event: Event::None };
		}
		// Complete a passive-open handshake: the ACK of our SYN-ACK establishes the
		// connection, which then awaits the listener's accept. Falls through so a
		// data-bearing ACK (the request piggybacked on the handshake) is handled below.
		if self.conns[ci].state == TcpState::SynRcvd {
			if flags & TCP_ACK != 0 && seg_ack == self.conns[ci].snd_nxt {
				self.conns[ci].snd_una = seg_ack;
				self.conns[ci].state = TcpState::Established;
				self.conns[ci].pending_accept = true;
			} else {
				return Outcome { reply_len: 0, event: Event::None };
			}
		}
		// Established (or tearing down): advance our send window from the ack.
		if flags & TCP_ACK != 0 && seq_gt(seg_ack, self.conns[ci].snd_una) && seq_le(seg_ack, self.conns[ci].snd_nxt) {
			self.conns[ci].snd_una = seg_ack;
		}
		// Accept in-order data into the receive buffer (bounded by the window).
		let mut progressed: bool = false;
		if !payload.is_empty() && seg_seq == self.conns[ci].rcv_nxt {
			let rx_len: usize = self.conns[ci].rx_len;
			let n: usize = payload.len().min(TCP_RX_MAX - rx_len);
			self.conns[ci].rx[rx_len..rx_len + n].copy_from_slice(&payload[..n]);
			self.conns[ci].rx_len += n;
			self.conns[ci].rcv_nxt = self.conns[ci].rcv_nxt.wrapping_add(n as u32);
			progressed = true;
		}
		// A FIN occupies the sequence just past the segment's data.
		if flags & TCP_FIN != 0 && seg_seq.wrapping_add(payload.len() as u32) == self.conns[ci].rcv_nxt {
			self.conns[ci].rcv_nxt = self.conns[ci].rcv_nxt.wrapping_add(1);
			self.conns[ci].peer_fin = true;
			progressed = true;
		}
		// Acknowledge any data or FIN we consumed.
		if progressed {
			let len: usize = self.build_tcp(ci, TCP_ACK, self.conns[ci].snd_nxt, self.conns[ci].rcv_nxt, &[], out);
			return Outcome { reply_len: len, event: Event::None };
		}
		Outcome { reply_len: 0, event: Event::None }
	}

	// Find the live connection an inbound segment belongs to (matched by its 4-tuple;
	// our address is fixed, so local_port plus the remote address/port), or None.
	fn find_conn(&self, remote_ip: Ipv4Addr, remote_port: u16, local_port: u16) -> Option<usize> {
		for (i, c) in self.conns.iter().enumerate() {
			if c.in_use && c.state != TcpState::Closed && c.remote_ip == remote_ip && c.remote_port == remote_port && c.local_port == local_port {
				return Some(i);
			}
		}
		None
	}

	// Open an inbound connection from a SYN to a listening port: allocate a pool slot,
	// record the peer (its address and source MAC), enter SynRcvd, and build the SYN-ACK
	// into `out`. No reply if the pool is full.
	fn passive_open(&mut self, frame: &[u8], src_ip: Ipv4Addr, src_port: u16, dst_port: u16, seg_seq: u32, out: &mut [u8]) -> Outcome {
		let ci: usize = match self.tcp_alloc() {
			Some(i) => i,
			None => return Outcome { reply_len: 0, event: Event::None },
		};
		let remote_mac: MacAddr = MacAddr([frame[6], frame[7], frame[8], frame[9], frame[10], frame[11]]);
		let iss: u32 = self.next_iss;
		self.next_iss = self.next_iss.wrapping_add(0x1000);
		let c: &mut TcpConn = &mut self.conns[ci];
		c.state = TcpState::SynRcvd;
		c.local_port = dst_port;
		c.remote_ip = src_ip;
		c.remote_port = src_port;
		c.remote_mac = remote_mac;
		c.rcv_nxt = seg_seq.wrapping_add(1);
		c.snd_una = iss;
		c.snd_nxt = iss.wrapping_add(1);
		c.rx_len = 0;
		let snd: u32 = self.conns[ci].snd_una;
		let rcv: u32 = self.conns[ci].rcv_nxt;
		let len: usize = self.build_tcp(ci, TCP_SYN | TCP_ACK, snd, rcv, &[], out);
		Outcome { reply_len: len, event: Event::None }
	}

	// Build an Ethernet + IPv4 + TCP segment to connection `ci`'s peer with `flags`,
	// sequence `seq`, acknowledgement `ack`, and `payload`, into `out`, returning its
	// length (0 if it does not fit). No TCP options are emitted (a 20-byte header).
	fn build_tcp(&self, ci: usize, flags: u8, seq: u32, ack: u32, payload: &[u8], out: &mut [u8]) -> usize {
		let total: usize = IPV4_HDR + TCP_HDR + payload.len();
		if ETH_HDR + total > out.len() {
			return 0;
		}
		out[0..6].copy_from_slice(&self.conns[ci].remote_mac.0);
		out[6..12].copy_from_slice(&self.mac.0);
		put16(out, 12, ETHERTYPE_IPV4);
		// TCP header + payload.
		let t: usize = ETH_HDR + IPV4_HDR;
		put16(out, t, self.conns[ci].local_port);
		put16(out, t + 2, self.conns[ci].remote_port);
		put32(out, t + 4, seq);
		put32(out, t + 8, ack);
		out[t + 12] = (TCP_HDR as u8 / 4) << 4;
		out[t + 13] = flags;
		put16(out, t + 14, TCP_RX_MAX as u16);
		put16(out, t + 16, 0);
		put16(out, t + 18, 0);
		out[t + TCP_HDR..t + TCP_HDR + payload.len()].copy_from_slice(payload);
		let tcp_csum: u16 = tcp_checksum(self.ip, self.conns[ci].remote_ip, &out[t..t + TCP_HDR + payload.len()]);
		put16(out, t + 16, tcp_csum);
		// IPv4 header.
		let ip: &mut [u8] = &mut out[ETH_HDR..ETH_HDR + IPV4_HDR];
		ip[0] = 0x45;
		ip[1] = 0;
		put16(ip, 2, total as u16);
		put16(ip, 4, 0);
		put16(ip, 6, 0);
		ip[8] = 64;
		ip[9] = IP_PROTO_TCP;
		put16(ip, 10, 0);
		ip[12..16].copy_from_slice(&self.ip.0);
		ip[16..20].copy_from_slice(&self.conns[ci].remote_ip.0);
		let csum: u16 = checksum(&ip[..IPV4_HDR]);
		put16(ip, 10, csum);
		ETH_HDR + total
	}

	// Allocate a free connection slot for a new open (outbound or accepted), marking it
	// in use and reset to a clean Closed state. The pool grows on demand when every slot
	// is in use, so an open never fails for lack of a slot.
	pub fn tcp_alloc(&mut self) -> Option<usize> {
		for (i, c) in self.conns.iter_mut().enumerate() {
			if !c.in_use {
				c.in_use = true;
				c.state = TcpState::Closed;
				c.aborted = false;
				c.peer_fin = false;
				c.pending_accept = false;
				c.rx_len = 0;
				return Some(i);
			}
		}
		let mut fresh: TcpConn = TcpConn::closed();
		fresh.in_use = true;
		self.conns.push(fresh);
		Some(self.conns.len() - 1)
	}

	// Release connection slot `ci` back to the pool (closed and free for reuse).
	pub fn tcp_free(&mut self, ci: usize) {
		let c: &mut TcpConn = &mut self.conns[ci];
		c.in_use = false;
		c.state = TcpState::Closed;
		c.aborted = false;
		c.peer_fin = false;
		c.pending_accept = false;
		c.rx_len = 0;
	}

	// Start accepting inbound connections on `port` (passive open). Idempotent; the
	// listen table grows on demand, so this always succeeds.
	pub fn listen(&mut self, port: u16) -> bool {
		for p in self.listen_ports.iter() {
			if *p == port {
				return true;
			}
		}
		for p in self.listen_ports.iter_mut() {
			if *p == 0 {
				*p = port;
				return true;
			}
		}
		self.listen_ports.push(port);
		true
	}

	// Stop accepting inbound connections on `port`.
	pub fn unlisten(&mut self, port: u16) {
		for p in self.listen_ports.iter_mut() {
			if *p == port {
				*p = 0;
			}
		}
	}

	// Whether we accept inbound connections on `port`.
	fn is_listening(&self, port: u16) -> bool {
		self.listen_ports.iter().any(|&p: &u16| p == port)
	}

	// Take the next established-but-not-yet-handed-out connection accepted on `port`
	// (clearing its pending flag), for the listener's `accept` to hand to a socket.
	pub fn take_accepted(&mut self, port: u16) -> Option<usize> {
		for (i, c) in self.conns.iter_mut().enumerate() {
			if c.in_use && c.pending_accept && c.local_port == port && c.state == TcpState::Established {
				c.pending_accept = false;
				return Some(i);
			}
		}
		None
	}

	// Open connection `ci` to `ip`:`port` (next-hop MAC `mac`) from `local_port` with
	// initial send sequence `iss`, entering SynSent. The caller then sends the SYN.
	// Fields are reset in place (no large by-value temporary on the caller's stack).
	pub fn tcp_open(&mut self, ci: usize, ip: Ipv4Addr, port: u16, mac: MacAddr, local_port: u16, iss: u32) {
		let c: &mut TcpConn = &mut self.conns[ci];
		c.in_use = true;
		c.state = TcpState::SynSent;
		c.aborted = false;
		c.peer_fin = false;
		c.pending_accept = false;
		c.local_port = local_port;
		c.remote_ip = ip;
		c.remote_port = port;
		c.remote_mac = mac;
		c.snd_una = iss;
		c.snd_nxt = iss.wrapping_add(1); // the SYN consumes one sequence number
		c.rcv_nxt = 0;
		c.rx_len = 0;
	}

	// Build connection `ci`'s SYN (seq = the initial send sequence) into `out`.
	pub fn tcp_build_syn(&self, ci: usize, out: &mut [u8]) -> usize {
		self.build_tcp(ci, TCP_SYN, self.conns[ci].snd_una, 0, &[], out)
	}

	// Build a data segment carrying `data` (PSH|ACK) on connection `ci` into `out` and
	// advance its send sequence past it.
	pub fn tcp_build_data(&mut self, ci: usize, data: &[u8], out: &mut [u8]) -> usize {
		let seq: u32 = self.conns[ci].snd_nxt;
		let ack: u32 = self.conns[ci].rcv_nxt;
		let len: usize = self.build_tcp(ci, TCP_PSH | TCP_ACK, seq, ack, data, out);
		self.conns[ci].snd_nxt = self.conns[ci].snd_nxt.wrapping_add(data.len() as u32);
		len
	}

	// Build a FIN|ACK to close our half of connection `ci` into `out`, advancing its
	// send sequence and entering FinWait.
	pub fn tcp_build_fin(&mut self, ci: usize, out: &mut [u8]) -> usize {
		let seq: u32 = self.conns[ci].snd_nxt;
		let ack: u32 = self.conns[ci].rcv_nxt;
		let len: usize = self.build_tcp(ci, TCP_FIN | TCP_ACK, seq, ack, &[], out);
		self.conns[ci].snd_nxt = self.conns[ci].snd_nxt.wrapping_add(1);
		self.conns[ci].state = TcpState::FinWait;
		len
	}

	// Whether connection `ci`'s handshake completed.
	pub fn tcp_established(&self, ci: usize) -> bool {
		self.conns[ci].state == TcpState::Established
	}

	// Whether the peer reset connection `ci` (refused / aborted).
	pub fn tcp_aborted(&self, ci: usize) -> bool {
		self.conns[ci].aborted
	}

	// Whether the peer has closed its half of connection `ci` (sent a FIN).
	pub fn tcp_peer_fin(&self, ci: usize) -> bool {
		self.conns[ci].peer_fin
	}

	// Drain buffered received data from connection `ci` into `dst`, returning the byte
	// count moved.
	pub fn tcp_take_rx(&mut self, ci: usize, dst: &mut [u8]) -> usize {
		let rx_len: usize = self.conns[ci].rx_len;
		let n: usize = rx_len.min(dst.len());
		dst[..n].copy_from_slice(&self.conns[ci].rx[..n]);
		if n < rx_len {
			self.conns[ci].rx.copy_within(n..rx_len, 0);
		}
		self.conns[ci].rx_len -= n;
		n
	}

	// Handle an ICMP message: reply to an echo request, report an echo reply.
	fn on_icmp(&mut self, frame: &[u8], ihl: usize, src_ip: Ipv4Addr, out: &mut [u8]) -> Outcome {
		let icmp: &[u8] = &frame[ETH_HDR + ihl..];
		if icmp.len() < ICMP_HDR {
			return Outcome { reply_len: 0, event: Event::None };
		}
		if icmp[0] == ICMP_ECHO_REQUEST {
			let len: usize = self.build_echo_reply(frame, ihl, src_ip, out);
			return Outcome { reply_len: len, event: Event::None };
		}
		if icmp[0] == ICMP_ECHO_REPLY {
			let ttl: u8 = frame[ETH_HDR + 8];
			let seq: u16 = be16(icmp, 6);
			return Outcome { reply_len: 0, event: Event::EchoReply(src_ip, ttl, seq) };
		}
		Outcome { reply_len: 0, event: Event::None }
	}

	// Turn a received ICMP echo request into its echo reply in `out`: swap the L2/L3
	// addresses, flip the ICMP type, and recompute both checksums.
	fn build_echo_reply(&self, frame: &[u8], ihl: usize, src_ip: Ipv4Addr, out: &mut [u8]) -> usize {
		let ip_total: usize = be16(&frame[ETH_HDR..], 2) as usize;
		let frame_len: usize = ETH_HDR + ip_total;
		if ip_total < ihl + ICMP_HDR || frame_len > frame.len() || frame_len > out.len() {
			return 0;
		}
		// Ethernet: destination = the requester, source = us.
		out[0..6].copy_from_slice(&frame[6..12]);
		out[6..12].copy_from_slice(&self.mac.0);
		put16(out, 12, ETHERTYPE_IPV4);
		out[ETH_HDR..frame_len].copy_from_slice(&frame[ETH_HDR..frame_len]);
		let ip: &mut [u8] = &mut out[ETH_HDR..frame_len];
		// Swap source/destination IP, then recompute the header checksum.
		ip[12..16].copy_from_slice(&self.ip.0);
		ip[16..20].copy_from_slice(&src_ip.0);
		put16(ip, 10, 0);
		let csum: u16 = checksum(&ip[..ihl]);
		put16(ip, 10, csum);
		// ICMP: echo reply, recompute its checksum over type/code/rest + payload.
		let icmp: &mut [u8] = &mut ip[ihl..];
		icmp[0] = ICMP_ECHO_REPLY;
		put16(icmp, 2, 0);
		let csum2: u16 = checksum(icmp);
		put16(icmp, 2, csum2);
		frame_len
	}

	// Build an ICMP echo request to `dst_ip` (whose MAC is `dst_mac`) with the given
	// identifier and sequence, into `out`, returning its length.
	pub fn build_icmp_echo(&self, dst_mac: MacAddr, dst_ip: Ipv4Addr, ident: u16, seq: u16, out: &mut [u8]) -> usize {
		let total: usize = IPV4_HDR + ICMP_HDR + ICMP_PAYLOAD;
		out[0..6].copy_from_slice(&dst_mac.0);
		out[6..12].copy_from_slice(&self.mac.0);
		put16(out, 12, ETHERTYPE_IPV4);
		let ip: &mut [u8] = &mut out[ETH_HDR..ETH_HDR + total];
		ip[0] = 0x45;
		ip[1] = 0;
		put16(ip, 2, total as u16);
		put16(ip, 4, 0);
		put16(ip, 6, 0);
		ip[8] = 64;
		ip[9] = IP_PROTO_ICMP;
		put16(ip, 10, 0);
		ip[12..16].copy_from_slice(&self.ip.0);
		ip[16..20].copy_from_slice(&dst_ip.0);
		let csum: u16 = checksum(&ip[..IPV4_HDR]);
		put16(ip, 10, csum);
		let icmp: &mut [u8] = &mut ip[IPV4_HDR..];
		icmp[0] = ICMP_ECHO_REQUEST;
		icmp[1] = 0;
		put16(icmp, 2, 0);
		put16(icmp, 4, ident);
		put16(icmp, 6, seq);
		// Fill the payload with a recognizable pattern; the checksum below covers it.
		for i in 0..ICMP_PAYLOAD {
			icmp[ICMP_HDR + i] = i as u8;
		}
		let csum2: u16 = checksum(icmp);
		put16(icmp, 2, csum2);
		ETH_HDR + total
	}

	// Build an Ethernet + IPv4 + UDP + DNS A-record query for `name` (sent to the DNS
	// server at `server_ip`, MAC `server_mac`) into `out`, returning its length, or 0
	// if the name does not fit. `txn` is the DNS transaction id and `src_port` our UDP
	// source port (echoed back by the response). The UDP checksum is left 0 (optional
	// for IPv4).
	pub fn build_dns_query(&self, server_mac: MacAddr, server_ip: Ipv4Addr, name: &[u8], txn: u16, src_port: u16, out: &mut [u8]) -> usize {
		let dns_off: usize = ETH_HDR + IPV4_HDR + UDP_HDR;
		if dns_off + 12 + name.len() + 6 > out.len() {
			return 0;
		}
		// DNS header: id, flags (recursion desired), one question, no answers.
		put16(out, dns_off, txn);
		put16(out, dns_off + 2, 0x0100);
		put16(out, dns_off + 4, 1);
		put16(out, dns_off + 6, 0);
		put16(out, dns_off + 8, 0);
		put16(out, dns_off + 10, 0);
		let mut p: usize = dns_off + 12;
		// Question name, encoded as length-prefixed labels split on '.'.
		let mut start: usize = 0;
		for i in 0..=name.len() {
			if i == name.len() || name[i] == b'.' {
				let label: usize = i - start;
				if label == 0 || label > 63 {
					return 0;
				}
				out[p] = label as u8;
				out[p + 1..p + 1 + label].copy_from_slice(&name[start..i]);
				p += 1 + label;
				start = i + 1;
			}
		}
		out[p] = 0;
		p += 1;
		put16(out, p, 1); // qtype A
		put16(out, p + 2, 1); // qclass IN
		p += 4;
		let dns_len: usize = p - dns_off;
		// UDP header.
		let udp_off: usize = ETH_HDR + IPV4_HDR;
		put16(out, udp_off, src_port);
		put16(out, udp_off + 2, DNS_PORT);
		put16(out, udp_off + 4, (UDP_HDR + dns_len) as u16);
		put16(out, udp_off + 6, 0);
		let udp_csum: u16 = udp_checksum(self.ip, server_ip, &out[udp_off..udp_off + UDP_HDR + dns_len]);
		put16(out, udp_off + 6, udp_csum);
		// IPv4 header.
		let total: usize = IPV4_HDR + UDP_HDR + dns_len;
		let ip: &mut [u8] = &mut out[ETH_HDR..ETH_HDR + IPV4_HDR];
		ip[0] = 0x45;
		ip[1] = 0;
		put16(ip, 2, total as u16);
		put16(ip, 4, 0);
		put16(ip, 6, 0);
		ip[8] = 64;
		ip[9] = IP_PROTO_UDP;
		put16(ip, 10, 0);
		ip[12..16].copy_from_slice(&self.ip.0);
		ip[16..20].copy_from_slice(&server_ip.0);
		let csum: u16 = checksum(&ip[..IPV4_HDR]);
		put16(ip, 10, csum);
		// Ethernet header.
		out[0..6].copy_from_slice(&server_mac.0);
		out[6..12].copy_from_slice(&self.mac.0);
		put16(out, 12, ETHERTYPE_IPV4);
		ETH_HDR + total
	}

	// Build an SNTP (NTP) client request to `server_ip` from `src_port` into `out`,
	// returning its length: a 48-byte NTP payload (only the first byte set - LI 0,
	// version 4, mode 3 = client; the rest zero) over UDP / IPv4 / Ethernet.
	pub fn build_sntp_request(&self, server_mac: MacAddr, server_ip: Ipv4Addr, src_port: u16, out: &mut [u8]) -> usize {
		let ntp_off: usize = ETH_HDR + IPV4_HDR + UDP_HDR;
		let ntp_len: usize = 48;
		if ntp_off + ntp_len > out.len() {
			return 0;
		}
		for b in out[ntp_off..ntp_off + ntp_len].iter_mut() {
			*b = 0;
		}
		out[ntp_off] = 0x23; // LI 0, VN 4, Mode 3 (client)
					   // UDP header.
		let udp_off: usize = ETH_HDR + IPV4_HDR;
		put16(out, udp_off, src_port);
		put16(out, udp_off + 2, NTP_PORT);
		put16(out, udp_off + 4, (UDP_HDR + ntp_len) as u16);
		put16(out, udp_off + 6, 0);
		let udp_csum: u16 = udp_checksum(self.ip, server_ip, &out[udp_off..udp_off + UDP_HDR + ntp_len]);
		put16(out, udp_off + 6, udp_csum);
		// IPv4 header.
		let total: usize = IPV4_HDR + UDP_HDR + ntp_len;
		let ip: &mut [u8] = &mut out[ETH_HDR..ETH_HDR + IPV4_HDR];
		ip[0] = 0x45;
		ip[1] = 0;
		put16(ip, 2, total as u16);
		put16(ip, 4, 0);
		put16(ip, 6, 0);
		ip[8] = 64;
		ip[9] = IP_PROTO_UDP;
		put16(ip, 10, 0);
		ip[12..16].copy_from_slice(&self.ip.0);
		ip[16..20].copy_from_slice(&server_ip.0);
		let csum: u16 = checksum(&ip[..IPV4_HDR]);
		put16(ip, 10, csum);
		// Ethernet header.
		out[0..6].copy_from_slice(&server_mac.0);
		out[6..12].copy_from_slice(&self.mac.0);
		put16(out, 12, ETHERTYPE_IPV4);
		ETH_HDR + total
	}

	// Build a DHCP DISCOVER (broadcast, no address yet) into `out`, returning its
	// length.
	pub fn build_dhcp_discover(&self, out: &mut [u8]) -> usize {
		self.build_dhcp(DHCP_DISCOVER, false, None, out)
	}

	// Build a DHCP REQUEST for the offered address into `out` (it carries the offered
	// address and the server id from the last parsed OFFER), returning its length.
	pub fn build_dhcp_request(&self, out: &mut [u8]) -> usize {
		self.build_dhcp(DHCP_REQUEST, false, None, out)
	}

	// Build the lease-extension REQUEST into `out`, returning its length: ciaddr
	// carries our bound address and the requested-address / server-id options are
	// omitted (the RFC 2131 RENEWING / REBINDING form). With `unicast` (the server's
	// resolved MAC) it goes straight to the server - the T1 renewal; without, it
	// broadcasts - the T2 rebinding, any server may extend the lease.
	pub fn build_dhcp_renew(&self, unicast: Option<MacAddr>, out: &mut [u8]) -> usize {
		self.build_dhcp(DHCP_REQUEST, true, unicast, out)
	}

	// Build a DHCP client message of `msg_type` into `out`, returning its length (0
	// if it does not fit). The initial exchange (DISCOVER, the selecting REQUEST)
	// broadcasts from 0.0.0.0 with the broadcast-reply flag, and the REQUEST carries
	// the requested-address and server-id options from the last OFFER. The `renew`
	// form instead fills ciaddr with the bound address and sends from it - unicast
	// to the server when its MAC is known, else broadcast.
	fn build_dhcp(&self, msg_type: u8, renew: bool, unicast: Option<MacAddr>, out: &mut [u8]) -> usize {
		let boot_off: usize = ETH_HDR + IPV4_HDR + UDP_HDR;
		if boot_off + BOOTP_HDR + 32 > out.len() {
			return 0;
		}
		// BOOTP fixed header: zero it, then set the request fields and our MAC.
		for b in out[boot_off..boot_off + BOOTP_HDR].iter_mut() {
			*b = 0;
		}
		out[boot_off] = BOOTP_REQUEST;
		out[boot_off + 1] = 1; // htype: Ethernet
		out[boot_off + 2] = 6; // hlen
		put32(out, boot_off + 4, 0x3903_f326); // xid (fixed; SLIRP is the only DHCP source)
		if renew {
			// ciaddr: the address whose lease this REQUEST extends (we can receive
			// unicast on it, so the broadcast-reply flag stays clear).
			out[boot_off + 12..boot_off + 16].copy_from_slice(&self.ip.0);
		} else {
			put16(out, boot_off + 10, 0x8000); // flags: ask the server to broadcast its reply
		}
		out[boot_off + 28..boot_off + 34].copy_from_slice(&self.mac.0); // chaddr
		// DHCP magic cookie + options.
		let mut p: usize = boot_off + BOOTP_HDR;
		put32(out, p, DHCP_MAGIC);
		p += 4;
		out[p] = DHCP_OPT_MSG_TYPE;
		out[p + 1] = 1;
		out[p + 2] = msg_type;
		p += 3;
		if msg_type == DHCP_REQUEST && !renew {
			out[p] = DHCP_OPT_REQUESTED_IP;
			out[p + 1] = 4;
			out[p + 2..p + 6].copy_from_slice(&self.dhcp.yiaddr.0);
			p += 6;
			out[p] = DHCP_OPT_SERVER_ID;
			out[p + 1] = 4;
			out[p + 2..p + 6].copy_from_slice(&self.dhcp.server.0);
			p += 6;
		}
		out[p] = DHCP_OPT_PARAM_LIST;
		out[p + 1] = 3;
		out[p + 2] = DHCP_OPT_MASK;
		out[p + 3] = DHCP_OPT_ROUTER;
		out[p + 4] = DHCP_OPT_DNS;
		p += 5;
		out[p] = DHCP_OPT_END;
		p += 1;
		let dhcp_len: usize = p - boot_off;
		// UDP header: 0.0.0.0:68 -> 255.255.255.255:67 for the initial exchange, our
		// bound address to the server (or broadcast when rebinding) for a renewal.
		let src: Ipv4Addr = if renew { self.ip } else { Ipv4Addr([0; 4]) };
		let dst: Ipv4Addr = if renew && unicast.is_some() { self.dhcp.server } else { Ipv4Addr([255; 4]) };
		let udp_off: usize = ETH_HDR + IPV4_HDR;
		put16(out, udp_off, DHCP_CLIENT_PORT);
		put16(out, udp_off + 2, DHCP_SERVER_PORT);
		put16(out, udp_off + 4, (UDP_HDR + dhcp_len) as u16);
		put16(out, udp_off + 6, 0);
		let udp_csum: u16 = udp_checksum(src, dst, &out[udp_off..udp_off + UDP_HDR + dhcp_len]);
		put16(out, udp_off + 6, udp_csum);
		// IPv4 header.
		let total: usize = IPV4_HDR + UDP_HDR + dhcp_len;
		let ip: &mut [u8] = &mut out[ETH_HDR..ETH_HDR + IPV4_HDR];
		ip[0] = 0x45;
		ip[1] = 0;
		put16(ip, 2, total as u16);
		put16(ip, 4, 0);
		put16(ip, 6, 0);
		ip[8] = 64;
		ip[9] = IP_PROTO_UDP;
		put16(ip, 10, 0);
		ip[12..16].copy_from_slice(&src.0);
		ip[16..20].copy_from_slice(&dst.0);
		let csum: u16 = checksum(&ip[..IPV4_HDR]);
		put16(ip, 10, csum);
		// Ethernet header: broadcast, or straight to the server for a unicast renewal.
		let dst_mac: MacAddr = unicast.unwrap_or(MacAddr::BROADCAST);
		out[0..6].copy_from_slice(&dst_mac.0);
		out[6..12].copy_from_slice(&self.mac.0);
		put16(out, 12, ETHERTYPE_IPV4);
		ETH_HDR + total
	}

	// Parse a DHCP reply (a BOOTP reply with the magic cookie): record the offered
	// address and the mask / gateway / DNS / server-id options into the stack's lease,
	// returning the DHCP message type (OFFER or ACK), or None if it is not a usable
	// DHCP reply.
	fn parse_dhcp(&mut self, dhcp: &[u8]) -> Option<u8> {
		if dhcp.len() < BOOTP_HDR + 4 || dhcp[0] != BOOTP_REPLY || be32(dhcp, BOOTP_HDR) != DHCP_MAGIC {
			return None;
		}
		let mut lease: DhcpLease = DhcpLease::empty();
		lease.yiaddr = Ipv4Addr([dhcp[16], dhcp[17], dhcp[18], dhcp[19]]);
		let mut msg_type: u8 = 0;
		let mut p: usize = BOOTP_HDR + 4;
		while p < dhcp.len() {
			let code: u8 = dhcp[p];
			if code == DHCP_OPT_END {
				break;
			}
			if code == 0 {
				p += 1;
				continue;
			}
			if p + 2 > dhcp.len() {
				break;
			}
			let len: usize = dhcp[p + 1] as usize;
			if p + 2 + len > dhcp.len() {
				break;
			}
			let val: &[u8] = &dhcp[p + 2..p + 2 + len];
			match code {
				DHCP_OPT_MSG_TYPE if len >= 1 => msg_type = val[0],
				DHCP_OPT_MASK if len >= 4 => lease.mask = Ipv4Addr([val[0], val[1], val[2], val[3]]),
				DHCP_OPT_ROUTER if len >= 4 => lease.gateway = Ipv4Addr([val[0], val[1], val[2], val[3]]),
				DHCP_OPT_DNS if len >= 4 => lease.dns = Ipv4Addr([val[0], val[1], val[2], val[3]]),
				DHCP_OPT_SERVER_ID if len >= 4 => lease.server = Ipv4Addr([val[0], val[1], val[2], val[3]]),
				DHCP_OPT_LEASE_TIME if len >= 4 => lease.lease_secs = be32(val, 0),
				DHCP_OPT_T1 if len >= 4 => lease.t1_secs = be32(val, 0),
				DHCP_OPT_T2 if len >= 4 => lease.t2_secs = be32(val, 0),
				_ => {}
			}
			p += 2 + len;
		}
		if msg_type == 0 {
			return None;
		}
		self.dhcp = lease;
		Some(msg_type)
	}

	// Apply the learned lease as our configuration: take the offered address, and the
	// mask / gateway / DNS where the server provided them.
	pub fn apply_dhcp(&mut self) {
		self.ip = self.dhcp.yiaddr;
		if self.dhcp.mask.0 != [0; 4] {
			self.mask = self.dhcp.mask;
		}
		if self.dhcp.gateway.0 != [0; 4] {
			self.gateway = self.dhcp.gateway;
		}
		if self.dhcp.dns.0 != [0; 4] {
			self.dns = self.dhcp.dns;
		}
	}

	// The lease clock in seconds - (T1 renewal, T2 rebinding, expiry) - with the
	// RFC 2132 defaults applied where the server sent no thresholds (T1 = half the
	// lease, T2 = seven eighths). None when the lease carries no duration or never
	// expires - nothing to renew.
	pub fn dhcp_times(&self) -> Option<(u32, u32, u32)> {
		let lease: u32 = self.dhcp.lease_secs;
		if lease == 0 || lease == DHCP_LEASE_INFINITE || self.dhcp.yiaddr.0 == [0; 4] {
			return None;
		}
		let t1: u32 = if self.dhcp.t1_secs != 0 { self.dhcp.t1_secs } else { lease / 2 };
		let t2: u32 = if self.dhcp.t2_secs != 0 { self.dhcp.t2_secs } else { (lease as u64 * 7 / 8) as u32 };
		Some((t1.min(lease), t2.min(lease), lease))
	}

	// The DHCP server the held lease came from (the renewal REQUEST's unicast
	// destination), 0.0.0.0 when no lease is held.
	pub fn dhcp_server(&self) -> Ipv4Addr {
		self.dhcp.server
	}
}

// Skip a DNS name starting at `off` in `buf`, returning the offset just past it.
// Handles both label sequences (terminated by a zero byte) and the 2-byte
// compression pointer (top two bits set). None if it runs off the end.
fn skip_name(buf: &[u8], mut off: usize) -> Option<usize> {
	loop {
		let b: u8 = *buf.get(off)?;
		if b == 0 {
			return Some(off + 1);
		}
		if b & 0xc0 == 0xc0 {
			return Some(off + 2);
		}
		off += 1 + b as usize;
	}
}

// Parse a DNS response message and return the first A record's address, if any.
fn parse_dns_response(dns: &[u8]) -> Option<Ipv4Addr> {
	if dns.len() < 12 {
		return None;
	}
	let qdcount: u16 = be16(dns, 4);
	let ancount: u16 = be16(dns, 6);
	let mut off: usize = 12;
	for _ in 0..qdcount {
		off = skip_name(dns, off)?;
		off += 4;
		if off > dns.len() {
			return None;
		}
	}
	for _ in 0..ancount {
		off = skip_name(dns, off)?;
		if off + 10 > dns.len() {
			return None;
		}
		let rtype: u16 = be16(dns, off);
		let rdlen: usize = be16(dns, off + 8) as usize;
		off += 10;
		if rtype == 1 && rdlen == 4 && off + 4 <= dns.len() {
			return Some(Ipv4Addr([dns[off], dns[off + 1], dns[off + 2], dns[off + 3]]));
		}
		off += rdlen;
		if off > dns.len() {
			return None;
		}
	}
	None
}

// Parse an SNTP response payload and return the transmit timestamp as a Unix time
// (seconds, UTC): the 64-bit transmit timestamp sits at offset 40, its integer-
// seconds half (since the NTP 1900 epoch) in the first 4 bytes. None if too short.
fn parse_sntp(ntp: &[u8]) -> Option<u64> {
	if ntp.len() < 44 {
		return None;
	}
	let ntp_secs: u32 = be32(ntp, 40);
	if ntp_secs < NTP_UNIX_OFFSET {
		return None;
	}
	Some((ntp_secs - NTP_UNIX_OFFSET) as u64)
}

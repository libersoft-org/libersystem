// A minimal userspace network stack: Ethernet II framing, ARP, IPv4, ICMP echo,
// and a UDP/DNS client. It is the core of NetworkService: the standing
// service owns this stack and receives each Ethernet frame from the frame-mover
// `driver.virtio-net` over a channel, hands it to `Stack::on_frame` (which parses
// it, updates the neighbor cache, and writes an optional reply frame), and sends
// any reply back to the driver to transmit. It carries no device knowledge - the
// driver owns the NIC; the service owns the protocol.

use alloc::vec::Vec;

// The IPv6 host this stack carries for the same link. Sibling module of this one, under the same
// binary: the two protocols share a NIC and a frame channel and nothing else.
use super::ipv6_host::Ipv6Host;
use service_logic::tcp_bind::{BindRefusal, BindTable, Binding, Local};
use service_logic::tcp_close::Closing;
use service_logic::tcp_queue::{AckOutcome, SendQueue};
use service_logic::tcp_rto::Rto;
use service_logic::tcp_window::{CongestionWindow, DuplicateAction, Persist};

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
pub const IP_PROTO_UDP: u8 = 17;
pub const IP_PROTO_TCP: u8 = 6;
// The fixed IPv6 header, which a segment size for that family is measured under.
const IPV6_HDR: usize = 40;

// ICMP message types.
const ICMP_ECHO_REQUEST: u8 = 8;
const ICMP_ECHO_REPLY: u8 = 0;
// The two errors a traceroute reads. A router that drops a datagram because its TTL ran out says
// so with type 11; a host or router that will not forward it at all says so with type 3.
const ICMP_DEST_UNREACHABLE: u8 = 3;
const ICMP_TIME_EXCEEDED: u8 = 11;
// Destination Unreachable, code 4: the datagram was too large for the next hop and carried
// don't-fragment. RFC 1191 puts the hop's MTU in the header's otherwise unused field.
const ICMP_FRAG_NEEDED: u8 = 4;
// The smallest MTU an IPv4 path is required to carry. A report below it is not a path.
const IPV4_MIN_MTU: u16 = 68;

// The DNS server port (UDP).
const DNS_PORT: u16 = 53;

// The NTP / SNTP server port (UDP), and the offset between the NTP epoch (1900) and
// the Unix epoch (1970) in seconds (70 years, 17 of them leap).
pub const NTP_PORT: u16 = 123;
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
// What the Ethernet + IPv4 + TCP headers take out of a frame - what remains of the
// frame buffer is the largest single TCP segment payload.

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

// The receive buffer / advertised window for one TCP connection. Without window
// scaling the header's 16-bit window field caps the advertisement at 65535 bytes
// per round-trip; the WS option (RFC 7323), negotiated on connect and accept,
// lifts it - a connection whose peer offered the option grows its buffer to the
// scaled size and advertises the free space shifted right by our scale. The
// buffer lives inside the heap-pooled connection state.
const TCP_RX_BASE: usize = 65535;
const TCP_WS_SHIFT: u8 = 2;
const TCP_RX_SCALED: usize = TCP_RX_BASE << TCP_WS_SHIFT;

// The initial TCP connection pool size: outbound `connect`s and inbound accepted
// connections share the pool, which grows on demand - a size hint, never a cap.
const TCP_CONN_MAX: usize = 4;

// The segment size assumed before a peer says otherwise: RFC 9293's default for a peer that sends no
// MSS option. Both sides raise it from the option and from the interface, and the sender uses the
// smaller of what the peer will accept and what the path will carry.
const TCP_MSS: u16 = 536;

// The smallest segment size this sender will use however small a peer's option is. A peer offering
// a handful of bytes per segment would otherwise turn every transfer into a header flood, and RFC
// 9293 section 3.7.1 permits a receiver's advertised MSS to be treated as a lower bound of this
// order rather than obeyed to the byte.
const TCP_MSS_FLOOR: u16 = 88;

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
	if c == 0 { 0xffff } else { c }
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

// Whether the TCP options of the segment (a SYN or SYN-ACK) carry the window-scale
// option (kind 3, RFC 7323). We ignore the peer's shift value itself - the stack
// tracks no send window - so the option's presence is all that matters: it licenses
// scaling our own advertised window.
// The largest segment the peer said it will accept, from its SYN's MSS option.
//
// ABSENT MEANS 536, which is RFC 9293's default and not "as large as we like": a peer that sends no
// option has told us nothing about its path, and assuming the interface's own MTU is how a sender
// discovers the answer by having every segment dropped.
fn peer_mss_option(tcp: &[u8]) -> u16 {
	let data_off: usize = ((tcp[12] >> 4) as usize) * 4;
	if data_off < TCP_HDR || data_off > tcp.len() {
		return TCP_MSS;
	}
	let mut i: usize = TCP_HDR;
	while i < data_off {
		match tcp[i] {
			0 => return TCP_MSS,
			1 => i += 1,
			2 if i + 3 < data_off && tcp[i + 1] == 4 => return u16::from_be_bytes([tcp[i + 2], tcp[i + 3]]).max(TCP_MSS_FLOOR),
			_ => {
				if i + 1 >= data_off || tcp[i + 1] < 2 {
					return TCP_MSS;
				}
				i += tcp[i + 1] as usize;
			}
		}
	}
	TCP_MSS
}

fn peer_offers_ws(tcp: &[u8]) -> bool {
	let data_off: usize = ((tcp[12] >> 4) as usize) * 4;
	if data_off < TCP_HDR || data_off > tcp.len() {
		return false;
	}
	let mut i: usize = TCP_HDR;
	while i < data_off {
		match tcp[i] {
			0 => return false,
			1 => i += 1,
			3 => return true,
			_ => {
				if i + 1 >= data_off || tcp[i + 1] < 2 {
					return false;
				}
				i += tcp[i + 1] as usize;
			}
		}
	}
	false
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
// on the heap. The live size is the operator's policy (the `net.arp-cache` config
// key, read by NetworkService at start); this is the default when no configuration
// answers.
pub const NEIGH_MAX: usize = 1024;

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
	// We learned a neighbor's MAC (from an ARP reply for an address we asked about). The pair
	// itself goes into the stack's neighbour table, which is where a reader takes it from.
	Learned,
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
	// A router discarded one of our datagrams because its TTL reached zero, and said so: the
	// router's address and the sequence number of the echo it was carrying.
	//
	// THE SEQUENCE COMES FROM INSIDE THE ERROR. An ICMP error quotes the datagram that caused it -
	// its IP header and the first eight bytes after - and for an echo those eight bytes include the
	// identifier and sequence we sent. That quote is the only thing that ties the answer to the
	// probe: several probes are in flight and every router answers from its own address, so
	// without it a reply cannot be attributed to a hop.
	TimeExceeded(Ipv4Addr, u16),
	// A host or a router refused the datagram outright - no route, a filtered port, an
	// administratively prohibited path. Distinct from a timeout, which says nothing came back at
	// all: one is an answer and the other is silence, and a traceroute that showed them the same
	// way would hide where a path is blocked.
	Unreachable(Ipv4Addr, u16),
}

// The sequence number of the echo an ICMP error is quoting, if it is quoting one.
//
// An error carries `[type, code, checksum, unused(4)]` and then the offending datagram: its IPv4
// header, and at least the first eight bytes after it. For an echo those eight are
// `[type, code, checksum(2), identifier(2), sequence(2)]`, so the sequence is the last two.
//
// EVERY LENGTH IS CHECKED AGAINST THE BUFFER, because this is data from the network describing the
// shape of data from the network: the quoted header's own IHL field says how long it is, and a
// hostile one can say anything. A quote that does not fit, does not carry IPv4, or does not carry
// ICMP is not an answer to a probe - and answering `None` is what makes it not one.
fn quoted_echo_sequence(icmp: &[u8]) -> Option<u16> {
	const ERROR_HEADER: usize = 8;
	let quoted: &[u8] = icmp.get(ERROR_HEADER..)?;
	let first: u8 = *quoted.first()?;
	if first >> 4 != 4 {
		return None;
	}
	let ihl: usize = (first & 0x0f) as usize * 4;
	if ihl < IPV4_HDR {
		return None;
	}
	if *quoted.get(9)? != IP_PROTO_ICMP {
		return None;
	}
	let inner: &[u8] = quoted.get(ihl..)?;
	if inner.len() < ICMP_HDR || inner[0] != ICMP_ECHO_REQUEST {
		return None;
	}
	Some(be16(inner, 6))
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
	// THE FULL KEY, FAMILY INCLUDED. Port 80 in one family and port 80 in the other are different
	// connections, and a table keyed on the port and the low address words would hand one family's
	// segments to the other's control block.
	local: Local,
	remote: Local,
	remote_port: u16,
	remote_mac: MacAddr,
	// Send sequence: oldest unacknowledged, and the next sequence to use.
	// THE HANDSHAKE'S SEQUENCE, and only the handshake's: once the connection is established the
	// transmit queue owns the sequence space, because the bytes and the sequence they occupy are the
	// same fact and keeping them in two places is how they come to disagree.
	snd_una: u32,
	snd_nxt: u32,
	// The sender: what is owed the wire, how long to wait for an acknowledgement, how much may be in
	// flight, what to do about a shut window, and where the closing handshake has got to.
	tx: SendQueue,
	rto: Rto,
	cwnd: CongestionWindow,
	persist: Persist,
	closing: Closing,
	// What the peer said it will hold, already unshifted by its window scale.
	peer_window: u32,
	peer_wscale: u8,
	// The largest segment the peer will accept, and the largest this path will carry. The sender
	// uses the smaller.
	peer_mss: u16,
	path_mss: u16,
	// When the oldest outstanding segment was sent, and when its timer expires.
	sent_at_ms: u64,
	rto_deadline_ms: Option<u64>,
	// The sender gave up: the retry limit expired with data or a FIN outstanding.
	send_failed: bool,
	// Receive sequence: the next in-order byte we expect.
	rcv_nxt: u32,
	// Our window scale (RFC 7323): TCP_WS_SHIFT when the peer offered the WS option
	// in its SYN, 0 otherwise (scaling only applies when both sides sent it).
	rcv_wscale: u8,
	// Received in-order data waiting to be read, and how much. Heap-allocated (the
	// window must never sit on a 16 kB user stack); grown to the scaled size when
	// window scaling is negotiated, shrunk back when the slot is freed.
	rx: Vec<u8>,
	rx_len: usize,
}

impl TcpConn {
	fn closed() -> TcpConn {
		TcpConn { in_use: false, state: TcpState::Closed, aborted: false, peer_fin: false, pending_accept: false, local_port: 0, local: Local::V4([0; 4]), remote: Local::V4([0; 4]), remote_port: 0, remote_mac: MacAddr::ZERO, snd_una: 0, snd_nxt: 0, tx: SendQueue::new(0), rto: Rto::new(), cwnd: CongestionWindow::new(u32::from(TCP_MSS)), persist: Persist::new(), closing: Closing::new(), peer_window: 0, peer_wscale: 0, peer_mss: TCP_MSS, path_mss: TCP_MSS, sent_at_ms: 0, rto_deadline_ms: None, send_failed: false, rcv_nxt: 0, rcv_wscale: 0, rx: alloc::vec![0; TCP_RX_BASE], rx_len: 0 }
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
	// The monotonic clock in milliseconds, set by the serve loop before each pass. See `set_clock`.
	clock_ms: u64,
	// THE IPv6 HOST, CARRIED RATHER THAN THREADED. Every blocking helper in the service pumps frames
	// through this stack, and an IPv6 frame arriving during a DNS wait must be processed rather than
	// dropped - which is exactly what M6's seam requires and what threading a second argument
	// through nine call chains would have made easy to forget in one of them. `None` until the
	// service stands it up, because forming a link-local address needs a secret and a digest this
	// module does not own.
	ipv6: Option<Ipv6Host>,
	mac: MacAddr,
	ip: Ipv4Addr,
	mask: Ipv4Addr,
	gateway: Ipv4Addr,
	dns: Ipv4Addr,
	// The interface MTU the frame buffers were sized by (the smaller of the link's
	// report and the `net.mtu` knob) - rendered by `ip`, bounds a TCP segment.
	mtu: u16,
	neigh: Vec<Neigh>,
	conns: Vec<TcpConn>,
	// The ports we accept inbound connections on (passive open); 0 = unused slot.
	// Grows on demand.
	// The listeners this host holds, and the rule for which claims may share a port.
	listeners: BindTable,
	// The next initial send sequence to hand a passively-opened connection (bumped per
	// accept; predictability is not a concern for this stack).
	next_iss: u32,
	dhcp: DhcpLease,
}

impl Stack {
	pub fn new(mac: MacAddr, ip: Ipv4Addr, mask: Ipv4Addr, gateway: Ipv4Addr, dns: Ipv4Addr, neigh_cap: usize, mtu: u16) -> Stack {
		let mut conns: Vec<TcpConn> = Vec::with_capacity(TCP_CONN_MAX);
		for _ in 0..TCP_CONN_MAX {
			conns.push(TcpConn::closed());
		}
		Stack { ipv6: None, clock_ms: 0, mac, ip, mask, gateway, dns, mtu, neigh: alloc::vec![Neigh { ip: Ipv4Addr([0; 4]), mac: MacAddr::ZERO, valid: false }; neigh_cap.max(1)], conns, listeners: BindTable::new(), next_iss: 0x1000_0000, dhcp: DhcpLease::empty() }
	}

	// Stand the IPv6 host up on this link.
	pub fn attach_ipv6(&mut self, host: Ipv6Host) {
		self.ipv6 = Some(host);
	}

	// The IPv6 host, if one was attached.
	pub fn ipv6(&mut self) -> Option<&mut Ipv6Host> {
		self.ipv6.as_mut()
	}

	// The IPv6 host, read-only, for a report that must not disturb it.
	// THE MONOTONIC CLOCK, SUPPLIED RATHER THAN READ. A retransmission timer needs the time on every
	// path that touches it - an arriving acknowledgement, a segment going out, a timer firing - and a
	// stack that read the clock itself would be a stack no host test could drive through a schedule.
	// It is the same arrangement the IPv6 host beside it uses and for the same reason.
	pub fn set_clock(&mut self, now_ms: u64) {
		self.clock_ms = now_ms;
	}

	pub fn ipv6_ref(&self) -> Option<&Ipv6Host> {
		self.ipv6.as_ref()
	}

	// This interface's identity, as the public contract carries it. Absent only on a boot with no
	// IPv6 host at all, which is a link that refused the family.
	pub fn ipv6_identity(&self) -> Option<(u32, u64)> {
		self.ipv6.as_ref().map(|host| {
			let interface = host.interface();
			(u32::from(interface.index), u64::from(interface.generation))
		})
	}

	// The IPv6 layer's next deadline, in milliseconds, or None when it has nothing pending.
	pub fn ipv6_deadline(&self) -> Option<u64> {
		self.ipv6.as_ref().and_then(|host| host.next_deadline())
	}

	pub fn mac(&self) -> MacAddr {
		self.mac
	}

	// The interface MTU the stack was built with.
	pub fn mtu(&self) -> u16 {
		self.mtu
	}

	pub fn ip(&self) -> Ipv4Addr {
		self.ip
	}

	// The netmask this interface is configured with, so a report can say what it got rather than
	// only that it got something.
	pub fn mask(&self) -> Ipv4Addr {
		self.mask
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
		if self.on_link(dst) { dst } else { self.gateway }
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
		for port in self.listening_ports() {
			out.push(SockEntry { local_port: port, remote_ip: Ipv4Addr([0; 4]), remote_port: 0, state: SockEntryState::Listen });
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
			out.push(SockEntry {
				local_port: c.local_port,
				remote_ip: match c.remote {
					Local::V4(octets) => Ipv4Addr(octets),
					Local::V6(_) => Ipv4Addr([0; 4]),
				},
				remote_port: c.remote_port,
				state,
			});
		}
		out
	}

	// One connection's endpoints: the local port, and the peer it runs to. An accepted connection is
	// handed to its owner with both, which is what lets a server say which of this host's addresses
	// a client reached it on rather than only that somebody connected.
	pub fn conn_endpoints(&self, ci: usize) -> Option<(u16, Ipv4Addr, u16)> {
		let conn = self.conns.get(ci)?;
		conn.in_use.then_some((
			conn.local_port,
			match conn.remote {
				Local::V4(octets) => Ipv4Addr(octets),
				Local::V6(_) => Ipv4Addr([0; 4]),
			},
			conn.remote_port,
		))
	}

	// The count of live (in-use) TCP connections in the pool - what `network.capacity`
	// reports as the connection utilization. The pool grows on demand, so this is a
	// live count, never a fraction of a fixed cap.
	pub fn conn_used(&self) -> usize {
		self.conns.iter().filter(|c| c.in_use).count()
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
			return Outcome { reply_len: 0, event: Event::Learned };
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
	// The frame is trimmed to the IP header's total length first: a short IP packet
	// rides a minimum-size (60-byte) Ethernet frame whose padding would otherwise
	// read as protocol payload - a bare ACK's padding once advanced a TCP window.
	fn on_ipv4(&mut self, frame: &[u8], out: &mut [u8]) -> Outcome {
		let ip: &[u8] = &frame[ETH_HDR..];
		if ip.len() < IPV4_HDR || ip[0] >> 4 != 4 {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let ihl: usize = (ip[0] & 0x0f) as usize * 4;
		if ihl < IPV4_HDR || ip.len() < ihl {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let total: usize = be16(ip, 2) as usize;
		if total < ihl || ip.len() < total {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let frame: &[u8] = &frame[..ETH_HDR + total];
		let ip: &[u8] = &frame[ETH_HDR..];
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

	// An IPv6 UDP datagram, delivered by the host that owns L3.
	//
	// THE CHECKSUM IS MANDATORY HERE AND A ZERO FIELD IS REFUSED. Over IPv4 a zero means "not
	// computed" and a receiver accepts it; IPv6 has no header checksum beneath the transport, so
	// RFC 8200 section 8.1 removes the exemption - and accepting a zero would be accepting a datagram
	// nothing has checked.
	pub fn on_udp6(&mut self, source: [u8; 16], destination: [u8; 16], datagram: &[u8]) -> Event {
		if datagram.len() < UDP_HDR {
			return Event::None;
		}
		let field: u16 = be16(datagram, 6);
		if !service_logic::ipv6_packet::udp_checksum_present(field) {
			return Event::None;
		}
		let from = service_logic::ipv6::Address::new(source);
		let to = service_logic::ipv6::Address::new(destination);
		// The checksum covers the whole datagram with the field zeroed; a copy is the honest way to
		// verify one rather than reaching into the caller's bytes.
		let mut copy: Vec<u8> = datagram.to_vec();
		put16(&mut copy, 6, 0);
		if service_logic::ipv6_packet::udp_checksum(from, to, &copy) != field {
			return Event::None;
		}
		let src_port: u16 = be16(datagram, 0);
		let body: &[u8] = &datagram[UDP_HDR..];
		match src_port {
			DNS_PORT => parse_dns_response(body).map(Event::DnsReply).unwrap_or(Event::None),
			NTP_PORT => parse_sntp(body).map(Event::SntpReply).unwrap_or(Event::None),
			_ => Event::None,
		}
	}

	// Send a UDP datagram over IPv6. Returns false when this host has no address it may use.
	pub fn send_udp6(&mut self, peer: [u8; 16], src_port: u16, dst_port: u16, body: &[u8]) -> bool {
		let destination = service_logic::ipv6::Address::new(peer);
		let Some(source) = self.ipv6.as_ref().and_then(|host| host.source_address(destination)) else {
			return false;
		};
		let mut datagram: Vec<u8> = alloc::vec![0u8; UDP_HDR + body.len()];
		put16(&mut datagram, 0, src_port);
		put16(&mut datagram, 2, dst_port);
		put16(&mut datagram, 4, (UDP_HDR + body.len()) as u16);
		datagram[UDP_HDR..].copy_from_slice(body);
		let checksum: u16 = service_logic::ipv6_packet::udp_checksum(source, destination, &datagram);
		put16(&mut datagram, 6, checksum);
		let now: u64 = self.clock_ms;
		match self.ipv6.as_mut() {
			Some(host) => host.send_transport(destination, source, service_logic::ipv6_packet::NEXT_UDP, datagram, now),
			None => false,
		}
	}

	// Handle an inbound TCP segment: demux it to the live connection it belongs to (by
	// its 4-tuple), complete the handshake (SYN-ACK -> ACK), accept in-order data and
	// acknowledge it, note a peer FIN, and abort on RST. Segments for no live
	// connection are ignored.
	fn on_tcp(&mut self, frame: &[u8], ihl: usize, src_ip: Ipv4Addr, out: &mut [u8]) -> Outcome {
		let tcp: &[u8] = &frame[ETH_HDR + ihl..];
		let remote_mac: MacAddr = MacAddr([frame[6], frame[7], frame[8], frame[9], frame[10], frame[11]]);
		let local: Local = Local::V4(self.ip.0);
		self.on_tcp_segment(Local::V4(src_ip.0), local, tcp, Some(remote_mac), out)
	}

	// An IPv6 TCP segment, delivered by the host that owns L3.
	//
	// THE SAME MACHINERY, KEYED BY A WIDER ADDRESS. Nothing about sequence numbers, windows or the
	// closing handshake is family-specific; what differs is the pseudo-header underneath and who
	// resolves the next hop, and both of those are below this.
	pub fn on_tcp6(&mut self, source: [u8; 16], destination: [u8; 16], segment: &[u8], out: &mut [u8]) -> usize {
		self.on_tcp_segment(Local::V6(source), Local::V6(destination), segment, None, out).reply_len
	}

	// The family-neutral half: everything from the four-tuple lookup onwards.
	fn on_tcp_segment(&mut self, remote: Local, local: Local, tcp: &[u8], remote_mac: Option<MacAddr>, out: &mut [u8]) -> Outcome {
		if tcp.len() < TCP_HDR {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let src_port: u16 = be16(tcp, 0);
		let dst_port: u16 = be16(tcp, 2);
		let ci: usize = match self.find_conn(remote, src_port, local, dst_port) {
			Some(i) => i,
			None => {
				// A SYN to a listening port opens a new inbound connection (passive open).
				let flags: u8 = tcp[13];
				if flags & TCP_SYN != 0 && flags & TCP_ACK == 0 && self.listens_for(local, dst_port) {
					let seg_seq: u32 = be32(tcp, 4);
					let peer_ws: bool = peer_offers_ws(tcp);
					let peer_mss: u16 = peer_mss_option(tcp);
					return self.passive_open(remote, local, src_port, dst_port, seg_seq, peer_ws, peer_mss, remote_mac, out);
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
		// Complete the handshake: a SYN-ACK acknowledging our SYN. The peer echoing
		// the WS option arms window scaling: the receive buffer grows to the scaled
		// size and later segments advertise the shifted window.
		if self.conns[ci].state == TcpState::SynSent {
			if flags & TCP_SYN != 0 && flags & TCP_ACK != 0 && seg_ack == self.conns[ci].snd_nxt {
				self.conns[ci].rcv_nxt = seg_seq.wrapping_add(1);
				self.conns[ci].snd_una = seg_ack;
				self.conns[ci].state = TcpState::Established;
				if peer_offers_ws(tcp) {
					self.conns[ci].rcv_wscale = TCP_WS_SHIFT;
					self.conns[ci].rx.resize(TCP_RX_SCALED, 0);
				}
				self.start_sender(ci, peer_mss_option(tcp), be16(tcp, 14));
				let len: usize = self.emit_tcp(ci, TCP_ACK, self.conns[ci].snd_nxt, self.conns[ci].rcv_nxt, &[], &[], out);
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
				self.start_sender(ci, self.conns[ci].peer_mss, be16(tcp, 14));
			} else {
				return Outcome { reply_len: 0, event: Event::None };
			}
		}
		// Established (or tearing down): the acknowledgement retires what it covers, and the peer's
		// window says how much more may go out.
		if flags & TCP_ACK != 0 {
			self.on_tcp_ack(ci, seg_ack, be16(tcp, 14));
		}
		// Accept in-order data into the receive buffer (bounded by the window). Data
		// at the expected sequence is acknowledged even when the buffer is full and
		// nothing was consumed - the ACK re-advertises the current window, which is
		// also what answers a peer's zero-window probe.
		let mut progressed: bool = false;
		if !payload.is_empty() && seg_seq == self.conns[ci].rcv_nxt {
			let rx_len: usize = self.conns[ci].rx_len;
			let n: usize = payload.len().min(self.conns[ci].rx.len() - rx_len);
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
			let len: usize = self.emit_tcp(ci, TCP_ACK, self.conns[ci].snd_nxt, self.conns[ci].rcv_nxt, &[], &[], out);
			return Outcome { reply_len: len, event: Event::None };
		}
		Outcome { reply_len: 0, event: Event::None }
	}

	// Stand the sender up once the handshake completes.
	//
	// THE QUEUE STARTS WHERE THE HANDSHAKE LEFT OFF. `snd_nxt` is the sequence after the SYN, which
	// is the first byte of data this connection will ever send - so the queue owns the sequence space
	// from here and the handshake's two fields stop being consulted.
	fn start_sender(&mut self, ci: usize, peer_mss: u16, raw_window: u16) {
		let path_mss: u16 = self.mtu.saturating_sub((IPV4_HDR + TCP_HDR) as u16).max(TCP_MSS_FLOOR);
		let first: u32 = self.conns[ci].snd_nxt;
		let c: &mut TcpConn = &mut self.conns[ci];
		c.tx = SendQueue::new(first);
		c.rto = Rto::new();
		c.peer_mss = peer_mss;
		c.path_mss = path_mss;
		// THE SMALLER OF THE TWO, because the peer's option says what it will accept and the
		// interface says what this link will carry; exceeding either drops the segment.
		c.cwnd = CongestionWindow::new(u32::from(peer_mss.min(path_mss)));
		c.persist = Persist::new();
		c.closing = Closing::new();
		// A SYN's window field is never scaled, whatever scale was negotiated.
		c.peer_window = u32::from(raw_window);
		c.peer_wscale = if c.rcv_wscale != 0 { TCP_WS_SHIFT } else { 0 };
		c.rto_deadline_ms = None;
		c.send_failed = false;
	}

	// The segment size in force: the smaller of what the peer accepts and what the path carries.
	fn effective_mss(&self, ci: usize) -> usize {
		usize::from(self.conns[ci].peer_mss.min(self.conns[ci].path_mss))
	}

	// An acknowledgement arrived. Retire what it covers, move the congestion window, and record the
	// peer's advertised window.
	fn on_tcp_ack(&mut self, ci: usize, seg_ack: u32, raw_window: u16) {
		let window: u32 = u32::from(raw_window) << self.conns[ci].peer_wscale;
		self.conns[ci].peer_window = window;
		let flight: u32 = self.conns[ci].tx.flight();
		let snd_nxt: u32 = self.conns[ci].tx.snd_nxt();
		let was_retransmitted: bool = self.conns[ci].tx.outstanding_was_retransmitted();
		match self.conns[ci].tx.on_ack(seg_ack) {
			AckOutcome::Advanced { bytes, covered_fin } => {
				// KARN'S RULE IS APPLIED HERE, where the queue knows whether these bytes had been on
				// the wire before.
				let now: u64 = self.clock_ms;
				let rtt: u32 = now.saturating_sub(self.conns[ci].sent_at_ms).min(u64::from(u32::MAX)) as u32;
				self.conns[ci].rto.sample(rtt, was_retransmitted);
				self.conns[ci].cwnd.on_new_ack(bytes, seg_ack);
				if covered_fin {
					self.conns[ci].closing.on_fin_acknowledged(now);
				}
				// The timer follows what is still outstanding: re-armed for the next segment, or
				// disarmed when nothing is owed.
				if self.conns[ci].tx.flight() == 0 {
					self.conns[ci].rto_deadline_ms = None;
					self.conns[ci].rto.restart();
				} else {
					self.conns[ci].rto_deadline_ms = Some(now + u64::from(self.conns[ci].rto.rto_ms()));
				}
			}
			AckOutcome::Duplicate => {
				let duplicates: u8 = self.conns[ci].tx.duplicates();
				if let DuplicateAction::FastRetransmit = self.conns[ci].cwnd.on_duplicate_ack(flight, snd_nxt) {
					// THE MISSING SEGMENT GOES BACK ON THE WIRE NOW rather than waiting for the
					// timer: three duplicates are three segments that arrived, which is evidence a
					// timeout would take a whole RTO to reach.
					self.conns[ci].tx.rewind();
				}
				let _ = duplicates;
			}
			AckOutcome::Old | AckOutcome::Invalid => {}
		}
		// A window that shut with something still owed starts the probe schedule; one that reopened
		// ends it.
		let waiting: usize = self.conns[ci].tx.pending().len();
		let rto_ms: u32 = self.conns[ci].rto.rto_ms();
		let now: u64 = self.clock_ms;
		self.conns[ci].persist.on_window(window, waiting, now, rto_ms);
	}

	// The soonest any live connection needs waking, across the whole pool.
	//
	// ONE DEADLINE FOR THE WHOLE STACK, because the serve loop waits once: a per-connection timer
	// that the loop did not know about would fire only when some other event happened to wake it,
	// which is a retransmission schedule decided by unrelated traffic.
	pub fn tcp_deadline_any(&self) -> Option<u64> {
		let mut soonest: Option<u64> = None;
		for index in 0..self.conns.len() {
			if let Some(deadline) = self.tcp_deadline(index) {
				soonest = Some(match soonest {
					Some(held) => held.min(deadline),
					None => deadline,
				});
			}
		}
		soonest
	}

	// Every connection whose timer has expired.
	pub fn tcp_due(&mut self, now_ms: u64) -> Vec<usize> {
		self.clock_ms = now_ms;
		let mut due: Vec<usize> = Vec::new();
		for index in 0..self.conns.len() {
			if self.conns[index].in_use && self.tcp_deadline(index).is_some_and(|deadline| now_ms >= deadline) {
				due.push(index);
			}
		}
		due
	}

	// The unacknowledged bytes every connection is holding together, which is what the service-wide
	// budget is measured against.
	pub fn tcp_outstanding_total(&self) -> usize {
		self.conns.iter().filter(|c| c.in_use).map(|c| c.tx.pending().len()).sum()
	}

	// A validated ICMPv6 error the IPv6 host handed up. Returns whether it changed anything.
	//
	// THE CONSUMER IS HERE BECAUSE THE FLOW STATE IS HERE. The layer below validated the quotation as
	// far as a layer holding no flow state can - the type, the code, and that the quoted source is an
	// address this interface holds - and deliberately stopped. Whether this host actually SENT the
	// quoted packet is a lookup in a send queue, and the send queue is this table.
	pub fn on_ipv6_error(&mut self, error: &service_logic::ipv6_events::QuotedError) -> bool {
		use service_logic::ipv6_events::{ErrorClass, QuotedTransport};
		use service_logic::tcp_flow::{FlowKey, PathMtu, Quotation, QuotedKind, path_mtu_for_tcp};
		let ErrorClass::PacketTooBig { mtu } = error.class else {
			return false;
		};
		let QuotedTransport::Tcp { source_port, destination_port, sequence } = error.transport else {
			return false;
		};
		let quote = Quotation { responder: Local::V6(error.reporter.octets()), local: Local::V6(error.quoted_source.octets()), local_port: source_port, remote: Local::V6(error.quoted_destination.octets()), remote_port: destination_port, interface_generation: error.interface.generation, transport: QuotedKind::Tcp { sequence } };
		for index in 0..self.conns.len() {
			if !self.conns[index].in_use {
				continue;
			}
			let flow = FlowKey { local: self.conns[index].local, local_port: self.conns[index].local_port, remote: self.conns[index].remote, remote_port: self.conns[index].remote_port, interface_generation: self.ipv6.as_ref().map(|host| host.interface().generation).unwrap_or(0) };
			let current: u32 = u32::from(self.conns[index].path_mss) + (IPV6_HDR + TCP_HDR) as u32;
			let snd_una: u32 = self.conns[index].tx.snd_una();
			let snd_nxt: u32 = self.conns[index].tx.snd_nxt();
			// THE ROUTE MUST STILL BE THERE. A report about a path this flow no longer takes has
			// nothing to lower.
			let route_live: bool = match self.conns[index].remote {
				Local::V6(peer) => self.ipv6.as_ref().is_some_and(|host| host.route_for(service_logic::ipv6::Address::new(peer), self.clock_ms).is_some()),
				Local::V4(_) => false,
			};
			if let PathMtu::Apply(limit) = path_mtu_for_tcp(&flow, &quote, snd_una, snd_nxt, route_live, mtu, current) {
				// THE FLOW KEEPS WHAT IT VALIDATED WHATEVER THE CACHE SAYS. The bounded path-MTU
				// cache may refuse the write; cache exhaustion must never restore a larger limit the
				// path has already refused to carry.
				let now: u64 = self.clock_ms;
				if let Some(host) = self.ipv6.as_mut() {
					host.record_path_mtu(
						service_logic::ipv6::Address::new(match flow.remote {
							Local::V6(peer) => peer,
							Local::V4(_) => [0; 16],
						}),
						limit,
						now,
					);
				}
				return self.tcp_path_mtu6(index, limit);
			}
		}
		false
	}

	// Lower an IPv6 connection's segment size and resegment what is outstanding.
	fn tcp_path_mtu6(&mut self, ci: usize, mtu: u32) -> bool {
		let mss: u16 = (mtu.saturating_sub((IPV6_HDR + TCP_HDR) as u32).max(u32::from(TCP_MSS_FLOOR))).min(u32::from(u16::MAX)) as u16;
		if mss >= self.conns[ci].path_mss {
			return false;
		}
		self.conns[ci].path_mss = mss;
		let effective: u32 = u32::from(mss.min(self.conns[ci].peer_mss));
		self.conns[ci].cwnd.set_smss(effective);
		self.conns[ci].tx.rewind();
		true
	}

	// A validated Packet Too Big for `ip`: lower the segment size of every connection to it and put
	// their outstanding bytes back on the wire cut to fit.
	pub fn tcp_on_path_mtu(&mut self, ip: Ipv4Addr, mtu: u16) -> bool {
		let mut changed: bool = false;
		for index in 0..self.conns.len() {
			if self.conns[index].in_use && self.conns[index].remote == Local::V4(ip.0) {
				changed |= self.tcp_path_mtu(index, mtu);
			}
		}
		changed
	}

	// Put one TCP segment for connection `ci` on the wire, whichever family it is.
	//
	// THE FAMILIES DIFFER BELOW THE SEGMENT AND NOWHERE ABOVE IT. The sequence numbers, the flags and
	// the payload are the same; what changes is the pseudo-header the checksum covers and who builds
	// the frame around it. An IPv4 segment is written into the caller's transmit buffer as before; an
	// IPv6 one is handed to the host that owns L3, which resolves the next hop and queues it. The
	// return is the IPv4 frame's length, and zero for IPv6 - the caller sends what it was given and
	// the host's queue carries the rest.
	fn emit_tcp(&mut self, ci: usize, flags: u8, seq: u32, ack: u32, opts: &[u8], payload: &[u8], out: &mut [u8]) -> usize {
		match self.conns[ci].local {
			Local::V4(_) => self.build_tcp_opts(ci, flags, seq, ack, opts, payload, out),
			Local::V6(local) => {
				let Local::V6(remote) = self.conns[ci].remote else {
					return 0;
				};
				let source = service_logic::ipv6::Address::new(local);
				let destination = service_logic::ipv6::Address::new(remote);
				let segment: Vec<u8> = self.build_tcp_segment(ci, flags, seq, ack, opts, payload, source, destination);
				let now: u64 = self.clock_ms;
				if let Some(host) = self.ipv6.as_mut() {
					host.send_transport(destination, source, IP_PROTO_TCP, segment, now);
				}
				0
			}
		}
	}

	// The TCP segment itself - header, options, payload - with the checksum its family's
	// pseudo-header produces.
	#[allow(clippy::too_many_arguments)]
	fn build_tcp_segment(&self, ci: usize, flags: u8, seq: u32, ack: u32, opts: &[u8], payload: &[u8], source: service_logic::ipv6::Address, destination: service_logic::ipv6::Address) -> Vec<u8> {
		let hdr: usize = TCP_HDR + opts.len();
		let mut segment: Vec<u8> = alloc::vec![0u8; hdr + payload.len()];
		put16(&mut segment, 0, self.conns[ci].local_port);
		put16(&mut segment, 2, self.conns[ci].remote_port);
		put32(&mut segment, 4, seq);
		put32(&mut segment, 8, ack);
		segment[12] = ((hdr / 4) as u8) << 4;
		segment[13] = flags;
		let free: usize = self.conns[ci].rx.len() - self.conns[ci].rx_len;
		// A SYN'S WINDOW FIELD IS NEVER SCALED, whatever scale is being negotiated in it.
		let advertised: usize = if flags & TCP_SYN != 0 { free.min(0xffff) } else { free >> self.conns[ci].rcv_wscale };
		put16(&mut segment, 14, advertised.min(0xffff) as u16);
		segment[hdr - opts.len()..hdr].copy_from_slice(opts);
		segment[hdr..].copy_from_slice(payload);
		let checksum: u16 = service_logic::ipv6_packet::pseudo_header_checksum(source, destination, IP_PROTO_TCP, &segment);
		put16(&mut segment, 16, checksum);
		segment
	}

	// The IPv4 peer of a connection that has one. The v4 frame builder needs an `Ipv4Addr`, and a
	// connection of the other family never reaches it.
	fn remote_v4(&self, ci: usize) -> Ipv4Addr {
		match self.conns[ci].remote {
			Local::V4(octets) => Ipv4Addr(octets),
			Local::V6(_) => Ipv4Addr([0; 4]),
		}
	}

	// Find the live connection an inbound segment belongs to (matched by its 4-tuple;
	// our address is fixed, so local_port plus the remote address/port), or None.
	fn find_conn(&self, remote: Local, remote_port: u16, local: Local, local_port: u16) -> Option<usize> {
		for (i, c) in self.conns.iter().enumerate() {
			if c.in_use && c.state != TcpState::Closed && c.remote == remote && c.remote_port == remote_port && c.local == local && c.local_port == local_port {
				return Some(i);
			}
		}
		None
	}

	// Open an inbound connection from a SYN to a listening port: allocate a pool slot,
	// record the peer (its address and source MAC), enter SynRcvd, and build the SYN-ACK
	// (carrying our MSS and, when the peer offered it, the WS option) into `out`. No
	// reply if the pool is full.
	#[allow(clippy::too_many_arguments)]
	fn passive_open(&mut self, remote: Local, local: Local, src_port: u16, dst_port: u16, seg_seq: u32, peer_ws: bool, peer_mss: u16, remote_mac: Option<MacAddr>, out: &mut [u8]) -> Outcome {
		let ci: usize = match self.tcp_alloc() {
			Some(i) => i,
			None => return Outcome { reply_len: 0, event: Event::None },
		};
		let iss: u32 = self.next_iss;
		self.next_iss = self.next_iss.wrapping_add(0x1000);
		let c: &mut TcpConn = &mut self.conns[ci];
		c.state = TcpState::SynRcvd;
		c.local_port = dst_port;
		c.remote = remote;
		c.local = local;
		c.remote_port = src_port;
		// AN IPv6 PEER HAS NO LEARNED MAC HERE. Its next hop is the IPv6 host's to resolve, and a
		// MAC copied off the arriving frame would be the ROUTER's on an off-link connection - which
		// is right only until the route changes.
		c.remote_mac = remote_mac.unwrap_or(MacAddr::ZERO);
		c.peer_mss = peer_mss;
		c.rcv_nxt = seg_seq.wrapping_add(1);
		c.snd_una = iss;
		c.snd_nxt = iss.wrapping_add(1);
		c.rx_len = 0;
		if peer_ws {
			c.rcv_wscale = TCP_WS_SHIFT;
			c.rx.resize(TCP_RX_SCALED, 0);
		}
		let snd: u32 = self.conns[ci].snd_una;
		let rcv: u32 = self.conns[ci].rcv_nxt;
		let opts: [u8; 8] = self.syn_options();
		let len: usize = self.emit_tcp(ci, TCP_SYN | TCP_ACK, snd, rcv, if peer_ws { &opts } else { &opts[..4] }, &[], out);
		Outcome { reply_len: len, event: Event::None }
	}

	// The TCP options our SYN and SYN-ACK carry: MSS (what a segment to us may carry,
	// the MTU minus the IP and TCP headers) and the WS option with our shift, NOP-padded
	// to a 4-byte boundary. A SYN-ACK answering a peer without WS truncates to the MSS
	// half (scaling only applies when both sides sent the option).
	fn syn_options(&self) -> [u8; 8] {
		let mss: u16 = self.mtu.saturating_sub((IPV4_HDR + TCP_HDR) as u16);
		[2, 4, (mss >> 8) as u8, mss as u8, 3, 3, TCP_WS_SHIFT, 1]
	}

	// Build an Ethernet + IPv4 + TCP segment to connection `ci`'s peer with `flags`,
	// sequence `seq`, acknowledgement `ack`, and `payload`, into `out`, returning its
	// length (0 if it does not fit). No TCP options are emitted (a 20-byte header).
	fn build_tcp(&self, ci: usize, flags: u8, seq: u32, ack: u32, payload: &[u8], out: &mut [u8]) -> usize {
		self.build_tcp_opts(ci, flags, seq, ack, &[], payload, out)
	}

	// `build_tcp` with TCP options: `opts` follows the fixed header (its length must
	// be a multiple of 4, NOP-padded by the caller) and widens the data offset. The
	// advertised window is the receive buffer's free space shifted right by the
	// connection's window scale - except on a SYN, where the field is never scaled.
	#[allow(clippy::too_many_arguments)]
	fn build_tcp_opts(&self, ci: usize, flags: u8, seq: u32, ack: u32, opts: &[u8], payload: &[u8], out: &mut [u8]) -> usize {
		let hdr: usize = TCP_HDR + opts.len();
		let total: usize = IPV4_HDR + hdr + payload.len();
		if ETH_HDR + total > out.len() {
			return 0;
		}
		let c: &TcpConn = &self.conns[ci];
		let window: usize = if flags & TCP_SYN != 0 { c.rx.len() } else { (c.rx.len() - c.rx_len) >> c.rcv_wscale };
		out[0..6].copy_from_slice(&c.remote_mac.0);
		out[6..12].copy_from_slice(&self.mac.0);
		put16(out, 12, ETHERTYPE_IPV4);
		// TCP header + options + payload.
		let t: usize = ETH_HDR + IPV4_HDR;
		put16(out, t, c.local_port);
		put16(out, t + 2, c.remote_port);
		put32(out, t + 4, seq);
		put32(out, t + 8, ack);
		out[t + 12] = (hdr as u8 / 4) << 4;
		out[t + 13] = flags;
		put16(out, t + 14, window.min(65535) as u16);
		put16(out, t + 16, 0);
		put16(out, t + 18, 0);
		out[t + TCP_HDR..t + hdr].copy_from_slice(opts);
		out[t + hdr..t + hdr + payload.len()].copy_from_slice(payload);
		let peer: Ipv4Addr = match c.remote {
			Local::V4(octets) => Ipv4Addr(octets),
			Local::V6(_) => Ipv4Addr([0; 4]),
		};
		let tcp_csum: u16 = tcp_checksum(self.ip, peer, &out[t..t + hdr + payload.len()]);
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
		ip[16..20].copy_from_slice(&self.remote_v4(ci).0);
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
		c.rcv_wscale = 0;
		// a scaled buffer shrinks back to the base size so idle slots stay small.
		if c.rx.len() > TCP_RX_BASE {
			c.rx = alloc::vec![0; TCP_RX_BASE];
		}
	}

	// Start accepting inbound connections for `binding`, or say why not.
	//
	// THE MATRIX DECIDES, AND IT IS ONE FUNCTION. Which claims may share a port is a rule the
	// admission and the demultiplexer are both written against; this is the admission half and
	// `listens_for` is the other, and they read the same table.
	pub fn listen(&mut self, binding: Binding) -> Result<(), BindRefusal> {
		self.listeners.bind(binding)
	}

	// Stop accepting inbound connections for `binding`.
	pub fn unlisten(&mut self, binding: &Binding) {
		self.listeners.unbind(binding);
	}

	// Every port this host is listening on, whatever family or address the claim names.
	pub fn listening_ports(&self) -> Vec<u16> {
		let mut ports: Vec<u16> = Vec::new();
		for port in 0..=u16::MAX {
			if self.listeners.lookup(Local::V4([0; 4]), port).is_some() || self.listeners.lookup(Local::V6([0; 16]), port).is_some() {
				ports.push(port);
			}
		}
		ports
	}

	// Whether we accept inbound connections on `port`.
	fn listens_for(&self, local: Local, port: u16) -> bool {
		self.listeners.lookup(local, port).is_some()
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
		c.remote = Local::V4(ip.0);
		c.local = Local::V4(self.ip.0);
		c.remote_port = port;
		c.remote_mac = mac;
		c.snd_una = iss;
		c.snd_nxt = iss.wrapping_add(1); // the SYN consumes one sequence number
		c.rcv_nxt = 0;
		c.rx_len = 0;
	}

	// Open an outbound IPv6 connection. Returns false when this host has no address it may use to
	// reach `peer`, which is the honest answer on a link whose IPv6 never came up.
	pub fn tcp_open6(&mut self, ci: usize, peer: [u8; 16], port: u16, local_port: u16, iss: u32) -> bool {
		let destination = service_logic::ipv6::Address::new(peer);
		let Some(source) = self.ipv6.as_ref().and_then(|host| host.source_address(destination)) else {
			return false;
		};
		let c: &mut TcpConn = &mut self.conns[ci];
		c.in_use = true;
		c.state = TcpState::SynSent;
		c.aborted = false;
		c.peer_fin = false;
		c.pending_accept = false;
		c.local_port = local_port;
		c.remote = Local::V6(peer);
		c.local = Local::V6(source.octets());
		c.remote_port = port;
		// NO MAC. The IPv6 host resolves the next hop when the segment goes out, which is what keeps
		// an off-link connection following the route rather than a MAC copied at open time.
		c.remote_mac = MacAddr::ZERO;
		c.snd_una = iss;
		c.snd_nxt = iss.wrapping_add(1);
		c.rcv_nxt = 0;
		c.rx_len = 0;
		true
	}

	// Build connection `ci`'s SYN (seq = the initial send sequence) into `out`.
	pub fn tcp_build_syn(&mut self, ci: usize, out: &mut [u8]) -> usize {
		let opts: [u8; 8] = self.syn_options();
		let seq: u32 = self.conns[ci].snd_una;
		self.emit_tcp(ci, TCP_SYN, seq, 0, &opts, &[], out)
	}

	// Hand `data` to connection `ci`'s transmit queue, and say how much was taken.
	//
	// THE COUNT IS BYTES ACCEPTED, NOT BYTES ACKNOWLEDGED, and they are COPIED before this returns -
	// so a caller may reuse its buffer immediately. Fewer than offered is the backpressure; zero is
	// what a caller turns into `Again`. The old path built ONE segment from the caller's buffer,
	// sent it once and reported the whole length as sent, which was a data-loss bug wearing a
	// success.
	pub fn tcp_send(&mut self, ci: usize, data: &[u8], aggregate_remaining: usize) -> usize {
		if self.conns[ci].send_failed || self.conns[ci].tx.fin_queued() {
			return 0;
		}
		self.conns[ci].tx.accept(data, aggregate_remaining)
	}

	// Queue the FIN behind everything already accepted.
	pub fn tcp_close_half(&mut self, ci: usize) {
		if self.conns[ci].tx.queue_fin() {
			self.conns[ci].state = TcpState::FinWait;
		}
	}

	// Build the next segment connection `ci` owes the wire into `out`, or 0 when it owes nothing
	// right now.
	//
	// ONE SEGMENT PER CALL, because the caller owns the transmit buffer and the frame channel; it
	// calls again until this returns nothing. What comes out is decided here: a zero-window probe if
	// the persist timer is due, otherwise as much queued data as the congestion window and the
	// peer's window jointly allow, cut to the segment size in force.
	pub fn tcp_pump(&mut self, ci: usize, out: &mut [u8]) -> usize {
		if !self.conns[ci].in_use || self.conns[ci].send_failed {
			return 0;
		}
		let now: u64 = self.clock_ms;
		// A ZERO-WINDOW PROBE IS ONE BYTE PAST THE WINDOW, deliberately: it is the segment the peer
		// must acknowledge, and its acknowledgement carries the window update that was lost.
		if self.conns[ci].persist.fire(now) {
			let sequence: u32 = self.conns[ci].tx.snd_nxt();
			let offset: usize = self.conns[ci].tx.flight() as usize;
			let Some(byte) = self.conns[ci].tx.pending().get(offset).copied() else {
				return 0;
			};
			let ack: u32 = self.conns[ci].rcv_nxt;
			return self.emit_tcp(ci, TCP_ACK, sequence, ack, &[], &[byte], out);
		}
		let flight: u32 = self.conns[ci].tx.flight();
		let usable: u32 = self.conns[ci].cwnd.usable(flight, self.conns[ci].peer_window);
		let mss: usize = self.effective_mss(ci);
		let Some(segment) = self.conns[ci].tx.next_segment(mss, usable) else {
			return 0;
		};
		let flags: u8 = if segment.fin { TCP_FIN | TCP_ACK } else { TCP_PSH | TCP_ACK };
		let ack: u32 = self.conns[ci].rcv_nxt;
		let payload: Vec<u8> = self.conns[ci].tx.pending()[segment.offset..segment.offset + segment.len].to_vec();
		let len: usize = self.emit_tcp(ci, flags, segment.sequence, ack, &[], &payload, out);
		// A LENGTH OF ZERO IS NOT ALWAYS "NOTHING WENT OUT": an IPv6 segment is queued on the host
		// rather than written here, so only an IPv4 connection can report failure this way.
		if len == 0 && matches!(self.conns[ci].local, Local::V4(_)) {
			return 0;
		}
		if segment.fin {
			self.conns[ci].closing.on_fin_sent(now);
		}
		// THE TIMER IS ARMED FROM THE FIRST SEGMENT OUTSTANDING, not from the last: what it is
		// waiting for is the oldest unacknowledged byte, and re-arming on every send would let a
		// steady stream of new data postpone a retransmission for ever.
		if flight == 0 {
			self.conns[ci].sent_at_ms = now;
			self.conns[ci].rto_deadline_ms = Some(now + u64::from(self.conns[ci].rto.rto_ms()));
		}
		len
	}

	// When connection `ci` next needs waking, if it does.
	pub fn tcp_deadline(&self, ci: usize) -> Option<u64> {
		let c: &TcpConn = &self.conns[ci];
		if !c.in_use {
			return None;
		}
		let mut deadline: Option<u64> = c.rto_deadline_ms;
		for candidate in [c.persist.due_ms(), c.closing.deadline_ms()] {
			deadline = match (deadline, candidate) {
				(Some(held), Some(next)) => Some(held.min(next)),
				(held, None) => held,
				(None, next) => next,
			};
		}
		deadline
	}

	// A timer fired on connection `ci`. Returns whether it has anything to put on the wire.
	//
	// A RETRANSMISSION TIMEOUT SAYS NOTHING IS GETTING THROUGH: the congestion window drops to one
	// segment, the timeout doubles, and everything outstanding is sent again from the oldest byte.
	// Past the retry limit the connection fails with a typed error rather than retrying for ever.
	pub fn tcp_on_timer(&mut self, ci: usize) -> bool {
		if !self.conns[ci].in_use {
			return false;
		}
		let now: u64 = self.clock_ms;
		// A control block in TIME-WAIT is released by its own timer and nothing else.
		if self.conns[ci].closing.tick(now) && self.conns[ci].state == TcpState::FinWait {
			self.conns[ci].state = TcpState::Closed;
			return false;
		}
		let Some(deadline) = self.conns[ci].rto_deadline_ms else {
			return self.conns[ci].persist.due_ms().is_some_and(|due| now >= due);
		};
		if now < deadline {
			return self.conns[ci].persist.due_ms().is_some_and(|due| now >= due);
		}
		if self.conns[ci].rto.back_off() >= service_logic::tcp_rto::MAX_DATA_RETRANSMISSIONS {
			// The retry limit expired with data or a FIN outstanding. That is a typed failure, not a
			// quiet close: the caller asked for bytes to arrive and they did not.
			self.conns[ci].send_failed = true;
			self.conns[ci].state = TcpState::Closed;
			self.conns[ci].rto_deadline_ms = None;
			return false;
		}
		let flight: u32 = self.conns[ci].tx.flight();
		self.conns[ci].cwnd.on_timeout(flight);
		self.conns[ci].tx.rewind();
		self.conns[ci].rto_deadline_ms = Some(now + u64::from(self.conns[ci].rto.rto_ms()));
		self.conns[ci].sent_at_ms = now;
		true
	}

	// Did the sender give up on connection `ci`?
	pub fn tcp_send_failed(&self, ci: usize) -> bool {
		self.conns[ci].send_failed
	}

	// Has everything this side owes been acknowledged, FIN included?
	pub fn tcp_fully_acknowledged(&self, ci: usize) -> bool {
		self.conns[ci].tx.fully_acknowledged()
	}

	// Lower the segment size after a validated Packet Too Big and put the outstanding bytes back on
	// the wire cut to fit.
	//
	// RESEGMENTATION IS THE REWIND. There is no separate mechanism: the unsent boundary goes back to
	// the oldest unacknowledged byte and the queue is cut at whatever size is in force now.
	pub fn tcp_path_mtu(&mut self, ci: usize, mtu: u16) -> bool {
		let mss: u16 = mtu.saturating_sub((IPV4_HDR + TCP_HDR) as u16).max(TCP_MSS_FLOOR);
		if mss >= self.conns[ci].path_mss {
			return false;
		}
		self.conns[ci].path_mss = mss;
		let effective: u32 = u32::from(mss.min(self.conns[ci].peer_mss));
		self.conns[ci].cwnd.set_smss(effective);
		self.conns[ci].tx.rewind();
		true
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

	// Drain EVERYTHING buffered on connection `ci` in one move: the chunk is as
	// large as the connection's own receive buffer holds (its runtime occupancy,
	// bounded by the ring's capacity) - no wire constant stands between the buffer
	// and the consumer. Empty when nothing is buffered.
	pub fn tcp_take_rx_all(&mut self, ci: usize) -> Vec<u8> {
		let rx_len: usize = self.conns[ci].rx_len;
		let taken: Vec<u8> = self.conns[ci].rx[..rx_len].to_vec();
		self.conns[ci].rx_len = 0;
		taken
	}

	// Build a bare ACK re-advertising the connection's current receive window into
	// `out` - sent after a drain empties the buffer, so a peer that filled the
	// advertised window learns it reopened instead of stalling on zero-window
	// probes. Empty (0) when the connection is not established.
	pub fn tcp_build_window_update(&mut self, ci: usize, out: &mut [u8]) -> usize {
		if !self.conns[ci].in_use || self.conns[ci].state != TcpState::Established {
			return 0;
		}
		let seq: u32 = self.conns[ci].snd_nxt;
		let ack: u32 = self.conns[ci].rcv_nxt;
		self.build_tcp(ci, TCP_ACK, seq, ack, &[], out)
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
		// THE TWO ERRORS A TRACEROUTE IS MADE OF. Each quotes the datagram that caused it, and the
		// quote is what attributes the answer to a probe - see `Event::TimeExceeded`.
		// FRAGMENTATION NEEDED IS A PATH MTU REPORT, and it is the one ICMP message a TCP sender must
		// act on rather than merely report: the segments it is sending are too large for a hop, and
		// nothing else will tell it so. The quoted packet's destination names the flow it is about,
		// which is the correlation this host can make without keeping a flow table.
		if icmp[0] == ICMP_DEST_UNREACHABLE && icmp.len() >= ICMP_HDR && icmp[1] == ICMP_FRAG_NEEDED {
			let reported: u16 = be16(icmp, 6);
			if let Some(quoted) = icmp.get(8..)
				&& quoted.len() >= IPV4_HDR
			{
				let destination: Ipv4Addr = Ipv4Addr([quoted[16], quoted[17], quoted[18], quoted[19]]);
				// A ZERO NEXT-HOP MTU IS AN OLD ROUTER that predates RFC 1191's field. There is
				// nothing to act on, and guessing a number would be inventing the report.
				if reported >= IPV4_MIN_MTU {
					self.tcp_on_path_mtu(destination, reported);
				}
			}
		}
		if icmp[0] == ICMP_TIME_EXCEEDED || icmp[0] == ICMP_DEST_UNREACHABLE {
			let Some(seq) = quoted_echo_sequence(icmp) else {
				return Outcome { reply_len: 0, event: Event::None };
			};
			let event = if icmp[0] == ICMP_TIME_EXCEEDED { Event::TimeExceeded(src_ip, seq) } else { Event::Unreachable(src_ip, seq) };
			return Outcome { reply_len: 0, event };
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
		self.build_icmp_echo_ttl(dst_mac, dst_ip, ident, seq, 64, out)
	}

	// The same echo with the IP TTL chosen by the caller.
	//
	// THE TTL IS THE WHOLE OF A TRACEROUTE. A datagram sent with a TTL of `n` is discarded by the
	// `n`-th router on the path, which reports itself - so the route is discovered one hop at a
	// time by a field that already existed for a different reason. It is a parameter here rather
	// than a second builder because everything else about the packet is identical, and two builders
	// that differ in one byte are two things that can drift.
	pub fn build_icmp_echo_ttl(&self, dst_mac: MacAddr, dst_ip: Ipv4Addr, ident: u16, seq: u16, ttl: u8, out: &mut [u8]) -> usize {
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
		ip[8] = ttl;
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

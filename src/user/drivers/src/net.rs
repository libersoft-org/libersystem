// A minimal userspace network stack for driver.virtio-net: Ethernet II framing,
// ARP, IPv4, and ICMP echo. It is deliberately housed inside the net driver for
// phase-2 M32 (the receive path plus the lowest layers); M33 extracts it into a
// standing NetworkService with typed sockets. The driver hands each received
// Ethernet frame to `Stack::on_frame`, which parses it, updates the neighbor cache,
// and writes an optional reply frame for the driver to transmit.

#![allow(dead_code)]

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

// ICMP message types.
const ICMP_ECHO_REQUEST: u8 = 8;
const ICMP_ECHO_REPLY: u8 = 0;

// The DNS server port (UDP).
const DNS_PORT: u16 = 53;

// Header sizes (bytes).
const ETH_HDR: usize = 14;
const ARP_LEN: usize = 28;
const IPV4_HDR: usize = 20;
const ICMP_HDR: usize = 8;
const UDP_HDR: usize = 8;

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

// One entry of the small ARP neighbor cache (IPv4 -> MAC).
#[derive(Clone, Copy)]
struct Neigh {
	ip: Ipv4Addr,
	mac: MacAddr,
	valid: bool,
}

const NEIGH_MAX: usize = 8;

// The notable thing a received frame did, for the driver to log or react to.
#[derive(Clone, Copy)]
pub enum Event {
	None,
	// We learned a neighbor's MAC (from an ARP reply for an address we asked about).
	Learned(Ipv4Addr, MacAddr),
	// An ICMP echo reply arrived (a `ping` we sent was answered).
	EchoReply(Ipv4Addr),
	// A DNS response resolved a name to this address.
	DnsReply(Ipv4Addr),
}

// The result of feeding one frame to the stack: an optional reply to transmit
// (`reply_len` bytes written to the caller's output buffer, 0 = none) and an event.
pub struct Outcome {
	pub reply_len: usize,
	pub event: Event,
}

// The interface's L2/L3 state: our addresses and the neighbor cache.
pub struct Stack {
	mac: MacAddr,
	ip: Ipv4Addr,
	gateway: Ipv4Addr,
	neigh: [Neigh; NEIGH_MAX],
}

impl Stack {
	pub fn new(mac: MacAddr, ip: Ipv4Addr, gateway: Ipv4Addr) -> Stack {
		Stack { mac, ip, gateway, neigh: [Neigh { ip: Ipv4Addr([0; 4]), mac: MacAddr::ZERO, valid: false }; NEIGH_MAX] }
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

	// Handle an IPv4 packet addressed to us; only ICMP is processed.
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
		if dst_ip != self.ip {
			return Outcome { reply_len: 0, event: Event::None };
		}
		let src_ip: Ipv4Addr = Ipv4Addr([ip[12], ip[13], ip[14], ip[15]]);
		match ip[9] {
			IP_PROTO_ICMP => self.on_icmp(frame, ihl, src_ip, out),
			IP_PROTO_UDP => self.on_udp(frame, ihl),
			_ => Outcome { reply_len: 0, event: Event::None },
		}
	}

	// Handle an inbound UDP datagram. Only DNS responses (source port 53) are
	// recognized for now: the payload is parsed into the resolved address.
	fn on_udp(&mut self, frame: &[u8], ihl: usize) -> Outcome {
		let udp: &[u8] = &frame[ETH_HDR + ihl..];
		if udp.len() < UDP_HDR {
			return Outcome { reply_len: 0, event: Event::None };
		}
		if be16(udp, 0) == DNS_PORT {
			if let Some(addr) = parse_dns_response(&udp[UDP_HDR..]) {
				return Outcome { reply_len: 0, event: Event::DnsReply(addr) };
			}
		}
		Outcome { reply_len: 0, event: Event::None }
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
			return Outcome { reply_len: 0, event: Event::EchoReply(src_ip) };
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
		let total: usize = IPV4_HDR + ICMP_HDR;
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

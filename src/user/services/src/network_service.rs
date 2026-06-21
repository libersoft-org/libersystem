// NetworkService - the standing userspace network service (M33).
//
// M32 housed the L2/L3 stack inside driver.virtio-net; M33 extracts it here. The
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

mod net;

use alloc::string::String;
use alloc::vec::Vec;
use rt::*;

use crate::net::{Event, Ipv4Addr, MacAddr, Stack};
use proto::system::{Error, Ipv4Addr as WireIp, Neighbor, NetInfo, PingStatus, TcpRequest, network};

// Static addressing for the QEMU user-mode (SLIRP) network: the guest is
// 10.0.2.15/24, the gateway/host is 10.0.2.2, and the DNS relay is 10.0.2.3. A DHCP
// client (M33) later replaces this static configuration.
const OUR_IP: Ipv4Addr = Ipv4Addr([10, 0, 2, 15]);
const OUR_MASK: Ipv4Addr = Ipv4Addr([255, 255, 255, 0]);
const GATEWAY_IP: Ipv4Addr = Ipv4Addr([10, 0, 2, 2]);
const DNS_SERVER: Ipv4Addr = Ipv4Addr([10, 0, 2, 3]);
// The UDP source port we send DNS queries from.
const DNS_SRC_PORT: u16 = 0x9876;

// How long a `ping` waits for its reply, and DNS for its response (100 Hz ticks).
const PING_TIMEOUT_TICKS: u64 = 50;
const DNS_TIMEOUT_TICKS: u64 = 300;
// TCP: total time to establish, the SYN/segment retransmit interval, and how long to
// read a response (100 Hz ticks).
const TCP_SYN_TIMEOUT_TICKS: u64 = 300;
const TCP_RETX_TICKS: u64 = 50;
const TCP_RECV_TIMEOUT_TICKS: u64 = 300;
// The base ephemeral local port for outgoing connections, and the cap on response
// bytes returned to the client.
const TCP_LOCAL_PORT_BASE: u16 = 0xc000;
const TCP_REPLY_MAX: usize = 512;

// The frame-buffer size: one full Ethernet frame (1514 bytes) with slack. The
// driver forwards frames without the virtio_net_hdr, so this is the L2 frame only.
const FRAME_MAX: usize = 2048;
// The typed request and reply buffers for one client call (a `fetch` reply carries
// up to TCP_REPLY_MAX response bytes plus the codec framing).
const REQ_MAX: usize = 256;
const REPLY_MAX: usize = 1024;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	unsafe {
		// 1. receive the driver's frame channel (we move frames over it) and the
		//    client channel the shell reaches us on (the `ip` / `ping` / `nslookup`
		//    control protocol).
		let frames: u64 = recv_tagged(bootstrap, &mut buf, b"FRAMES").unwrap_or_else(|| exit());
		let client: u64 = recv_tagged(bootstrap, &mut buf, b"SERVE").unwrap_or_else(|| exit());
		// 2. the frame-mover driver leads with our NIC's MAC over the frame channel
		//    (it owns the device; we own the protocol), so we can build the stack.
		let mac: MacAddr = match recv_blocking(frames, &mut buf) {
			Received::Message { len, .. } if len >= 9 && &buf[..3] == b"MAC" => MacAddr([buf[3], buf[4], buf[5], buf[6], buf[7], buf[8]]),
			_ => exit(),
		};
		let mut stack: Stack = Stack::new(mac, OUR_IP, OUR_MASK, GATEWAY_IP);
		// 3. report in, then serve the network and the client at once (serve announces
		//    us on the link with a gratuitous ARP first).
		send_blocking(bootstrap, b"NetworkService: online", 0);
		serve(frames, client, &mut stack);
	}
}

// Send a built frame to the driver to transmit. A zero-length frame (the stack
// produced no reply) sends nothing.
unsafe fn send_frame(frames: u64, frame: &[u8]) {
	unsafe {
		if !frame.is_empty() {
			send_blocking(frames, frame, 0);
		}
	}
}

// Receive one frame from the driver, run it through the stack, send any reply frame
// back to the driver, and return the stack event it produced (an echo / DNS reply
// an in-flight `ping` / `nslookup` is waiting for, or `None`).
unsafe fn pump(frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Event {
	unsafe {
		match recv_blocking(frames, rx) {
			Received::Message { len, .. } => {
				let outcome: net::Outcome = stack.on_frame(&rx[..len], tx);
				send_frame(frames, &tx[..outcome.reply_len]);
				outcome.event
			}
			Received::Closed => Event::None,
		}
	}
}

// Stand on the driver's frame channel and the client's typed request channel at
// once (wait_any): a frame from the driver is parsed (answering ARP / ICMP, any
// reply sent back to transmit); a client request is decoded, dispatched to the
// generated `network` server, and answered. The client channel closing (the shell
// exited) drops us back to serving only the network. The gratuitous ARP that
// announces us on the link goes out first.
unsafe fn serve(frames: u64, client: u64, stack: &mut Stack) -> ! {
	unsafe {
		let mut rx: [u8; FRAME_MAX] = [0u8; FRAME_MAX];
		let mut tx: [u8; FRAME_MAX] = [0u8; FRAME_MAX];
		let mut req: [u8; REQ_MAX] = [0u8; REQ_MAX];
		let mut out: [u8; REPLY_MAX] = [0u8; REPLY_MAX];
		let arp: usize = stack.build_arp_request(GATEWAY_IP, &mut tx);
		send_frame(frames, &tx[..arp]);
		let mut client_open: bool = true;
		loop {
			let ready: i64 = if client_open {
				wait_any(&[frames, client], 0)
			} else {
				wait(frames, 0);
				0
			};
			if ready == 0 {
				pump(frames, stack, &mut rx, &mut tx);
			} else if ready == 1 {
				match recv_blocking(client, &mut req) {
					Received::Message { len, handle } => {
						let mut svc: Net = Net { frames, seq: 0, stack: &mut *stack, rx: &mut rx, tx: &mut tx };
						let mut reply_handle: u64 = 0;
						if let Some(n) = network::dispatch(&mut svc, &req[..len], handle, &mut out, &mut reply_handle) {
							send_blocking(client, &out[..n], reply_handle);
						}
					}
					Received::Closed => client_open = false,
				}
			}
		}
	}
}

// The state the typed `network` service operates on for one client request: the
// driver frame channel, an ICMP/DNS sequence counter, the stack, and the frame
// scratch buffers - the stack and buffers are borrowed from the serve loop, so the
// connection state and the 2 KiB frame buffers are not duplicated on the stack.
struct Net<'a> {
	frames: u64,
	seq: u16,
	stack: &'a mut Stack,
	rx: &'a mut [u8],
	tx: &'a mut [u8],
}

// Convert the stack's internal address to the wire (canonical) form, and back.
fn to_wire(ip: Ipv4Addr) -> WireIp {
	WireIp { a: ip.0[0], b: ip.0[1], c: ip.0[2], d: ip.0[3] }
}

fn from_wire(ip: &WireIp) -> Ipv4Addr {
	Ipv4Addr([ip.a, ip.b, ip.c, ip.d])
}

impl network::Service for Net<'_> {
	// The interface state: our address, MAC, gateway, and the neighbor cache.
	fn info(&mut self) -> Result<NetInfo, Error> {
		let mut neighbors: Vec<Neighbor> = Vec::new();
		let mut i: usize = 0;
		while let Some((nip, nmac)) = self.stack.neigh_at(i) {
			neighbors.push(Neighbor { addr: to_wire(nip), mac: nmac.0.to_vec() });
			i += 1;
		}
		Ok(NetInfo { addr: to_wire(self.stack.ip()), mac: self.stack.mac().0.to_vec(), gateway: to_wire(self.stack.gateway()), neighbors })
	}

	// Resolve a name to an address via the DNS client.
	fn resolve(&mut self, name: String) -> Result<WireIp, Error> {
		unsafe {
			match do_dns(name.as_bytes(), self.frames, self.stack, &mut self.seq, self.rx, self.tx) {
				Some(addr) => Ok(to_wire(addr)),
				None => Err(Error::NotFound),
			}
		}
	}

	// Ping an address: a reply, a timeout, or unreachable (no route / no ARP).
	fn ping(&mut self, addr: WireIp) -> Result<PingStatus, Error> {
		unsafe {
			Ok(match do_ping(from_wire(&addr), self.frames, self.stack, &mut self.seq, self.rx, self.tx) {
				1 => PingStatus::Reply,
				2 => PingStatus::Unreachable,
				_ => PingStatus::Timeout,
			})
		}
	}

	// A one-shot TCP exchange: connect, send the request, read the response, close.
	// Maps the connect failure modes onto the error enum.
	fn fetch(&mut self, req: TcpRequest) -> Result<Vec<u8>, Error> {
		unsafe {
			let mut data: [u8; 1 + TCP_REPLY_MAX] = [0u8; 1 + TCP_REPLY_MAX];
			let n: usize = do_tcp(from_wire(&req.ep.addr), req.ep.port, &req.request, self.frames, self.stack, self.rx, self.tx, &mut data);
			match data[0] {
				1 => Ok(data[1..n].to_vec()),
				2 => Err(Error::NotFound),
				3 => Err(Error::Denied),
				_ => Err(Error::Again),
			}
		}
	}
}

// ARP-resolve `ip` to its MAC, sending a request and pumping received frames if it
// is not already cached. None if it does not answer in time.
unsafe fn resolve(ip: Ipv4Addr, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Option<MacAddr> {
	unsafe {
		if stack.lookup(ip).is_none() {
			let arp: usize = stack.build_arp_request(ip, tx);
			send_frame(frames, &tx[..arp]);
			let deadline: u64 = clock() + PING_TIMEOUT_TICKS;
			while clock() < deadline && stack.lookup(ip).is_none() {
				if wait(frames, deadline) != 0 {
					break;
				}
				pump(frames, stack, rx, tx);
			}
		}
		stack.lookup(ip)
	}
}

// Send an ICMP echo request to `ip` and wait for the reply, pumping received frames
// as they arrive. Returns 1 = reply received, 0 = timed out, 2 = unresolved.
unsafe fn do_ping(ip: Ipv4Addr, frames: u64, stack: &mut Stack, seq: &mut u16, rx: &mut [u8], tx: &mut [u8]) -> u8 {
	unsafe {
		let hop: Ipv4Addr = stack.next_hop(ip);
		let mac: MacAddr = match resolve(hop, frames, stack, rx, tx) {
			Some(m) => m,
			None => return 2,
		};
		*seq = seq.wrapping_add(1);
		let echo: usize = stack.build_icmp_echo(mac, ip, 1, *seq, tx);
		send_frame(frames, &tx[..echo]);
		let deadline: u64 = clock() + PING_TIMEOUT_TICKS;
		while clock() < deadline {
			if wait(frames, deadline) != 0 {
				break;
			}
			if let Event::EchoReply(reply) = pump(frames, stack, rx, tx) {
				if reply == ip {
					return 1;
				}
			}
		}
		0
	}
}

// Resolve `name` to an IPv4 address via a DNS A-record query to the SLIRP DNS
// server, pumping received frames for the response. None on timeout or failure.
unsafe fn do_dns(name: &[u8], frames: u64, stack: &mut Stack, txn: &mut u16, rx: &mut [u8], tx: &mut [u8]) -> Option<Ipv4Addr> {
	unsafe {
		let hop: Ipv4Addr = stack.next_hop(DNS_SERVER);
		let mac: MacAddr = match resolve(hop, frames, stack, rx, tx) {
			Some(m) => m,
			None => return None,
		};
		*txn = txn.wrapping_add(1);
		let query: usize = stack.build_dns_query(mac, DNS_SERVER, name, *txn, DNS_SRC_PORT, tx);
		if query == 0 {
			return None;
		}
		send_frame(frames, &tx[..query]);
		let deadline: u64 = clock() + DNS_TIMEOUT_TICKS;
		while clock() < deadline {
			if wait(frames, deadline) != 0 {
				break;
			}
			if let Event::DnsReply(addr) = pump(frames, stack, rx, tx) {
				return Some(addr);
			}
		}
		None
	}
}

// Open a TCP connection to `ip`:`port` (next-hop via the gateway when off-link),
// send `request`, read the response, and close. Writes a status byte then the
// received bytes into `reply` (status 1 = connected with data, 0 = no SYN-ACK in
// time, 2 = unreachable / no ARP, 3 = refused / reset) and returns the total length.
#[allow(clippy::too_many_arguments)]
unsafe fn do_tcp(ip: Ipv4Addr, port: u16, request: &[u8], frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8], reply: &mut [u8]) -> usize {
	unsafe {
		let hop: Ipv4Addr = stack.next_hop(ip);
		let mac: MacAddr = match resolve(hop, frames, stack, rx, tx) {
			Some(m) => m,
			None => {
				reply[0] = 2;
				return 1;
			}
		};
		// Open and send the SYN, retransmitting it until the handshake completes.
		let iss: u32 = clock() as u32;
		let local_port: u16 = TCP_LOCAL_PORT_BASE | (clock() as u16 & 0x0fff);
		stack.tcp_open(ip, port, mac, local_port, iss);
		let syn: usize = stack.tcp_build_syn(tx);
		send_frame(frames, &tx[..syn]);
		let overall: u64 = clock() + TCP_SYN_TIMEOUT_TICKS;
		while clock() < overall && !stack.tcp_established() && !stack.tcp_aborted() {
			let attempt: u64 = clock() + TCP_RETX_TICKS;
			let until: u64 = if attempt < overall { attempt } else { overall };
			if wait(frames, until) != 0 {
				let s: usize = stack.tcp_build_syn(tx);
				send_frame(frames, &tx[..s]);
			} else {
				pump(frames, stack, rx, tx);
			}
		}
		if stack.tcp_aborted() {
			reply[0] = 3;
			return 1;
		}
		if !stack.tcp_established() {
			reply[0] = 0;
			return 1;
		}
		// Send the request, then read the response until the peer closes, the buffer
		// fills, or it falls quiet.
		if !request.is_empty() {
			let d: usize = stack.tcp_build_data(request, tx);
			send_frame(frames, &tx[..d]);
		}
		let cap: usize = reply.len() - 1;
		let mut got: usize = 0;
		let recv_deadline: u64 = clock() + TCP_RECV_TIMEOUT_TICKS;
		while clock() < recv_deadline && !stack.tcp_peer_fin() && !stack.tcp_aborted() && got < cap {
			if wait(frames, recv_deadline) != 0 {
				break;
			}
			pump(frames, stack, rx, tx);
			got += stack.tcp_take_rx(&mut reply[1 + got..]);
		}
		got += stack.tcp_take_rx(&mut reply[1 + got..]);
		// Close our half and briefly pump to acknowledge the peer's FIN.
		let fin: usize = stack.tcp_build_fin(tx);
		send_frame(frames, &tx[..fin]);
		let close_deadline: u64 = clock() + TCP_RETX_TICKS;
		while clock() < close_deadline && !stack.tcp_aborted() && !stack.tcp_peer_fin() {
			if wait(frames, close_deadline) != 0 {
				break;
			}
			pump(frames, stack, rx, tx);
		}
		reply[0] = 1;
		1 + got
	}
}

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
use proto::system::{Chunk, Endpoint, Error, Ipv4Addr as WireIp, Neighbor, NetInfo, PingStatus, TcpRequest, network, socket};

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
// How many bytes a single socket `recv` returns (the client calls `recv` repeatedly
// to drain a larger response); kept small for the 16 KiB user stack.
const SOCK_RECV_MAX: usize = 512;

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

// Stand on the driver's frame channel, the client's typed request channel, and the
// active socket's channel at once (wait_any): a frame from the driver is parsed
// (answering ARP / ICMP, any reply sent back to transmit); a client request is
// decoded, dispatched to the generated `network` server, and answered; a socket
// request (send / recv / close) is dispatched to the `socket` server for the one
// connection `network.connect` opened. The client channel closing (the shell
// exited) drops us back to serving only the network; the socket channel closing
// (the client dropped its socket) tears the connection down. The gratuitous ARP
// that announces us on the link goes out first.
unsafe fn serve(frames: u64, client: u64, stack: &mut Stack) -> ! {
	unsafe {
		let mut rx: [u8; FRAME_MAX] = [0u8; FRAME_MAX];
		let mut tx: [u8; FRAME_MAX] = [0u8; FRAME_MAX];
		let mut req: [u8; REQ_MAX] = [0u8; REQ_MAX];
		let mut out: [u8; REPLY_MAX] = [0u8; REPLY_MAX];
		let arp: usize = stack.build_arp_request(GATEWAY_IP, &mut tx);
		send_frame(frames, &tx[..arp]);
		let mut client_open: bool = true;
		// The server end of the one active socket (0 = none). The stack has a single
		// TCP connection, so at most one socket is open at a time.
		let mut sock: u64 = 0;
		// The producer end of the active received-data stream (0 = none) and its frame
		// sequence counter: the loop frames each newly received chunk onto it and closes
		// it when the peer closes, marking end of stream.
		let mut stream_prod: u64 = 0;
		let mut stream_seq: u32 = 0;
		loop {
			// Build the wait set: the driver frame channel always, the client channel
			// while the shell is connected, and the socket channel while one is open.
			let mut waits: [u64; 3] = [frames, 0, 0];
			let mut n: usize = 1;
			let client_idx: usize = if client_open {
				waits[n] = client;
				let i: usize = n;
				n += 1;
				i
			} else {
				usize::MAX
			};
			let sock_idx: usize = if sock != 0 {
				waits[n] = sock;
				let i: usize = n;
				n += 1;
				i
			} else {
				usize::MAX
			};
			let ready: usize = wait_any(&waits[..n], 0) as usize;
			if ready == 0 {
				pump(frames, stack, &mut rx, &mut tx);
				// Feed any newly received bytes to the active recv stream, and close the
				// producer (signalling end of stream) once the peer closes or resets.
				if stream_prod != 0 {
					stream_prod = stream_pump(stack, &mut out, stream_prod, &mut stream_seq);
					if stream_prod != 0 && (stack.tcp_peer_fin() || stack.tcp_aborted()) {
						close(stream_prod);
						stream_prod = 0;
					}
				}
			} else if ready == client_idx {
				match recv_blocking(client, &mut req) {
					Received::Message { len, handle } => {
						let mut new_sock: u64 = 0;
						let mut svc: Net = Net { frames, seq: 0, stack: &mut *stack, rx: &mut rx, tx: &mut tx, cur_sock: sock, new_sock: &mut new_sock };
						let mut reply_handle: u64 = 0;
						if let Some(n2) = network::dispatch(&mut svc, &req[..len], handle, &mut out, &mut reply_handle) {
							send_blocking(client, &out[..n2], reply_handle);
						}
						if new_sock != 0 {
							sock = new_sock;
						}
					}
					Received::Closed => client_open = false,
				}
			} else if ready == sock_idx {
				match recv_blocking(sock, &mut req) {
					Received::Message { len, handle } => {
						let op: u16 = if len >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
						let mut closing: bool = false;
						{
							let mut svc: Sock = Sock { frames, stack: &mut *stack, tx: &mut tx, closing: &mut closing };
							if op == socket::OP_RECV {
								// OP_RECV opens the received-data stream out of band: mint a
								// sub-channel, hand the consumer end back with the correlation
								// id, then frame any already-buffered bytes onto the producer
								// (the serve loop streams everything that arrives afterwards).
								if let Some((corr, items)) = socket::recv_open(&mut svc, &req[..len]) {
									if stream_prod != 0 {
										close(stream_prod);
										stream_prod = 0;
									}
									if let Some((producer, consumer)) = channel() {
										send_blocking(sock, &corr.to_le_bytes(), consumer);
										stream_seq = 0;
										for item in &items {
											if let Some(fl) = socket::recv_frame(stream_seq, item, &mut out) {
												send_blocking(producer, &out[..fl], 0);
												stream_seq += 1;
											}
										}
										stream_prod = producer;
									}
								}
							} else {
								let mut reply_handle: u64 = 0;
								if let Some(n2) = socket::dispatch(&mut svc, &req[..len], handle, &mut out, &mut reply_handle) {
									send_blocking(sock, &out[..n2], reply_handle);
								}
							}
						}
						if closing {
							if stream_prod != 0 {
								close(stream_prod);
								stream_prod = 0;
							}
							socket_teardown(frames, stack, &mut rx, &mut tx);
							close(sock);
							sock = 0;
						}
					}
					// The client dropped its socket without calling close(): tear the
					// connection down so the next `connect` starts clean.
					Received::Closed => {
						if stream_prod != 0 {
							close(stream_prod);
							stream_prod = 0;
						}
						socket_teardown(frames, stack, &mut rx, &mut tx);
						close(sock);
						sock = 0;
					}
				}
			}
		}
	}
}

// The state the typed `network` service operates on for one client request: the
// driver frame channel, an ICMP/DNS sequence counter, the stack, and the frame
// scratch buffers - the stack and buffers are borrowed from the serve loop, so the
// connection state and the 2 KiB frame buffers are not duplicated on the stack.
// `cur_sock` is the server end of an already-open socket (0 = none), so `connect`
// can refuse a second concurrent socket; `new_sock` is where `connect` parks the
// server end of the socket it opens, for the serve loop to pick up.
struct Net<'a> {
	frames: u64,
	seq: u16,
	stack: &'a mut Stack,
	rx: &'a mut [u8],
	tx: &'a mut [u8],
	cur_sock: u64,
	new_sock: &'a mut u64,
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

	// Open a TCP connection to `ep` and hand the client the socket as a capability:
	// the client end of a fresh channel on which we serve the `socket` interface. The
	// server end is parked in `new_sock` for the serve loop to start waiting on. One
	// socket at a time (the stack has a single connection), so a second `connect`
	// while one is open is refused with `Again`.
	fn connect(&mut self, ep: Endpoint) -> Result<u64, Error> {
		if self.cur_sock != 0 {
			return Err(Error::Again);
		}
		unsafe {
			match tcp_establish(from_wire(&ep.addr), ep.port, self.frames, self.stack, self.rx, self.tx) {
				1 => match channel() {
					Some((server, peer)) => {
						*self.new_sock = server;
						Ok(peer)
					}
					None => Err(Error::Again),
				},
				2 => Err(Error::NotFound),
				3 => Err(Error::Denied),
				_ => Err(Error::Again),
			}
		}
	}
}

// The state the typed `socket` service operates on for one connected socket: the
// frame channel, the stack (holding the single TCP connection), and the transmit
// buffer - all borrowed from the serve loop. `closing` is set by `close` so the
// loop tears the connection down after replying.
struct Sock<'a> {
	frames: u64,
	stack: &'a mut Stack,
	tx: &'a mut [u8],
	closing: &'a mut bool,
}

impl socket::Service for Sock<'_> {
	// Send bytes on the connection (one TCP data segment); the ack arrives via the
	// serve loop's frame pump. Closed once the connection is reset or gone.
	fn send(&mut self, data: Vec<u8>) -> Result<u32, Error> {
		if !self.stack.tcp_established() || self.stack.tcp_aborted() {
			return Err(Error::Closed);
		}
		unsafe { socket_send(&data, self.frames, self.stack, self.tx) };
		Ok(data.len() as u32)
	}

	// The snapshot of bytes already buffered when the recv stream opens; the serve
	// loop streams everything that arrives afterwards. One chunk, or empty.
	fn recv(&mut self) -> Vec<Chunk> {
		let mut chunks: Vec<Chunk> = Vec::new();
		let mut buf: [u8; SOCK_RECV_MAX] = [0u8; SOCK_RECV_MAX];
		let n: usize = self.stack.tcp_take_rx(&mut buf);
		if n > 0 {
			chunks.push(Chunk { data: buf[..n].to_vec() });
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
		// Establish the connection; on failure write the status byte and stop (the
		// establish status bytes 2 / 3 / 0 map straight onto the reply status).
		match tcp_establish(ip, port, frames, stack, rx, tx) {
			1 => {}
			other => {
				reply[0] = other;
				return 1;
			}
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

// Establish a TCP connection to `ip`:`port` (next-hop via the gateway when off-link):
// resolve the next hop, open the connection, and send the SYN, retransmitting it
// until the handshake completes. Returns 1 = established, 2 = unreachable (no ARP),
// 3 = refused (reset), 0 = timed out. Shared by `fetch` (do_tcp) and `connect`.
unsafe fn tcp_establish(ip: Ipv4Addr, port: u16, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> u8 {
	unsafe {
		let hop: Ipv4Addr = stack.next_hop(ip);
		let mac: MacAddr = match resolve(hop, frames, stack, rx, tx) {
			Some(m) => m,
			None => return 2,
		};
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
			return 3;
		}
		if !stack.tcp_established() {
			return 0;
		}
		1
	}
}

// Send `data` on the established connection as a single TCP data segment; the ack
// arrives later via the serve loop's frame pump.
unsafe fn socket_send(data: &[u8], frames: u64, stack: &mut Stack, tx: &mut [u8]) {
	unsafe {
		if !data.is_empty() {
			let d: usize = stack.tcp_build_data(data, tx);
			send_frame(frames, &tx[..d]);
		}
	}
}

// Drain newly received bytes from the connection and frame each chunk onto the recv
// stream `producer`. Returns the producer handle, or 0 if the consumer was dropped
// (in which case the producer is closed here).
unsafe fn stream_pump(stack: &mut Stack, out: &mut [u8], producer: u64, seq: &mut u32) -> u64 {
	unsafe {
		let mut buf: [u8; SOCK_RECV_MAX] = [0u8; SOCK_RECV_MAX];
		loop {
			let n: usize = stack.tcp_take_rx(&mut buf);
			if n == 0 {
				return producer;
			}
			let chunk: Chunk = Chunk { data: buf[..n].to_vec() };
			match socket::recv_frame(*seq, &chunk, out) {
				Some(fl) => {
					if !send_blocking(producer, &out[..fl], 0) {
						close(producer);
						return 0;
					}
					*seq += 1;
				}
				None => return producer,
			}
		}
	}
}

// Close our half of the connection: send a FIN (unless already reset) and briefly
// pump to acknowledge the peer's FIN, so the connection winds down before the next
// `connect` reopens the stack's single TCP connection.
unsafe fn socket_teardown(frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) {
	unsafe {
		if !stack.tcp_aborted() {
			let fin: usize = stack.tcp_build_fin(tx);
			send_frame(frames, &tx[..fin]);
		}
		let deadline: u64 = clock() + TCP_RETX_TICKS;
		while clock() < deadline && !stack.tcp_aborted() && !stack.tcp_peer_fin() {
			if wait(frames, deadline) != 0 {
				break;
			}
			pump(frames, stack, rx, tx);
		}
	}
}

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

use crate::net::{Event, Ipv4Addr, MacAddr, SockEntry, SockEntryState, Stack, DHCP_ACK, DHCP_OFFER};
use proto::codec::Buffer;
use proto::system::{listener, network, socket, Chunk, Endpoint, Error, Ipv4Addr as WireIp, Neighbor, NetInfo, PingReply, PingStatus, SockInfo, SockState, TcpRequest};

// Static addressing for the QEMU user-mode (SLIRP) network: the guest is
// 10.0.2.15/24, the gateway/host is 10.0.2.2, and the DNS relay is 10.0.2.3. A DHCP
// client (M33) later replaces this static configuration.
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
// The typed request and reply buffers for one client call. The request buffer fits
// any op comfortably (a DNS name alone may be 253 bytes plus framing); the reply
// buffer matches the 4096 wire ceiling every other service uses.
const REQ_MAX: usize = 1024;
const REPLY_MAX: usize = 4096;
// How many bytes a single socket `recv` returns (the client calls `recv` repeatedly
// to drain a larger response); kept small for the 16 KiB user stack.
const SOCK_RECV_MAX: usize = 512;
// The initial sizes of the client / socket / listener sets. Each set grows on
// demand (a slot is reused when free, a new one pushed otherwise), so these are
// size hints, never caps - the kernel's wait_any bound (64 handles) is the only
// ceiling on how many channels the serve loop can stand on at once.
const MAX_CLIENTS: usize = 4;
const MAX_SOCKS: usize = 4;
const MAX_LISTEN: usize = 2;

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
		let mut stack: Stack = Stack::new(mac, OUR_IP, OUR_MASK, GATEWAY_IP, DNS_SERVER);
		// 3. learn our address / mask / gateway / DNS from DHCP, falling back to the
		//    static config above if no server answers. The frame buffers are scoped so
		//    they are freed before serve allocates its own (the 16 KiB user stack).
		{
			let mut drx: [u8; FRAME_MAX] = [0u8; FRAME_MAX];
			let mut dtx: [u8; FRAME_MAX] = [0u8; FRAME_MAX];
			if do_dhcp(frames, &mut stack, &mut drx, &mut dtx) {
				print(b"network: configured via DHCP\n");
			} else {
				print(b"network: DHCP unanswered, using static config\n");
			}
		}
		// 4. report in, then serve the network and the client at once (serve announces
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

// Stand on the driver's frame channel, every client's typed request channel, and
// the active socket's channel at once (wait_any): a frame from the driver is parsed
// (answering ARP / ICMP, any reply sent back to transmit); a client request is
// decoded, dispatched to the generated `network` server, and answered; a socket
// request (send / recv / close) is dispatched to the `socket` server for the one
// connection `network.connect` opened. `network.open` mints a fresh client channel
// (one per spawned net tool) added to the client set; a client channel closing
// drops it from the set, the socket channel closing tears the connection down. The
// gratuitous ARP that announces us on the link goes out first.
unsafe fn serve(frames: u64, client: u64, stack: &mut Stack) -> ! {
	unsafe {
		// The frame and reply buffers live on the heap, not in this function's frame:
		// serve holds all of them for its whole lifetime and the connect handshake
		// nests a deep call chain on top, which would overflow the 16 KiB user stack.
		let mut rx: Vec<u8> = alloc::vec![0u8; FRAME_MAX];
		let mut tx: Vec<u8> = alloc::vec![0u8; FRAME_MAX];
		let mut req: [u8; REQ_MAX] = [0u8; REQ_MAX];
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
			let ready: usize = wait_any(&waits[..n], 0) as usize;
			if ready == 0 {
				pump(frames, stack, &mut rx, &mut tx);
				// Feed any newly received bytes to each active recv stream, closing the
				// producer (end of stream) once that connection's peer closes or resets.
				let mut si: usize = 0;
				while si < socks.len() {
					if socks[si].chan != 0 && socks[si].stream_prod != 0 {
						let ci: usize = socks[si].ci;
						let prod: u64 = stream_pump(ci, stack, &mut out, socks[si].stream_prod, &mut socks[si].stream_seq);
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
						if let Some(ci) = stack.take_accepted(listeners[li].port) {
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
				match recv_blocking(chan, &mut req) {
					Received::Message { len, handle } => {
						let mut new_sock: u64 = 0;
						let mut new_sock_ci: usize = 0;
						let mut new_client: u64 = 0;
						let mut new_listener: u64 = 0;
						let mut new_listener_port: u16 = 0;
						// every set grows on demand, so there is always room.
						let client_room: bool = true;
						let sock_room: bool = true;
						let listener_room: bool = true;
						{
							let mut svc: Net = Net { frames, seq: 0, stack: &mut *stack, rx: &mut rx[..], tx: &mut tx[..], new_sock: &mut new_sock, new_sock_ci: &mut new_sock_ci, new_client: &mut new_client, new_listener: &mut new_listener, new_listener_port: &mut new_listener_port, sock_room, client_room, listener_room };
							let mut reply_handle: u64 = 0;
							if let Some(n2) = network::dispatch(&mut svc, &req[..len], handle, &mut out, &mut reply_handle) {
								send_blocking(chan, &out[..n2], reply_handle);
							}
						}
						if new_sock != 0 {
							place_sock(&mut socks, SockSlot { chan: new_sock, ci: new_sock_ci, stream_prod: 0, stream_seq: 0 });
						}
						if new_listener != 0 {
							place_listener(&mut listeners, Listener { chan: new_listener, port: new_listener_port, pending_corr: 0, pending: false });
						}
						if new_client != 0 {
							place_client(&mut clients, new_client);
						}
					}
					Received::Closed => {
						close(chan);
						clients[slot] = 0;
					}
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
unsafe fn serve_socket(slot: &mut SockSlot, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8], out: &mut [u8], req: &mut [u8]) {
	unsafe {
		let chan: u64 = slot.chan;
		let ci: usize = slot.ci;
		match recv_blocking(chan, req) {
			Received::Message { len, handle } => {
				let op: u16 = if len >= 2 { u16::from_le_bytes([req[0], req[1]]) } else { 0 };
				let mut closing: bool = false;
				{
					let mut svc: Sock = Sock { ci, frames, stack: &mut *stack, tx, closing: &mut closing };
					if op == socket::OP_RECV {
						if let Some((corr, items)) = socket::recv_open(&mut svc, &req[..len]) {
							if slot.stream_prod != 0 {
								close(slot.stream_prod);
								slot.stream_prod = 0;
							}
							if let Some((producer, consumer)) = channel() {
								send_blocking(chan, &corr.to_le_bytes(), consumer);
								slot.stream_seq = 0;
								for item in &items {
									if let Some(fl) = socket::recv_frame(slot.stream_seq, item, out) {
										send_blocking(producer, &out[..fl], 0);
										slot.stream_seq += 1;
									}
								}
								slot.stream_prod = producer;
							}
						}
					} else {
						let mut reply_handle: u64 = 0;
						if let Some(n2) = socket::dispatch(&mut svc, &req[..len], handle, out, &mut reply_handle) {
							send_blocking(chan, &out[..n2], reply_handle);
						}
					}
				}
				if closing {
					teardown_socket(slot, frames, stack, rx, tx);
				}
			}
			// The client dropped its socket without calling close(): tear it down.
			Received::Closed => {
				teardown_socket(slot, frames, stack, rx, tx);
			}
		}
	}
}

// Tear a socket slot down: close its recv stream, send the connection's FIN, free the
// stack connection, close the channel, and empty the slot.
unsafe fn teardown_socket(slot: &mut SockSlot, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) {
	unsafe {
		if slot.stream_prod != 0 {
			close(slot.stream_prod);
			slot.stream_prod = 0;
		}
		socket_teardown(slot.ci, frames, stack, rx, tx);
		stack.tcp_free(slot.ci);
		close(slot.chan);
		slot.chan = 0;
	}
}

// One active listening socket the serve loop multiplexes: the channel the `listener`
// interface is served on, the port it accepts inbound connections on, and a deferred
// `accept` (the correlation id to answer once a connection completes, when `accept`
// was called with none pending).
#[derive(Clone, Copy)]
struct Listener {
	chan: u64,
	port: u16,
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
unsafe fn serve_listener(listener: &mut Listener, socks: &mut Vec<SockSlot>, stack: &mut Stack, req: &mut [u8]) {
	unsafe {
		match recv_blocking(listener.chan, req) {
			Received::Message { len, .. } => {
				// The request frames as [op u16][corr u32]; OP_ACCEPT is the only op.
				if len >= 6 {
					let op: u16 = u16::from_le_bytes([req[0], req[1]]);
					let corr: u32 = u32::from_le_bytes([req[2], req[3], req[4], req[5]]);
					if op == listener::OP_ACCEPT {
						match stack.take_accepted(listener.port) {
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
				stack.unlisten(listener.port);
				close(listener.chan);
				listener.chan = 0;
			}
		}
	}
}

// Hand accepted connection `ci` to the listener as a socket: mint the socket channel,
// place it in the socket set (which grows on demand), and reply to the pending
// `accept` (correlation `corr`) with the client end out of band. The reply frames a
// `result<handle<channel>, error>` Ok: [corr u32][tag 1][u32 0] inline, the channel
// out of band. The connection is dropped if the channel cannot be minted.
unsafe fn accept_handoff(listener_chan: u64, corr: u32, ci: usize, socks: &mut Vec<SockSlot>, stack: &mut Stack) -> bool {
	unsafe {
		match channel() {
			Some((server, peer)) => {
				place_sock(socks, SockSlot { chan: server, ci, stream_prod: 0, stream_seq: 0 });
				let mut reply: [u8; 9] = [0u8; 9];
				reply[0..4].copy_from_slice(&corr.to_le_bytes());
				reply[4] = 1;
				send_blocking(listener_chan, &reply, peer);
				true
			}
			None => {
				stack.tcp_free(ci);
				false
			}
		}
	}
}

// The state the typed `network` service operates on for one client request: the
// driver frame channel, an ICMP/DNS sequence counter, the stack, and the frame
// scratch buffers - the stack and buffers are borrowed from the serve loop, so the
// connection state and the 2 KiB frame buffers are not duplicated on the stack.
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
	new_listener_port: &'a mut u16,
	sock_room: bool,
	client_room: bool,
	listener_room: bool,
}

// Convert the stack's internal address to the wire (canonical) form, and back.
fn to_wire(ip: Ipv4Addr) -> WireIp {
	WireIp { a: ip.0[0], b: ip.0[1], c: ip.0[2], d: ip.0[3] }
}

fn from_wire(ip: &WireIp) -> Ipv4Addr {
	Ipv4Addr([ip.a, ip.b, ip.c, ip.d])
}

// Map a stack socket snapshot to the typed `sock-info` the `ss` tool renders.
fn to_sock_info(s: SockEntry) -> SockInfo {
	let state: SockState = match s.state {
		SockEntryState::Closed => SockState::Closed,
		SockEntryState::SynSent => SockState::SynSent,
		SockEntryState::SynRcvd => SockState::SynRcvd,
		SockEntryState::Established => SockState::Established,
		SockEntryState::FinWait => SockState::FinWait,
		SockEntryState::Listen => SockState::Listen,
	};
	SockInfo { local_port: s.local_port, remote: Endpoint { addr: to_wire(s.remote_ip), port: s.remote_port }, state }
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

	// Ping an address: a reply (with its TTL and round-trip time), a timeout, or
	// unreachable (no route / no ARP).
	fn ping(&mut self, addr: WireIp) -> Result<PingReply, Error> {
		unsafe {
			let (status, ttl, rtt_us): (u8, u8, u32) = do_ping(from_wire(&addr), self.frames, self.stack, &mut self.seq, self.rx, self.tx);
			let status: PingStatus = match status {
				1 => PingStatus::Reply,
				2 => PingStatus::Unreachable,
				_ => PingStatus::Timeout,
			};
			Ok(PingReply { status, ttl, rtt_us })
		}
	}

	// A one-shot TCP exchange: connect, send the request, read the response, close.
	// Maps the connect failure modes onto the error enum.
	fn fetch(&mut self, req: TcpRequest) -> Result<Vec<u8>, Error> {
		unsafe {
			let ci: usize = match self.stack.tcp_alloc() {
				Some(i) => i,
				None => return Err(Error::Again),
			};
			let mut data: [u8; 1 + TCP_REPLY_MAX] = [0u8; 1 + TCP_REPLY_MAX];
			let n: usize = do_tcp(ci, from_wire(&req.ep.addr), req.ep.port, &req.request, self.frames, self.stack, self.rx, self.tx, &mut data);
			self.stack.tcp_free(ci);
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
	// server end (and its stack connection index) are parked in `new_sock`/`new_sock_ci`
	// for the serve loop to start waiting on. Refused with `Again` when the socket pool
	// or the connection pool is full.
	fn connect(&mut self, ep: Endpoint) -> Result<u64, Error> {
		if !self.sock_room {
			return Err(Error::Again);
		}
		unsafe {
			let ci: usize = match self.stack.tcp_alloc() {
				Some(i) => i,
				None => return Err(Error::Again),
			};
			match tcp_establish(ci, from_wire(&ep.addr), ep.port, self.frames, self.stack, self.rx, self.tx) {
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
		unsafe {
			match channel() {
				Some((server, peer)) => {
					*self.new_client = server;
					Ok(peer)
				}
				None => Err(Error::Again),
			}
		}
	}

	// Open a listening socket on `port` (passive open) and hand the caller a `listener`
	// capability: the client end of a fresh channel on which we serve `accept`. The
	// server end (and its port) are parked in `new_listener`/`new_listener_port` for the
	// serve loop to start waiting on, and the stack starts accepting inbound connections
	// on the port. Refused with `Again` when the listener set or the listen table is
	// full.
	fn listen(&mut self, port: u16) -> Result<u64, Error> {
		if !self.listener_room || !self.stack.listen(port) {
			return Err(Error::Again);
		}
		unsafe {
			match channel() {
				Some((server, peer)) => {
					*self.new_listener = server;
					*self.new_listener_port = port;
					Ok(peer)
				}
				None => {
					self.stack.unlisten(port);
					Err(Error::Again)
				}
			}
		}
	}

	// The live sockets in the stack's table: the listening ports and every open
	// connection, with its local port, remote endpoint, and TCP state - what `ss` lists.
	fn sockets(&mut self) -> Result<Vec<SockInfo>, Error> {
		Ok(self.stack.sockets().into_iter().map(to_sock_info).collect())
	}

	// Query an NTP server for the wall-clock time, returning the Unix epoch seconds from
	// its reply. The TimeService combines this with the monotonic clock and the RTC.
	fn sntp(&mut self, server: WireIp) -> Result<u64, Error> {
		unsafe {
			match do_sntp(from_wire(&server), self.frames, self.stack, self.rx, self.tx) {
				Some(unix) => Ok(unix),
				None => Err(Error::Again),
			}
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
			unsafe { close(data.handle) };
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
			// Bound the view to a single segment - the memory object is at least one
			// page, so this slice never runs past the mapping even for a bogus length.
			let n: usize = (data.len as usize).min(FRAME_MAX);
			let bytes: &[u8] = core::slice::from_raw_parts(base as *const u8, n);
			socket_send(self.ci, bytes, self.frames, self.stack, self.tx);
			unmap_object(data.handle);
			close(data.handle);
			Ok(n as u32)
		}
	}

	// The snapshot of bytes already buffered when the recv stream opens; the serve
	// loop streams everything that arrives afterwards. One chunk, or empty.
	fn recv(&mut self) -> Vec<Chunk> {
		let mut chunks: Vec<Chunk> = Vec::new();
		let mut buf: [u8; SOCK_RECV_MAX] = [0u8; SOCK_RECV_MAX];
		let n: usize = self.stack.tcp_take_rx(self.ci, &mut buf);
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
// as they arrive. Returns (status, ttl, rtt_us): status 1 = reply received (ttl is
// the reply's IP TTL, rtt_us the round-trip time in microseconds), 0 = timed out,
// 2 = unresolved (ttl/rtt 0 in both).
unsafe fn do_ping(ip: Ipv4Addr, frames: u64, stack: &mut Stack, seq: &mut u16, rx: &mut [u8], tx: &mut [u8]) -> (u8, u8, u32) {
	unsafe {
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
			if wait(frames, deadline) != 0 {
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
}

// Run the DHCP client handshake (DISCOVER -> OFFER -> REQUEST -> ACK), pumping
// received frames for the replies, and on success apply the learned address / mask /
// gateway / DNS to the stack. Returns whether a lease was obtained (false = the
// caller keeps the static configuration).
unsafe fn do_dhcp(frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> bool {
	unsafe {
		// Broadcast a DISCOVER and wait for the server's OFFER.
		let discover: usize = stack.build_dhcp_discover(tx);
		send_frame(frames, &tx[..discover]);
		let mut offered: bool = false;
		let deadline: u64 = clock() + DHCP_TIMEOUT_TICKS;
		while clock() < deadline && !offered {
			if wait(frames, deadline) != 0 {
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
			if wait(frames, deadline) != 0 {
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
}

// Resolve `name` to an IPv4 address via a DNS A-record query to the SLIRP DNS
// server, pumping received frames for the response. None on timeout or failure.
unsafe fn do_dns(name: &[u8], frames: u64, stack: &mut Stack, txn: &mut u16, rx: &mut [u8], tx: &mut [u8]) -> Option<Ipv4Addr> {
	unsafe {
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

// Send an SNTP request to `server` and return the Unix epoch seconds from its reply,
// or None on timeout / no route. A one-shot UDP query/response, mirroring do_dns.
unsafe fn do_sntp(server: Ipv4Addr, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Option<u64> {
	unsafe {
		let hop: Ipv4Addr = stack.next_hop(server);
		let mac: MacAddr = match resolve(hop, frames, stack, rx, tx) {
			Some(m) => m,
			None => return None,
		};
		let query: usize = stack.build_sntp_request(mac, server, NTP_SRC_PORT, tx);
		if query == 0 {
			return None;
		}
		send_frame(frames, &tx[..query]);
		let deadline: u64 = clock() + NTP_TIMEOUT_TICKS;
		while clock() < deadline {
			if wait(frames, deadline) != 0 {
				break;
			}
			if let Event::SntpReply(unix) = pump(frames, stack, rx, tx) {
				return Some(unix);
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
unsafe fn do_tcp(ci: usize, ip: Ipv4Addr, port: u16, request: &[u8], frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8], reply: &mut [u8]) -> usize {
	unsafe {
		// Establish the connection; on failure write the status byte and stop (the
		// establish status bytes 2 / 3 / 0 map straight onto the reply status).
		match tcp_establish(ci, ip, port, frames, stack, rx, tx) {
			1 => {}
			other => {
				reply[0] = other;
				return 1;
			}
		}
		// Send the request, then read the response until the peer closes, the buffer
		// fills, or it falls quiet.
		if !request.is_empty() {
			let d: usize = stack.tcp_build_data(ci, request, tx);
			send_frame(frames, &tx[..d]);
		}
		let cap: usize = reply.len() - 1;
		let mut got: usize = 0;
		let recv_deadline: u64 = clock() + TCP_RECV_TIMEOUT_TICKS;
		while clock() < recv_deadline && !stack.tcp_peer_fin(ci) && !stack.tcp_aborted(ci) && got < cap {
			if wait(frames, recv_deadline) != 0 {
				break;
			}
			pump(frames, stack, rx, tx);
			got += stack.tcp_take_rx(ci, &mut reply[1 + got..]);
		}
		got += stack.tcp_take_rx(ci, &mut reply[1 + got..]);
		// Close our half and briefly pump to acknowledge the peer's FIN.
		let fin: usize = stack.tcp_build_fin(ci, tx);
		send_frame(frames, &tx[..fin]);
		let close_deadline: u64 = clock() + TCP_RETX_TICKS;
		while clock() < close_deadline && !stack.tcp_aborted(ci) && !stack.tcp_peer_fin(ci) {
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
unsafe fn tcp_establish(ci: usize, ip: Ipv4Addr, port: u16, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> u8 {
	unsafe {
		let hop: Ipv4Addr = stack.next_hop(ip);
		let mac: MacAddr = match resolve(hop, frames, stack, rx, tx) {
			Some(m) => m,
			None => return 2,
		};
		let iss: u32 = clock() as u32;
		let local_port: u16 = TCP_LOCAL_PORT_BASE | (clock() as u16 & 0x0fff);
		stack.tcp_open(ci, ip, port, mac, local_port, iss);
		let syn: usize = stack.tcp_build_syn(ci, tx);
		send_frame(frames, &tx[..syn]);
		let overall: u64 = clock() + TCP_SYN_TIMEOUT_TICKS;
		while clock() < overall && !stack.tcp_established(ci) && !stack.tcp_aborted(ci) {
			let attempt: u64 = clock() + TCP_RETX_TICKS;
			let until: u64 = if attempt < overall { attempt } else { overall };
			if wait(frames, until) != 0 {
				let s: usize = stack.tcp_build_syn(ci, tx);
				send_frame(frames, &tx[..s]);
			} else {
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
}

// Send `data` on connection `ci` as a single TCP data segment; the ack arrives later
// via the serve loop's frame pump.
unsafe fn socket_send(ci: usize, data: &[u8], frames: u64, stack: &mut Stack, tx: &mut [u8]) {
	unsafe {
		if !data.is_empty() {
			let d: usize = stack.tcp_build_data(ci, data, tx);
			send_frame(frames, &tx[..d]);
		}
	}
}

// Drain newly received bytes from the connection and frame each chunk onto the recv
// stream `producer`. Returns the producer handle, or 0 if the consumer was dropped
// (in which case the producer is closed here).
unsafe fn stream_pump(ci: usize, stack: &mut Stack, out: &mut [u8], producer: u64, seq: &mut u32) -> u64 {
	unsafe {
		let mut buf: [u8; SOCK_RECV_MAX] = [0u8; SOCK_RECV_MAX];
		loop {
			let n: usize = stack.tcp_take_rx(ci, &mut buf);
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

// Close our half of connection `ci`: send a FIN (unless already reset) and briefly
// pump to acknowledge the peer's FIN, so the connection winds down before its slot is
// freed.
unsafe fn socket_teardown(ci: usize, frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) {
	unsafe {
		if !stack.tcp_aborted(ci) {
			let fin: usize = stack.tcp_build_fin(ci, tx);
			send_frame(frames, &tx[..fin]);
		}
		let deadline: u64 = clock() + TCP_RETX_TICKS;
		while clock() < deadline && !stack.tcp_aborted(ci) && !stack.tcp_peer_fin(ci) {
			if wait(frames, deadline) != 0 {
				break;
			}
			pump(frames, stack, rx, tx);
		}
	}
}

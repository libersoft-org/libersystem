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

mod net;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use rt::*;

use crate::net::{DHCP_ACK, DHCP_NAK, DHCP_OFFER, Event, Ipv4Addr, MacAddr, NEIGH_MAX, SockEntry, SockEntryState, Stack, TCP_SEGMENT_OVERHEAD};
use proto::codec::Buffer;
use proto::system::{Chunk, Endpoint, Error, Ipv4Addr as WireIp, Neighbor, NetCapacity, NetInfo, PingReply, PingStatus, SockInfo, SockState, TcpRequest, config, listener, network, socket};

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
// TCP: total time to establish, the SYN/segment retransmit interval, and how long to
// read a response (100 Hz ticks).
const TCP_SYN_TIMEOUT_TICKS: u64 = 300;
const TCP_RETX_TICKS: u64 = 50;
const TCP_RECV_TIMEOUT_TICKS: u64 = 300;
// The base ephemeral local port for outgoing connections.
const TCP_LOCAL_PORT_BASE: u16 = 0xc000;

// The default MTU: standard Ethernet, when neither the driver nor the config tree
// says otherwise. The effective MTU - the smaller of the link's report and the
// `net.mtu` config knob - sizes every frame buffer at start; there is no
// compile-time frame cap.
const DEFAULT_MTU: usize = 1500;
// The typed request and reply buffers for one client call. The request buffer fits
// any op comfortably (a DNS name alone may be 253 bytes plus framing); replies
// that can grow without bound (`fetch`, socket recv chunks) are built in Vecs and
// received exactly-sized on the client, so this bounds only the fixed-shape ops.
const REQ_MAX: usize = 1024;
const REPLY_MAX: usize = 4096;
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
fn net_policy(config: u64) -> (usize, usize) {
	if config == 0 {
		return (NEIGH_MAX, DEFAULT_MTU);
	}
	let mut client = config::Client::new(ChannelTransport { chan: config });
	let neigh: usize = match client.get("net.arp-cache") {
		Some(Ok(value)) => value.parse::<usize>().ok().filter(|&n| n > 0).unwrap_or(NEIGH_MAX),
		_ => NEIGH_MAX,
	};
	let mtu: usize = match client.get("net.mtu") {
		Some(Ok(value)) => value.parse::<usize>().ok().filter(|&n| n >= 576).unwrap_or(DEFAULT_MTU),
		_ => DEFAULT_MTU,
	};
	unsafe { close(config) };
	(neigh, mtu)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	unsafe {
		// 1. receive the driver's frame channel (we move frames over it), the
		//    ConfigService client the supervisor minted for us (handle 0 when no
		//    config tree serves this boot - a test scenario), and the client channel
		//    the shell reaches us on (the `ip` / `ping` / `nslookup` control
		//    protocol).
		let frames: u64 = recv_tagged(bootstrap, &mut buf, b"FRAMES").unwrap_or_else(|| fail_bootstrap(bootstrap, b"frames", b"driver frame channel not delivered"));
		let config: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if len >= 6 && &buf[..6] == b"CONFIG" => handle,
			_ => fail_bootstrap(bootstrap, b"config", b"config client not delivered"),
		};
		let client: u64 = recv_tagged(bootstrap, &mut buf, b"SERVE").unwrap_or_else(|| fail_bootstrap(bootstrap, b"serve", b"missing serve channel"));
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
		let (neigh_cap, mtu_knob): (usize, usize) = net_policy(config);
		let mtu: usize = mtu_knob.min(link_mtu);
		let frame_max: usize = mtu + 14;
		let mut stack: Stack = Stack::new(mac, OUR_IP, OUR_MASK, GATEWAY_IP, DNS_SERVER, neigh_cap, mtu as u16);
		// 3. learn our address / mask / gateway / DNS from DHCP, falling back to the
		//    static config above if no server answers. The frame buffers are heap Vecs
		//    scoped so they are freed before serve allocates its own.
		let mut lease: LeaseClock = LeaseClock::none();
		{
			let mut drx: Vec<u8> = alloc::vec![0u8; frame_max];
			let mut dtx: Vec<u8> = alloc::vec![0u8; frame_max];
			if do_dhcp(frames, &mut stack, &mut drx, &mut dtx) {
				print(b"network: configured via DHCP\n");
				lease = LeaseClock::bound(&stack);
			} else {
				print(b"network: DHCP unanswered, using static config\n");
			}
		}
		// 4. report in, then serve the network and the client at once (serve announces
		//    us on the link with a gratuitous ARP first).
		send_blocking(bootstrap, b"NetworkService: online", 0);
		serve(frames, client, &mut stack, lease, frame_max);
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
// an in-flight `ping` / `nslookup` is waiting for, or `None`). The frame channel
// closing means the driver is gone - there is no network left to serve, and a wait
// on the closed channel would be forever-ready, so the service exits instead of
// spinning on it.
unsafe fn pump(frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8]) -> Event {
	unsafe {
		match recv_blocking(frames, rx) {
			Received::Message { len, .. } => {
				let outcome: net::Outcome = stack.on_frame(&rx[..len], tx);
				send_frame(frames, &tx[..outcome.reply_len]);
				outcome.event
			}
			Received::Closed => exit(),
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
// gratuitous ARP that announces us on the link goes out first. While a DHCP lease
// is held, its clock arms the wait's deadline (a periodic housekeeping wake) and
// `lease_due` extends the lease when a threshold comes due.
unsafe fn serve(frames: u64, client: u64, stack: &mut Stack, mut lease: LeaseClock, frame_max: usize) -> ! {
	unsafe {
		// The frame and reply buffers live on the heap, not in this function's frame:
		// serve holds all of them for its whole lifetime and the connect handshake
		// nests a deep call chain on top, which would overflow the 16 kB user stack.
		let mut rx: Vec<u8> = alloc::vec![0u8; frame_max];
		let mut tx: Vec<u8> = alloc::vec![0u8; frame_max];
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
			// While a lease clock runs, its next threshold bounds the wait as a periodic
			// housekeeping wake; without one the wait has no deadline at all.
			let ready_raw: i64 = match lease.next_due() {
				Some(deadline) => wait_any_periodic(&waits[..n], deadline),
				None => wait_any(&waits[..n], 0),
			};
			if ready_raw == ERR_TIMED_OUT {
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
					Received::Message { len, mut handle } => {
						let mut new_sock: u64 = 0;
						let mut new_sock_ci: usize = 0;
						let mut new_client: u64 = 0;
						let mut new_listener: u64 = 0;
						let mut new_listener_port: u16 = 0;
						// every set grows on demand, so there is always room.
						let client_room: bool = true;
						let sock_room: bool = true;
						let listener_room: bool = true;
						// the live pool utilization, for the `capacity` reply (observability).
						let clients_used: u32 = clients.iter().filter(|&&c| c != 0).count() as u32;
						let sockets_used: u32 = socks.iter().filter(|s| s.chan != 0).count() as u32;
						let listeners_used: u32 = listeners.iter().filter(|l| l.chan != 0).count() as u32;
						{
							let mut svc: Net = Net { frames, seq: 0, stack: &mut *stack, rx: &mut rx[..], tx: &mut tx[..], new_sock: &mut new_sock, new_sock_ci: &mut new_sock_ci, new_client: &mut new_client, new_listener: &mut new_listener, new_listener_port: &mut new_listener_port, sock_room, client_room, listener_room, clients_used, sockets_used, listeners_used };
							let mut reply_handle: u64 = 0;
							if let Some(n2) = network::dispatch(&mut svc, &req[..len], &mut handle, &mut out, &mut reply_handle) {
								if !send_blocking(chan, &out[..n2], reply_handle) && reply_handle != 0 {
									close(reply_handle);
								}
							}
						}
						if handle != 0 {
							close(handle);
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
			Received::Message { len, mut handle } => {
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
									let mut frame_handle: u64 = 0;
									if let Some(fl) = socket::recv_frame(slot.stream_seq, item, out, &mut frame_handle) {
										if !send_blocking(producer, &out[..fl], frame_handle) && frame_handle != 0 {
											close(frame_handle);
										}
										slot.stream_seq += 1;
									} else if frame_handle != 0 {
										close(frame_handle);
									}
								}
								slot.stream_prod = producer;
							}
						}
					} else {
						let mut reply_handle: u64 = 0;
						if let Some(n2) = socket::dispatch(&mut svc, &req[..len], &mut handle, out, &mut reply_handle) {
							if !send_blocking(chan, &out[..n2], reply_handle) && reply_handle != 0 {
								close(reply_handle);
							}
						}
					}
					if handle != 0 {
						close(handle);
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
	new_listener_port: &'a mut u16,
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
	// The interface state: our address, MAC, MTU, gateway, and the neighbor cache.
	fn info(&mut self) -> Result<NetInfo, Error> {
		let mut neighbors: Vec<Neighbor> = Vec::new();
		let mut i: usize = 0;
		while let Some((nip, nmac)) = self.stack.neigh_at(i) {
			neighbors.push(Neighbor { addr: to_wire(nip), mac: nmac.0.to_vec() });
			i += 1;
		}
		Ok(NetInfo { addr: to_wire(self.stack.ip()), mac: self.stack.mac().0.to_vec(), mtu: self.stack.mtu(), gateway: to_wire(self.stack.gateway()), neighbors })
	}

	// The live pool utilization: the client, socket and listener channels the serve loop
	// stands on, and the stack's live TCP connections. Every pool grows on demand (the
	// domain's handle budget is the only ceiling), so these are observability counts -
	// what `ss` prints and the graph folds in - not a fraction of a fixed cap.
	fn capacity(&mut self) -> Result<NetCapacity, Error> {
		Ok(NetCapacity { clients: self.clients_used, sockets: self.sockets_used, listeners: self.listeners_used, connections: self.stack.conn_used() as u32 })
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
	// Maps the connect failure modes onto the error enum. The response accumulates
	// in a Vec and rides an exactly-sized reply - its size is bounded by the peer
	// closing, never by a wire constant.
	fn fetch(&mut self, req: TcpRequest) -> Result<Vec<u8>, Error> {
		unsafe {
			let ci: usize = match self.stack.tcp_alloc() {
				Some(i) => i,
				None => return Err(Error::Again),
			};
			let mut data: Vec<u8> = Vec::new();
			let status: u8 = do_tcp(ci, from_wire(&req.ep.addr), req.ep.port, &req.request, self.frames, self.stack, self.rx, self.tx, &mut data);
			self.stack.tcp_free(ci);
			match status {
				1 => Ok(data),
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
			// Bound the view to a single segment - what fits the transmit frame buffer
			// behind the Ethernet/IP/TCP headers; the memory object is at least one
			// page, so this slice never runs past the mapping even for a bogus length.
			let n: usize = (data.len as usize).min(self.tx.len() - TCP_SEGMENT_OVERHEAD);
			let bytes: &[u8] = core::slice::from_raw_parts(base as *const u8, n);
			socket_send(self.ci, bytes, self.frames, self.stack, self.tx);
			unmap_object(data.handle);
			close(data.handle);
			Ok(n as u32)
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
			unsafe {
				let w: usize = self.stack.tcp_build_window_update(self.ci, self.tx);
				send_frame(self.frames, &self.tx[..w]);
			}
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
	unsafe fn bound(stack: &Stack) -> LeaseClock {
		unsafe {
			match stack.dhcp_times() {
				Some((t1, t2, lease)) => {
					let now: u64 = clock();
					LeaseClock { phase: LeasePhase::Bound, t1: now + t1 as u64 * TICKS_PER_SEC, t2: now + t2 as u64 * TICKS_PER_SEC, expiry: now + lease as u64 * TICKS_PER_SEC, retry: 0 }
				}
				None => LeaseClock::none(),
			}
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
	unsafe fn on_reply(&mut self, msg_type: u8, stack: &mut Stack) {
		unsafe {
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
}

// A lease threshold came due (the serve loop's periodic wake fired): send what the
// phase calls for and advance the clock. Renewal REQUESTs go out here and their
// ACK arrives through the standing loop's pump (`LeaseClock::on_reply`); only a
// full re-acquisition after expiry runs the blocking handshake, since there is no
// held address left to serve with in the meantime.
unsafe fn lease_due(frames: u64, stack: &mut Stack, lease: &mut LeaseClock, rx: &mut [u8], tx: &mut [u8]) {
	unsafe {
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
// One-shot TCP exchange driver for `fetch`: establish, send the request, then
// accumulate the response into `reply` until the peer closes or it falls quiet.
// Returns the establish status (1 = ok, 2 = unreachable, 3 = refused, 0 = timeout).
#[allow(clippy::too_many_arguments)]
unsafe fn do_tcp(ci: usize, ip: Ipv4Addr, port: u16, request: &[u8], frames: u64, stack: &mut Stack, rx: &mut [u8], tx: &mut [u8], reply: &mut Vec<u8>) -> u8 {
	unsafe {
		// Establish the connection; on failure report the status and stop (the
		// establish status bytes 2 / 3 / 0 map straight onto the fetch errors).
		match tcp_establish(ci, ip, port, frames, stack, rx, tx) {
			1 => {}
			other => return other,
		}
		// Send the request, then read the response until the peer closes or it
		// falls quiet - the response grows in the Vec, never against a cap.
		if !request.is_empty() {
			let d: usize = stack.tcp_build_data(ci, request, tx);
			send_frame(frames, &tx[..d]);
		}
		let recv_deadline: u64 = clock() + TCP_RECV_TIMEOUT_TICKS;
		while clock() < recv_deadline && !stack.tcp_peer_fin(ci) && !stack.tcp_aborted(ci) {
			if wait(frames, recv_deadline) != 0 {
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
		1
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
// stream `producer` - one chunk per drain, as large as the connection's receive
// buffer held; each drained chunk is followed by a window-update ACK to the peer
// (the drain reopened the receive window). Returns the producer handle, or 0 if
// the consumer was dropped (in which case the producer is closed here).
unsafe fn stream_pump(ci: usize, frames: u64, stack: &mut Stack, tx: &mut [u8], producer: u64, seq: &mut u32) -> u64 {
	unsafe {
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
			let mut frame_handle: u64 = 0;
			match socket::recv_frame(*seq, &chunk, &mut frame, &mut frame_handle) {
				Some(fl) => {
					if !send_blocking(producer, &frame[..fl], frame_handle) {
						if frame_handle != 0 {
							close(frame_handle);
						}
						close(producer);
						return 0;
					}
					*seq += 1;
				}
				None => {
					if frame_handle != 0 {
						close(frame_handle);
					}
					return producer;
				}
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

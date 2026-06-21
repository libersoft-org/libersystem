// NetworkService - the standing userspace network service (M33).
//
// M32 housed the L2/L3 stack inside driver.virtio-net; M33 extracts it here. The
// driver is now a pure frame-mover: it owns the NIC and the virtqueues and, over a
// single channel, forwards each received Ethernet frame to this service and
// transmits each frame this service hands back. NetworkService owns the stack
// (`net`): it learns the NIC's MAC from the driver, answers ARP and ICMP, and
// serves clients (the shell, later the net tools) the `ip` / `ping` / `nslookup`
// control protocol - the same raw protocol M32's shell spoke to the driver, moved
// behind the service. It stands on the driver's frame channel and its client
// channel at once with `wait_any`, so an inbound frame and a client request never
// block each other. The typed sockets API (Endpoint/SocketAddr, listen/accept/
// connect) and many-client serving land on top of this seam next.

#![no_std]
#![no_main]

mod net;

use rt::*;

use crate::net::{Event, Ipv4Addr, MacAddr, Stack};

// Static addressing for the QEMU user-mode (SLIRP) network: the guest is
// 10.0.2.15/24, the gateway/host is 10.0.2.2, and the DNS relay is 10.0.2.3. A DHCP
// client (M33) later replaces this static configuration.
const OUR_IP: Ipv4Addr = Ipv4Addr([10, 0, 2, 15]);
const GATEWAY_IP: Ipv4Addr = Ipv4Addr([10, 0, 2, 2]);
const DNS_SERVER: Ipv4Addr = Ipv4Addr([10, 0, 2, 3]);
// The UDP source port we send DNS queries from.
const DNS_SRC_PORT: u16 = 0x9876;

// How long a `ping` waits for its reply, and DNS for its response (100 Hz ticks).
const PING_TIMEOUT_TICKS: u64 = 50;
const DNS_TIMEOUT_TICKS: u64 = 300;

// The frame-buffer size: one full Ethernet frame (1514 bytes) with slack. The
// driver forwards frames without the virtio_net_hdr, so this is the L2 frame only.
const FRAME_MAX: usize = 2048;

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
		let mut stack: Stack = Stack::new(mac, OUR_IP, GATEWAY_IP);
		// 3. report in, then announce ourselves on the link with a gratuitous ARP for
		//    the gateway (the driver transmits it).
		send_blocking(bootstrap, b"NetworkService: online", 0);
		let mut tx: [u8; FRAME_MAX] = [0u8; FRAME_MAX];
		let arp: usize = stack.build_arp_request(GATEWAY_IP, &mut tx);
		send_frame(frames, &tx[..arp]);
		// 4. serve the network and the client at once.
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

// Stand on the driver's frame channel and the client's control channel at once
// (wait_any): a frame from the driver is parsed (answering ARP / ICMP, any reply
// sent back to transmit); a control message is answered (`ip` interface state,
// `ping` an echo, `nslookup` a DNS lookup). The client channel closing (the shell
// exited) drops us back to serving only the network.
unsafe fn serve(frames: u64, client: u64, stack: &mut Stack) -> ! {
	unsafe {
		let mut rx: [u8; FRAME_MAX] = [0u8; FRAME_MAX];
		let mut tx: [u8; FRAME_MAX] = [0u8; FRAME_MAX];
		let mut ctl: [u8; 128] = [0u8; 128];
		let mut seq: u16 = 0;
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
				match recv_blocking(client, &mut ctl) {
					Received::Message { len, .. } => handle_control(&ctl[..len], client, frames, stack, &mut seq, &mut rx, &mut tx),
					Received::Closed => client_open = false,
				}
			}
		}
	}
}

// Answer one control request: `ip` replies with the serialized interface state;
// `PING` + a 4-byte address sends an echo request and reports the reply; `DNS` + a
// name resolves it. Each reply leads with a status byte.
#[allow(clippy::too_many_arguments)]
unsafe fn handle_control(req: &[u8], client: u64, frames: u64, stack: &mut Stack, seq: &mut u16, rx: &mut [u8], tx: &mut [u8]) {
	unsafe {
		let mut out: [u8; 128] = [0u8; 128];
		if req == b"IP" {
			let n: usize = stack.write_state(&mut out);
			send_blocking(client, &out[..n], 0);
		} else if req.len() >= 8 && &req[..4] == b"PING" {
			let ip: Ipv4Addr = Ipv4Addr([req[4], req[5], req[6], req[7]]);
			let status: u8 = do_ping(ip, frames, stack, seq, rx, tx);
			out[0] = status;
			out[1..5].copy_from_slice(&ip.0);
			send_blocking(client, &out[..5], 0);
		} else if req.len() > 3 && &req[..3] == b"DNS" {
			match do_dns(&req[3..], frames, stack, seq, rx, tx) {
				Some(addr) => {
					out[0] = 1;
					out[1..5].copy_from_slice(&addr.0);
					send_blocking(client, &out[..5], 0);
				}
				None => {
					out[0] = 0;
					send_blocking(client, &out[..1], 0);
				}
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
		let mac: MacAddr = match resolve(ip, frames, stack, rx, tx) {
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
		let mac: MacAddr = match resolve(DNS_SERVER, frames, stack, rx, tx) {
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

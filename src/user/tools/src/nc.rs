// nc - a standalone foreground net tool the shell spawns: a raw TCP client.
//
// The general form of `tcp` (which is a fixed HTTP GET probe): the shell mints a
// fresh NetworkService client channel (network.open), spawns this program, and
// transfers that channel alongside `<ip> <port> [request...]`. nc opens a TCP
// connection over its OWN channel - NetworkService hands back the socket as a
// capability (the channel a `socket` interface is served on) - reports it, and, if a
// request was given, sends it (zero-copy as a buffer) and drains the response stream
// until the peer closes; with no request it is a bare connectivity check. A
// standalone program, not a shell built-in.

#![no_std]
#![no_main]

extern crate alloc;

use proto::codec::Buffer;
use proto::system::{Endpoint, Error, Ipv4Addr, network, socket};
use rt::*;
use tools::{parse_port, trim};

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// The shell hands us `<ip> <port> [request...]` plus our NetworkService channel.
		inherit_stdout(bootstrap);
		let (len, netsvc): (usize, u64) = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } => (len, handle),
			Received::Closed => exit(),
		};
		connect(netsvc, &buf[..len]);
		close(netsvc);
	}
	exit();
}

// Parse `<ip> <port> [request...]`, open the connection, optionally send the request
// and stream the response.
unsafe fn connect(netsvc: u64, args: &[u8]) {
	unsafe {
		let sp: usize = match args.iter().position(|&b: &u8| b == b' ') {
			Some(i) => i,
			None => {
				print(b"nc: usage: nc <ip> <port> [request]\n");
				return;
			}
		};
		let host: &[u8] = trim(&args[..sp]);
		let rest: &[u8] = trim(&args[sp + 1..]);
		// The port runs to the next space; anything after it is the request payload.
		let (port_bytes, request): (&[u8], &[u8]) = match rest.iter().position(|&b: &u8| b == b' ') {
			Some(i) => (trim(&rest[..i]), trim(&rest[i + 1..])),
			None => (rest, &[][..]),
		};
		let addr: Ipv4Addr = match Ipv4Addr::parse(host) {
			Some(a) => a,
			None => {
				print(b"nc: invalid address\n");
				return;
			}
		};
		let port: u16 = match parse_port(port_bytes) {
			Some(p) => p,
			None => {
				print(b"nc: invalid port\n");
				return;
			}
		};
		// connect() returns the socket as a capability (the channel it is served on).
		let mut net = network::Client::new(ChannelTransport { chan: netsvc });
		let ep: Endpoint = Endpoint { addr, port };
		let sockh: u64 = match net.connect(&ep) {
			Some(Ok(h)) => h,
			Some(Err(Error::NotFound)) => {
				print(b"nc: unreachable (no route)\n");
				return;
			}
			Some(Err(Error::Denied)) => {
				print(b"nc: connection refused\n");
				return;
			}
			Some(Err(_)) => {
				print(b"nc: connection timed out\n");
				return;
			}
			None => {
				print(b"nc: service unavailable\n");
				return;
			}
		};
		let mut sock = socket::Client::new(ChannelTransport { chan: sockh });
		print(b"nc ");
		print(host);
		print(b": connected\n");
		// With a request, send it (zero-copy as a buffer, terminated so an HTTP request
		// line completes) and drain the response stream until the peer closes; with no
		// request, a bare connectivity check - close without draining (a server that
		// never closes would otherwise wedge us, as there is no stdin to drive nc).
		if !request.is_empty() {
			if send_request(&mut sock, request) {
				drain(&mut sock);
			} else {
				print(b"nc: send failed\n");
			}
		}
		let _ = sock.close();
		close(sockh);
	}
}

// Send `request` followed by a blank line (`\r\n\r\n`, so an HTTP request line ends)
// as a zero-copy buffer: the bytes live in a fresh shared memory object whose handle
// is transferred by the send (consumed by the transfer, so we map-fill-unmap but do
// not close it). Returns whether the send succeeded.
unsafe fn send_request(sock: &mut socket::Client<ChannelTransport>, request: &[u8]) -> bool {
	unsafe {
		let n: usize = request.len();
		if n > 256 {
			return false;
		}
		let handle: i64 = memory_object_create((n + 4) as u64);
		if handle < 0 {
			return false;
		}
		let handle: u64 = handle as u64;
		let base: u64 = match map_object(handle) {
			Some(b) => b,
			None => {
				close(handle);
				return false;
			}
		};
		let dst: *mut u8 = base as *mut u8;
		core::ptr::copy_nonoverlapping(request.as_ptr(), dst, n);
		core::ptr::copy_nonoverlapping(b"\r\n\r\n".as_ptr(), dst.add(n), 4);
		unmap_object(handle);
		let buf: Buffer = Buffer { handle, len: (n + 4) as u64 };
		matches!(sock.send(&buf), Some(Ok(_)))
	}
}

// Drain the socket's received-data stream (a sub-channel of framed chunks), printing
// each chunk, until the producer closes - end of stream (the peer's FIN).
unsafe fn drain(sock: &mut socket::Client<ChannelTransport>) {
	unsafe {
		if let Some(rxstream) = sock.recv() {
			let mut frame: [u8; 1024] = [0u8; 1024];
			loop {
				match recv_blocking(rxstream, &mut frame) {
					Received::Message { len, .. } => {
						if let Some(chunk) = socket::recv_read(&frame[..len]) {
							print(&chunk.data);
						}
					}
					Received::Closed => break,
				}
			}
			close(rxstream);
		}
		print(b"\n");
	}
}

// `parse_port` and `trim` come from the shared tools crate.

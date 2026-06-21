// tcp - a standalone foreground net tool the shell spawns (the `tcp` command).
//
// The shell mints a fresh NetworkService client channel (network.open), spawns this
// program, and transfers that channel to it alongside `<ip> <port>` as its
// arguments. tcp opens a TCP connection over its OWN channel - NetworkService hands
// back the socket as a capability (the channel a `socket` interface is served on) -
// sends a minimal HTTP/1.0 GET probe, drains the response as a wait-drained event
// stream of chunks until end of stream, closes, signals completion, and exits. A
// standalone program, not a shell built-in - the last of the network commands to
// move out of the shell.

#![no_std]
#![no_main]

extern crate alloc;

use proto::system::{Endpoint, Error, Ipv4Addr, network, socket};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 128] = [0u8; 128];
	unsafe {
		// The shell hands us `<ip> <port>` plus our NetworkService client channel.
		let (len, netsvc): (usize, u64) = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } => (len, handle),
			Received::Closed => exit(),
		};
		connect(netsvc, &buf[..len]);
		close(netsvc);
		send_blocking(bootstrap, b"done", 0);
	}
	exit();
}

// Parse `<ip> <port>`, open the connection, send a probe, and stream the response.
unsafe fn connect(netsvc: u64, args: &[u8]) {
	unsafe {
		let sp: usize = match args.iter().position(|&b: &u8| b == b' ') {
			Some(i) => i,
			None => {
				print(b"tcp: usage: tcp <ip> <port>\n");
				return;
			}
		};
		let host: &[u8] = trim(&args[..sp]);
		let addr: Ipv4Addr = match Ipv4Addr::parse(host) {
			Some(a) => a,
			None => {
				print(b"tcp: invalid address\n");
				return;
			}
		};
		let port: u16 = match parse_port(trim(&args[sp + 1..])) {
			Some(p) => p,
			None => {
				print(b"tcp: invalid port\n");
				return;
			}
		};
		// connect() returns the socket as a capability (the channel it is served on).
		let mut net = network::Client::new(ChannelTransport { chan: netsvc });
		let ep: Endpoint = Endpoint { addr, port };
		let sockh: u64 = match net.connect(&ep) {
			Some(Ok(h)) => h,
			Some(Err(Error::NotFound)) => {
				print(b"tcp: unreachable (no route)\n");
				return;
			}
			Some(Err(Error::Denied)) => {
				print(b"tcp: connection refused\n");
				return;
			}
			Some(Err(_)) => {
				print(b"tcp: connection timed out\n");
				return;
			}
			None => {
				print(b"tcp: service unavailable\n");
				return;
			}
		};
		let mut sock = socket::Client::new(ChannelTransport { chan: sockh });
		print(b"tcp ");
		print(host);
		print(b": connected\n");
		// Send the probe, then drain the received-data stream (a sub-channel of framed
		// chunks) until the producer closes - end of stream.
		if let Some(Ok(_)) = sock.send(&b"GET / HTTP/1.0\r\n\r\n".to_vec()) {
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
		} else {
			print(b"tcp: send failed\n");
		}
		let _ = sock.close();
		close(sockh);
	}
}

// Parse a decimal port number (0-65535), or None if malformed or out of range.
fn parse_port(s: &[u8]) -> Option<u16> {
	if s.is_empty() || s.len() > 5 {
		return None;
	}
	let mut v: u32 = 0;
	for &b in s {
		if !b.is_ascii_digit() {
			return None;
		}
		v = v * 10 + (b - b'0') as u32;
		if v > 65535 {
			return None;
		}
	}
	Some(v as u16)
}

// Trim leading and trailing ASCII whitespace.
fn trim(s: &[u8]) -> &[u8] {
	let mut start: usize = 0;
	let mut end: usize = s.len();
	while start < end && s[start].is_ascii_whitespace() {
		start += 1;
	}
	while end > start && s[end - 1].is_ascii_whitespace() {
		end -= 1;
	}
	&s[start..end]
}

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

use network_client::{NetworkClient, SocketClient};
use proto::codec::Buffer;
use proto::system::{Error, LaunchContext, OpenTarget, socket};
use rt::*;
#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 128] = [0u8; 128];
	// Governed launch sends arguments first, then the tagged NetworkService grant.
	inherit_stdout(bootstrap);
	let Some((context_bytes, attached)) = recv_launch_with(bootstrap) else { exit() };
	let context: LaunchContext = match LaunchContext::decode(&context_bytes) {
		Some(context) => context,
		None => exit(),
	};
	// THE ARGUMENTS ARE THE LAUNCH CONTEXT'S, NOT THE CAPABILITY BUFFER'S. `granted_capability`
	// receives a tagged message into `buf`, so reading `buf` back as the argument reads whatever that
	// left there - or nothing at all on the governed path, where the channel is already attached and
	// `buf` is never written.
	let argument: alloc::vec::Vec<u8> = context.arguments.clone().into_bytes();
	let netsvc: u64 = granted_capability(bootstrap, attached, CAP_NETWORK, &mut buf).unwrap_or_else(|| exit());
	connect(netsvc, &argument);
	close(netsvc);
	exit();
}

fn trim(mut bytes: &[u8]) -> &[u8] {
	while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[1..];
	}
	while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
		bytes = &bytes[..bytes.len() - 1];
	}
	bytes
}

fn parse_port(bytes: &[u8]) -> Option<u16> {
	if bytes.is_empty() || bytes.len() > 5 {
		return None;
	}
	let mut value: u32 = 0;
	for &byte in bytes {
		if !byte.is_ascii_digit() {
			return None;
		}
		value = value.checked_mul(10)?.checked_add((byte - b'0') as u32)?;
	}
	u16::try_from(value).ok()
}

// Parse `<ip> <port>`, open the connection, send a probe, and stream the response.
fn connect(netsvc: u64, args: &[u8]) {
	unsafe {
		let sp: usize = match args.iter().position(|&b: &u8| b == b' ') {
			Some(i) => i,
			None => {
				eprint(b"tcp: usage: tcp <ip> <port>\n");
				return;
			}
		};
		let host: &[u8] = trim(&args[..sp]);

		let port: u16 = match parse_port(trim(&args[sp + 1..])) {
			Some(p) => p,
			None => {
				eprint(b"tcp: invalid port\n");
				return;
			}
		};
		// connect() returns the socket as a capability (the channel it is served on).
		let mut net = NetworkClient::new(netsvc);
		// A NAME OR AN ADDRESS, AND EVERY ADDRESS THE NAME HAS. The service owns the order and tries
		// the candidates in it; handing over only the first would make this tool decide, badly, what
		// happens when a host's first address is unreachable.
		let Some(destinations) = tools::resolve_all(&mut net, host) else {
			eprint(b"tcp: cannot resolve the host\n");
			return;
		};
		let target: OpenTarget = OpenTarget { destinations, port, source: None };
		let sockh: u64 = match net.connect(&target) {
			Some(Ok(h)) => h,
			Some(Err(Error::NotFound)) => {
				eprint(b"tcp: unreachable (no route)\n");
				return;
			}
			Some(Err(Error::Denied)) => {
				eprint(b"tcp: connection refused\n");
				return;
			}
			Some(Err(_)) => {
				eprint(b"tcp: connection timed out\n");
				return;
			}
			None => {
				eprint(b"tcp: service unavailable\n");
				return;
			}
		};
		let mut sock = SocketClient::new(sockh);
		print(b"tcp ");
		print(host);
		print(b": connected\n");
		// Send the probe as a zero-copy buffer - the request bytes live in a shared
		// memory object whose handle we hand to NetworkService, so the payload never
		// crosses the channel - then drain the received-data stream (a sub-channel of
		// framed chunks) until the producer closes - end of stream.
		let probe: Buffer = match make_buffer(b"GET / HTTP/1.0\r\n\r\n") {
			Some(b) => b,
			None => {
				eprint(b"tcp: out of memory\n");
				let _ = sock.close();
				close(sockh);
				return;
			}
		};
		if let Some(Ok(_)) = sock.send(&probe) {
			if let Some(rxstream) = sock.recv() {
				let mut frame: [u8; 1024] = [0u8; 1024];
				loop {
					match recv_caps_blocking(rxstream, &mut frame) {
						ReceivedCaps::Message { len, handles: mut frame_handles } => {
							if let Some(chunk) = socket::recv_read(&frame[..len], &mut frame_handles) {
								print(&chunk.data);
							}
							for handle in frame_handles.as_slice() {
								close(*handle);
							}
						}
						ReceivedCaps::Closed => break,
					}
				}
				close(rxstream);
			}
			print(b"\n");
		} else {
			eprint(b"tcp: send failed\n");
		}
		let _ = sock.close();
		close(sockh);
	}
}

// Pack `bytes` into a fresh shared memory object and describe it as a `buffer`: the
// returned handle is transferred when the buffer is sent (consumed by the transfer),
// so we map-fill-unmap here but must not close it. None if the object cannot be made.
unsafe fn make_buffer(bytes: &[u8]) -> Option<Buffer> {
	unsafe {
		let handle: i64 = memory_object_create(bytes.len() as u64);
		if handle < 0 {
			return None;
		}
		let handle: u64 = handle as u64;
		let base: u64 = match map_object(handle) {
			Some(b) => b,
			None => {
				close(handle);
				return None;
			}
		};
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), base as *mut u8, bytes.len());
		unmap_object(handle);
		Some(Buffer { handle, len: bytes.len() as u64 })
	}
}

// `parse_port` and `trim` come from the shared tools crate.

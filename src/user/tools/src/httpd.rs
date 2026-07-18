// httpd - a minimal standalone HTTP server the shell spawns (in the background).
//
// The shell mints a fresh NetworkService client channel (network.open), spawns this
// program, and transfers that channel to it. httpd opens a listening socket on port
// 80 (passive open), then loops: accept an inbound connection - NetworkService hands
// it back as a `socket` capability (the channel a `socket` interface is served on) -
// serve a canned HTTP response (zero-copy as a `buffer`), and close. It runs until its
// NetworkService channel closes. The server counterpart to the `tcp` / `nc` clients,
// demonstrating listen / accept and concurrent sockets.

#![no_std]
#![no_main]

extern crate alloc;

use network_client::{ListenerClient, NetworkClient, SocketClient};
use proto::codec::Buffer;
use rt::*;

// The port we listen on, and the canned response - `Connection: close` so the client
// reads the close-delimited body (no Content-Length needed).
const HTTP_PORT: u16 = 80;
const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body><h1>Hello from LiberSystem httpd</h1></body></html>\n";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	unsafe {
		// The shell transfers our NetworkService client channel (we take no arguments).
		inherit_stdout(bootstrap);
		let netsvc: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { handle, .. } => handle,
			Received::Closed => exit(),
		};
		serve(netsvc);
		close(netsvc);
	}
	exit();
}

// Open the listening socket and accept connections forever, serving each one.
unsafe fn serve(netsvc: u64) {
	unsafe {
		let mut net = NetworkClient::new(netsvc);
		let listen_chan: u64 = match net.listen(&HTTP_PORT) {
			Some(Ok(h)) => h,
			_ => {
				print(b"httpd: listen failed\n");
				return;
			}
		};
		print(b"httpd: listening on port 80\n");
		let mut lis = ListenerClient::new(listen_chan);
		// accept() blocks until an inbound connection completes; the loop ends when the
		// listener channel closes (NetworkService gone).
		loop {
			let sockh: u64 = match lis.accept() {
				Some(Ok(h)) => h,
				_ => break,
			};
			respond(sockh);
			close(sockh);
		}
		close(listen_chan);
	}
}

// Serve one connection: send the canned response zero-copy (a shared memory object
// whose handle transfers to NetworkService) and close. We respond without reading the
// request first - `Connection: close` plus a fixed body is a complete exchange, and
// not blocking on a read keeps a request-less connection from wedging the accept loop.
unsafe fn respond(sockh: u64) {
	unsafe {
		let mut sock = SocketClient::new(sockh);
		match make_buffer(RESPONSE) {
			Some(body) => {
				let _ = sock.send(&body);
				print(b"httpd: served a request\n");
			}
			None => print(b"httpd: out of memory\n"),
		}
		let _ = sock.close();
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

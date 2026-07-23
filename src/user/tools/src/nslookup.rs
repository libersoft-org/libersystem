// nslookup - a standalone foreground net tool the shell spawns (also reached as
// `host`).
//
// The shell mints a fresh NetworkService client channel (network.open), spawns this
// program, and transfers that channel to it alongside the name to resolve as its
// argument. nslookup asks NetworkService's DNS client to resolve the name over its
// OWN channel and renders the address, then signals completion and exits. A
// standalone program, not a shell built-in.

#![no_std]
#![no_main]

extern crate alloc;

use network_client::NetworkClient;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 128] = [0u8; 128];
	unsafe {
		// Governed launch sends arguments first, then the tagged NetworkService grant.
		inherit_stdout(bootstrap);
		let len: usize = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => len,
			Received::Closed => exit(),
		};
		let netsvc: u64 = recv_tagged(bootstrap, &mut buf, b"NETWORK").unwrap_or_else(|| exit());
		resolve(netsvc, &buf[..len]);
		close(netsvc);
	}
	exit();
}

// Resolve `name` over NetworkService's DNS client and render the address, or a
// not-found message.
unsafe fn resolve(netsvc: u64, name: &[u8]) {
	unsafe {
		if name.is_empty() || name.len() > 120 {
			print(b"nslookup: invalid name\n");
			return;
		}
		let name_str: &str = match core::str::from_utf8(name) {
			Ok(s) => s,
			Err(_) => {
				print(b"nslookup: invalid name\n");
				return;
			}
		};
		let mut client = NetworkClient::new(netsvc);
		match client.resolve(name_str) {
			Some(Ok(addr)) => {
				print(name);
				print(b" has address ");
				let mut tmp: [u8; 16] = [0u8; 16];
				let n: usize = addr.render(&mut tmp);
				print(&tmp[..n]);
				print(b"\n");
			}
			Some(Err(_)) => {
				print(b"nslookup: could not resolve ");
				print(name);
				print(b"\n");
			}
			None => print(b"nslookup: network service gone\n"),
		}
	}
}

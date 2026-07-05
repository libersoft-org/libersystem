// ip - a standalone foreground net tool the shell spawns (also reached as `net`).
//
// The shell mints a fresh NetworkService client channel (network.open), spawns this
// program, and transfers that channel to it. ip asks NetworkService for the
// interface state over its OWN channel and renders it - our address, MAC, gateway,
// and the neighbor cache - then signals completion and exits. A standalone program,
// not a shell built-in.

#![no_std]
#![no_main]

extern crate alloc;

use proto::addr::write_mac;
use proto::system::network;
use rt::*;

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
		show(netsvc);
		close(netsvc);
	}
	exit();
}

// Query and render the interface state: our address, MAC, gateway, and neighbors.
unsafe fn show(netsvc: u64) {
	unsafe {
		let mut client = network::Client::new(ChannelTransport { chan: netsvc });
		match client.info() {
			Some(Ok(info)) => {
				let mut tmp: [u8; 18] = [0u8; 18];
				print(b"net0: ");
				let n: usize = info.addr.render(&mut tmp);
				print(&tmp[..n]);
				print(b"  mac ");
				let n: usize = write_mac(&info.mac, &mut tmp);
				print(&tmp[..n]);
				print(b"  gateway ");
				let n: usize = info.gateway.render(&mut tmp);
				print(&tmp[..n]);
				print(b"\n");
				if !info.neighbors.is_empty() {
					print(b"neighbors:\n");
					for ngh in &info.neighbors {
						print(b"  ");
						let n: usize = ngh.addr.render(&mut tmp);
						print(&tmp[..n]);
						print(b"  ");
						let n: usize = write_mac(&ngh.mac, &mut tmp);
						print(&tmp[..n]);
						print(b"\n");
					}
				}
			}
			Some(Err(_)) => print(b"ip: network error\n"),
			None => print(b"ip: service unavailable\n"),
		}
	}
}

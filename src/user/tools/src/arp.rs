// arp - a standalone foreground net tool the shell spawns: the ARP / neighbor table.
//
// A focused subset of `ip`: the shell mints a fresh NetworkService client channel
// (network.open), spawns this program, and transfers that channel. arp asks
// NetworkService for the interface state over its OWN channel and renders just the
// neighbor cache - the on-link address -> MAC mappings the stack has resolved - then
// signals completion and exits. A standalone program, not a shell built-in.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
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

// Query the interface state and render just the neighbor cache (the ARP table), one
// `<addr> at <mac>` line per entry.
unsafe fn show(netsvc: u64) {
	unsafe {
		let mut client = network::Client::new(ChannelTransport { chan: netsvc });
		match client.info() {
			Some(Ok(info)) => {
				if info.neighbors.is_empty() {
					print(b"arp: no neighbors\n");
					return;
				}
				let mut tmp: [u8; 18] = [0u8; 18];
				for ngh in &info.neighbors {
					let n: usize = ngh.addr.render(&mut tmp);
					print(&tmp[..n]);
					print(b" at ");
					let n: usize = write_mac(&ngh.mac, &mut tmp);
					print(&tmp[..n]);
					print(b"\n");
				}
			}
			Some(Err(_)) => print(b"arp: network error\n"),
			None => print(b"arp: service unavailable\n"),
		}
	}
}

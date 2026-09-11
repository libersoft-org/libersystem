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

use network_client::NetworkClient;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	// Governed launch sends arguments first, then the tagged NetworkService grant.
	inherit_stdout(bootstrap);
	let Some((_, attached)) = recv_launch_with(bootstrap) else { exit() };
	let netsvc: u64 = granted_capability(bootstrap, attached, CAP_NETWORK, &mut buf).unwrap_or_else(|| exit());
	show(netsvc);
	close(netsvc);
	exit();
}

// Query the interface state and render just the neighbor cache (the ARP table), one
// `<addr> at <mac>` line per entry.
fn show(netsvc: u64) {
	let mut client = NetworkClient::new(netsvc);
	match client.info() {
		Some(Ok(info)) => {
			// THIS TOOL IS EXPLICITLY IPv4. The name is ARP's and the table it shows is ARP's; the
			// other family's neighbours are shown by `ip`, which has a section for them. Putting them
			// here would make one view of two tables under a name that means one of them.
			if !info.neighbors.iter().any(|ngh| ngh.addr.is_v4()) {
				eprint(b"arp: no neighbors\n");
				return;
			}
			// Wide enough for the longest address there is, so a shared buffer never truncates.
			let mut tmp: [u8; 64] = [0u8; 64];
			for ngh in info.neighbors.iter().filter(|ngh| ngh.addr.is_v4()) {
				let n: usize = ngh.addr.render(&mut tmp);
				print(&tmp[..n]);
				print(b" at ");
				let n: usize = ngh.mac.render(&mut tmp);
				print(&tmp[..n]);
				print(b"\n");
			}
		}
		Some(Err(_)) => eprint(b"arp: network error\n"),
		None => eprint(b"arp: service unavailable\n"),
	}
}

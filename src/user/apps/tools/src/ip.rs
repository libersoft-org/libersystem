// ip - a standalone foreground net tool the shell spawns (also reached as `net`).
//
// The shell mints a fresh NetworkService client channel (network.open), spawns this
// program, and transfers that channel to it. ip asks NetworkService for the
// interface state over its OWN channel and renders it - every address the interface
// holds, every route, router and resolver it knows, and its neighbor cache - then
// signals completion and exits. A standalone program, not a shell built-in.

#![no_std]
#![no_main]

extern crate alloc;

use network_client::NetworkClient;
use proto::generated::liber::network::v1::{AddressState, NextHop, Reachability, RoutePreference};
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

// Query and render the interface state.
//
// ONE SECTION PER TABLE, because they are separate tables and folding them into one line is what the
// single-address, single-gateway view used to do - a shape that could not say "two routers" or "this
// address is deprecated" at all.
fn show(netsvc: u64) {
	let mut client = NetworkClient::new(netsvc);
	let info = match client.info() {
		Some(Ok(info)) => info,
		Some(Err(_)) => return eprint(b"ip: network error\n"),
		None => return eprint(b"ip: service unavailable\n"),
	};
	// Wide enough for the longest address there is, its prefix length and its scope.
	let mut tmp: [u8; 96] = [0u8; 96];

	print(info.name.as_bytes());
	print(b" (if");
	emit_dec(u64::from(info.scope.index), &mut tmp);
	print(b".");
	emit_dec(info.scope.generation, &mut tmp);
	print(b"): mac ");
	let n: usize = info.mac.render(&mut tmp);
	print(&tmp[..n]);
	print(b"  mtu ");
	emit_dec(u64::from(info.mtu), &mut tmp);
	print(b"\n");

	for address in &info.addresses {
		print(b"  address ");
		let n: usize = address.addr.render(&mut tmp);
		print(&tmp[..n]);
		print(b"/");
		emit_dec(u64::from(address.prefix_len), &mut tmp);
		print(b" ");
		print(match address.state {
			AddressState::Tentative => b"tentative".as_slice(),
			AddressState::Preferred => b"preferred".as_slice(),
			AddressState::Deprecated => b"deprecated".as_slice(),
			AddressState::Invalid => b"invalid".as_slice(),
		});
		print(b"\n");
	}
	for route in &info.routes {
		print(b"  route ");
		let n: usize = route.destination.render(&mut tmp);
		print(&tmp[..n]);
		print(b"/");
		emit_dec(u64::from(route.prefix_len), &mut tmp);
		match &route.hop {
			// A DIRECT ROUTE HAS NO NEXT HOP AND SAYS SO. Printing an all-zeros address here is how
			// a reader comes to believe there is a router at `::`.
			NextHop::Direct => print(b" direct"),
			NextHop::Via(addr) => {
				print(b" via ");
				let n: usize = addr.render(&mut tmp);
				print(&tmp[..n]);
			}
		}
		print(b"\n");
	}
	for router in &info.routers {
		print(b"  router ");
		let n: usize = router.addr.render(&mut tmp);
		print(&tmp[..n]);
		print(b" ");
		print(preference(&router.preference));
		print(b" ");
		print(reachability(&router.state));
		print(b"\n");
	}
	for server in &info.dns {
		print(b"  dns ");
		let n: usize = server.addr.render(&mut tmp);
		print(&tmp[..n]);
		print(b"\n");
	}
	for ngh in &info.neighbors {
		print(b"  neighbor ");
		let n: usize = ngh.addr.render(&mut tmp);
		print(&tmp[..n]);
		print(b" at ");
		let n: usize = ngh.mac.render(&mut tmp);
		print(&tmp[..n]);
		print(b"\n");
	}
}

fn preference(value: &RoutePreference) -> &'static [u8] {
	match value {
		RoutePreference::Low => b"low",
		RoutePreference::Medium => b"medium",
		RoutePreference::High => b"high",
	}
}

fn reachability(value: &Reachability) -> &'static [u8] {
	match value {
		Reachability::Incomplete => b"incomplete",
		Reachability::Reachable => b"reachable",
		Reachability::Stale => b"stale",
		Reachability::Delay => b"delay",
		Reachability::Probe => b"probe",
		Reachability::Unreachable => b"unreachable",
	}
}

fn emit_dec(value: u64, out: &mut [u8]) {
	let n: usize = write_dec(value, out);
	print(&out[..n]);
}

// Render a decimal number into `out`, returning the rendered length.
fn write_dec(mut v: u64, out: &mut [u8]) -> usize {
	let mut digits: [u8; 20] = [0u8; 20];
	let mut n: usize = 0;
	loop {
		digits[n] = b'0' + (v % 10) as u8;
		v /= 10;
		n += 1;
		if v == 0 {
			break;
		}
	}
	for i in 0..n {
		out[i] = digits[n - 1 - i];
	}
	n
}

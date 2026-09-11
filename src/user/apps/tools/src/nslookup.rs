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
use proto::system::LaunchContext;
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
	let argument: &[u8] = context.arguments.as_bytes();
	let len: usize = argument.len();
	let netsvc: u64 = granted_capability(bootstrap, attached, CAP_NETWORK, &mut buf).unwrap_or_else(|| exit());
	resolve(netsvc, &buf[..len]);
	close(netsvc);
	exit();
}

// Resolve `name` over NetworkService's DNS client and render the address, or a
// not-found message.
fn resolve(netsvc: u64, name: &[u8]) {
	if name.is_empty() || name.len() > 120 {
		eprint(b"nslookup: invalid name\n");
		return;
	}
	let name_str: &str = match core::str::from_utf8(name) {
		Ok(s) => s,
		Err(_) => {
			eprint(b"nslookup: invalid name\n");
			return;
		}
	};
	let mut client = NetworkClient::new(netsvc);
	match client.resolve(name_str) {
		Some(Ok(addresses)) => {
			// EVERY ADDRESS, IN THE ORDER THE SERVICE CHOSE. A name with four addresses had three of
			// them thrown away by a contract that returned one, and the one kept was whichever the
			// resolver happened to put first - so a caller could not prefer, retry or even see the
			// rest.
			if addresses.is_empty() {
				eprint(b"nslookup: no addresses for ");
				eprint(name);
				eprint(b"\n");
				return;
			}
			let mut tmp: [u8; 64] = [0u8; 64];
			for addr in &addresses {
				print(name);
				print(b" has address ");
				let n: usize = addr.render(&mut tmp);
				print(&tmp[..n]);
				print(b"\n");
			}
		}
		Some(Err(_)) => {
			eprint(b"nslookup: could not resolve ");
			eprint(name);
			eprint(b"\n");
		}
		None => eprint(b"nslookup: network service gone\n"),
	}
}

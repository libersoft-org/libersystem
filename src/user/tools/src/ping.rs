// ping - a standalone foreground net tool the shell spawns.
//
// The shell mints a fresh NetworkService client channel (network.open), spawns this
// program, and transfers that channel to it alongside the target address as its
// argument. ping resolves the address, asks NetworkService to ping it over its OWN
// client channel, prints the outcome, signals completion, and exits. The net tools
// are separate programs - each with its own NetworkService capability - not shell
// built-ins; this is the first, replacing the shell's `ping` command.

#![no_std]
#![no_main]

extern crate alloc;

use proto::system::{Ipv4Addr, PingStatus, network};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 128] = [0u8; 128];
	unsafe {
		// The shell hands us our argument (the target address) plus our NetworkService
		// client channel as a transferred capability, in one message.
		inherit_stdout(bootstrap);
		let (len, netsvc): (usize, u64) = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } => (len, handle),
			Received::Closed => exit(),
		};
		ping(netsvc, &buf[..len]);
		// Drop our client channel (NetworkService reclaims the slot), then signal
		// completion so the shell's foreground wait returns, and exit.
		close(netsvc);
		send_blocking(bootstrap, b"done", 0);
	}
	exit();
}

// Resolve `target` (a dotted-decimal address) and ping it over NetworkService,
// rendering the outcome.
unsafe fn ping(netsvc: u64, target: &[u8]) {
	unsafe {
		let addr: Ipv4Addr = match Ipv4Addr::parse(target) {
			Some(a) => a,
			None => {
				print(b"ping: invalid address\n");
				return;
			}
		};
		let mut client = network::Client::new(ChannelTransport { chan: netsvc });
		print(b"ping ");
		print(target);
		match client.ping(&addr) {
			Some(Ok(PingStatus::Reply)) => print(b": reply\n"),
			Some(Ok(PingStatus::Unreachable)) => print(b": unreachable (no route)\n"),
			Some(Ok(PingStatus::Timeout)) => print(b": no reply (timeout)\n"),
			Some(Err(_)) => print(b": error\n"),
			None => print(b": service unavailable\n"),
		}
	}
}

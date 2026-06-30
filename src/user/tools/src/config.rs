// config - read the system configuration store, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - a ConfigService client - and forwards it the shell's stdout console and
// the argument string (the sub-form: "" lists the whole store, "<key>" reads one node).
// config queries the store through its grant and prints the result to the inherited stdout,
// then exits. A standalone command, not a shell built-in: it reaches the service only through
// the one capability the permission store granted it, and renders on the same terminal as the
// shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::config;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the sub-form ("" lists all, "<key>" reads one node).
		let args: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the one capability the manifest grants: a ConfigService client.
		let cfgsvc: u64 = recv_tagged(bootstrap, &mut buf, b"CONFIG").unwrap_or_else(|| exit());
		if args.is_empty() {
			list_config(cfgsvc);
		} else {
			get_config(cfgsvc, &args[..]);
		}
	}
	exit();
}

// List the whole configuration store through the grant, printing each node as one text line.
unsafe fn list_config(cfgsvc: u64) {
	unsafe {
		let mut client = config::Client::new(ChannelTransport { chan: cfgsvc });
		match client.list() {
			Some(Ok(entries)) => {
				for e in &entries {
					print(e.to_text().as_bytes());
					print(b"\n");
				}
			}
			Some(Err(_)) => print(b"config: query error\n"),
			None => print(b"config: service unavailable\n"),
		}
	}
}

// Read one configuration node by key through the grant and print its value.
unsafe fn get_config(cfgsvc: u64, key: &[u8]) {
	unsafe {
		let key = match core::str::from_utf8(key) {
			Ok(s) => s,
			Err(_) => {
				print(b"config: invalid key\n");
				return;
			}
		};
		let mut client = config::Client::new(ChannelTransport { chan: cfgsvc });
		match client.get(key) {
			Some(Ok(value)) => {
				print(value.as_bytes());
				print(b"\n");
			}
			Some(Err(_)) => {
				print(b"config: no such key ");
				print(key.as_bytes());
				print(b"\n");
			}
			None => print(b"config: service unavailable\n"),
		}
	}
}

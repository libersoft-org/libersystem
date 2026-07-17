// config - read and set the system configuration store, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - a ConfigService client - and forwards it the shell's stdout console and
// the argument string (the sub-form: "" lists the whole store, "<key>" reads one node,
// "set <key> <value>" writes one node - the owning service reads the new value at its next
// documented read point, a new VT for the console keys, the next boot for the boot-read ones).
// config drives the store through its grant and prints the result to the inherited stdout,
// then exits. A standalone command, not a shell built-in: it reaches the service only through
// the one capability the permission store granted it, and renders on the same terminal as the
// shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::ConfigEntry;
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
		} else if let Some(rest) = args.strip_prefix(b"set ") {
			set_config(cfgsvc, rest);
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

// Write one configuration node ("<key> <value>") through the grant and print the node
// back, confirming what the store now holds.
unsafe fn set_config(cfgsvc: u64, rest: &[u8]) {
	unsafe {
		let split: usize = match rest.iter().position(|&b| b == b' ') {
			Some(i) if i > 0 && i + 1 < rest.len() => i,
			_ => {
				print(b"config: usage: config set <key> <value>\n");
				return;
			}
		};
		let (key, value): (&str, &str) = match (core::str::from_utf8(&rest[..split]), core::str::from_utf8(&rest[split + 1..])) {
			(Ok(k), Ok(v)) => (k, v),
			_ => {
				print(b"config: invalid key or value\n");
				return;
			}
		};
		let entry: ConfigEntry = ConfigEntry { key: String::from(key), value: String::from(value) };
		let mut client = config::Client::new(ChannelTransport { chan: cfgsvc });
		match client.set(&entry) {
			Some(Ok(())) => {
				print(entry.to_text().as_bytes());
				print(b"\n");
			}
			Some(Err(_)) => print(b"config: set refused\n"),
			None => print(b"config: service unavailable\n"),
		}
	}
}

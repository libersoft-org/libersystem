// lsdev - list the system's device nodes, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - a DeviceService client - and forwards it the shell's stdout console and
// the argument string (the sub-form: "" for text or "json"). lsdev lists the device nodes
// through its grant and prints each entry (as text or JSON) to the inherited stdout, then
// exits. A standalone command, not a shell built-in: it reaches the service only through the
// one capability the permission store granted it, and renders on the same terminal as the
// shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::device;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the sub-form ("" for text, "json" for JSON).
		let args: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the one capability the manifest grants: a DeviceService client.
		let devsvc: u64 = recv_tagged(bootstrap, &mut buf, b"DEVICE").unwrap_or_else(|| exit());
		query_devices(devsvc, &args[..] == b"json");
	}
	exit();
}

// List the device nodes through the grant and print each entry, as text (the default) or as
// a JSON array, rendered on the client side - reporting a concise error if the query fails.
unsafe fn query_devices(devsvc: u64, json: bool) {
	unsafe {
		let mut client = device::Client::new(ChannelTransport { chan: devsvc });
		match client.list() {
			Some(Ok(entries)) => {
				if json {
					print(b"[");
					let mut first: bool = true;
					for e in &entries {
						if !first {
							print(b",");
						}
						first = false;
						print(e.to_json().as_bytes());
					}
					print(b"]\n");
				} else {
					for e in &entries {
						print(e.to_text().as_bytes());
						print(b"\n");
					}
				}
			}
			Some(Err(_)) => print(b"lsdev: query error\n"),
			None => print(b"lsdev: service unavailable\n"),
		}
	}
}

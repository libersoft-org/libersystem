// stop - stop a running service via the ServiceManager admin channel, run as its own
// sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - a ServiceManager admin channel (the supervisor capability) - and forwards
// it the shell's stdout console and the argument string (the name of the service to stop).
// stop drives the admin channel's bare request/reply protocol: it sends the service name and
// prints the supervisor's reply - the newline-joined teardown order on success, or a
// not-found notice - then exits. A standalone command, not a shell built-in: it reaches the
// supervisor only through the one capability the permission store granted it, and renders on
// the same terminal as the shell that launched it. The admin channel is not a serve_multi
// service; the granted client is a direct channel to the supervisor's admin handler.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the name of the service to stop.
		let name: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the one capability the manifest grants: a ServiceManager admin channel.
		let admin: u64 = recv_tagged(bootstrap, &mut buf, b"SUPERVISOR").unwrap_or_else(|| exit());
		stop_service(admin, &name[..]);
	}
	exit();
}

// Ask ServiceManager to stop a service and its dependents over the admin channel: send the
// bare service name and print the reply - the newline-joined teardown order on success, or a
// not-found notice.
unsafe fn stop_service(admin: u64, name: &[u8]) {
	unsafe {
		if name.is_empty() {
			print(b"stop: usage: stop <service>\n");
			return;
		}
		if !send_blocking(admin, name, 0) {
			print(b"stop: request failed\n");
			return;
		}
		let mut rbuf: [u8; 512] = [0u8; 512];
		match recv_blocking(admin, &mut rbuf) {
			Received::Message { len, .. } => {
				if rbuf[..len].starts_with(b"STOPPED\n") {
					print(b"stopped:\n");
					print(&rbuf[8..len]);
					print(b"\n");
				} else if len >= 8 && &rbuf[..8] == b"NOTFOUND" {
					print(b"stop: no such running service\n");
				} else {
					print(&rbuf[..len]);
					print(b"\n");
				}
			}
			Received::Closed => print(b"stop: supervisor gone\n"),
		}
	}
}

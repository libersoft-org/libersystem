// perm - show the permission audit trail, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - a PermissionManager client connected to the very manager that launched it
// (a capability the manager grants to a copy of itself) - and forwards it the shell's stdout
// console and the argument string (the sub-form: "" for text or "json"). perm reads the audit
// trail through its grant and prints each typed decision to the inherited stdout, then exits.
// A standalone command, not a shell built-in: it reaches the service only through the one
// capability the permission store granted it, and renders on the same terminal as the shell
// that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::codec::JsonMode;
use proto::system::permission;
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
		// 3. receive the one capability the manifest grants: a PermissionManager client.
		let permsvc: u64 = recv_tagged(bootstrap, &mut buf, b"PERMISSION").unwrap_or_else(|| exit());
		query_permission(permsvc, JsonMode::parse(&args));
	}
	exit();
}

// Read the audit trail through the grant and render each typed decision: as JSON (the
// generated wire form, one document per entry) or as text - one line per entry, each showing
// the component, the capability, and whether it was granted. The trail arrives as a
// stream of entries, rendered as they arrive - it never has to fit one reply.
unsafe fn query_permission(permsvc: u64, mode: Option<JsonMode>) {
	unsafe {
		let mut client = permission::Client::new(ChannelTransport { chan: permsvc });
		let consumer: u64 = match client.audit() {
			Some(c) => c,
			None => {
				print(b"perm: service unavailable\n");
				return;
			}
		};
		let mut out = String::from("[");
		let mut first: bool = true;
		loop {
			match recv_vec_blocking(consumer) {
				ReceivedVec::Message { bytes, mut handle } => {
					if let Some(e) = permission::audit_read(&bytes, &mut handle) {
						if mode.is_some() {
							if !first {
								out.push(',');
							}
							first = false;
							out.push_str(&e.to_json());
						} else {
							print(e.to_text().as_bytes());
							print(b"\n");
						}
					}
					if handle != 0 {
						close(handle);
					}
				}
				ReceivedVec::Closed => break,
			}
		}
		close(consumer);
		if let Some(mode) = mode {
			out.push(']');
			print(mode.render(out).as_bytes());
			print(b"\n");
		}
	}
}

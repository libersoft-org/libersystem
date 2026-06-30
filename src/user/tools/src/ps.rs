// ps - list the processes ProcessService has started, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - a ProcessService client - and forwards it the shell's stdout console and
// the argument string (ps takes none). ps lists the processes through its grant and prints
// each entry to the inherited stdout, then exits. A standalone command, not a shell built-in:
// it reaches the service only through the one capability the permission store granted it, and
// renders on the same terminal as the shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use proto::system::process;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. consume the argument string (ps takes none, but the run protocol always sends one).
		let _ = recv_blocking(bootstrap, &mut buf);
		// 3. receive the one capability the manifest grants: a ProcessService client.
		let procsvc: u64 = recv_tagged(bootstrap, &mut buf, b"PROCESS").unwrap_or_else(|| exit());
		query_processes(procsvc);
	}
	exit();
}

// List the processes through the grant and print each typed entry, one per line.
unsafe fn query_processes(procsvc: u64) {
	unsafe {
		let mut client = process::Client::new(ChannelTransport { chan: procsvc });
		match client.list() {
			Some(Ok(procs)) => {
				for p in &procs {
					print(p.to_text().as_bytes());
					print(b"\n");
				}
			}
			Some(Err(_)) => print(b"ps: query error\n"),
			None => print(b"ps: service unavailable\n"),
		}
	}
}

// date - print the current wall-clock instant, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - a TimeService client - and forwards it the shell's stdout console
// and an (empty) argument string. date reads the wall clock through its time grant, renders
// the instant as ISO-8601 UTC, prints it to the inherited stdout, and exits. A standalone
// command, not a shell built-in: it reaches the clock only through the one capability the
// permission store granted it (no storage, no log, no network - no ambient authority to fall
// back on), and renders on the same terminal as the shell that launched it.

#![no_std]
#![no_main]

use proto::system::Timestamp;
use rt::*;
use time_client::TimeClient;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string (date takes none, but the launch protocol sends one).
		let _ = recv_blocking(bootstrap, &mut buf);
		// 3. receive the one capability the manifest grants: a TimeService client.
		let timesvc: u64 = recv_tagged(bootstrap, &mut buf, b"TIME").unwrap_or_else(|| exit());
		date(timesvc);
	}
	exit();
}

// Read the wall clock through the time grant, render it as ISO-8601 UTC
// ("YYYY-MM-DDTHH:MM:SSZ"), and print it to stdout - reporting a concise error if the grant
// cannot be reached (no ambient fallback, the capability is the only way to the clock). The
// instant is printed as its own message, then the trailing newline, so a launcher capturing
// the first message reads exactly the rendered instant.
unsafe fn date(timesvc: u64) {
	unsafe {
		let mut client = TimeClient::new(timesvc);
		let ts: Timestamp = match client.now() {
			Some(Ok(t)) => t,
			_ => {
				print(b"date: time error\n");
				return;
			}
		};
		let mut out: [u8; 24] = [0u8; 24];
		let n: usize = ts.render(&mut out);
		print(&out[..n]);
		print(b"\n");
	}
}

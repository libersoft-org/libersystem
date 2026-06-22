// echo - a standalone foreground tool the shell spawns from the init package.
//
// This is the first program to prove the foreground program-execution primitive:
// the shell loads this ELF from the init package, spawns it as its own ring-3
// process, hands it its arguments over a bootstrap channel, and waits for it to
// finish. echo prints the arguments it was given to the console (SYS_DEBUG_WRITE
// reaches the console directly, so a program's "stdout" just works), signals
// completion to its parent, and exits. The net tools (ping / ip / nslookup, ...)
// are built on this same mechanism - separate binaries the shell execs, rather than
// commands compiled into the shell.

#![no_std]
#![no_main]

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		inherit_stdout(bootstrap);
		// The parent (the shell) hands us our arguments as one message; echo them.
		if let Received::Message { len, .. } = recv_blocking(bootstrap, &mut buf) {
			print(&buf[..len]);
			print(b"\n");
		}
		// Signal completion so the parent's foreground wait returns, then exit. (An
		// exited process is briefly a zombie whose channel has not yet closed, so the
		// parent waits on this explicit message rather than the channel closing.)
		send_blocking(bootstrap, b"done", 0);
	}
	exit();
}

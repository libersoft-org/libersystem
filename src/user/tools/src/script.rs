#![no_std]
#![no_main]

use rt::*;

// Record a pty session: given the master end of a pseudo-terminal hosting a fresh shell
// (the console opened it on the shell's behalf and handed us the master), drive the shell
// with the command line we were given, then read and print the whole session - the prompt,
// the echoed command, its output, and the goodbye - until the shell exits and the pty
// closes. This is the host side of the M35i PTY abstraction: a program hosting a terminal
// it is not the hardware console for, exactly as a future ssh would.
#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// The shell hands us our stdout (the real console), then the command line to record
		// plus the pty master channel.
		inherit_stdout(bootstrap);
		let (len, master): (usize, u64) = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } => (len, handle),
			Received::Closed => exit(),
		};
		if master == 0 {
			print(b"script: no pty\n");
		} else {
			record(master, &buf[..len]);
			close(master);
		}
		send_blocking(bootstrap, b"done", 0);
	}
	exit();
}

// Drive the pty's shell and print the captured session.
unsafe fn record(master: u64, cmd: &[u8]) {
	unsafe {
		print(b"script: recording a pty session\n");
		print(b"-------- session --------\n");
		// run the command (if any), then exit, so the session ends and the pty closes.
		if !cmd.is_empty() {
			send_blocking(master, cmd, 0);
			send_blocking(master, b"\n", 0);
		}
		send_blocking(master, b"exit\n", 0);
		// read the whole session until the shell exits and the master closes.
		let mut out: [u8; 512] = [0u8; 512];
		loop {
			match recv_blocking(master, &mut out) {
				Received::Message { len, .. } => print(&out[..len]),
				Received::Closed => break,
			}
		}
		print(b"\n-------- end --------\n");
	}
}

#![no_std]
#![no_main]

use rt::*;

// A minimal PTY slave used to exercise the pseudo-terminal path end to end. It reads a
// cooked line from its console (delivered through the line discipline, exactly as a shell
// reads keystrokes), echoes it back prefixed with "pty:", and loops until the console
// closes (the host released the pty) or an empty line (the tty's EOF) arrives. It is the
// slave side of the M35i PTY abstraction - a program reading and writing a terminal it is
// handed, with no hardware console of its own - and the deterministic slave the
// `pty_hosts_a_program` kernel test drives.
#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		let console: u64 = recv_tagged(bootstrap, &mut buf, b"CONSOLE").unwrap_or_else(|| exit());
		// Hold the control channel (winsize / signals) so its peer stays open, even though
		// this minimal slave does not drive it.
		let _control: u64 = recv_tagged(bootstrap, &mut buf, b"CONTROL").unwrap_or_else(|| exit());
		loop {
			let n: usize = match recv_blocking(console, &mut buf) {
				Received::Message { len, .. } => len,
				Received::Closed => break,
			};
			// a zero-byte read is the tty's EOF (Ctrl+D on the master): stop.
			if n == 0 {
				break;
			}
			let mut out: [u8; 260] = [0u8; 260];
			out[..4].copy_from_slice(b"pty:");
			let m: usize = n.min(out.len() - 4);
			out[4..4 + m].copy_from_slice(&buf[..m]);
			send_blocking(console, &out[..4 + m], 0);
		}
	}
	exit();
}

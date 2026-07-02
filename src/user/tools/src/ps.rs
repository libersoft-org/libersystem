// ps - list the processes ProcessService has started, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it two
// capabilities - a ResourceManager client and a ProcessService client - and forwards it
// the shell's stdout console and the argument string ("" or "-i"). Plain ps lists the
// processes through its grant and prints each entry to the inherited stdout, then exits.
// `ps -i` is the live view (M63): the shell hands it the console itself (full duplex),
// so it flips the tty raw (ESC[?9001h), enters the alternate screen, and redraws a
// process / resource snapshot in place about once a second - querying both grants each
// frame - until `q` (or Ctrl+C, caught as a pending signal) quits, when it restores the
// terminal and leaves the alternate screen. A standalone command, not a shell built-in:
// it reaches the services only through the capabilities the permission store granted it.

#![no_std]
#![no_main]

extern crate alloc;

use proto::system::{process, resources};
use rt::*;

// The refresh period of the live view, in clock ticks (~100 Hz, so ~1 s).
const REFRESH_TICKS: u64 = 100;

// Enter the live view: alternate screen, hidden cursor, raw tty, no echo.
const TTY_ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?9001h\x1b[?9002l";
// Leave the live view: cooked tty, echo, cursor, main screen - the defaults back.
const TTY_LEAVE: &[u8] = b"\x1b[?9001l\x1b[?9002h\x1b[?25h\x1b[?1049l";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us. For `ps -i` the
		//    shell hands the console itself (full duplex), so the same handle is our stdin.
		inherit_stdout(bootstrap);
		// 2. receive the argument string: "" for the plain list, "-i" for the live view.
		let interactive: bool = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => &buf[..len] == b"-i",
			Received::Closed => exit(),
		};
		// 3. receive the two capabilities the manifest grants, in vocabulary order: a
		//    ResourceManager client, then a ProcessService client.
		let ressvc: u64 = recv_tagged(bootstrap, &mut buf, b"RESOURCE").unwrap_or_else(|| exit());
		let procsvc: u64 = recv_tagged(bootstrap, &mut buf, b"PROCESS").unwrap_or_else(|| exit());
		if interactive {
			live_view(procsvc, ressvc);
		} else {
			query_processes(procsvc);
		}
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

// The live view: flip the terminal into the alternate screen and raw input, then redraw
// a fresh process / resource snapshot in place about once a second, waking early on a
// keypress - `q` quits (and Ctrl+C, which the tty delivers as a caught signal because we
// are its foreground job). On exit the terminal is restored and the shell's screen comes
// back exactly as it was (the alternate screen holds no scrollback).
unsafe fn live_view(procsvc: u64, ressvc: u64) {
	unsafe {
		let inp: u64 = stdin();
		if inp == 0 {
			// no terminal (a background / relayed launch): render one plain list instead.
			query_processes(procsvc);
			return;
		}
		// Ctrl+C must not kill us mid-alternate-screen: catch it and exit cleanly below.
		catch_interrupt();
		print(TTY_ENTER);
		loop {
			render_frame(procsvc, ressvc);
			// sleep until a key arrives or the refresh deadline passes.
			let deadline: u64 = clock() + REFRESH_TICKS;
			let ready: i64 = wait_any(&[inp], deadline);
			if interrupted() {
				break;
			}
			if ready == 0 {
				// drain the pending keystrokes; `q` (or the tty closing) quits.
				let mut quit: bool = false;
				let mut key: [u8; 8] = [0u8; 8];
				loop {
					match try_recv(inp, &mut key) {
						Polled::Message { len, .. } => {
							if key[..len].contains(&b'q') {
								quit = true;
							}
						}
						Polled::Empty => break,
						Polled::Closed => {
							quit = true;
							break;
						}
					}
				}
				if quit {
					break;
				}
			}
		}
		print(TTY_LEAVE);
	}
}

// Draw one snapshot: home the cursor, clear the (alternate) screen, and print the
// process list and the per-Domain resource budgets, each queried fresh through its
// grant - the same data the plain `ps` and `usage` commands print, redrawn in place.
unsafe fn render_frame(procsvc: u64, ressvc: u64) {
	unsafe {
		print(b"\x1b[H\x1b[2J");
		print(b"\x1b[1mps - live process / resource view\x1b[0m (refresh 1s, q quits)\n\n");
		let mut proc_client = process::Client::new(ChannelTransport { chan: procsvc });
		match proc_client.list() {
			Some(Ok(procs)) => {
				print(b"\x1b[4mprocesses\x1b[0m\n");
				for p in &procs {
					print(b"  ");
					print(p.to_text().as_bytes());
					print(b"\n");
				}
			}
			_ => print(b"processes: unavailable\n"),
		}
		print(b"\n");
		let mut res_client = resources::Client::new(ChannelTransport { chan: ressvc });
		match res_client.usage() {
			Some(Ok(budgets)) => {
				print(b"\x1b[4mdomain budgets\x1b[0m\n");
				for b in budgets.iter() {
					print(b"  ");
					print(b.to_text().as_bytes());
					print(b"\n");
				}
			}
			_ => print(b"budgets: unavailable\n"),
		}
	}
}

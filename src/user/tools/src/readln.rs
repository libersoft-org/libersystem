// readln - a standalone foreground tool that reads its standard input and echoes it.
//
// This is the first program to prove the foreground STDIN primitive: the shell hands a
// foreground child a full-duplex dup of its console (the controlling terminal), so the
// child both prints to and reads from the same VT. readln reads cooked lines from its
// stdin - edited and echoed by ConsoleService's line discipline, exactly as the shell
// reads keystrokes at its prompt - prints each one back prefixed with "in> ", and loops
// until end-of-input (Ctrl+D on an empty line, or the terminal closing). It is the
// interactive counterpart to `echo` (which only prints) and the deterministic reader the
// `interactive_tool_reads_stdin` kernel test drives.

#![no_std]
#![no_main]

extern crate alloc;

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// The line buffer matches the terminal's cooked line maximum (4 kB + newline)
	// and lives on the heap, clear of the 16 kB user stack.
	let mut buf: alloc::vec::Vec<u8> = alloc::vec![0u8; 4200];
	unsafe {
		// Adopt the console as stdout AND stdin (a foreground launch grants RECEIVE too).
		inherit_stdout(bootstrap);
		// Drain the argv/capability message so the bootstrap protocol stays in step; readln
		// takes no arguments.
		let _ = recv_blocking(bootstrap, &mut buf);
		// Read cooked input lines and echo each back until end-of-input.
		while let Some(n) = read_line(&mut buf) {
			print(b"in> ");
			// The line already carries its trailing newline from the line discipline.
			print(&buf[..n]);
		}
		// Signal completion so the parent's foreground wait returns, then exit.
		send_blocking(bootstrap, b"done", 0);
	}
	exit();
}

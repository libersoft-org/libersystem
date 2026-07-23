// dmesg - print the kernel boot log, run as its own sandboxed ELF.
//
// PermissionManager launches this program under an empty permission manifest - the
// boot log is read over its own syscall (SYS_CONSOLE_READLOG, the same one
// ConsoleService replays the boot screen from), no channel capability needed - and
// forwards it the shell's stdout console and an (empty) argument string. dmesg
// reads the log text and prints it in line-aligned chunks (the console relay caps
// a single message, so one big print would truncate; and the channel queue caps
// how many messages may sit in flight, so per-line prints of a long log would
// fill it), then exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use rt::*;

// The boot log capture buffer; the kernel copies at most this much (its own ring
// holds less - ConsoleService replays it through a 16 kB buffer of the same shape).
const LOG_CAPACITY: usize = 32 * 1024;

// One printed chunk: as many whole lines as fit, safely under the console relay's
// per-message cap.
const CHUNK: usize = 1024;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our
		//    output renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string (dmesg takes none, but the launch protocol
		//    sends one).
		let _ = recv_blocking(bootstrap, &mut buf);
		// 3. read the kernel boot log and print it, breaking at line boundaries.
		let mut log: Vec<u8> = alloc::vec![0u8; LOG_CAPACITY];
		let n: i64 = console_readlog(&mut log);
		if n <= 0 {
			print(b"dmesg: no kernel log\n");
			exit();
		}
		let text: &[u8] = &log[..n as usize];
		let mut start: usize = 0;
		while start < text.len() {
			let rest: &[u8] = &text[start..];
			// the whole tail, or the longest run of whole lines within the chunk
			// (a single overlong line goes out as-is; the relay clips, not us).
			let len: usize = if rest.len() <= CHUNK {
				rest.len()
			} else {
				match rest[..CHUNK].iter().rposition(|&b| b == b'\n') {
					Some(i) => i + 1,
					None => rest.iter().position(|&b| b == b'\n').map_or(rest.len(), |i| i + 1),
				}
			};
			print(&rest[..len]);
			start += len;
		}
		if text.last() != Some(&b'\n') {
			print(b"\n");
		}
	}
	exit();
}

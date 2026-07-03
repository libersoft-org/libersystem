// dmesg - print the kernel boot log, run as its own sandboxed ELF.
//
// PermissionManager launches this program under an empty permission manifest - the
// boot log is read over its own syscall (SYS_CONSOLE_READLOG, the same one
// ConsoleService replays the boot screen from), no channel capability needed - and
// forwards it the shell's stdout console and an (empty) argument string. dmesg
// reads the log text and prints it line by line (the console relay caps a single
// message, so one big print would truncate), then exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use rt::*;

// The boot log capture buffer; the kernel copies at most this much (its own ring
// holds less - ConsoleService replays it through a 16 kB buffer of the same shape).
const LOG_CAPACITY: usize = 32 * 1024;

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
		// 3. read the kernel boot log and print it line by line.
		let mut log: Vec<u8> = alloc::vec![0u8; LOG_CAPACITY];
		let n: i64 = console_readlog(&mut log);
		if n <= 0 {
			print(b"dmesg: no kernel log\n");
			exit();
		}
		let text: &[u8] = &log[..n as usize];
		let mut start: usize = 0;
		for (i, &b) in text.iter().enumerate() {
			if b == b'\n' {
				print(&text[start..=i]);
				start = i + 1;
			}
		}
		if start < text.len() {
			print(&text[start..]);
			print(b"\n");
		}
	}
	exit();
}

// shell - the userspace command shell (the last component up in the boot chain).
//
// ServiceManager starts this program and hands it the StorageService client
// channel. The shell first proves the service round-trip works by reading a file
// (`cat`), then reports in and becomes the system's interactive console: it
// registers a channel the kernel feeds keystrokes to (the kernel owns the serial
// UART until a virtio-console driver exists), runs a read-eval-print loop over it,
// and drives the services over IPC. This is the phase-0 kernel CLI moved into a
// userspace component.

#![no_std]
#![no_main]

use rt::*;

// the file the shell reads at startup to prove the StorageService round-trip works
const SELF_CHECK_URI: &[u8] = b"vol://system/hello.txt";

// the open reply status that means the buffer capability is present
const STATUS_OK: u32 = 0;

// maximum length of a typed command line
const LINE_MAX: usize = 128;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the StorageService client channel from ServiceManager.
	let storage: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 && &buf[..7] == b"STORAGE" => handle,
		_ => exit(),
	};

	// 2. self-check: prove the StorageService round-trip works by reading a file.
	if !unsafe { cat(storage, SELF_CHECK_URI, &mut buf) } {
		exit();
	}

	// 3. report in once the service round-trip has succeeded.
	unsafe {
		send_blocking(bootstrap, b"Shell: online", 0);
	}

	// 4. become the interactive console and run the read-eval-print loop.
	unsafe {
		repl(storage, &mut buf);
	}
	exit();
}

// Register a console channel with the kernel and run the read-eval-print loop. The
// kernel feeds keystrokes on the channel; we line-edit them (echoing input,
// handling backspace) and dispatch each completed line. Returns when the user
// types `exit`.
unsafe fn repl(storage: u64, buf: &mut [u8]) {
	unsafe {
		// The kernel sends console input on `feed`; we receive it on `input`.
		let (feed, input): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => return,
		};
		if sys_is_err(syscall(SYS_CONSOLE_ATTACH, feed, 0, 0, 0)) {
			return;
		}
		let mut line: [u8; LINE_MAX] = [0u8; LINE_MAX];
		let mut len: usize = 0;
		let mut work: [u8; 256] = [0u8; 256];
		loop {
			let n: usize = match recv_blocking(input, buf) {
				Received::Message { len, .. } => len,
				Received::Closed => return,
			};
			for i in 0..n {
				match buf[i] {
					b'\n' | b'\r' => {
						print(b"\n");
						if dispatch(&line[..len], storage, &mut work) {
							return;
						}
						len = 0;
						print(b"> ");
					}
					0x08 | 0x7f => {
						if len > 0 {
							len -= 1;
							// erase the character on the terminal: back up, overwrite, back up
							print(b"\x08 \x08");
						}
					}
					byte @ 0x20..=0x7e => {
						if len < LINE_MAX {
							line[len] = byte;
							len += 1;
							print(&[byte]);
						}
					}
					_ => {}
				}
			}
		}
	}
}

// Dispatch one command line. Returns true if the shell should exit.
unsafe fn dispatch(line: &[u8], storage: u64, work: &mut [u8]) -> bool {
	unsafe {
		let line = trim(line);
		if line.is_empty() {
			return false;
		}
		if line == b"exit" || line == b"quit" {
			print(b"shell: exiting\n");
			return true;
		}
		if line == b"help" {
			print(b"commands:\n");
			print(b"  help             show this help\n");
			print(b"  echo <text>      print text\n");
			print(b"  cat <vol://...>  read a file via StorageService\n");
			print(b"  exit             stop the shell and halt\n");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"echo ") {
			print(rest);
			print(b"\n");
			return false;
		}
		if let Some(rest) = line.strip_prefix(b"cat ") {
			let uri = trim(rest);
			if !cat(storage, uri, work) {
				print(b"cat: could not read ");
				print(uri);
				print(b"\n");
			}
			return false;
		}
		print(b"unknown command: ");
		print(line);
		print(b" (try 'help')\n");
		false
	}
}

// Trim leading and trailing ASCII spaces from a byte slice.
fn trim(mut s: &[u8]) -> &[u8] {
	while let [b' ', rest @ ..] = s {
		s = rest;
	}
	while let [rest @ .., b' '] = s {
		s = rest;
	}
	s
}

// Open `uri` through the StorageService channel `storage`, map the returned shared
// buffer, print its bytes to the console, and release it. Returns true on success.
unsafe fn cat(storage: u64, uri: &[u8], buf: &mut [u8]) -> bool {
	unsafe {
		// open request: [rights u32 LE][vol:// URI]. Ask for a read-only view.
		let want_rights: u32 = RIGHT_READ | RIGHT_MAP;
		let mut request: [u8; 64] = [0u8; 64];
		if 4 + uri.len() > request.len() {
			return false;
		}
		request[0..4].copy_from_slice(&want_rights.to_le_bytes());
		request[4..4 + uri.len()].copy_from_slice(uri);
		if !send_blocking(storage, &request[..4 + uri.len()], 0) {
			return false;
		}
		// reply: [status u32 LE][size u64 LE] + shared-buffer capability.
		let (status, size, buffer): (u32, usize, u64) = match recv_blocking(storage, buf) {
			Received::Message { len, handle } if len >= 12 => {
				let status: u32 = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
				let size: u64 = u64::from_le_bytes([buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10], buf[11]]);
				(status, size as usize, handle)
			}
			_ => return false,
		};
		if status != STATUS_OK || buffer == 0 || size == 0 {
			return false;
		}
		// map the shared buffer, print the file, then release it.
		let mapped: u64 = syscall(SYS_MEMORY_MAP, buffer, 0, 0, 0);
		if sys_is_err(mapped) {
			return false;
		}
		let contents: &[u8] = core::slice::from_raw_parts(mapped as *const u8, size);
		print(contents);
		if contents.last() != Some(&b'\n') {
			print(b"\n");
		}
		syscall(SYS_MEMORY_UNMAP, buffer, 0, 0, 0);
		syscall(SYS_HANDLE_CLOSE, buffer, 0, 0, 0);
		true
	}
}

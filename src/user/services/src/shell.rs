// shell - the userspace command shell (the last component up in the boot chain).
//
// ServiceManager starts this program from the init package and hands it a bootstrap
// channel, then over it the StorageService client channel. For this step the shell
// demonstrates that a userspace component drives the system services over IPC: it
// reads a file with `cat`, which round-trips through StorageService (open request
// -> shared-buffer capability -> mapped read), prints it to the console, and
// reports in. A later step grows it into an interactive REPL that reads commands
// from the console.

#![no_std]
#![no_main]

use rt::*;

// the file the shell reads to prove the service round-trip works
const CAT_URI: &[u8] = b"vol://system/hello.txt";

// the open reply status that means the buffer capability is present
const STATUS_OK: u32 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// 1. receive the StorageService client channel from ServiceManager.
	let storage: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 && &buf[..7] == b"STORAGE" => handle,
		_ => exit(),
	};

	// 2. talk to a service over IPC: cat a file through StorageService and print it.
	if unsafe { cat(storage, CAT_URI, &mut buf) } {
		// 3. report in once the service round-trip has succeeded.
		unsafe {
			send_blocking(bootstrap, b"Shell: online", 0);
		}
	}
	exit();
}

// Open `uri` through the StorageService channel `storage`, map the returned shared
// buffer, print its bytes to the console, and unmap it. Returns true on success.
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
		print(b"shell: cat ");
		print(uri);
		print(b" -> ");
		print(contents);
		print(b"\n");
		syscall(SYS_MEMORY_UNMAP, buffer, 0, 0, 0);
		syscall(SYS_HANDLE_CLOSE, buffer, 0, 0, 0);
		true
	}
}

// storage_client - a userspace demo client for the StorageManager.
//
// The kernel hands this program a bootstrap channel and sends "CONNECT" with a
// capability to the manager's service channel. The client opens a known vol://
// path, receives a shared-buffer capability to the file's bytes, maps it, and
// sends the contents back to the kernel over its bootstrap channel - proving an
// end-to-end zero-copy read brokered entirely by capabilities.

#![no_std]
#![no_main]

mod runtime;
use runtime::*;

// the file this client opens through the StorageManager
const TARGET_URI: &[u8] = b"vol://system/hello.txt";

#[no_mangle]
pub extern "C" fn __storage_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	// 1. connect: receive the manager's service channel.
	let service: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 && &buf[..7] == b"CONNECT" => handle,
		_ => exit(),
	};
	// 2. open: [rights u32 LE][vol:// URI]. Ask for read + map (a read-only view).
	let want_rights: u32 = RIGHT_READ | RIGHT_MAP;
	let mut request: [u8; 64] = [0u8; 64];
	request[0..4].copy_from_slice(&want_rights.to_le_bytes());
	request[4..4 + TARGET_URI.len()].copy_from_slice(TARGET_URI);
	let request_len: usize = 4 + TARGET_URI.len();
	if !unsafe { send_blocking(service, &request[..request_len], 0) } {
		exit();
	}
	// 3. reply: [status u32 LE][size u64 LE] + shared-buffer capability.
	let (status, size, buffer): (u32, usize, u64) = match unsafe { recv_blocking(service, &mut buf) } {
		Received::Message { len, handle } if len >= 12 => {
			let status: u32 = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
			let size: u64 = u64::from_le_bytes([buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10], buf[11]]);
			(status, size as usize, handle)
		}
		_ => exit(),
	};
	if status != STATUS_OK || buffer == 0 {
		exit();
	}
	// 4. map the shared buffer and report its contents back to the kernel.
	let mapped: u64 = unsafe { syscall(SYS_MEMORY_MAP, buffer, 0, 0, 0) };
	if sys_is_err(mapped) {
		exit();
	}
	let contents: &[u8] = unsafe { core::slice::from_raw_parts(mapped as *const u8, size) };
	unsafe {
		send_blocking(bootstrap, contents, 0);
		syscall(SYS_MEMORY_UNMAP, buffer, 0, 0, 0);
	}
	exit();
}

// the open reply status that means the buffer capability is present
const STATUS_OK: u32 = 0;

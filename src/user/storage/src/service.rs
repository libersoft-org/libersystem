// StorageService - a userspace service that resolves vol:// paths on a ramdisk.
//
// The kernel loads this program from the init package into a ring-3 process and
// hands it a bootstrap channel. Over that channel the kernel sends, in order:
//   1. "RAMDISK" + the volume length, with a MemoryObject capability backing the
//      ramdisk (a PKGARCH1 archive of the volume's files);
//   2. "SERVE", with a channel capability on which clients send open requests.
// The service maps the ramdisk, then serves open requests until the client side
// closes. Each request is [rights u32][vol:// URI]; the reply is [status u32]
// [size u64], carrying a MemoryObject capability to a freshly filled buffer of
// the file's bytes on success (status 0). The file content crosses as a shared
// buffer handle, never copied through the channel - a zero-copy read.

#![no_std]
#![no_main]

use rt::*;

// the single volume this service serves; the URI's volume component must match
const VOLUME_NAME: &[u8] = b"system";

// open reply status codes
const STATUS_OK: u32 = 0;
const STATUS_NOT_FOUND: u32 = 1;
const STATUS_DENIED: u32 = 2;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	// 1. ramdisk: map the volume archive and remember its extent.
	let (volume_base, volume_len): (u64, usize) = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 + 8 && &buf[..7] == b"RAMDISK" => {
			let length: usize = u64::from_le_bytes([buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14]]) as usize;
			let base: u64 = unsafe { syscall(SYS_MEMORY_MAP, handle, 0, 0, 0) };
			if sys_is_err(base) {
				exit();
			}
			(base, length)
		}
		_ => exit(),
	};
	// 2. service endpoint: clients reach the service here.
	let service: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"SERVE" => handle,
		_ => exit(),
	};
	// 3. report in over the bootstrap channel (the supervisor that started us is
	//    listening there), then serve until the client side closes.
	unsafe {
		send_blocking(bootstrap, b"StorageService: online", 0);
	}
	loop {
		match unsafe { recv_blocking(service, &mut buf) } {
			// An empty message is an explicit quit sentinel: a client that cannot
			// close its endpoint to signal end-of-stream (e.g. the kernel keeping the
			// peer to read the reply) sends a zero-length message to end the session.
			Received::Message { len, .. } if len == 0 => break,
			Received::Message { len, .. } => unsafe { serve_open(service, volume_base, volume_len, &buf[..len]) },
			Received::Closed => break,
		}
	}
	exit();
}

// Resolve one open request and answer on the service channel.
unsafe fn serve_open(service: u64, volume_base: u64, volume_len: usize, request: &[u8]) {
	unsafe {
		// request: [rights u32 LE][vol:// URI]
		if request.len() < 4 {
			reply(service, STATUS_DENIED, 0, 0);
			return;
		}
		let want_rights: u32 = u32::from_le_bytes([request[0], request[1], request[2], request[3]]);
		let uri: &[u8] = &request[4..];
		let target: VolumePath = match VolumePath::parse(uri) {
			Some(t) => t,
			None => {
				reply(service, STATUS_NOT_FOUND, 0, 0);
				return;
			}
		};
		if target.volume != VOLUME_NAME {
			reply(service, STATUS_NOT_FOUND, 0, 0);
			return;
		}
		let archive: &[u8] = core::slice::from_raw_parts(volume_base as *const u8, volume_len);
		let file: &[u8] = match Package::parse(archive).and_then(|p| p.lookup(target.path)) {
			Some(f) => f,
			None => {
				reply(service, STATUS_NOT_FOUND, 0, 0);
				return;
			}
		};
		// the ramdisk is read-only: grant at most read+map, deny anything more.
		let allowed: u32 = RIGHT_READ | RIGHT_MAP;
		if want_rights & !allowed != 0 {
			reply(service, STATUS_DENIED, 0, 0);
			return;
		}
		// fill a fresh shared buffer with the file's bytes, then hand it over.
		let buffer: u64 = syscall(SYS_MEMORY_OBJECT_CREATE, file.len() as u64, 0, 0, 0);
		if sys_is_err(buffer) {
			reply(service, STATUS_DENIED, 0, 0);
			return;
		}
		let mapped: u64 = syscall(SYS_MEMORY_MAP, buffer, 0, 0, 0);
		if sys_is_err(mapped) {
			syscall(SYS_HANDLE_CLOSE, buffer, 0, 0, 0);
			reply(service, STATUS_DENIED, 0, 0);
			return;
		}
		core::ptr::copy_nonoverlapping(file.as_ptr(), mapped as *mut u8, file.len());
		syscall(SYS_MEMORY_UNMAP, buffer, 0, 0, 0);
		// attenuate to exactly what was asked for, plus the transfer right needed to
		// hand the capability across, then transfer that weaker handle.
		let granted: u64 = syscall(SYS_HANDLE_DUPLICATE, buffer, (want_rights | RIGHT_TRANSFER) as u64, 0, 0);
		syscall(SYS_HANDLE_CLOSE, buffer, 0, 0, 0);
		if sys_is_err(granted) {
			reply(service, STATUS_DENIED, 0, 0);
			return;
		}
		reply(service, STATUS_OK, file.len() as u64, granted);
	}
}

// Send an open reply: [status u32 LE][size u64 LE], carrying the handle `xfer`
// (0 = none). On success the handle is the shared buffer of the file's bytes.
unsafe fn reply(service: u64, status: u32, size: u64, xfer: u64) {
	unsafe {
		let mut out: [u8; 12] = [0u8; 12];
		out[0..4].copy_from_slice(&status.to_le_bytes());
		out[4..12].copy_from_slice(&size.to_le_bytes());
		send_blocking(service, &out, xfer);
	}
}

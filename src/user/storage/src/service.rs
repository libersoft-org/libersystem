// StorageService - a userspace service that resolves vol:// paths on a volume.
//
// The kernel loads this program from the init package into a ring-3 process and
// hands it a bootstrap channel. Over that channel it receives, in order:
//   1. the volume backing, one of:
//        "RAMDISK" + length, with a MemoryObject capability holding the volume's
//          PKGARCH1 archive (the kernel's direct-client test path); or
//        "BLOCK", with a channel capability to the virtio-blk driver's block-read
//          service, from which the archive is read off the disk (the boot path);
//   2. "SERVE", with a channel capability on which clients send open requests.
// Either way the volume's PKGARCH1 archive ends up mapped in this process; the
// service then serves open requests until the client side closes. Each request is
// [rights u32][vol:// URI]; the reply is [status u32][size u64], carrying a
// MemoryObject capability to a freshly filled buffer of the file's bytes on success
// (status 0). The file content crosses as a shared buffer handle, never copied
// through the channel - a zero-copy read.

#![no_std]
#![no_main]

use rt::*;

// the single volume this service serves; the URI's volume component must match
const VOLUME_NAME: &[u8] = b"system";

// open reply status codes
const STATUS_OK: u32 = 0;
const STATUS_NOT_FOUND: u32 = 1;
const STATUS_DENIED: u32 = 2;

// block-read protocol with driver.virtio-blk: request [lba u64][count u32], reply
// [status u32] carrying a MemoryObject of count*512 bytes. A single request reads at
// most one DMA page (8 sectors).
const SECTOR_SIZE: usize = 512;
const MAX_SECTORS_PER_READ: usize = 8;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	// 1. volume backing: either the legacy ramdisk MemoryObject (kernel test) or a
	//    block service channel to the virtio-blk disk (real boot). Both resolve to
	//    the volume's PKGARCH1 archive mapped at (volume_base, volume_len).
	let (volume_base, volume_len): (u64, usize) = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 + 8 && &buf[..7] == b"RAMDISK" => {
			let length: usize = u64::from_le_bytes([buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14]]) as usize;
			let base: u64 = unsafe { syscall(SYS_MEMORY_MAP, handle, 0, 0, 0) };
			if sys_is_err(base) {
				exit();
			}
			(base, length)
		}
		Received::Message { len, handle } if handle != 0 && len >= 5 && &buf[..5] == b"BLOCK" => match unsafe { load_archive_from_disk(handle) } {
			Some(extent) => extent,
			None => exit(),
		},
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

// Read the volume's PKGARCH1 archive off the virtio-blk disk over the block-read
// service channel `block_client`, into a freshly created+mapped MemoryObject.
// Returns the archive's (base, len) on success. The archive stays mapped for the
// serve loop; `block_client` is closed when done (the driver then shuts down).
// Phase 1 assumes the archive header + entry table fit the first sector.
unsafe fn load_archive_from_disk(block_client: u64) -> Option<(u64, usize)> {
	unsafe {
		// read sector 0 to find the archive's magic and total size.
		let mut head: [u8; SECTOR_SIZE] = [0u8; SECTOR_SIZE];
		if !block_read(block_client, 0, 1, head.as_mut_ptr()) {
			return None;
		}
		if &head[..PKG_MAGIC.len()] != PKG_MAGIC {
			return None;
		}
		let count: usize = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
		let table_end: usize = PKG_HEADER_LEN + PKG_ENTRY_LEN * count;
		if table_end > SECTOR_SIZE {
			return None; // phase 1: the header + entry table fit one sector
		}
		// the archive's total size is the end of its last blob.
		let mut total: usize = table_end;
		let mut i: usize = 0;
		while i < count {
			let e: usize = PKG_HEADER_LEN + PKG_ENTRY_LEN * i;
			let off: usize = u32::from_le_bytes([head[e + PKG_NAME_LEN], head[e + PKG_NAME_LEN + 1], head[e + PKG_NAME_LEN + 2], head[e + PKG_NAME_LEN + 3]]) as usize;
			let size: usize = u32::from_le_bytes([head[e + PKG_NAME_LEN + 4], head[e + PKG_NAME_LEN + 5], head[e + PKG_NAME_LEN + 6], head[e + PKG_NAME_LEN + 7]]) as usize;
			if off + size > total {
				total = off + size;
			}
			i += 1;
		}
		// allocate the archive buffer and fill it from the disk in page-sized chunks.
		let obj: u64 = syscall(SYS_MEMORY_OBJECT_CREATE, total as u64, 0, 0, 0);
		if sys_is_err(obj) {
			return None;
		}
		let base: u64 = syscall(SYS_MEMORY_MAP, obj, 0, 0, 0);
		if sys_is_err(base) {
			syscall(SYS_HANDLE_CLOSE, obj, 0, 0, 0);
			return None;
		}
		let mut filled: usize = 0;
		while filled < total {
			let lba: u64 = (filled / SECTOR_SIZE) as u64;
			let remaining: usize = total - filled;
			let sectors: usize = ((remaining + SECTOR_SIZE - 1) / SECTOR_SIZE).min(MAX_SECTORS_PER_READ);
			if !block_read(block_client, lba, sectors as u32, (base as *mut u8).add(filled)) {
				syscall(SYS_MEMORY_UNMAP, obj, 0, 0, 0);
				syscall(SYS_HANDLE_CLOSE, obj, 0, 0, 0);
				return None;
			}
			filled += sectors * SECTOR_SIZE;
		}
		// the disk is no longer needed; closing it lets the driver shut down.
		syscall(SYS_HANDLE_CLOSE, block_client, 0, 0, 0);
		Some((base, total))
	}
}

// Send one block-read request [lba u64][count u32] to the driver and copy the
// returned sectors into `dst`. The reply is [status u32] carrying, on success, a
// MemoryObject of count*512 bytes which we map, copy out, and release. Returns true
// on success. `dst` must have room for count*512 bytes.
unsafe fn block_read(block_client: u64, lba: u64, count: u32, dst: *mut u8) -> bool {
	unsafe {
		let mut req: [u8; 12] = [0u8; 12];
		req[..8].copy_from_slice(&lba.to_le_bytes());
		req[8..12].copy_from_slice(&count.to_le_bytes());
		if !send_blocking(block_client, &req, 0) {
			return false;
		}
		let mut rep: [u8; 16] = [0u8; 16];
		let (status, handle): (u32, u64) = match recv_blocking(block_client, &mut rep) {
			Received::Message { len, handle } if len >= 4 => (u32::from_le_bytes([rep[0], rep[1], rep[2], rep[3]]), handle),
			_ => return false,
		};
		if status != 0 || handle == 0 {
			if handle != 0 {
				syscall(SYS_HANDLE_CLOSE, handle, 0, 0, 0);
			}
			return false;
		}
		let src: u64 = syscall(SYS_MEMORY_MAP, handle, 0, 0, 0);
		if sys_is_err(src) {
			syscall(SYS_HANDLE_CLOSE, handle, 0, 0, 0);
			return false;
		}
		core::ptr::copy_nonoverlapping(src as *const u8, dst, count as usize * SECTOR_SIZE);
		syscall(SYS_MEMORY_UNMAP, handle, 0, 0, 0);
		syscall(SYS_HANDLE_CLOSE, handle, 0, 0, 0);
		true
	}
}

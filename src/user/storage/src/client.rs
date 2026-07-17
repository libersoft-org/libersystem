// storage_client - a userspace demo client for the StorageService.
//
// The kernel hands this program a bootstrap channel and sends "CONNECT" with a
// capability to the manager's service channel. The client opens a known vol://
// path through the generated Storage.Volume client, receives a shared-buffer
// capability to the file's bytes, maps it, and sends the contents back to the
// kernel over its bootstrap channel - proving an end-to-end zero-copy read brokered
// entirely by capabilities, over the typed Storage.Volume API.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::{OpenOpts, volume};
use rt::*;

// the file this client opens through the StorageService
const TARGET_URI: &[u8] = b"vol://system/hello.txt";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	// 1. connect: receive the manager's service channel.
	let service: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"CONNECT") }.unwrap_or_else(|| exit());
	// 2. open the target file over the generated volume client (read-only view).
	let mut client = volume::Client::new(ChannelTransport { chan: service });
	let opts: OpenOpts = OpenOpts { path: alloc::string::String::from_utf8_lossy(TARGET_URI).into_owned(), write: false, create: false };
	let result = match client.open(&opts) {
		Some(Ok(r)) => r,
		_ => exit(),
	};
	if result.file == 0 {
		exit();
	}
	// 3. map the returned shared buffer and report its contents back to the kernel.
	let mapped: u64 = match unsafe { map_object(result.file) } {
		Some(base) => base,
		None => exit(),
	};
	let contents: &[u8] = unsafe { core::slice::from_raw_parts(mapped as *const u8, result.size as usize) };
	unsafe {
		send_blocking(bootstrap, contents, 0);
		unmap_object(result.file);
	}
	exit();
}

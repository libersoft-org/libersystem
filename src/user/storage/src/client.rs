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

use proto::codec::Transport;
use proto::system::{OpenOpts, volume};
use rt::*;

// the file this client opens through the StorageService
const TARGET_URI: &[u8] = b"vol://system/hello.txt";

// A proto Transport over an rt channel: send the request, then block for the reply.
// The generated volume client calls through this to reach StorageService.
struct ChannelTransport {
	chan: u64,
}

impl Transport for ChannelTransport {
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(alloc::vec::Vec<u8>, u64)> {
		unsafe {
			if !send_blocking(self.chan, request, request_handle) {
				return None;
			}
			let mut reply: [u8; 256] = [0u8; 256];
			match recv_blocking(self.chan, &mut reply) {
				Received::Message { len, handle } => Some((reply[..len].to_vec(), handle)),
				Received::Closed => None,
			}
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	// 1. connect: receive the manager's service channel.
	let service: u64 = match unsafe { recv_blocking(bootstrap, &mut buf) } {
		Received::Message { len, handle } if handle != 0 && len >= 7 && &buf[..7] == b"CONNECT" => handle,
		_ => exit(),
	};
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
	let mapped: u64 = unsafe { syscall(SYS_MEMORY_MAP, result.file, 0, 0, 0) };
	if sys_is_err(mapped) {
		exit();
	}
	let contents: &[u8] = unsafe { core::slice::from_raw_parts(mapped as *const u8, result.size as usize) };
	unsafe {
		send_blocking(bootstrap, contents, 0);
		syscall(SYS_MEMORY_UNMAP, result.file, 0, 0, 0);
	}
	exit();
}

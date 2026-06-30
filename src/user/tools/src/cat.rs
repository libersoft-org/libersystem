// cat - print a file's contents, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - a StorageService (volume) client - and forwards it the shell's
// stdout console, the argument string (the file path, relative or absolute), and the
// inherited working directory. cat resolves the path against that cwd, opens the file
// through its storage grant, maps it, prints it to the inherited stdout, and exits. A
// standalone command, not a shell built-in: it reaches the filesystem only through the one
// capability the permission store granted it, and renders on the same terminal as the shell
// that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::path;
use proto::system::{volume, OpenOpts};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the file path (relative to cwd or an absolute URI).
		let arg: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the one capability the manifest grants: a StorageService client.
		let storage: u64 = recv_tagged(bootstrap, &mut buf, b"STORAGE").unwrap_or_else(|| exit());
		// 4. receive the inherited working directory (the last bootstrap message), and resolve
		//    the path argument against it so a relative path reaches the same file the shell would.
		let cwd: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
		let uri: String = match path::resolve(core::str::from_utf8(&cwd).unwrap_or(""), &arg) {
			Some(u) => u,
			None => {
				print(b"cat: invalid path\n");
				exit();
			}
		};
		cat(storage, uri.as_bytes());
	}
	exit();
}

// Open the file through the storage grant, map its shared buffer, print it to stdout, then
// release it - reporting a concise error if it cannot be read.
unsafe fn cat(storage: u64, uri: &[u8]) {
	unsafe {
		let opts: OpenOpts = OpenOpts { path: String::from_utf8_lossy(uri).into_owned(), write: false, create: false };
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let result = match client.open(&opts) {
			Some(Ok(r)) => r,
			_ => {
				print(b"cat: ");
				print(uri);
				print(b": cannot open\n");
				return;
			}
		};
		if result.file == 0 || result.size == 0 {
			return;
		}
		let mapped: u64 = match map_object(result.file) {
			Some(base) => base,
			None => return,
		};
		let contents: &[u8] = core::slice::from_raw_parts(mapped as *const u8, result.size as usize);
		print(contents);
		if contents.last() != Some(&b'\n') {
			print(b"\n");
		}
		unmap_object(result.file);
		close(result.file);
	}
}

// ls - list a directory's entries, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - a StorageService (volume) client - and forwards it the shell's
// stdout console, the argument string (the directory path, relative or absolute), and the
// inherited working directory. ls resolves the path against that cwd, lists the directory
// through its storage grant and prints each entry (a trailing '/' for subdirectories, a byte
// size for files) to the inherited stdout, then exits. A standalone command, not a shell
// built-in: it reaches the filesystem only through the one capability the permission store
// granted it, and renders on the same terminal as the shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::path;
use proto::system::{volume, FileKind};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the directory path (relative to cwd or an absolute URI).
		let arg: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the one capability the manifest grants: a StorageService client.
		let storage: u64 = recv_tagged(bootstrap, &mut buf, b"STORAGE").unwrap_or_else(|| exit());
		// 4. receive the inherited working directory (the last bootstrap message), and resolve
		//    the path argument against it so a relative path reaches the same directory the shell would.
		let cwd: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
		let uri: String = match path::resolve(core::str::from_utf8(&cwd).unwrap_or(""), &arg) {
			Some(u) => u,
			None => {
				print(b"ls: invalid path\n");
				exit();
			}
		};
		ls(storage, uri.as_bytes());
	}
	exit();
}

// List the directory through the storage grant and print its entries to stdout - reporting a
// concise error if it cannot be listed.
unsafe fn ls(storage: u64, uri: &[u8]) {
	unsafe {
		let path: &str = match core::str::from_utf8(uri) {
			Ok(s) => s,
			Err(_) => {
				print(b"ls: invalid path\n");
				return;
			}
		};
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let files = match client.list(path) {
			Some(Ok(f)) => f,
			_ => {
				print(b"ls: StorageService unavailable\n");
				return;
			}
		};
		print(uri);
		print(b" (");
		print_usize(files.len());
		print(b" entries):\n");
		for f in &files {
			print(b"  ");
			print(f.name.as_bytes());
			match f.kind {
				FileKind::Dir => print(b"/\n"),
				FileKind::File => {
					print(b" ");
					print_usize(f.size as usize);
					print(b" bytes\n");
				}
			}
		}
	}
}

// Print a usize as decimal digits to stdout.
unsafe fn print_usize(mut n: usize) {
	unsafe {
		if n == 0 {
			print(b"0");
			return;
		}
		let mut buf: [u8; 20] = [0u8; 20];
		let mut i: usize = 20;
		while n > 0 {
			i -= 1;
			buf[i] = b'0' + (n % 10) as u8;
			n /= 10;
		}
		print(&buf[i..]);
	}
}

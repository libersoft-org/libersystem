// mkdir - create a directory, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - a StorageService (volume) client - and forwards it the shell's
// stdout console and the argument string (the directory URI to create). mkdir creates the
// directory (and any missing parents) through its storage grant, prints a one-line
// confirmation to the inherited stdout, and exits. A standalone command, not a shell
// built-in: it reaches the filesystem only through the one capability the permission store
// granted it, and renders on the same terminal as the shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::volume;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the directory URI to create.
		let uri: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the one capability the manifest grants: a StorageService client.
		let storage: u64 = recv_tagged(bootstrap, &mut buf, b"STORAGE").unwrap_or_else(|| exit());
		mkdir(storage, &uri);
	}
	exit();
}

// Create the directory through the storage grant, making any missing parents (mkdir -p), and
// print a one-line confirmation - reporting a concise error if it cannot be created.
unsafe fn mkdir(storage: u64, uri: &[u8]) {
	unsafe {
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.mkdir(&path) {
			Some(Ok(())) => {
				print(b"created ");
				print(uri);
				print(b"\n");
			}
			_ => {
				print(b"mkdir: could not create ");
				print(uri);
				print(b"\n");
			}
		}
	}
}

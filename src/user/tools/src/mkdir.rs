// mkdir - create a directory, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - a StorageService (volume) client - and forwards it the shell's
// stdout console, the argument string (the directory path, relative or absolute), and the
// inherited working directory. mkdir resolves the path against that cwd, creates the
// directory (and any missing parents) through its storage grant, prints a one-line
// confirmation to the inherited stdout, and exits. A standalone command, not a shell
// built-in: it reaches the filesystem only through the one capability the permission store
// granted it, and renders on the same terminal as the shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::path;
use proto::system::volume;
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
		// 3. receive the four volume clients the `volumes` capability bundles (SYSTEM / MEDIA /
		//    ISO / UDF, in grant order); a volume whose disk is absent arrives as 0.
		let system: u64 = recv_tagged(bootstrap, &mut buf, b"SYSTEM").unwrap_or(0);
		let media: u64 = recv_tagged(bootstrap, &mut buf, b"MEDIA").unwrap_or(0);
		let iso: u64 = recv_tagged(bootstrap, &mut buf, b"ISO").unwrap_or(0);
		let udf: u64 = recv_tagged(bootstrap, &mut buf, b"UDF").unwrap_or(0);
		// 4. receive the inherited working directory (the last bootstrap message), and resolve
		//    the path argument against it so a relative path reaches the same place the shell would.
		let cwd: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
		let cwd_str: &str = core::str::from_utf8(&cwd).unwrap_or("");
		let uri: String = match path::resolve(cwd_str, &arg) {
			Some(u) => u,
			None => {
				print(b"mkdir: invalid path\n");
				exit();
			}
		};
		// route the path to the client for the volume it names.
		let storage: u64 = path::volume_client(cwd_str, &arg, system, media, iso, udf);
		mkdir(storage, uri.as_bytes());
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

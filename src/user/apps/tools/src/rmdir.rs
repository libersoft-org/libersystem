// rmdir - remove an empty directory, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - a StorageService (volume) client - and forwards it the shell's
// stdout console, the argument string (the directory path, relative or absolute), and the
// inherited working directory. rmdir resolves the path against that cwd, removes the empty
// directory through its storage grant, prints a one-line confirmation to the inherited
// stdout, and exits. A standalone command, not a shell built-in: it reaches the filesystem
// only through the one capability the permission store granted it, and renders on the same
// terminal as the shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::LaunchContext;
use rt::*;
use storage_proto::path;
use volume_client::VolumeClient;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the directory path (relative to cwd or an absolute URI).
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let arg: Vec<u8> = context.arguments.clone().into_bytes();
		// 3. receive the four volume clients the `volumes` capability bundles (SYSTEM / MEDIA /
		//    ISO / UDF, in grant order); a volume whose disk is absent arrives as 0.
		// Taken BY NAME out of the bundle, which ends at READY. The volumes this tool has no use
		// for are simply not taken, and the set closes them when it drops - where before they had
		// to be drained by hand, because a message left on the channel was read as the NEXT thing
		// this tool expected, and the thing after the bundle is the working directory.
		let mut volumes: CapSet = recv_caps(bootstrap);
		let system: u64 = volumes.take(CAP_SYSTEM);
		let media: u64 = volumes.take(CAP_MEDIA);
		let iso: u64 = volumes.take(CAP_ISO);
		let udf: u64 = volumes.take(CAP_UDF);
		let usb: u64 = volumes.take(CAP_USB);
		// 4. receive the inherited working directory (the last bootstrap message), and resolve
		//    the path argument against it so a relative path reaches the same directory the shell would.
		let cwd: Vec<u8> = context.cwd.clone().into_bytes();
		let cwd_str: &str = core::str::from_utf8(&cwd).unwrap_or("");
		let uri: String = match path::resolve(cwd_str, &arg) {
			Some(u) => u,
			None => {
				eprint(b"rmdir: invalid path\n");
				exit();
			}
		};
		// route the path to the client for the volume it names.
		let storage: u64 = path::volume_client(cwd_str, &arg, system, media, iso, udf, usb, path::NOT_GRANTED, path::NOT_GRANTED);
		rmdir(storage, uri.as_bytes());
	}
	exit();
}

// Remove the empty directory through the storage grant and print a one-line confirmation -
// reporting a concise error if it cannot be removed.
unsafe fn rmdir(storage: u64, uri: &[u8]) {
	unsafe {
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = VolumeClient::new(storage);
		match client.rmdir(&path) {
			Some(Ok(())) => {
				print(b"removed ");
				print(uri);
				print(b"\n");
			}
			_ => {
				eprint(b"rmdir: could not remove ");
				eprint(uri);
				eprint(b"\n");
			}
		}
	}
}

// cat - print a file's contents, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it the
// `volumes` capability - the four volume StorageService clients (system / media / iso /
// udf) - and forwards it the shell's stdout console, the argument string (the file path,
// relative or absolute), and the inherited working directory. cat resolves the path against
// that cwd, routes it to the volume it names, opens the file through that grant, maps it,
// prints it to the inherited stdout, and exits. A standalone command, not a shell built-in:
// it reaches the filesystem only through the capability the permission store granted it, and
// renders on the same terminal as the shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use proto::system::{LaunchContext, OpenOpts};
use rt::*;
use storage_proto::path;
use volume_client::VolumeClient;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the launch context: the argument string (the file path, relative to the
		//    working directory or an absolute URI) and that working directory, in one versioned
		//    record. They used to be two bare messages with the capability grants between them,
		//    and the cwd had to be LAST, because a bare message arriving before the grants is
		//    consumed by the tagged receive that reads them.
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
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
		// 4. resolve the path argument against the inherited working directory, so a relative
		//    path reaches the same file the shell would.
		let arg: &[u8] = context.arguments.as_bytes();
		let cwd_str: &str = &context.cwd;
		let uri: String = match path::resolve(cwd_str, arg) {
			Some(u) => u,
			None => {
				eprint(b"cat: invalid path\n");
				exit();
			}
		};
		// route the path to the client for the volume it names.
		let storage: u64 = path::volume_client(cwd_str, arg, system, media, iso, udf, usb);
		cat(storage, uri.as_bytes());
	}
	exit();
}

// Open the file through the storage grant, map its shared buffer, print it to stdout, then
// release it - reporting a concise error if it cannot be read.
unsafe fn cat(storage: u64, uri: &[u8]) {
	unsafe {
		let opts: OpenOpts = OpenOpts { path: String::from_utf8_lossy(uri).into_owned(), write: false, create: false };
		let mut client = VolumeClient::new(storage);
		let result = match client.open(&opts) {
			Some(Ok(r)) => r,
			_ => {
				eprint(b"cat: ");
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

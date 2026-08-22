// write - create or overwrite a file, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - a StorageService (volume) client - and forwards it the shell's
// stdout console, the argument string ("<path> <text>"), and the inherited working
// directory. write splits the path from the text, resolves the path against that cwd,
// stages the text in a shared buffer, writes it through its storage grant, prints a
// one-line confirmation to the inherited stdout, and exits. A standalone command, not a
// shell built-in: it reaches the filesystem only through the one capability the permission
// store granted it, and renders on the same terminal as the shell that launched it.

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
		// 2. receive the argument string - "<path> <text>".
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let args: Vec<u8> = context.arguments.clone().into_bytes();
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
		// 4. receive the inherited working directory (the last bootstrap message), used to
		//    resolve a relative path so it reaches the same file the shell would.
		let cwd: Vec<u8> = context.cwd.clone().into_bytes();
		// Split the argument string into the path and the text (on the first space), then
		// resolve the path against the inherited cwd.
		let (path_arg, text): (&[u8], &[u8]) = match args.iter().position(|&b: &u8| b == b' ') {
			Some(sp) => (&args[..sp], &args[sp + 1..]),
			None => (&args[..], b""),
		};
		let cwd_str: &str = core::str::from_utf8(&cwd).unwrap_or("");
		let uri: String = match path::resolve(cwd_str, path_arg) {
			Some(u) => u,
			None => {
				eprint(b"write: invalid path\n");
				exit();
			}
		};
		// route the path to the client for the volume it names.
		let storage: u64 = path::volume_client(cwd_str, path_arg, system, media, iso, udf, usb, path::NOT_GRANTED, path::NOT_GRANTED);
		write(storage, &uri, text);
	}
	exit();
}

// One streaming-write chunk: bounded so the sender never outruns the service's
// drain by more than the channel queue absorbs (backpressure yields, the service
// keeps draining), never a bound on the file.
const WRITE_CHUNK: usize = 32 * 1024;

// Send the text through the storage grant's streaming write form - the file's bytes
// travel as plain messages on a fresh channel (closed = end of data), so a file's
// size is bounded by the filesystem and by the service's own accumulation policy, never by one transfer.
unsafe fn write(storage: u64, uri: &str, text: &[u8]) {
	unsafe {
		let (producer, consumer): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => {
				eprint(b"write: out of memory\n");
				return;
			}
		};
		let pending = match VolumeClient::new(storage).begin_write_stream(uri, consumer) {
			Some(pending) => pending,
			None => {
				close(producer);
				eprint(b"write: could not write ");
				eprint(uri.as_bytes());
				eprint(b"\n");
				return;
			}
		};
		for chunk in text.chunks(WRITE_CHUNK) {
			if !send_blocking(producer, chunk, 0) {
				break;
			}
		}
		close(producer);
		if matches!(pending.finish(), Some(Ok(()))) {
			print(b"wrote ");
			print(uri.as_bytes());
			print(b"\n");
		} else {
			eprint(b"write: could not write ");
			eprint(uri.as_bytes());
			eprint(b"\n");
		}
	}
}

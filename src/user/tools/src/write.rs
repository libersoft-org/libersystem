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
use proto::path;
use rt::*;
use volume_client::VolumeClient;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 1024] = [0u8; 1024];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - "<path> <text>".
		let args: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the four volume clients the `volumes` capability bundles (SYSTEM / MEDIA /
		//    ISO / UDF, in grant order); a volume whose disk is absent arrives as 0.
		let system: u64 = recv_tagged(bootstrap, &mut buf, b"SYSTEM").unwrap_or(0);
		let media: u64 = recv_tagged(bootstrap, &mut buf, b"MEDIA").unwrap_or(0);
		let iso: u64 = recv_tagged(bootstrap, &mut buf, b"ISO").unwrap_or(0);
		let udf: u64 = recv_tagged(bootstrap, &mut buf, b"UDF").unwrap_or(0);
		let usb: u64 = recv_tagged(bootstrap, &mut buf, b"USB").unwrap_or(0);
		// 4. receive the inherited working directory (the last bootstrap message), used to
		//    resolve a relative path so it reaches the same file the shell would.
		let cwd: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => Vec::new(),
		};
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
				print(b"write: invalid path\n");
				exit();
			}
		};
		// route the path to the client for the volume it names.
		let storage: u64 = path::volume_client(cwd_str, path_arg, system, media, iso, udf, usb);
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
// size is bounded by the filesystem, never by one transfer.
unsafe fn write(storage: u64, uri: &str, text: &[u8]) {
	unsafe {
		let (producer, consumer): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => {
				print(b"write: out of memory\n");
				return;
			}
		};
		let pending = match VolumeClient::new(storage).begin_write_stream(uri, consumer) {
			Some(pending) => pending,
			None => {
				close(producer);
				print(b"write: could not write ");
				print(uri.as_bytes());
				print(b"\n");
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
			print(b"write: could not write ");
			print(uri.as_bytes());
			print(b"\n");
		}
	}
}

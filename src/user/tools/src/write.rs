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
use proto::system::volume;
use rt::*;

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
		let storage: u64 = path::volume_client(cwd_str, path_arg, system, media, iso, udf);
		write(storage, uri.as_bytes(), text);
	}
	exit();
}

// Stage the text in a shared buffer, write it through the storage grant, and print a
// one-line confirmation - reporting a concise error if it cannot be written.
unsafe fn write(storage: u64, uri: &[u8], text: &[u8]) {
	unsafe {
		let data: proto::codec::Buffer = match make_buffer(text) {
			Some(b) => b,
			None => {
				print(b"write: out of memory\n");
				return;
			}
		};
		let path: String = String::from_utf8_lossy(uri).into_owned();
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		match client.write(&path, &data) {
			Some(Ok(())) => {
				print(b"wrote ");
				print(uri);
				print(b"\n");
			}
			_ => {
				print(b"write: could not write ");
				print(uri);
				print(b"\n");
			}
		}
	}
}

// Stage `bytes` in a fresh shared memory object and return a Buffer capability (a
// transferable read-only handle plus length) to hand to StorageService zero-copy.
unsafe fn make_buffer(bytes: &[u8]) -> Option<proto::codec::Buffer> {
	unsafe {
		let alloc_len: usize = bytes.len().max(1);
		let obj: i64 = memory_object_create(alloc_len as u64);
		if obj < 0 {
			return None;
		}
		let obj: u64 = obj as u64;
		let mapped: u64 = match map_object(obj) {
			Some(base) => base,
			None => {
				close(obj);
				return None;
			}
		};
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped as *mut u8, bytes.len());
		unmap_object(obj);
		let granted: i64 = duplicate(obj, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(obj);
		if granted < 0 {
			return None;
		}
		Some(proto::codec::Buffer { handle: granted as u64, len: bytes.len() as u64 })
	}
}

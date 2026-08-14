// redirect_in - the read half of shell input redirection, as a pipeline stage.
//
// `cmd < path` becomes `redirect_in path | cmd`. That is the whole design, and it is the reason
// there is a program here rather than a branch inside the shell or the broker.
//
// WHAT THE ALTERNATIVES COST. The shell could open the file and push it down a channel itself, and
// then a BACKGROUND pipeline has nobody to pump it - the shell has returned to its prompt. The
// broker could do it, and then the service that every launch goes through blocks on a file read.
// Both also have to invent a lifetime for the source capability: the milestone's rule is that the
// child receives only the stream endpoint, never the volume grant or a reusable file capability,
// and a pump inside the shell has to hold one while the child runs.
//
// A STAGE HAS NONE OF THOSE PROBLEMS. It is governed like any other program - `manifest_for` grants
// it `volumes` and nothing else - it holds the file for exactly as long as it is reading, and every
// lifecycle rule the pipeline already has applies to it unchanged: it closes its end at EOF so the
// consumer sees end-of-stream, a consumer that exits early breaks its pipe, and the ProcessGroup
// the shell already builds covers it for signals.
//
// The path is resolved against the SHELL's working directory, because the shell expands the
// redirection and passes the path as this stage's argument - so `< notes.txt` means the same file it
// would have meant to any other tool on that line.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use proto::system::{LaunchContext, OpenOpts};
use rt::*;
use storage_proto::path;
use volume_client::VolumeClient;

// One send per chunk, and the chunk is the shell's own relay size. A pipe is a bounded queue: a
// writer that fills it blocks until the reader drains it, which is the backpressure the whole
// design wants - so the only thing this constant decides is how much is in flight, not whether a
// large file works.
const CHUNK: usize = 4096;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		inherit_stdout(bootstrap);
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let mut volumes: CapSet = recv_caps(bootstrap);
		let system: u64 = volumes.take(CAP_SYSTEM);
		let media: u64 = volumes.take(CAP_MEDIA);
		let iso: u64 = volumes.take(CAP_ISO);
		let udf: u64 = volumes.take(CAP_UDF);
		let usb: u64 = volumes.take(CAP_USB);
		let arg: &[u8] = context.arguments.as_bytes();
		if arg.is_empty() {
			eprint(b"redirect_in: no source path\n");
			exit();
		}
		let cwd_str: &str = &context.cwd;
		let uri: String = match path::resolve(cwd_str, arg) {
			Some(u) => u,
			None => {
				eprint(b"redirect_in: ");
				eprint(arg);
				eprint(b": invalid path\n");
				exit();
			}
		};
		let storage: u64 = path::volume_client(cwd_str, arg, system, media, iso, udf, usb);
		pump(storage, uri.as_bytes());
	}
	exit();
}

// Open the source read-only and copy it to stdout in bounded chunks.
//
// FAILS BEFORE IT WRITES ANYTHING. A source that cannot be opened - missing, a directory, on a
// backend that cannot serve it - reports and exits without sending a byte, so the consumer sees an
// immediately closed input rather than a truncated one. That is the difference between "the file was
// empty" and "the file was not there", and a redirection that cannot tell them apart hands a tool a
// silent lie.
unsafe fn pump(storage: u64, uri: &[u8]) {
	unsafe {
		let opts: OpenOpts = OpenOpts { path: String::from_utf8_lossy(uri).into_owned(), write: false, create: false };
		let mut client = VolumeClient::new(storage);
		let result = match client.open(&opts) {
			Some(Ok(r)) => r,
			_ => {
				eprint(b"redirect_in: ");
				eprint(uri);
				eprint(b": cannot open\n");
				return;
			}
		};
		if result.file == 0 {
			eprint(b"redirect_in: ");
			eprint(uri);
			eprint(b": cannot open\n");
			return;
		}
		if result.size == 0 {
			close(result.file);
			return;
		}
		let mapped: u64 = match map_object(result.file) {
			Some(base) => base,
			None => {
				close(result.file);
				eprint(b"redirect_in: ");
				eprint(uri);
				eprint(b": cannot map\n");
				return;
			}
		};
		let contents: &[u8] = core::slice::from_raw_parts(mapped as *const u8, result.size as usize);
		let mut at: usize = 0;
		while at < contents.len() {
			let end: usize = (at + CHUNK).min(contents.len());
			// UNMODIFIED BYTES. `cat` appends a newline when a file has none, because a person is
			// reading its output; a redirection is a byte stream into another program, and adding
			// a byte the file does not contain would make `wc -c < f` disagree with the file.
			print(&contents[at..end]);
			at = end;
		}
		unmap_object(result.file);
		close(result.file);
	}
}

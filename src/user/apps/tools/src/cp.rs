// cp - copy files within or across granted volumes, run as its own sandboxed ELF.
//
// EVERY COPY IS A TRANSACTION. The destination is written through `volume.open-writer`, which
// publishes nothing until `commit` - so a copy that runs out of space, loses its source or is
// killed half way leaves the destination exactly as it was, rather than truncated or half-written.
// That is the property the writer resource exists for and the reason this tool does not use
// `volume.write`.
//
// The bytes stream: a window is read from the source and written to the session, so the memory cost
// is the window rather than the file. The session's own ceiling still applies, and a file past it
// fails on the write that crosses the line rather than at commit.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, classify};
use proto::system::{LaunchContext, WriterMode};
use rt::*;
use tools::{VolumeSet, push_decimal, split_args};
use volume_client::{VolumeClient, WriterClient};

// One transfer. The writer's `write` carries a length-prefixed list, so a chunk may not exceed what
// that length can express; 32 KiB leaves room and keeps the round trips few.
const CHUNK: u32 = 32 * 1024;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		inherit_stdout(bootstrap);
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let arguments: Vec<u8> = context.arguments.clone().into_bytes();
		let volumes: VolumeSet = VolumeSet::receive(bootstrap, &mut buf);
		let cwd: String = context.cwd.clone();

		let mut force = false;
		let mut words: Vec<&[u8]> = Vec::new();
		for word in split_args(&arguments) {
			match classify(word) {
				Arg::Long(b"force", None) => force = true,
				Arg::Short(b'f') => force = true,
				Arg::Value(value) => {
					if words.try_reserve(1).is_err() {
						eprint(b"cp: out of memory\n");
						exit();
					}
					words.push(value);
				}
				_ => {
					eprint(b"cp: usage: cp [-f] <source> <destination>\n");
					exit();
				}
			}
		}
		if words.len() != 2 {
			eprint(b"cp: usage: cp [-f] <source> <destination>\n");
			exit();
		}
		let (Some(source), Some(destination)) = (storage_proto::path::resolve(&cwd, words[0]), storage_proto::path::resolve(&cwd, words[1])) else {
			eprint(b"cp: invalid path\n");
			exit();
		};
		// THE SAME PATH IS REFUSED before anything opens: a copy onto itself through a
		// truncate-first writer is how a file is destroyed by a command that looks harmless.
		if source == destination {
			eprint(b"cp: source and destination are the same file\n");
			exit();
		}
		let from: u64 = volumes.client_for(&cwd, words[0]);
		let to: u64 = volumes.client_for(&cwd, words[1]);
		if from == 0 || to == 0 {
			eprint(b"cp: no volume\n");
			exit();
		}
		copy(from, &source, to, &destination, force);
	}
	exit();
}

unsafe fn copy(from: u64, source: &str, to: u64, destination: &str, force: bool) {
	unsafe {
		let mut source_client = VolumeClient::new(from);
		let Some(Ok(info)) = source_client.stat(source) else {
			eprint(b"cp: cannot read ");
			eprint(source.as_bytes());
			eprint(b"\n");
			return;
		};
		let mut destination_client = VolumeClient::new(to);
		// AN EXISTING DESTINATION IS A DECISION, not a default. Overwriting without being asked is
		// how a copy destroys the file it was meant to sit beside.
		if !force && matches!(destination_client.stat(destination), Some(Ok(_))) {
			eprint(b"cp: ");
			eprint(destination.as_bytes());
			eprint(b" exists; pass -f to replace it\n");
			return;
		}
		let mut writer: WriterClient = match destination_client.open_writer(destination, WriterMode::Replace) {
			Some(Ok(writer)) => writer,
			_ => {
				eprint(b"cp: cannot write ");
				eprint(destination.as_bytes());
				eprint(b"\n");
				return;
			}
		};
		let mut offset: u64 = 0;
		loop {
			let window = match tools::read_volume_window(from, source, offset, CHUNK) {
				Ok(window) => window,
				Err(_) => {
					// The source failed part way. ABORT rather than commit: a destination
					// published from a partial read is a file that looks complete and is not.
					let _ = writer.abort();
					close(writer.handle());
					eprint(b"cp: read failed part way; nothing was published\n");
					return;
				}
			};
			if window.is_empty() {
				break;
			}
			offset = offset.saturating_add(window.len() as u64);
			for chunk in window.chunks(CHUNK as usize) {
				if !matches!(writer.write(chunk), Some(Ok(_))) {
					let _ = writer.abort();
					close(writer.handle());
					eprint(b"cp: write failed; nothing was published\n");
					return;
				}
			}
		}
		let published = match writer.commit() {
			Some(Ok(published)) => published,
			_ => {
				close(writer.handle());
				eprint(b"cp: could not publish ");
				eprint(destination.as_bytes());
				eprint(b"\n");
				return;
			}
		};
		close(writer.handle());
		// THE LENGTH IS CHECKED, because a copy that published fewer bytes than it read is the
		// failure a transaction is supposed to make impossible - and a check that is only in the
		// implementation is a check nobody sees fail.
		if published != info.size {
			eprint(b"cp: published length differs from the source; the destination is suspect\n");
			return;
		}
		let mut line = String::new();
		push_decimal(&mut line, published);
		print(b"copied ");
		print(line.as_bytes());
		print(b" bytes to ");
		print(destination.as_bytes());
		print(b"\n");
	}
}

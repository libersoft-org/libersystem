// mv - move a file, run as its own sandboxed ELF.
//
// WITHIN ONE VOLUME IT IS A RENAME, which moves the directory entry and never touches the contents:
// nothing can observe a half-moved file, and a huge file moves in the time a small one does.
//
// ACROSS VOLUMES IT IS NOT ATOMIC AND DOES NOT PRETEND TO BE. It is copy -> verify -> publish ->
// delete the source, in that order, and the source survives every failure before the delete. Saying
// "moved" over a sequence that can lose the file at four points is the dishonesty this ordering
// exists to prevent - so the source is removed only after the destination is published and its
// length checked.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, classify};
use proto::system::{LaunchContext, WriterMode};
use rt::*;
use tools::{VolumeSet, split_args};
use volume_client::{VolumeClient, WriterClient};

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
						eprint(b"mv: out of memory\n");
						exit();
					}
					words.push(value);
				}
				_ => {
					eprint(b"mv: usage: mv [-f] <source> <destination>\n");
					exit();
				}
			}
		}
		if words.len() != 2 {
			eprint(b"mv: usage: mv [-f] <source> <destination>\n");
			exit();
		}
		let (Some(source), Some(destination)) = (storage_proto::path::resolve(&cwd, words[0]), storage_proto::path::resolve(&cwd, words[1])) else {
			eprint(b"mv: invalid path\n");
			exit();
		};
		if source == destination {
			eprint(b"mv: source and destination are the same file\n");
			exit();
		}
		let from: u64 = volumes.client_for(&cwd, words[0]);
		let to: u64 = volumes.client_for(&cwd, words[1]);
		if from == 0 || to == 0 {
			eprint(b"mv: no volume\n");
			exit();
		}
		let same_volume = storage_proto::path::volume(&cwd, words[0]) == storage_proto::path::volume(&cwd, words[1]);
		let mut destination_client = VolumeClient::new(to);
		// The destination is checked ONCE, here, for both paths: `rename` refuses an existing
		// destination and so does the cross-volume form, so `-f` means the same thing either way.
		if matches!(destination_client.stat(&destination), Some(Ok(_))) {
			if !force {
				eprint(b"mv: ");
				eprint(destination.as_bytes());
				eprint(b" exists; pass -f to replace it\n");
				exit();
			}
			if !matches!(destination_client.remove(&destination), Some(Ok(()))) {
				eprint(b"mv: cannot replace ");
				eprint(destination.as_bytes());
				eprint(b"\n");
				exit();
			}
		}
		if same_volume {
			match VolumeClient::new(from).rename(&source, &destination) {
				Some(Ok(())) => {
					print(b"moved ");
					print(destination.as_bytes());
					print(b"\n");
				}
				// A backend that cannot rename atomically says `invalid` rather than doing it in
				// two steps behind the caller's back; the cross-volume path is where two steps are
				// declared, and it is not taken silently.
				_ => {
					eprint(b"mv: this volume cannot rename ");
					eprint(source.as_bytes());
					eprint(b"\n");
				}
			}
			exit();
		}
		transfer(from, &source, to, &destination);
	}
	exit();
}

// Copy, verify, publish, then delete the source - and stop at the first failure, which always
// leaves the source intact.
unsafe fn transfer(from: u64, source: &str, to: u64, destination: &str) {
	unsafe {
		let mut source_client = VolumeClient::new(from);
		let Some(Ok(info)) = source_client.stat(source) else {
			eprint(b"mv: cannot read ");
			eprint(source.as_bytes());
			eprint(b"\n");
			return;
		};
		let mut writer: WriterClient = match VolumeClient::new(to).open_writer(destination, WriterMode::Replace) {
			Some(Ok(writer)) => writer,
			_ => {
				eprint(b"mv: cannot write ");
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
					let _ = writer.abort();
					close(writer.handle());
					eprint(b"mv: read failed part way; the source is untouched\n");
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
					eprint(b"mv: write failed; the source is untouched\n");
					return;
				}
			}
		}
		let published = match writer.commit() {
			Some(Ok(published)) => published,
			_ => {
				close(writer.handle());
				eprint(b"mv: could not publish; the source is untouched\n");
				return;
			}
		};
		close(writer.handle());
		// VERIFY BEFORE DELETING. This is the whole difference between a move and a way to lose a
		// file: the source goes only once the destination is known to hold the same number of
		// bytes, and a mismatch keeps both.
		if published != info.size {
			eprint(b"mv: published length differs from the source; both files were kept\n");
			return;
		}
		match source_client.remove(source) {
			Some(Ok(())) => {
				print(b"moved ");
				print(destination.as_bytes());
				print(b"\n");
			}
			// The destination is published and the source could not be removed, so the file now
			// exists twice. That is stated rather than reported as success: the caller has to
			// decide what to do about the copy left behind.
			_ => {
				eprint(b"mv: published ");
				eprint(destination.as_bytes());
				eprint(b" but could not remove the source; both files exist\n");
			}
		}
	}
}

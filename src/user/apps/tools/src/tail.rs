// tail - print the last lines of a file, and optionally follow it, run as its own sandboxed ELF.
//
// THE LAST N LINES ARE HELD IN A RING, not by keeping the file: `cli::LastLines` holds exactly the
// window asked for, so tailing a log costs the size of the tail rather than the size of the log.
//
// `--follow` RIDES `volume.watch`, not a poll. The service tells the tool when the file changed and
// the tool reads from where it stopped; a polling `tail -f` wakes the whole system on a timer to
// learn that nothing happened. What a watcher promises is stated in the storage contract and is
// worth repeating here: it reports the changes that pass through StorageService, so a file edited
// behind the service's back is not seen - and an event is a hint to re-read rather than a record to
// reconstruct from.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, LastLines, classify, parse_u64};
use proto::system::{LaunchContext, volume};
use rt::*;
use tools::{VolumeSet, split_args};
use volume_client::VolumeClient;

const WINDOW: u32 = 16 * 1024;
const DEFAULT_LINES: u64 = 10;

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

		let mut lines: u64 = DEFAULT_LINES;
		let mut follow = false;
		let mut path: Option<&[u8]> = None;
		let mut expect = false;
		for word in split_args(&arguments) {
			if expect {
				let Some(count) = parse_u64(word) else {
					eprint(b"tail: not a count\n");
					exit();
				};
				lines = count;
				expect = false;
				continue;
			}
			match classify(word) {
				Arg::Long(b"lines", Some(value)) => match parse_u64(value) {
					Some(count) => lines = count,
					None => {
						eprint(b"tail: not a count\n");
						exit();
					}
				},
				Arg::Long(b"lines", None) => expect = true,
				Arg::Long(b"follow", None) => follow = true,
				Arg::Short(b'n') => expect = true,
				Arg::Short(b'f') => follow = true,
				// ONE PATH. A followed tail of several files would interleave two streams with no
				// way to tell them apart, so the multi-file form waits until there is a header
				// convention worth having.
				Arg::Value(value) if path.is_none() => path = Some(value),
				_ => {
					eprint(b"tail: usage: tail [-n lines] [-f] <path>\n");
					exit();
				}
			}
		}
		let Some(argument) = path.filter(|_| !expect) else {
			eprint(b"tail: usage: tail [-n lines] [-f] <path>\n");
			exit();
		};
		let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
			eprint(b"tail: invalid path\n");
			exit();
		};
		let storage: u64 = volumes.client_for(&cwd, argument);
		if storage == 0 {
			eprint(b"tail: no volume\n");
			exit();
		}
		let Some(end) = tail(storage, &uri, lines) else {
			eprint(b"tail: cannot read ");
			eprint(uri.as_bytes());
			eprint(b"\n");
			exit();
		};
		if follow {
			follow_file(storage, &uri, end);
		}
	}
	exit();
}

// Print the last `lines` lines and return the offset the file ended at, which is where a follow
// resumes.
unsafe fn tail(storage: u64, path: &str, lines: u64) -> Option<u64> {
	unsafe {
		let mut ring = LastLines::new(usize::try_from(lines).ok()?);
		let mut offset: u64 = 0;
		let mut partial: Vec<u8> = Vec::new();
		loop {
			let window = tools::read_volume_window(storage, path, offset, WINDOW).ok()?;
			if window.is_empty() {
				break;
			}
			offset = offset.saturating_add(window.len() as u64);
			for &byte in &window {
				if byte == b'\n' {
					if !ring.push(&partial) {
						eprint(b"tail: out of memory\n");
						return None;
					}
					partial.clear();
					continue;
				}
				if partial.try_reserve(1).is_err() {
					eprint(b"tail: out of memory\n");
					return None;
				}
				partial.push(byte);
			}
		}
		// A trailing line without a newline is a line, for the reason `head` gives.
		if !partial.is_empty() && !ring.push(&partial) {
			eprint(b"tail: out of memory\n");
			return None;
		}
		for line in ring.lines() {
			print(line);
			print(b"\n");
		}
		Some(offset)
	}
}

// Follow the file: wait for the service to say it changed, then print what was appended.
//
// A TRUNCATION IS NOT AN APPEND. If the file is shorter than where we stopped, it was replaced or
// truncated, and continuing from the old offset would print whatever now happens to be there - so
// the follow restarts from the beginning of the new contents, which is what a reader wants and what
// a naive `tail -f` gets wrong.
unsafe fn follow_file(storage: u64, path: &str, mut offset: u64) {
	unsafe {
		let mut client = VolumeClient::new(storage);
		let events: u64 = match client.watch(path) {
			Some(Ok(events)) => events,
			_ => {
				eprint(b"tail: cannot watch ");
				eprint(path.as_bytes());
				eprint(b"\n");
				return;
			}
		};
		loop {
			let mut frame_handles = proto::codec::Handles::new();
			let bytes = match recv_vec_caps_blocking(events, &mut frame_handles) {
				ReceivedVecCaps::Message { bytes } => bytes,
				// The stream ended: either the volume dropped this watcher (it could not keep up)
				// or the service is gone. Either way there is nothing left to follow, and saying
				// so is better than looping on a closed channel.
				_ => {
					close(events);
					return;
				}
			};
			for handle in frame_handles.as_slice() {
				close(*handle);
			}
			let Some(event) = volume::watch_read(&bytes, &frame_handles) else { continue };
			if matches!(event.kind, proto::system::FileEventKind::Removed) {
				eprint(b"tail: file removed\n");
				close(events);
				return;
			}
			if event.size < offset {
				offset = 0;
			}
			// Read everything that arrived since we last looked, in windows.
			loop {
				let Ok(window) = tools::read_volume_window(storage, path, offset, WINDOW) else {
					close(events);
					return;
				};
				if window.is_empty() {
					break;
				}
				offset = offset.saturating_add(window.len() as u64);
				print(&window);
			}
		}
	}
}

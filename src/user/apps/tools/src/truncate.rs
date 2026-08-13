// truncate - set a file's length, run as its own sandboxed ELF.
//
// Shorter drops the tail; longer zero-extends, and the zeros are a promise the storage contract
// makes rather than a side effect of how a filesystem happens to allocate. Neither form touches the
// bytes outside the change.
//
// The size may be ABSOLUTE (`4K`) or RELATIVE (`+1M`, `-512`), and a relative size is resolved
// against the length the file has NOW - which is why it stats first and reports a file it cannot
// stat rather than guessing zero.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::parse_size;
use proto::system::LaunchContext;
use rt::*;
use tools::{VolumeSet, split_args};
use volume_client::VolumeClient;

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

		let mut size: Option<&[u8]> = None;
		let mut paths: Vec<&[u8]> = Vec::new();
		let mut expect = false;
		for word in split_args(&arguments) {
			if expect {
				size = Some(word);
				expect = false;
				continue;
			}
			match word {
				b"-s" | b"--size" => expect = true,
				value if value.starts_with(b"--size=") => size = Some(&value[7..]),
				value if value.starts_with(b"-") && value != b"-" => {
					eprint(b"truncate: usage: truncate -s <size> <path> [path...]\n");
					exit();
				}
				value => {
					if paths.try_reserve(1).is_err() {
						eprint(b"truncate: out of memory\n");
						exit();
					}
					paths.push(value);
				}
			}
		}
		let (Some(size), false) = (size, expect) else {
			eprint(b"truncate: usage: truncate -s <size> <path> [path...]\n");
			exit();
		};
		if paths.is_empty() {
			eprint(b"truncate: usage: truncate -s <size> <path> [path...]\n");
			exit();
		}
		// The sign is read here rather than in the size parser, because `+`/`-` is a property of
		// this argument (a length relative to the file's own) and not of sizes generally.
		let (relative, magnitude): (i8, &[u8]) = match size.first() {
			Some(b'+') => (1, &size[1..]),
			Some(b'-') => (-1, &size[1..]),
			_ => (0, size),
		};
		let Some(magnitude) = parse_size(magnitude) else {
			eprint(b"truncate: not a size\n");
			exit();
		};
		for argument in &paths {
			let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
				eprint(b"truncate: invalid path\n");
				continue;
			};
			let storage: u64 = volumes.client_for(&cwd, argument);
			if storage == 0 {
				eprint(b"truncate: no volume\n");
				continue;
			}
			let mut client = VolumeClient::new(storage);
			let length: u64 = if relative == 0 {
				magnitude
			} else {
				let Some(Ok(info)) = client.stat(&uri) else {
					eprint(b"truncate: cannot stat ");
					eprint(uri.as_bytes());
					eprint(b"\n");
					continue;
				};
				// SATURATING at zero rather than wrapping: `truncate -s -1G` of a small file means
				// empty, not four exabytes.
				if relative > 0 { info.size.saturating_add(magnitude) } else { info.size.saturating_sub(magnitude) }
			};
			match client.truncate(&uri, length) {
				Some(Ok(())) => {}
				_ => {
					eprint(b"truncate: cannot resize ");
					eprint(uri.as_bytes());
					eprint(b"\n");
				}
			}
		}
	}
	exit();
}

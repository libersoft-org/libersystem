// head - print the first lines or bytes of one or more files, run as its own sandboxed ELF.
//
// It reads only what it prints. `head -n 10` of a gigabyte reads the windows that hold the first
// ten lines and stops - which is the whole point of the bounded read, and the difference between a
// `head` and a `cat` that gives up early.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, classify, parse_size, parse_u64};
use proto::system::LaunchContext;
use rt::*;
use tools::{VolumeSet, split_args};

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

		let mut lines: Option<u64> = None;
		let mut bytes: Option<u64> = None;
		let mut quiet = false;
		let mut paths: Vec<&[u8]> = Vec::new();
		let mut expect: Option<u8> = None;
		for word in split_args(&arguments) {
			if let Some(letter) = expect.take() {
				let value = match letter {
					b'n' => parse_u64(word),
					_ => parse_size(word),
				};
				let Some(value) = value else {
					eprint(b"head: not a count\n");
					exit();
				};
				if letter == b'n' {
					lines = Some(value)
				} else {
					bytes = Some(value)
				}
				continue;
			}
			match classify(word) {
				Arg::Long(b"lines", Some(value)) => lines = Some(parse_or_die(value, b"head: not a count\n")),
				Arg::Long(b"bytes", Some(value)) => bytes = Some(parse_size_or_die(value)),
				Arg::Long(b"lines", None) => expect = Some(b'n'),
				Arg::Long(b"bytes", None) => expect = Some(b'c'),
				Arg::Long(b"quiet", None) => quiet = true,
				Arg::Short(b'n') => expect = Some(b'n'),
				Arg::Short(b'c') => expect = Some(b'c'),
				Arg::Short(b'q') => quiet = true,
				Arg::Value(path) => {
					if paths.try_reserve(1).is_err() {
						eprint(b"head: out of memory\n");
						exit();
					}
					paths.push(path);
				}
				// Unknown options fail before a byte is printed.
				_ => {
					usage();
					exit();
				}
			}
		}
		if expect.is_some() || paths.is_empty() {
			usage();
			exit();
		}
		// BYTES WIN when both are given, because a count of bytes is exact and a count of lines is
		// a count of what the bytes happen to contain; silently applying both would print the
		// shorter of two answers the caller did not ask for.
		let many = paths.len() > 1;
		for (index, argument) in paths.iter().enumerate() {
			let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
				eprint(b"head: invalid path\n");
				continue;
			};
			let storage: u64 = volumes.client_for(&cwd, argument);
			if many && !quiet {
				if index > 0 {
					print(b"\n");
				}
				print(b"==> ");
				print(uri.as_bytes());
				print(b" <==\n");
			}
			let ok = match bytes {
				Some(limit) => head_bytes(storage, &uri, limit),
				None => head_lines(storage, &uri, lines.unwrap_or(DEFAULT_LINES)),
			};
			if !ok {
				eprint(b"head: cannot read ");
				eprint(uri.as_bytes());
				eprint(b"\n");
			}
		}
	}
	exit();
}

unsafe fn usage() {
	unsafe { eprint(b"head: usage: head [-n lines] [-c bytes] [-q] <path> [path...]\n") };
}

fn parse_or_die(value: &[u8], message: &[u8]) -> u64 {
	match parse_u64(value) {
		Some(value) => value,
		None => unsafe {
			eprint(message);
			exit()
		},
	}
}

fn parse_size_or_die(value: &[u8]) -> u64 {
	match parse_size(value) {
		Some(value) => value,
		None => unsafe {
			eprint(b"head: not a size\n");
			exit()
		},
	}
}

// The first `limit` bytes, a window at a time.
unsafe fn head_bytes(storage: u64, path: &str, limit: u64) -> bool {
	unsafe {
		if storage == 0 {
			return false;
		}
		let mut left: u64 = limit;
		let mut offset: u64 = 0;
		while left > 0 {
			let want: u32 = core::cmp::min(left, WINDOW as u64) as u32;
			let Ok(window) = tools::read_volume_window(storage, path, offset, want) else { return false };
			if window.is_empty() {
				return true;
			}
			offset = offset.saturating_add(window.len() as u64);
			left = left.saturating_sub(window.len() as u64);
			print(&window);
		}
		true
	}
}

// The first `limit` lines, stopping at the window that completes the last one.
//
// The final line is printed even WITHOUT its newline, because a file whose last line has no
// terminator still has that line - dropping it would make `head` of a whole file differ from the
// file.
unsafe fn head_lines(storage: u64, path: &str, limit: u64) -> bool {
	unsafe {
		if storage == 0 {
			return false;
		}
		if limit == 0 {
			return true;
		}
		let mut seen: u64 = 0;
		let mut offset: u64 = 0;
		loop {
			let Ok(window) = tools::read_volume_window(storage, path, offset, WINDOW) else { return false };
			if window.is_empty() {
				return true;
			}
			offset = offset.saturating_add(window.len() as u64);
			for (index, &byte) in window.iter().enumerate() {
				if byte != b'\n' {
					continue;
				}
				seen += 1;
				if seen < limit {
					continue;
				}
				// EVERYTHING IN THIS WINDOW UP TO HERE, not just the last line: the lines before it
				// have not been printed yet, because a window is printed once and only when it is
				// known to be wholly wanted.
				print(&window[..=index]);
				return true;
			}
			print(&window);
		}
	}
}

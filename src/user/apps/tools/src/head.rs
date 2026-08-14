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
use tools::{Source, VolumeSet, Window, split_args};

// One window, and now also the most a single `-c` read overshoots by.
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
		if expect.is_some() {
			usage();
			exit();
		}
		// NO PATH MEANS STDIN. `head` is the tool this milestone's broken-pipe requirement names:
		// it stops after `limit` and dropping its `Source` closes the read end, which the stage
		// upstream sees as a broken pipe at its next write. Nothing here does that explicitly -
		// it falls out of owning the endpoint.
		if paths.is_empty() {
			let Some(mut source) = Source::from_stdin() else {
				usage();
				exit();
			};
			let ok = match bytes {
				Some(limit) => head_bytes(&mut source, limit),
				None => head_lines(&mut source, lines.unwrap_or(DEFAULT_LINES)),
			};
			if !ok {
				eprint(b"head: input stream failed\n");
			}
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
				Some(limit) => head_bytes(&mut Source::from_path(storage, &uri, WINDOW), limit),
				None => head_lines(&mut Source::from_path(storage, &uri, WINDOW), lines.unwrap_or(DEFAULT_LINES)),
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
unsafe fn head_bytes(source: &mut Source, limit: u64) -> bool {
	unsafe {
		let mut left: u64 = limit;
		while left > 0 {
			let window = match source.next() {
				Window::Bytes(bytes) => bytes,
				Window::End => return true,
				Window::Failed => return false,
			};
			// A WINDOW CAN OVERSHOOT THE LIMIT NOW, which it could not when the caller chose the
			// read size: a stream window is whatever the producer sent. So the cut happens here,
			// on the bytes in hand, rather than in the request.
			let take: usize = core::cmp::min(left, window.len() as u64) as usize;
			left = left.saturating_sub(take as u64);
			if !write_stdout(&window[..take]) {
				return true;
			}
		}
		true
	}
}

// The first `limit` lines, stopping at the window that completes the last one.
//
// The final line is printed even WITHOUT its newline, because a file whose last line has no
// terminator still has that line - dropping it would make `head` of a whole file differ from the
// file.
unsafe fn head_lines(source: &mut Source, limit: u64) -> bool {
	unsafe {
		if limit == 0 {
			return true;
		}
		let mut seen: u64 = 0;
		loop {
			let window = match source.next() {
				Window::Bytes(bytes) => bytes,
				Window::End => return true,
				Window::Failed => return false,
			};
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
				write_stdout(&window[..=index]);
				return true;
			}
			if !write_stdout(&window) {
				return true;
			}
		}
	}
}

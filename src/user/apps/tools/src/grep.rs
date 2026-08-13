// grep - print the lines of a file that contain a fixed string, as its own sandboxed ELF.
//
// FIXED STRING, not a regular expression. A regex engine is a real piece of work with its own
// bounds to prove, and P02M0101 says it is added once and shared when `find` and `grep` both want
// it - so this searches for the bytes it was given, and says so in its help rather than accepting a
// pattern it would silently mis-read.
//
// It streams line by line through `cli::Lines`, so the memory it uses is one line and one window
// whatever the file's size - and one LINE is bounded too, because "the input is a text file" is an
// assumption rather than a fact on media this system did not write.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, ChunkError, LineOutcome, Lines, classify};
use proto::system::LaunchContext;
use rt::*;
use tools::{VolumeSet, VolumeSource, push_decimal, split_args};

const WINDOW: u32 = 16 * 1024;
// The longest line this tool will hold. A file with no newline in it is otherwise a way to grow a
// program by handing it a file.
const MAX_LINE: usize = 64 * 1024;

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

		let mut ignore_case = false;
		let mut invert = false;
		let mut numbers = false;
		let mut count_only = false;
		let mut names_only = false;
		let mut words: Vec<&[u8]> = Vec::new();
		for word in split_args(&arguments) {
			match classify(word) {
				Arg::Long(b"ignore-case", None) => ignore_case = true,
				Arg::Long(b"invert", None) => invert = true,
				Arg::Long(b"number", None) => numbers = true,
				Arg::Long(b"count", None) => count_only = true,
				Arg::Long(b"files", None) => names_only = true,
				Arg::Short(b'i') => ignore_case = true,
				Arg::Short(b'v') => invert = true,
				Arg::Short(b'n') => numbers = true,
				Arg::Short(b'c') => count_only = true,
				Arg::Short(b'l') => names_only = true,
				Arg::Value(value) => {
					if words.try_reserve(1).is_err() {
						eprint(b"grep: out of memory\n");
						exit();
					}
					words.push(value);
				}
				_ => {
					eprint(b"grep: usage: grep [-i][-v][-n][-c][-l] <text> <path> [path...]\n");
					exit();
				}
			}
		}
		if words.len() < 2 {
			eprint(b"grep: usage: grep [-i][-v][-n][-c][-l] <text> <path> [path...]\n");
			exit();
		}
		let needle: &[u8] = words[0];
		let paths = &words[1..];
		let many = paths.len() > 1;
		for argument in paths {
			let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
				eprint(b"grep: invalid path\n");
				continue;
			};
			let storage: u64 = volumes.client_for(&cwd, argument);
			if storage == 0 {
				eprint(b"grep: no volume\n");
				continue;
			}
			search(storage, &uri, needle, ignore_case, invert, numbers, count_only, names_only, many);
		}
	}
	exit();
}

#[allow(clippy::too_many_arguments)]
unsafe fn search(storage: u64, path: &str, needle: &[u8], ignore_case: bool, invert: bool, numbers: bool, count_only: bool, names_only: bool, many: bool) {
	unsafe {
		let mut lines = Lines::new(VolumeSource::new(storage, path, WINDOW), MAX_LINE);
		let mut matches: u64 = 0;
		let mut number: u64 = 0;
		loop {
			match lines.next_line() {
				LineOutcome::Line => {
					number += 1;
					let hit = contains(lines.line(), needle, ignore_case) != invert;
					if !hit {
						continue;
					}
					matches += 1;
					if names_only {
						print(path.as_bytes());
						print(b"\n");
						return;
					}
					if count_only {
						continue;
					}
					let mut prefix = String::new();
					if many {
						prefix.push_str(path);
						prefix.push(':');
					}
					if numbers {
						push_decimal(&mut prefix, number);
						prefix.push(':');
					}
					print(prefix.as_bytes());
					print(lines.line());
					print(b"\n");
				}
				LineOutcome::End => break,
				// A LINE TOO LONG IS REPORTED, not skipped. A `grep` that silently ignored the one
				// line it could not hold would answer "not found" about a file that contains the
				// text - the exact shape of a wrong answer that looks right.
				LineOutcome::TooLong => {
					eprint(b"grep: ");
					eprint(path.as_bytes());
					eprint(b": a line is longer than this tool will hold\n");
					return;
				}
				LineOutcome::Failed(ChunkError::Unavailable) => {
					eprint(b"grep: cannot read ");
					eprint(path.as_bytes());
					eprint(b"\n");
					return;
				}
				LineOutcome::Failed(_) => {
					eprint(b"grep: out of memory\n");
					return;
				}
			}
		}
		if count_only {
			let mut out = String::new();
			if many {
				out.push_str(path);
				out.push(':');
			}
			push_decimal(&mut out, matches);
			out.push('\n');
			print(out.as_bytes());
		}
	}
}

// Whether `haystack` contains `needle`, optionally ignoring ASCII case.
//
// ASCII case only, and it says so: folding case for the rest of Unicode needs the tables the
// localization phase brings, and a partial fold that worked for `A` and not for `Á` would be a
// search that misses in a way nobody could predict.
fn contains(haystack: &[u8], needle: &[u8], ignore_case: bool) -> bool {
	if needle.is_empty() {
		return true;
	}
	if needle.len() > haystack.len() {
		return false;
	}
	haystack.windows(needle.len()).any(|window| if ignore_case { window.iter().zip(needle).all(|(a, b)| a.eq_ignore_ascii_case(b)) } else { window == needle })
}

// cut - select bytes, characters or delimited fields from each line, as its own sandboxed ELF.
//
// It streams line by line and holds one line, so a file's size does not decide the memory - and the
// line itself is bounded, because a file with no newline in it is a way to grow a program.
//
// CHARACTERS ARE UTF-8 SCALARS and bytes are bytes; the two modes are different on purpose. `-b`
// can split a character in half, which is what a caller asking for bytes asked for; `-c` never
// does. Malformed input under `-c` is passed through as the bytes it is rather than replaced,
// because a `cut` that rewrote what it could not decode would change a file it was only reading.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, ChunkError, LineOutcome, Lines, Range, classify, parse_ranges};
use proto::system::LaunchContext;
use rt::*;
use tools::{VolumeSet, VolumeSource, split_args};

const WINDOW: u32 = 16 * 1024;
const MAX_LINE: usize = 64 * 1024;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
	Bytes,
	Chars,
	Fields,
}

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

		let mut mode: Option<Mode> = None;
		let mut ranges: Option<Vec<Range>> = None;
		let mut delimiter: u8 = b'\t';
		let mut output_delimiter: Option<u8> = None;
		let mut complement = false;
		let mut path: Option<&[u8]> = None;
		let mut expect: Option<u8> = None;
		for word in split_args(&arguments) {
			if let Some(letter) = expect.take() {
				match letter {
					b'd' | b'o' => {
						// ONE BYTE. A multi-byte delimiter is a different feature (it needs a
						// matcher rather than a comparison), and accepting the first byte of one
						// would split on something the caller did not name.
						if word.len() != 1 {
							eprint(b"cut: the delimiter is one byte\n");
							exit();
						}
						if letter == b'd' { delimiter = word[0] } else { output_delimiter = Some(word[0]) }
					}
					kind => {
						mode = Some(match kind {
							b'b' => Mode::Bytes,
							b'c' => Mode::Chars,
							_ => Mode::Fields,
						});
						// Byte and character positions count from one, like the fields.
						match parse_ranges(word, false) {
							Some(parsed) => ranges = Some(parsed),
							None => {
								eprint(b"cut: not a range list\n");
								exit();
							}
						}
					}
				}
				continue;
			}
			match classify(word) {
				Arg::Short(b'b') => expect = Some(b'b'),
				Arg::Short(b'c') => expect = Some(b'c'),
				Arg::Short(b'f') => expect = Some(b'f'),
				Arg::Short(b'd') => expect = Some(b'd'),
				Arg::Long(b"complement", None) => complement = true,
				Arg::Long(b"output-delimiter", None) => expect = Some(b'o'),
				Arg::Value(value) if path.is_none() => path = Some(value),
				_ => {
					usage();
					exit();
				}
			}
		}
		let (Some(mode), Some(ranges), Some(argument)) = (mode, ranges, path.filter(|_| expect.is_none())) else {
			usage();
			exit();
		};
		let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
			eprint(b"cut: invalid path\n");
			exit();
		};
		let storage: u64 = volumes.client_for(&cwd, argument);
		if storage == 0 {
			eprint(b"cut: no volume\n");
			exit();
		}
		let out_delimiter: u8 = output_delimiter.unwrap_or(delimiter);
		let mut lines = Lines::new(VolumeSource::new(storage, &uri, WINDOW), MAX_LINE);
		loop {
			match lines.next_line() {
				LineOutcome::Line => {
					let mut out: Vec<u8> = Vec::new();
					let selected = select(lines.line(), mode, &ranges, complement, delimiter, out_delimiter, &mut out);
					if !selected {
						eprint(b"cut: out of memory\n");
						exit();
					}
					print(&out);
					print(b"\n");
				}
				LineOutcome::End => break,
				LineOutcome::TooLong => {
					eprint(b"cut: a line is longer than this tool will hold\n");
					exit();
				}
				LineOutcome::Failed(ChunkError::Unavailable) => {
					eprint(b"cut: cannot read ");
					eprint(uri.as_bytes());
					eprint(b"\n");
					exit();
				}
				LineOutcome::Failed(_) => {
					eprint(b"cut: out of memory\n");
					exit();
				}
			}
		}
	}
	exit();
}

unsafe fn usage() {
	unsafe { eprint(b"cut: usage: cut -b|-c|-f RANGES [-d CHAR] [--output-delimiter CHAR] [--complement] <path>\n") };
}

// Build one output line. Returns false only when the line could not be held.
fn select(line: &[u8], mode: Mode, ranges: &[Range], complement: bool, delimiter: u8, out_delimiter: u8, out: &mut Vec<u8>) -> bool {
	let wanted = |position: u64| -> bool { ranges.iter().any(|range| range.contains(position)) != complement };
	match mode {
		Mode::Bytes => {
			for (index, &byte) in line.iter().enumerate() {
				if wanted(index as u64 + 1) {
					if out.try_reserve(1).is_err() {
						return false;
					}
					out.push(byte);
				}
			}
		}
		Mode::Chars => {
			// Positions count CHARACTERS, and each character's bytes travel together. Undecodable
			// bytes are one position each, so a malformed file still cuts predictably.
			let mut position: u64 = 0;
			let mut at: usize = 0;
			while at < line.len() {
				let width: usize = utf8_width(line[at]).min(line.len() - at);
				position += 1;
				if wanted(position) {
					if out.try_reserve(width).is_err() {
						return false;
					}
					out.extend_from_slice(&line[at..at + width]);
				}
				at += width;
			}
		}
		Mode::Fields => {
			// A LINE WITH NO DELIMITER IS PASSED THROUGH WHOLE, which is what makes `cut -f` safe
			// over a file that mixes tabular and prose lines: it is one field, and dropping it
			// would delete the lines that do not fit the caller's model of the file.
			if !line.contains(&delimiter) {
				if out.try_reserve(line.len()).is_err() {
					return false;
				}
				out.extend_from_slice(line);
				return true;
			}
			let mut first = true;
			for (index, field) in line.split(|&byte| byte == delimiter).enumerate() {
				if !wanted(index as u64 + 1) {
					continue;
				}
				if !first {
					if out.try_reserve(1).is_err() {
						return false;
					}
					out.push(out_delimiter);
				}
				if out.try_reserve(field.len()).is_err() {
					return false;
				}
				out.extend_from_slice(field);
				first = false;
			}
		}
	}
	true
}

// The number of bytes the UTF-8 character starting with this byte occupies. A continuation or
// invalid lead byte counts as one, so undecodable input advances rather than stalling.
fn utf8_width(lead: u8) -> usize {
	match lead {
		0x00..=0x7f => 1,
		0xc0..=0xdf => 2,
		0xe0..=0xef => 3,
		0xf0..=0xf7 => 4,
		_ => 1,
	}
}

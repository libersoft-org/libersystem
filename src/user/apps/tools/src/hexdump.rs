// hexdump - render a file as offsets, hexadecimal bytes and ASCII, run as its own sandboxed ELF.
//
// It streams: any file size is dumpable because the tool holds one window, and `--skip` / `--length`
// bound what it asks for rather than what it prints. Repeated lines are folded to a single `*`, the
// convention that makes a dump of a mostly-zero file readable.
//
// It reads through the volume contract like every other tool. A dump of a raw block device needs a
// capability to that device, which is a separate grant nobody has here - so `hexdump vol://...` is
// a file dump, and says so rather than pretending.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, classify, parse_size};
use proto::system::LaunchContext;
use rt::*;
use tools::{VolumeSet, split_args};

const WINDOW: u32 = 16 * 1024;
const PER_LINE: usize = 16;

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

		let mut skip: u64 = 0;
		let mut length: Option<u64> = None;
		let mut path: Option<&[u8]> = None;
		let mut expect: Option<u8> = None;
		for word in split_args(&arguments) {
			if let Some(letter) = expect.take() {
				let Some(value) = parse_size(word) else {
					eprint(b"hexdump: not a size\n");
					exit();
				};
				if letter == b's' {
					skip = value
				} else {
					length = Some(value)
				}
				continue;
			}
			match classify(word) {
				Arg::Long(b"skip", Some(value)) => skip = size_or_die(value),
				Arg::Long(b"length", Some(value)) => length = Some(size_or_die(value)),
				Arg::Long(b"skip", None) => expect = Some(b's'),
				Arg::Long(b"length", None) => expect = Some(b'n'),
				Arg::Short(b's') => expect = Some(b's'),
				Arg::Short(b'n') => expect = Some(b'n'),
				Arg::Value(value) if path.is_none() => path = Some(value),
				_ => {
					eprint(b"hexdump: usage: hexdump [-s skip] [-n length] <path>\n");
					exit();
				}
			}
		}
		let Some(argument) = path.filter(|_| expect.is_none()) else {
			eprint(b"hexdump: usage: hexdump [-s skip] [-n length] <path>\n");
			exit();
		};
		let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
			eprint(b"hexdump: invalid path\n");
			exit();
		};
		let storage: u64 = volumes.client_for(&cwd, argument);
		if storage == 0 || !dump(storage, &uri, skip, length) {
			eprint(b"hexdump: cannot read ");
			eprint(uri.as_bytes());
			eprint(b"\n");
		}
	}
	exit();
}

fn size_or_die(value: &[u8]) -> u64 {
	match parse_size(value) {
		Some(value) => value,
		None => unsafe {
			eprint(b"hexdump: not a size\n");
			exit()
		},
	}
}

unsafe fn dump(storage: u64, path: &str, skip: u64, length: Option<u64>) -> bool {
	unsafe {
		let mut offset: u64 = skip;
		let mut left: u64 = length.unwrap_or(u64::MAX);
		// One line's worth carried between windows, so a window boundary does not break the
		// sixteen-byte rows - a dump whose row width depended on the chunking would not line up
		// with a second dump of the same file.
		let mut row: Vec<u8> = Vec::new();
		let mut row_offset: u64 = skip;
		let mut previous: Vec<u8> = Vec::new();
		let mut folding = false;
		while left > 0 {
			let want: u32 = core::cmp::min(left, WINDOW as u64).min(WINDOW as u64) as u32;
			let Ok(window) = tools::read_volume_window(storage, path, offset, want) else { return false };
			if window.is_empty() {
				break;
			}
			offset = offset.saturating_add(window.len() as u64);
			left = left.saturating_sub(window.len() as u64);
			for &byte in &window {
				if row.try_reserve(1).is_err() {
					eprint(b"hexdump: out of memory\n");
					return false;
				}
				row.push(byte);
				if row.len() == PER_LINE {
					emit(&row, row_offset, &mut previous, &mut folding);
					row_offset = row_offset.saturating_add(PER_LINE as u64);
					row.clear();
				}
			}
		}
		if !row.is_empty() {
			// The last, short row is never folded: it is the end of the file and a `*` in its
			// place would hide where the file stops.
			folding = false;
			previous.clear();
			emit(&row, row_offset, &mut previous, &mut folding);
			row_offset = row_offset.saturating_add(row.len() as u64);
		}
		// The final offset line, so a reader can see the length without counting rows.
		let mut tail = String::new();
		push_hex_offset(&mut tail, row_offset);
		tail.push('\n');
		print(tail.as_bytes());
		true
	}
}

// One row, unless it repeats the row before it - in which case a single `*` stands for the run.
unsafe fn emit(row: &[u8], offset: u64, previous: &mut Vec<u8>, folding: &mut bool) {
	unsafe {
		if row == previous.as_slice() {
			if !*folding {
				print(b"*\n");
				*folding = true;
			}
			return;
		}
		*folding = false;
		previous.clear();
		if previous.try_reserve_exact(row.len()).is_err() {
			// A row that cannot be remembered simply is not folded against; the dump is still
			// correct, only longer.
			print_row(row, offset);
			return;
		}
		previous.extend_from_slice(row);
		print_row(row, offset);
	}
}

unsafe fn print_row(row: &[u8], offset: u64) {
	unsafe {
		let mut line = String::new();
		push_hex_offset(&mut line, offset);
		line.push(' ');
		for index in 0..PER_LINE {
			match row.get(index) {
				Some(&byte) => {
					line.push(' ');
					push_hex_byte(&mut line, byte);
				}
				None => line.push_str("   "),
			}
			if index == PER_LINE / 2 - 1 {
				line.push(' ');
			}
		}
		line.push_str("  |");
		for &byte in row {
			// The printable ASCII range, and a dot for everything else. A byte rendered as itself
			// would put control characters on the terminal, which is how a dump of a binary file
			// changes the terminal's mode.
			line.push(if (0x20..0x7f).contains(&byte) { byte as char } else { '.' });
		}
		line.push_str("|\n");
		print(line.as_bytes());
	}
}

fn push_hex_offset(out: &mut String, value: u64) {
	for shift in (0..8).rev() {
		push_hex_byte(out, (value >> (shift * 8)) as u8);
	}
}

fn push_hex_byte(out: &mut String, byte: u8) {
	const DIGITS: &[u8; 16] = b"0123456789abcdef";
	out.push(DIGITS[(byte >> 4) as usize] as char);
	out.push(DIGITS[(byte & 0xf) as usize] as char);
}

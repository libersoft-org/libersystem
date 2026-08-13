// sort - order a file's lines, run as its own sandboxed ELF.
//
// IN MEMORY, WITH A STATED CEILING. P02M0101 describes spilling sorted runs to a granted scratch
// directory and merging them, and that is the right shape for an input larger than the Domain's
// budget - but it needs a per-launch scratch grant that does not exist yet, and inventing an
// ambient `/tmp` to fake it would be exactly the assumption this system does not make. So this
// sorts what fits and REFUSES what does not, by name, instead of failing at the allocator: a
// refusal a caller can act on beats an abort it cannot.
//
// The ceiling is the number of lines and the total bytes, both checked as the input streams in.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, ChunkError, HoldError, LineBuffer, LineOutcome, Lines, classify, parse_u64};
use proto::system::LaunchContext;
use rt::*;
use tools::{VolumeSet, VolumeSource, split_args};

const WINDOW: u32 = 16 * 1024;
const MAX_LINE: usize = 64 * 1024;
// What one sort may hold. Reached, it refuses rather than growing: see the note above.
const MAX_LINES: usize = 100_000;
const MAX_BYTES: usize = 16 * 1024 * 1024;

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

		let mut reverse = false;
		let mut unique = false;
		let mut numeric = false;
		let mut field: Option<usize> = None;
		let mut path: Option<&[u8]> = None;
		let mut expect = false;
		for word in split_args(&arguments) {
			if expect {
				let Some(value) = parse_u64(word).and_then(|value| usize::try_from(value).ok()).filter(|value| *value > 0) else {
					eprint(b"sort: not a field number (they start at one)\n");
					exit();
				};
				field = Some(value - 1);
				expect = false;
				continue;
			}
			match classify(word) {
				Arg::Long(b"reverse", None) => reverse = true,
				Arg::Long(b"unique", None) => unique = true,
				Arg::Long(b"numeric", None) => numeric = true,
				Arg::Long(b"key", None) => expect = true,
				Arg::Short(b'r') => reverse = true,
				Arg::Short(b'u') => unique = true,
				Arg::Short(b'n') => numeric = true,
				Arg::Short(b'k') => expect = true,
				Arg::Value(value) if path.is_none() => path = Some(value),
				_ => {
					eprint(b"sort: usage: sort [-r][-u][-n][-k FIELD] <path>\n");
					exit();
				}
			}
		}
		let Some(argument) = path.filter(|_| !expect) else {
			eprint(b"sort: usage: sort [-r][-u][-n][-k FIELD] <path>\n");
			exit();
		};
		let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
			eprint(b"sort: invalid path\n");
			exit();
		};
		let storage: u64 = volumes.client_for(&cwd, argument);
		if storage == 0 {
			eprint(b"sort: no volume\n");
			exit();
		}
		let Some(mut lines) = collect(storage, &uri) else { exit() };
		// A STABLE SORT, and the key decides only the comparison: two lines with equal keys keep
		// the order the file had, which is what makes `-k` predictable and a second sort by another
		// key meaningful.
		lines.sort_by(|a, b| {
			let ordering = if numeric { compare_numeric(key(a, field), key(b, field)) } else { key(a, field).cmp(key(b, field)) };
			if reverse { ordering.reverse() } else { ordering }
		});
		let mut previous: Option<usize> = None;
		for at in 0..lines.len() {
			let line = lines.line(at);
			// `-u` compares WHOLE LINES, not keys: two different lines that share a key are two
			// lines, and dropping one of them would lose data the caller never asked to lose.
			if unique && previous.is_some_and(|previous| lines.line(previous) == line) {
				continue;
			}
			print(line);
			print(b"\n");
			previous = Some(at);
		}
	}
	exit();
}

// The whitespace-separated field to compare by, or the whole line when none was asked for. A line
// with fewer fields than the key compares as empty, which groups the short lines together rather
// than ordering them by an accident of what follows.
fn key(line: &[u8], field: Option<usize>) -> &[u8] {
	match field {
		None => line,
		Some(index) => line.split(|byte| byte.is_ascii_whitespace()).filter(|part| !part.is_empty()).nth(index).unwrap_or(&[]),
	}
}

// Numeric order, with non-numeric text ordering before every number.
//
// A LINE THAT IS NOT A NUMBER IS NOT SILENTLY ZERO. Reading `apple` as 0 puts it among the zeros,
// where a reader will not look for it; ordering the unparseable together and before the numbers
// keeps them visible.
fn compare_numeric(a: &[u8], b: &[u8]) -> core::cmp::Ordering {
	match (cli::parse_u64(a), cli::parse_u64(b)) {
		(Some(a), Some(b)) => a.cmp(&b),
		(Some(_), None) => core::cmp::Ordering::Greater,
		(None, Some(_)) => core::cmp::Ordering::Less,
		(None, None) => a.cmp(b),
	}
}

unsafe fn collect(storage: u64, path: &str) -> Option<LineBuffer> {
	unsafe {
		let mut lines = LineBuffer::new(MAX_LINES, MAX_BYTES);
		let mut reader = Lines::new(VolumeSource::new(storage, path, WINDOW), MAX_LINE);
		loop {
			match reader.next_line() {
				LineOutcome::Line => match lines.push(reader.line()) {
					Ok(()) => {}
					Err(HoldError::Full) => {
						eprint(b"sort: the input is larger than this sort will hold; nothing was printed\n");
						return None;
					}
					Err(HoldError::OutOfMemory) => {
						eprint(b"sort: out of memory\n");
						return None;
					}
				},
				LineOutcome::End => return Some(lines),
				LineOutcome::TooLong => {
					eprint(b"sort: a line is longer than this tool will hold\n");
					return None;
				}
				LineOutcome::Failed(ChunkError::Unavailable) => {
					eprint(b"sort: cannot read ");
					eprint(path.as_bytes());
					eprint(b"\n");
					return None;
				}
				LineOutcome::Failed(_) => {
					eprint(b"sort: out of memory\n");
					return None;
				}
			}
		}
	}
}

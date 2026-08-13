// wc - count bytes, lines, words and Unicode scalars in files, run as its own sandboxed ELF.
//
// STREAMING, so the file's size is not the tool's memory: it reads one bounded window at a time
// through `volume.read` and folds each window into counters. A `wc` that read the whole file to
// count its lines is a `wc` that fails on the file you most want to count.
//
// The counters SATURATE rather than wrap. A count that wrapped would report a small number for an
// enormous stream, which is the one answer worse than refusing - and saturating is honest at a
// scale no volume can reach anyway.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::codec::{JsonMode, json_escape};
use proto::system::LaunchContext;
use rt::*;
use tools::{VolumeSet, push_decimal, split_args};

// One window. Large enough that a big file costs few round trips, small enough that the tool's
// footprint does not depend on what it is counting.
const WINDOW: u32 = 64 * 1024;

#[derive(Clone, Copy, Default)]
struct Counts {
	bytes: u64,
	lines: u64,
	words: u64,
	scalars: u64,
}

impl Counts {
	fn add(&mut self, other: &Counts) {
		self.bytes = self.bytes.saturating_add(other.bytes);
		self.lines = self.lines.saturating_add(other.lines);
		self.words = self.words.saturating_add(other.words);
		self.scalars = self.scalars.saturating_add(other.scalars);
	}
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

		let mut json: Option<JsonMode> = None;
		let mut paths: Vec<&[u8]> = Vec::new();
		for word in split_args(&arguments) {
			match word {
				b"json" | b"json-min" => json = JsonMode::parse(word),
				// Unknown options fail BEFORE any output, so a mistyped flag never half-counts.
				_ if word.starts_with(b"-") => {
					eprint(b"wc: usage: wc <path> [path...] [json]\n");
					exit();
				}
				_ => {
					if paths.try_reserve(1).is_err() {
						eprint(b"wc: out of memory\n");
						exit();
					}
					paths.push(word);
				}
			}
		}
		if paths.is_empty() {
			eprint(b"wc: usage: wc <path> [path...] [json]\n");
			exit();
		}
		let mut document = String::from("[");
		let mut total = Counts::default();
		let mut counted: usize = 0;
		for (index, argument) in paths.iter().enumerate() {
			let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
				eprint(b"wc: invalid path\n");
				continue;
			};
			let storage: u64 = volumes.client_for(&cwd, argument);
			let Some(counts) = count(storage, &uri) else {
				eprint(b"wc: cannot read ");
				eprint(uri.as_bytes());
				eprint(b"\n");
				continue;
			};
			total.add(&counts);
			counted += 1;
			match json {
				Some(_) => {
					if index > 0 && document.len() > 1 {
						document.push(',');
					}
					document.push_str("{\"path\":");
					json_escape(&uri, &mut document);
					push_field(&mut document, ",\"bytes\":", counts.bytes);
					push_field(&mut document, ",\"lines\":", counts.lines);
					push_field(&mut document, ",\"words\":", counts.words);
					push_field(&mut document, ",\"scalars\":", counts.scalars);
					document.push('}');
				}
				None => print_row(&counts, uri.as_bytes()),
			}
		}
		match json {
			Some(mode) => {
				document.push(']');
				let rendered = mode.render(document);
				print(rendered.as_bytes());
				print(b"\n");
			}
			// The total is printed only when there was more than one file to total, which is what
			// makes a one-file `wc` one line.
			None if counted > 1 => print_row(&total, b"total"),
			None => {}
		}
	}
	exit();
}

fn push_field(out: &mut String, label: &str, value: u64) {
	out.push_str(label);
	push_decimal(out, value);
}

unsafe fn print_row(counts: &Counts, label: &[u8]) {
	unsafe {
		let mut line = String::new();
		push_decimal(&mut line, counts.lines);
		line.push(' ');
		push_decimal(&mut line, counts.words);
		line.push(' ');
		push_decimal(&mut line, counts.bytes);
		line.push(' ');
		push_decimal(&mut line, counts.scalars);
		line.push(' ');
		print(line.as_bytes());
		print(label);
		print(b"\n");
	}
}

// Fold one file into counters, one window at a time.
//
// WORDS AND SCALARS ARE COUNTED ACROSS WINDOW BOUNDARIES, which is the only part of this that is
// not obvious: a word split by a window boundary is one word, and a UTF-8 character split by one
// is one character. The two carried states - "was the previous byte whitespace" and "is this byte
// a continuation" - are what make the answer independent of the window size, and a `wc` whose
// answer depended on its chunking would be wrong in a way nothing would notice.
unsafe fn count(storage: u64, path: &str) -> Option<Counts> {
	unsafe {
		if storage == 0 {
			return None;
		}
		let mut counts = Counts::default();
		let mut offset: u64 = 0;
		let mut in_word = false;
		loop {
			let window = tools::read_volume_window(storage, path, offset, WINDOW).ok()?;
			if window.is_empty() {
				return Some(counts);
			}
			offset = offset.saturating_add(window.len() as u64);
			for &byte in &window {
				counts.bytes = counts.bytes.saturating_add(1);
				if byte == b'\n' {
					counts.lines = counts.lines.saturating_add(1);
				}
				// A scalar is counted at its FIRST byte: every byte that is not a continuation
				// (0b10xxxxxx) starts one. Malformed UTF-8 is counted rather than refused - the
				// answer is a count of what is there, and a file that is not text still has a
				// length.
				if byte & 0b1100_0000 != 0b1000_0000 {
					counts.scalars = counts.scalars.saturating_add(1);
				}
				let space = byte.is_ascii_whitespace();
				if !space && !in_word {
					counts.words = counts.words.saturating_add(1);
				}
				in_word = !space;
			}
		}
	}
}

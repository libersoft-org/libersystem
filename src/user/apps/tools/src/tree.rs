// tree - render a directory tree, run as its own sandboxed ELF.
//
// It uses the SHARED ITERATIVE WALKER (`tools::walk`), so its depth is bounded by an argument
// rather than by the stack: a tree deep enough to overflow a recursive walker is a tree somebody
// can make with `mkdir`, and that is not an acceptable way to end a program.
//
// Entries stream as they are discovered, so a huge tree begins rendering at once instead of after.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, classify, parse_u64};
use proto::codec::{JsonMode, json_escape};
use proto::system::{FileType, LaunchContext};
use rt::*;
use tools::{Step, Visit, VolumeSet, push_decimal, split_args};

// What one listing may hold, and how many directories the walk may owe itself. Both are refusals
// rather than truncations: a tree printed short looks like a small tree.
const MAX_ENTRIES: usize = 4096;
const MAX_PENDING: usize = 1024;
const DEFAULT_DEPTH: usize = 16;

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

		let mut depth: usize = DEFAULT_DEPTH;
		let mut dirs_only = false;
		let mut files_only = false;
		let mut sizes = false;
		let mut json: Option<JsonMode> = None;
		let mut root: Option<&[u8]> = None;
		let mut expect = false;
		for word in split_args(&arguments) {
			if expect {
				let Some(value) = parse_u64(word).and_then(|value| usize::try_from(value).ok()) else {
					eprint(b"tree: not a depth\n");
					exit();
				};
				depth = value;
				expect = false;
				continue;
			}
			match classify(word) {
				Arg::Long(b"depth", Some(value)) => match parse_u64(value).and_then(|value| usize::try_from(value).ok()) {
					Some(value) => depth = value,
					None => {
						eprint(b"tree: not a depth\n");
						exit();
					}
				},
				Arg::Long(b"depth", None) => expect = true,
				Arg::Long(b"dirs", None) => dirs_only = true,
				Arg::Long(b"files", None) => files_only = true,
				Arg::Long(b"size", None) => sizes = true,
				Arg::Short(b'L') => expect = true,
				Arg::Short(b'd') => dirs_only = true,
				Arg::Short(b'f') => files_only = true,
				Arg::Short(b's') => sizes = true,
				Arg::Value(b"json") => json = JsonMode::parse(b"json"),
				Arg::Value(b"json-min") => json = JsonMode::parse(b"json-min"),
				Arg::Value(value) if root.is_none() => root = Some(value),
				_ => {
					eprint(b"tree: usage: tree [-L depth] [-d|-f] [-s] [json] [path]\n");
					exit();
				}
			}
		}
		if expect || (dirs_only && files_only) {
			eprint(b"tree: usage: tree [-L depth] [-d|-f] [-s] [json] [path]\n");
			exit();
		}
		let argument: &[u8] = root.unwrap_or(b".");
		let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
			eprint(b"tree: invalid path\n");
			exit();
		};
		let storage: u64 = volumes.client_for(&cwd, argument);
		if storage == 0 {
			eprint(b"tree: no volume\n");
			exit();
		}
		let mut document = String::from("[");
		let mut directories: u64 = 0;
		let mut files: u64 = 0;
		let outcome = tools::walk(storage, &uri, depth, MAX_PENDING, MAX_ENTRIES, |visit: Visit<'_>| {
			let is_dir = visit.entry.r#type == FileType::Dir;
			if is_dir {
				directories = directories.saturating_add(1);
			} else {
				files = files.saturating_add(1);
			}
			// A filter hides an entry from the OUTPUT, never from the walk: `-f` still descends,
			// because the files it wants live inside the directories it does not print.
			if (dirs_only && !is_dir) || (files_only && is_dir) {
				return Step::Continue;
			}
			match json {
				Some(_) => {
					if document.len() > 1 {
						document.push(',');
					}
					document.push_str("{\"path\":");
					json_escape(visit.path, &mut document);
					document.push_str(",\"type\":");
					document.push_str(if is_dir { "\"dir\"" } else { "\"file\"" });
					document.push_str(",\"depth\":");
					push_decimal(&mut document, visit.depth as u64);
					if sizes {
						document.push_str(",\"size\":");
						push_decimal(&mut document, visit.entry.size);
					}
					document.push('}');
				}
				None => {
					let mut line = String::new();
					for _ in 0..visit.depth {
						line.push_str("  ");
					}
					line.push_str(&visit.entry.name);
					if is_dir {
						line.push('/');
					} else if sizes {
						line.push_str("  ");
						push_decimal(&mut line, visit.entry.size);
					}
					line.push('\n');
					print(line.as_bytes());
				}
			}
			Step::Continue
		});
		match json {
			Some(mode) => {
				document.push(']');
				let rendered = mode.render(document);
				print(rendered.as_bytes());
				print(b"\n");
			}
			None => {
				let mut summary = String::new();
				push_decimal(&mut summary, directories);
				summary.push_str(" directories, ");
				push_decimal(&mut summary, files);
				summary.push_str(" files\n");
				print(summary.as_bytes());
			}
		}
		// A walk that could not read part of the tree SAYS SO, after printing what it could: the
		// entries are real, and a summary that counted only the readable part without saying so
		// would read as the whole tree.
		match outcome {
			Ok(()) => {}
			Err(tools::WalkError::Unreadable) => eprint(b"tree: some directories could not be read\n"),
			Err(tools::WalkError::Bounded) => eprint(b"tree: the tree is wider than this walk allows\n"),
			Err(tools::WalkError::OutOfMemory) => eprint(b"tree: out of memory\n"),
		}
	}
	exit();
}

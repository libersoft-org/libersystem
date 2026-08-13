// find - walk a tree and print the paths that match, run as its own sandboxed ELF.
//
// It SELECTS AND PRINTS. There is deliberately no `-exec` and no `-delete`: an expression language
// that mutates is a second, weaker launcher with none of the launcher's authority checks, and the
// shell can already run a command per line.
//
// The walk is the shared iterative one, so its cost is bounded by the arguments rather than by the
// tree, and matches stream as they are found rather than being collected into a list first.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, classify, glob_match, parse_size, parse_u64};
use proto::system::{FileType, LaunchContext};
use rt::*;
use tools::{Step, Visit, VolumeSet, split_args};

const MAX_ENTRIES: usize = 4096;
const MAX_PENDING: usize = 1024;
const DEFAULT_DEPTH: usize = 32;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Want {
	Any,
	Files,
	Dirs,
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

		let mut pattern: Option<Vec<u8>> = None;
		let mut want = Want::Any;
		let mut depth: usize = DEFAULT_DEPTH;
		let mut smaller: Option<u64> = None;
		let mut larger: Option<u64> = None;
		let mut newer: Option<u64> = None;
		let mut roots: Vec<&[u8]> = Vec::new();
		let mut expect: Option<u8> = None;
		for word in split_args(&arguments) {
			if let Some(letter) = expect.take() {
				match letter {
					b'n' => {
						let mut owned = Vec::new();
						if owned.try_reserve_exact(word.len()).is_err() {
							eprint(b"find: out of memory\n");
							exit();
						}
						owned.extend_from_slice(word);
						pattern = Some(owned);
					}
					b't' => {
						want = match word {
							b"f" | b"file" => Want::Files,
							b"d" | b"dir" => Want::Dirs,
							_ => {
								eprint(b"find: type is f or d\n");
								exit();
							}
						}
					}
					b'L' => match parse_u64(word).and_then(|value| usize::try_from(value).ok()) {
						Some(value) => depth = value,
						None => {
							eprint(b"find: not a depth\n");
							exit();
						}
					},
					b'-' => smaller = Some(size_or_die(word)),
					b'+' => larger = Some(size_or_die(word)),
					_ => match parse_u64(word) {
						Some(value) => newer = Some(value),
						None => {
							eprint(b"find: not a timestamp\n");
							exit();
						}
					},
				}
				continue;
			}
			match classify(word) {
				Arg::Long(b"name", None) => expect = Some(b'n'),
				Arg::Long(b"type", None) => expect = Some(b't'),
				Arg::Long(b"depth", None) => expect = Some(b'L'),
				Arg::Long(b"smaller", None) => expect = Some(b'-'),
				Arg::Long(b"larger", None) => expect = Some(b'+'),
				Arg::Long(b"newer", None) => expect = Some(b'm'),
				Arg::Long(b"name", Some(value)) => {
					let mut owned = Vec::new();
					if owned.try_reserve_exact(value.len()).is_err() {
						eprint(b"find: out of memory\n");
						exit();
					}
					owned.extend_from_slice(value);
					pattern = Some(owned);
				}
				Arg::Short(b'n') => expect = Some(b'n'),
				Arg::Short(b't') => expect = Some(b't'),
				Arg::Short(b'L') => expect = Some(b'L'),
				Arg::Value(value) => {
					if roots.try_reserve(1).is_err() {
						eprint(b"find: out of memory\n");
						exit();
					}
					roots.push(value);
				}
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
		if roots.is_empty() {
			roots.push(b".");
		}
		for argument in &roots {
			let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
				eprint(b"find: invalid path\n");
				continue;
			};
			let storage: u64 = volumes.client_for(&cwd, argument);
			if storage == 0 {
				eprint(b"find: no volume\n");
				continue;
			}
			// ONE VOLUME PER ROOT, and no crossing: every path the walk produces is below the URI
			// it started from, on the client it was routed to, so a walk cannot wander into a
			// volume this tool was never granted.
			let outcome = tools::walk(storage, &uri, depth, MAX_PENDING, MAX_ENTRIES, |visit: Visit<'_>| {
				let is_dir = visit.entry.r#type == FileType::Dir;
				let kind_ok = match want {
					Want::Any => true,
					Want::Files => !is_dir,
					Want::Dirs => is_dir,
				};
				let name_ok = pattern.as_ref().is_none_or(|pattern| glob_match(pattern, visit.entry.name.as_bytes()));
				let size_ok = smaller.is_none_or(|limit| visit.entry.size < limit) && larger.is_none_or(|limit| visit.entry.size > limit);
				let time_ok = newer.is_none_or(|since| visit.entry.mtime > since);
				if kind_ok && name_ok && size_ok && time_ok {
					print(visit.path.as_bytes());
					print(b"\n");
				}
				Step::Continue
			});
			match outcome {
				Ok(()) => {}
				Err(tools::WalkError::Unreadable) => eprint(b"find: some directories could not be read\n"),
				Err(tools::WalkError::Bounded) => eprint(b"find: the tree is wider than this walk allows\n"),
				Err(tools::WalkError::OutOfMemory) => eprint(b"find: out of memory\n"),
			}
		}
	}
	exit();
}

unsafe fn usage() {
	unsafe { eprint(b"find: usage: find [path...] [--name GLOB] [--type f|d] [--depth N] [--smaller N] [--larger N] [--newer SECS]\n") };
}

fn size_or_die(value: &[u8]) -> u64 {
	match parse_size(value) {
		Some(value) => value,
		None => unsafe {
			eprint(b"find: not a size\n");
			exit()
		},
	}
}

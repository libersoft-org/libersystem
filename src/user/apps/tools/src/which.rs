// which - resolve command names to the exact artifact each one would launch.
//
// It answers from the INHERITED PATH: the launch context carries an immutable snapshot of the
// session's variable table, so `which` reads `PATH` without holding a session capability it could
// use to change it. That separation is the point of the context - a child reads what it inherited
// and cannot alter what its parent or its session will see.
//
// The name it looks for is the canonical one: `ping` is `ping.lsexe`, and the one-final-suffix rule
// (`logic::executable`) decides that rather than this tool guessing. It reports the artifact's URI
// and never launches anything.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::LaunchContext;
use rt::*;
use service_logic::executable;
use tools::split_args;
use volume_client::VolumeClient;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		inherit_stdout(bootstrap);
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let arguments: Vec<u8> = context.arguments.clone().into_bytes();
		let mut volumes: CapSet = recv_caps(bootstrap);
		let system: u64 = volumes.take(CAP_SYSTEM);
		let media: u64 = volumes.take(CAP_MEDIA);
		let iso: u64 = volumes.take(CAP_ISO);
		let udf: u64 = volumes.take(CAP_UDF);
		let usb: u64 = volumes.take(CAP_USB);
		let _ = volumes.take(CAP_TMP);
		// The inherited PATH, or nothing. An EMPTY path is not the same as a missing one and both
		// resolve nothing, which is the honest answer: a tool that fell back to a built-in
		// directory would report an artifact the launcher would not have used.
		let search: &str = context.environment.iter().find(|variable| variable.name == "PATH").map(|variable| variable.value.as_str()).unwrap_or("");
		let mut found_all = true;
		let mut asked = false;
		for name in split_args(&arguments) {
			asked = true;
			if !resolve(name, search, context.cwd.as_str(), system, media, iso, udf, usb) {
				found_all = false;
			}
		}
		if !asked {
			eprint(b"which: usage: which <name> [name...]\n");
			exit();
		}
		// A name that did not resolve was reported on stderr as it was found; there is no exit
		// status to carry it yet (P02M0102 adds one), so the diagnostic is the whole answer.
		let _ = found_all;
	}
	exit();
}

// Print where one command name would come from. Returns whether it was found.
unsafe fn resolve(name: &[u8], search: &str, cwd: &str, system: u64, media: u64, iso: u64, udf: u64, usb: u64) -> bool {
	unsafe {
		let Ok(name) = core::str::from_utf8(name) else {
			eprint(b"which: name is not text\n");
			return false;
		};
		// AN EXPLICIT PATH IS NOT SEARCHED FOR. `which vol://system/bin/ls.lsexe` asks about that
		// artifact, and answering with something from PATH would name a different program.
		if let Some((full, _)) = executable::explicit_path(name) {
			let client = storage_proto::path::volume_client(cwd, full.as_bytes(), system, media, iso, udf, usb, storage_proto::path::NOT_GRANTED, storage_proto::path::NOT_GRANTED);
			if exists(client, full) {
				print(full.as_bytes());
				print(b"\n");
				return true;
			}
			report_missing(name);
			return false;
		}
		let Some(candidates) = executable::launch_candidates(name) else {
			// Not a legal command name at all - a different answer from "not found", and one the
			// caller can act on: no directory would ever hold it.
			eprint(b"which: ");
			eprint(name.as_bytes());
			eprint(b": not a command name\n");
			return false;
		};
		// Every directory in PATH, in order, and the first hit wins - the same order the launcher
		// searches, because answering with a later one would name a program that never runs.
		for directory in executable::path_entries(search) {
			for candidate in &candidates {
				let mut full = String::new();
				if full.try_reserve_exact(directory.len() + 1 + candidate.len()).is_err() {
					eprint(b"which: out of memory\n");
					return false;
				}
				full.push_str(directory);
				if !full.ends_with('/') {
					full.push('/');
				}
				full.push_str(candidate);
				let client = storage_proto::path::volume_client(cwd, full.as_bytes(), system, media, iso, udf, usb, storage_proto::path::NOT_GRANTED, storage_proto::path::NOT_GRANTED);
				if exists(client, &full) {
					print(full.as_bytes());
					print(b"\n");
					return true;
				}
				// The candidate that did not resolve, and whether a volume was even reachable for
				// it: "not found" over a path this tool could not ask about is a different fact
				// from "not found" over one it did.
				eprint(b"which: tried ");
				eprint(full.as_bytes());
				eprint(if client == 0 { b" (no volume)\n" } else { b" (absent)\n" });
			}
		}
		report_missing(name);
		// THE PATH IT SEARCHED, because "not found" alone cannot be acted on: an empty inherited
		// PATH and a name that is genuinely absent produce the same message otherwise, and they
		// want different fixes.
		eprint(b"which: searched PATH=");
		eprint(search.as_bytes());
		eprint(b"\n");
		false
	}
}

unsafe fn report_missing(name: &str) {
	unsafe {
		eprint(b"which: ");
		eprint(name.as_bytes());
		eprint(b": not found\n");
	}
}

// Whether the artifact is there, asked with `stat` rather than by listing its directory: `which`
// wants one name and has no use for the rest, and a directory it may not list is one it can still
// be told about a single file in.
unsafe fn exists(storage: u64, path: &str) -> bool {
	{
		if storage == 0 {
			return false;
		}
		let mut client = VolumeClient::new(storage);
		matches!(client.stat(path), Some(Ok(_)))
	}
}

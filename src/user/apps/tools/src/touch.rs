// touch - stamp a file's modification time, creating it when asked, as its own sandboxed ELF.
//
// THE TIME COMES FROM A GRANTED CLOCK, not from the filesystem. `touch` holds a TimeService client
// and passes the UTC instant it reads to StorageService, so the stamp is a decision made where the
// clock is rather than a guess made where the bytes are. A caller that wants a particular time says
// so with `--date <unix-seconds>`, and then no clock is consulted at all.
//
// `--no-create` is the difference between "make sure this exists" and "note that this changed", and
// the two are different requests: without it a missing file is created, with it a missing file is
// an error and nothing is written.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, classify, parse_u64};
use proto::system::LaunchContext;
use rt::*;
use time_client::TimeClient;
use tools::{VolumeSet, split_args};
use volume_client::VolumeClient;

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
		// The clock, granted separately from the volumes: a tool that may write a file is not
		// thereby a tool that may read the wall clock, and the manifest says both.
		let clock: u64 = recv_tagged(bootstrap, &mut buf, b"TIME").unwrap_or(0);
		let cwd: String = context.cwd.clone();

		let mut create = true;
		let mut at: Option<u64> = None;
		let mut paths: Vec<&[u8]> = Vec::new();
		let mut expect = false;
		for word in split_args(&arguments) {
			if expect {
				let Some(seconds) = parse_u64(word) else {
					eprint(b"touch: not a timestamp\n");
					exit();
				};
				at = Some(seconds);
				expect = false;
				continue;
			}
			match classify(word) {
				Arg::Long(b"no-create", None) => create = false,
				Arg::Long(b"date", Some(value)) => match parse_u64(value) {
					Some(seconds) => at = Some(seconds),
					None => {
						eprint(b"touch: not a timestamp\n");
						exit();
					}
				},
				Arg::Long(b"date", None) => expect = true,
				Arg::Short(b'c') => create = false,
				Arg::Short(b'd') => expect = true,
				Arg::Value(value) => {
					if paths.try_reserve(1).is_err() {
						eprint(b"touch: out of memory\n");
						exit();
					}
					paths.push(value);
				}
				_ => {
					eprint(b"touch: usage: touch [-c] [-d unix-seconds] <path> [path...]\n");
					exit();
				}
			}
		}
		if paths.is_empty() || expect {
			eprint(b"touch: usage: touch [-c] [-d unix-seconds] <path> [path...]\n");
			exit();
		}
		// Read ONCE, before any file is touched, so a set of files touched together carries one
		// timestamp rather than a spread of them.
		let stamp: u64 = match at {
			Some(seconds) => seconds,
			None => match TimeClient::new(clock).now() {
				Some(Ok(now)) => now.unix_secs,
				// NO CLOCK IS SAID, not guessed: zero tells the service to use its own, and the
				// caller is told that the stamp is not the one it asked for.
				_ => {
					eprint(b"touch: no clock; the volume will stamp its own time\n");
					0
				}
			},
		};
		for argument in &paths {
			let Some(uri) = storage_proto::path::resolve(&cwd, argument) else {
				eprint(b"touch: invalid path\n");
				continue;
			};
			let storage: u64 = volumes.client_for(&cwd, argument);
			if storage == 0 {
				eprint(b"touch: no volume\n");
				continue;
			}
			match VolumeClient::new(storage).touch(&uri, create, stamp) {
				Some(Ok(())) => {}
				_ => {
					eprint(b"touch: cannot touch ");
					eprint(uri.as_bytes());
					eprint(b"\n");
				}
			}
		}
	}
	exit();
}

// kill - signal a job this session owns, run as its own sandboxed ELF.
//
// THERE IS NO AMBIENT PID NAMESPACE HERE, and this tool does not pretend otherwise: it takes the
// session's small job ids - the ones `jobs` prints - and asks SessionService to act. The session
// owns the Process handle and keeps it; `kill` never receives one, so a tool that may ask for a job
// to be stopped cannot do anything else to it. That is the whole difference between this and a
// POSIX `kill`, where authority comes from a numeric namespace anybody can name into.
//
// Signals are named, not numbered. `kill -9` is a convention whose meaning a mistyped digit
// changes; `kill --kill 2` says which job and which signal.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, classify, parse_u64};
use proto::system::{JobSignalKind, LaunchContext};
use rt::*;
use session_client::SessionClient;
use tools::split_args;

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
		let session: u64 = recv_tagged(bootstrap, &mut buf, b"SESSION").unwrap_or(0);
		if session == 0 {
			eprint(b"kill: no session\n");
			exit();
		}

		let mut signal = JobSignalKind::Term;
		let mut ids: Vec<u32> = Vec::new();
		for word in split_args(&arguments) {
			match classify(word) {
				Arg::Long(b"term", None) => signal = JobSignalKind::Term,
				Arg::Long(b"kill", None) => signal = JobSignalKind::Kill,
				Arg::Long(b"interrupt", None) => signal = JobSignalKind::Interrupt,
				Arg::Long(b"stop", None) => signal = JobSignalKind::Stop,
				Arg::Long(b"continue", None) => signal = JobSignalKind::Cont,
				Arg::Value(value) => {
					// `%1` is how a job is named where a pid would be, and it is accepted so a
					// caller can write what `jobs` prints.
					let digits = value.strip_prefix(b"%").unwrap_or(value);
					let Some(id) = parse_u64(digits).and_then(|id| u32::try_from(id).ok()) else {
						eprint(b"kill: not a job id\n");
						exit();
					};
					if ids.try_reserve(1).is_err() {
						eprint(b"kill: out of memory\n");
						exit();
					}
					ids.push(id);
				}
				_ => {
					usage();
					exit();
				}
			}
		}
		if ids.is_empty() {
			usage();
			exit();
		}
		let mut client = SessionClient::new(session);
		for id in ids {
			match client.job_signal(id, signal) {
				Some(Ok(info)) => {
					let mut line = String::new();
					line.push_str(match signal {
						JobSignalKind::Term => "terminated",
						JobSignalKind::Kill => "killed",
						JobSignalKind::Interrupt => "interrupted",
						JobSignalKind::Stop => "stopped",
						JobSignalKind::Cont => "continued",
					});
					line.push_str(" job ");
					tools::push_decimal(&mut line, info.id as u64);
					line.push(' ');
					line.push_str(&info.name);
					line.push('\n');
					print(line.as_bytes());
				}
				// A job that is not this session's answers exactly like one that never existed,
				// which is the session's decision rather than this tool's: confirming that some
				// OTHER session has a job with that id would be a fact this caller is not entitled
				// to.
				_ => {
					eprint(b"kill: no such job\n");
				}
			}
		}
	}
	exit();
}

unsafe fn usage() {
	unsafe { eprint(b"kill: usage: kill [--term|--kill|--interrupt|--stop|--continue] <job-id> [job-id...]\n") };
}

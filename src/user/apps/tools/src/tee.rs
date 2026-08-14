// tee - copy stdin to stdout and to files at the same time.
//
// THE FAN-OUT CASE, which is the one this milestone wanted a tool for. Every other stream tool has
// one input and one output; `tee` has one input and several outputs, so it is where the questions
// about a bounded stream stop being theoretical: it blocks on whichever consumer is slowest, and
// the block IS the backpressure - a `tee` that buffered instead would turn a slow reader into
// unbounded memory in the middle of a pipeline.
//
// FILES ARE TRANSACTIONAL, exactly as `redirect_out` makes them. The destinations are staged and
// published only when the input ends NORMALLY, so a producer that faults or a pipeline that is
// interrupted leaves every destination as it was rather than half-written. `tee` differs from
// `redirect_out` only in also passing the bytes on, and it deliberately does not share code with
// it: `redirect_out` exists to be the shell's expansion of `>` and has no stdout of its own.
//
// A DESTINATION THAT FAILS DOES NOT STOP THE PIPELINE. `tee log | grep error` is a line the user
// typed to see the output; a full volume is worth reporting on stderr and worth aborting that
// destination for, but not worth killing the stream the rest of the line is built on. Every
// destination that failed is named, and the exit says whether any did.
//
// STDOUT CLOSING DOES stop it, which is the opposite decision for the opposite reason: with no
// consumer left, reading the rest of the input produces nothing anybody asked for. The
// destinations opened so far are still published, because they got every byte that was read.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, classify};
use proto::system::{LaunchContext, WriterMode};
use rt::*;
use storage_proto::path;
use tools::{Source, VolumeSet, Window, split_args};
use volume_client::{VolumeClient, WriterClient};

// At most this many destinations in one run. A bound rather than a limit anybody will reach: the
// point is that a caller cannot ask one launch for an unbounded number of writer sessions.
const MAX_TARGETS: usize = 8;

// One destination, and whether it is still taking bytes.
struct Target {
	uri: String,
	writer: WriterClient,
	failed: bool,
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

		let mut append = false;
		let mut paths: Vec<&[u8]> = Vec::new();
		for word in split_args(&arguments) {
			match classify(word) {
				Arg::Long(b"append", None) => append = true,
				Arg::Short(b'a') => append = true,
				Arg::Value(value) => {
					if paths.len() >= MAX_TARGETS {
						eprint(b"tee: too many destinations\n");
						exit();
					}
					if paths.try_reserve(1).is_err() {
						eprint(b"tee: out of memory\n");
						exit();
					}
					paths.push(value);
				}
				_ => {
					eprint(b"tee: usage: tee [-a] <path> [path...]\n");
					exit();
				}
			}
		}
		// NO STDIN IS NOT A USAGE ERROR, it is the whole reason this tool exists. `tee` with no
		// input stream was launched as the first stage of something, which is a line that cannot
		// mean anything - it would copy nothing to everywhere.
		let Some(mut source) = Source::from_stdin() else {
			eprint(b"tee: nothing is wired to this stage's input\n");
			exit();
		};
		// EVERY DESTINATION IS OPENED BEFORE A BYTE IS READ, for `redirect_out`'s reason: a
		// destination that cannot be written should fail while the producer is still at its first
		// write, not after its output has been consumed and thrown away.
		let mut targets: Vec<Target> = Vec::new();
		let mode: WriterMode = if append { WriterMode::Append } else { WriterMode::Replace };
		let mut refused = false;
		for argument in &paths {
			let Some(uri) = path::resolve(&cwd, argument) else {
				eprint(b"tee: ");
				eprint(argument);
				eprint(b": invalid path\n");
				refused = true;
				continue;
			};
			let storage: u64 = volumes.client_for(&cwd, argument);
			let mut client = VolumeClient::new(storage);
			match client.open_writer(&uri, mode) {
				Some(Ok(writer)) => {
					if targets.try_reserve(1).is_err() {
						eprint(b"tee: out of memory\n");
						exit();
					}
					targets.push(Target { uri, writer, failed: false });
				}
				_ => {
					eprint(b"tee: ");
					eprint(uri.as_bytes());
					eprint(b": cannot open for writing\n");
					refused = true;
				}
			}
		}
		let outcome = pump(&mut source, &mut targets);
		// The destinations are published only when the INPUT ended normally. A producer that
		// failed mid-stream leaves every one of them exactly as it was - that is the difference
		// between a `tee` that records a run and a `tee` that records half of one as if it were
		// whole.
		let publish: bool = outcome != Outcome::InputFailed;
		for target in &mut targets {
			if publish && !target.failed {
				if !matches!(target.writer.commit(), Some(Ok(_))) {
					eprint(b"tee: ");
					eprint(target.uri.as_bytes());
					eprint(b": could not publish\n");
					refused = true;
					let _ = target.writer.abort();
				}
			} else {
				let _ = target.writer.abort();
			}
			close(target.writer.handle());
		}
		if outcome == Outcome::InputFailed {
			eprint(b"tee: the input stream failed; no destination was published\n");
			exit();
		}
		if refused || targets.iter().any(|target| target.failed) {
			exit();
		}
	}
	exit();
}

// How the copy ended.
#[derive(PartialEq, Eq)]
enum Outcome {
	// The input ended normally, which is the only outcome that publishes.
	Done,
	// The consumer on stdout went away. The destinations still got every byte that was read.
	ConsumerGone,
	// The producer reported it could not finish.
	InputFailed,
}

// Copy the input to stdout and to every live destination, one window at a time.
//
// A WINDOW GOES EVERYWHERE OR THE DESTINATION IS OUT. A destination that refuses a write is marked
// failed and skipped from then on, rather than being retried per window - a half-written
// destination that then resumes would hold a file with a hole in it, which is the one result worse
// than not having the file.
unsafe fn pump(source: &mut Source, targets: &mut [Target]) -> Outcome {
	unsafe {
		loop {
			let window = match source.next() {
				Window::Bytes(bytes) => bytes,
				Window::End => return Outcome::Done,
				Window::Failed => return Outcome::InputFailed,
			};
			for target in targets.iter_mut() {
				if target.failed {
					continue;
				}
				if !matches!(target.writer.write(&window), Some(Ok(_))) {
					eprint(b"tee: ");
					eprint(target.uri.as_bytes());
					eprint(b": write failed\n");
					target.failed = true;
				}
			}
			// STDOUT LAST, because it is the one that blocks. Writing the destinations first means
			// a stalled consumer holds up the staging by exactly one window rather than getting
			// ahead of it - and the block is the backpressure that keeps this stage from becoming
			// the place where a pipeline's memory goes.
			if !write_stdout(&window) {
				return Outcome::ConsumerGone;
			}
		}
	}
}

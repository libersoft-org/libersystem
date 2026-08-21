// redirect_out - the write half of shell output redirection, as a pipeline stage.
//
// `cmd > path` becomes `cmd | redirect_out path`, and `cmd >> path` passes `--append`. See
// `redirect_in` for why redirection is a stage rather than a branch in the shell or the broker.
//
// THE DESTINATION IS PUBLISHED ONLY ON A NORMAL CLOSE, which is the property this program exists
// for. It writes through StorageService's transactional writer: every byte is staged and nothing is
// visible under the destination's name until `commit`. So
//
//   - the producer finishing normally closes the pipe, this reads end-of-stream and commits;
//   - the producer being killed, faulting, or the pipeline being interrupted tears the channel down
//     without a clean close, this exits without committing, and the writer session dying aborts -
//     the previous contents of the destination are exactly as they were;
//   - a staging or storage failure aborts explicitly and says so, rather than leaving a half-written
//     file under the name the user redirected to.
//
// A CLEAN NON-ZERO EXIT STILL PUBLISHES, deliberately. `grep` with no match exits non-zero and
// produces no output, and a redirection that threw that away would make `grep x < a > b` leave `b`
// holding whatever it held before - which is not what the user asked for. The producer's exit code
// is the pipeline's business; this stage's business is whether the STREAM ended normally.
//
// Append is the writer's own append mode, not a read-modify-write: reading the destination back to
// rewrite it would need read authority this does not have, would lose the transaction, and would
// race any other writer.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use proto::system::{LaunchContext, WriterMode};
use rt::*;
use storage_proto::path;
use volume_client::VolumeClient;

// One receive per chunk. This is the shell's relay size - what the producers on the other end of
// the pipe send - and it is also the writer's own bound, so a full window is exactly one request.
const CHUNK: usize = volume_client::WRITER_CHUNK;

// The flag the shell passes for `>>`. A word rather than a mode byte in the argument string,
// because the launch contract carries arguments as text and a tool that parses its own flags is
// one whose behaviour can be read off the command line.
const APPEND_FLAG: &[u8] = b"--append";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		inherit_stdout(bootstrap);
		let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
			Some(context) => context,
			None => exit(),
		};
		let mut volumes: CapSet = recv_caps(bootstrap);
		let system: u64 = volumes.take(CAP_SYSTEM);
		let media: u64 = volumes.take(CAP_MEDIA);
		let iso: u64 = volumes.take(CAP_ISO);
		let udf: u64 = volumes.take(CAP_UDF);
		let usb: u64 = volumes.take(CAP_USB);
		// `[--append] <path>`: the flag first, the path last, and nothing else accepted. A
		// redirection target is one path, so anything more is the shell having built the line
		// wrongly rather than a user having typed something interesting.
		let arg: &[u8] = context.arguments.as_bytes();
		let mut append: bool = false;
		let mut target: &[u8] = b"";
		for word in arg.split(|byte| *byte == b' ').filter(|word| !word.is_empty()) {
			if word == APPEND_FLAG {
				append = true;
			} else if target.is_empty() {
				target = word;
			} else {
				eprint(b"redirect_out: one destination path only\n");
				exit();
			}
		}
		if target.is_empty() {
			eprint(b"redirect_out: no destination path\n");
			exit();
		}
		let cwd_str: &str = &context.cwd;
		let uri: String = match path::resolve(cwd_str, target) {
			Some(u) => u,
			None => {
				eprint(b"redirect_out: ");
				eprint(target);
				eprint(b": invalid path\n");
				exit();
			}
		};
		let storage: u64 = path::volume_client(cwd_str, target, system, media, iso, udf, usb, path::NOT_GRANTED, path::NOT_GRANTED);
		pump(storage, &uri, append);
	}
	exit();
}

// Stage everything that arrives on stdin, and publish it only if the stream ended normally.
unsafe fn pump(storage: u64, uri: &str, append: bool) {
	unsafe {
		let mut client = VolumeClient::new(storage);
		let mode: WriterMode = if append { WriterMode::Append } else { WriterMode::Replace };
		// OPENED BEFORE A BYTE IS READ, so a destination that cannot be written fails while the
		// producer is still at its first write - and the producer's own pipe breaking is how it
		// learns. Opening lazily on the first chunk would consume the producer's output and then
		// discover there was nowhere to put it.
		let mut writer = match client.open_writer(uri, mode) {
			Some(Ok(writer)) => writer,
			_ => {
				eprint(b"redirect_out: ");
				eprint(uri.as_bytes());
				eprint(b": cannot open for writing\n");
				return;
			}
		};
		let input: u64 = stdin();
		if input == 0 {
			// Nothing is wired to this stage's input, which for a redirection means the producer
			// never existed. An empty commit would TRUNCATE the destination on the strength of a
			// launch that did not happen.
			eprint(b"redirect_out: no input stream\n");
			let _ = writer.abort();
			close(writer.handle());
			return;
		}
		let mut buffer: [u8; CHUNK] = [0u8; CHUNK];
		loop {
			match recv_blocking(input, &mut buffer) {
				// END OF STREAM: the producer closed its end, which is the only thing that
				// publishes.
				Received::Closed => break,
				Received::Message { len: 0, .. } => continue,
				Received::Message { len, .. } => {
					match writer.write(&buffer[..len]) {
						Some(Ok(_)) => {}
						_ => {
							// The staging failed - out of space, a backend that stopped
							// answering. Abort explicitly and leave the destination alone.
							eprint(b"redirect_out: ");
							eprint(uri.as_bytes());
							eprint(b": write failed\n");
							let _ = writer.abort();
							close(writer.handle());
							return;
						}
					}
				}
			}
		}
		match writer.commit() {
			Some(Ok(_)) => {}
			_ => {
				eprint(b"redirect_out: ");
				eprint(uri.as_bytes());
				eprint(b": could not publish\n");
				let _ = writer.abort();
			}
		}
		close(writer.handle());
	}
}

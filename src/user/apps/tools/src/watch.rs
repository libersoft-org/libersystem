// watch - run a governed command over and over and show its latest output.
//
// IT LAUNCHES THROUGH PERMISSIONMANAGER, which is the whole reason this is a program and not a
// shell loop. The command it watches is started under the command's OWN manifest, so `watch ls`
// gives `ls` exactly what `ls` is granted and gives `watch` nothing it did not already hold -
// there is no path by which watching a command lends it authority, or borrows its.
//
// `watch` itself is granted one capability: a PermissionManager client. It holds no volume, no
// network and no session, which is what makes the previous paragraph true rather than merely
// intended: it could not widen a child's world if it wanted to, because it has nothing to widen it
// with.
//
// ONE CHILD AT A TIME. The interval is when the next run STARTS, not how long to wait after the
// last one ended, and a run that overruns its interval simply delays the next - it is never
// overlapped. A `watch` that started a second copy of a slow command would turn a display into a
// fork bomb at exactly the moment the machine was already busy.
//
// THE OUTPUT IS BOUNDED and the newest wins: a command that prints more than the screen can hold
// has its tail kept, because the point of watching is to see what it says NOW. An unbounded
// capture would make this tool's memory a function of how long it has been left running.
//
// TERMINAL MODES ARE RESTORED ON EVERY EXIT PATH, including Ctrl+C, which is why the interrupt is
// caught rather than left to kill the process: a tool that leaves the terminal in the alternate
// screen has broken the shell that comes back.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use cli::{Arg, classify, parse_u64};
use proto::system::LaunchContext;
use rt::*;
use security_client::PermissionClient;
use tools::{push_decimal, split_args};

// The default interval, in seconds - the one every other `watch` uses, and the one a person means
// when they do not say.
const DEFAULT_INTERVAL: u64 = 2;
// The most output one run may keep. Two hundred lines of a wide terminal, which is more than a
// screen; past it the TAIL is what survives.
const MAX_CAPTURE: usize = 64 * 1024;
// One receive from the child's output channel.
const CHUNK: usize = 4096;
// How long a single run may take before it is abandoned, in intervals. A command that never exits
// would otherwise stop the display forever with nothing said about why.
const RUN_TIMEOUT_INTERVALS: u64 = 5;

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
		let cwd: String = context.cwd.clone();
		let permsvc: u64 = recv_tagged(bootstrap, &mut buf, b"PERMISSION").unwrap_or_else(|| exit());

		let mut interval: u64 = DEFAULT_INTERVAL;
		let mut expect = false;
		let mut command: Vec<&[u8]> = Vec::new();
		for word in split_args(&arguments) {
			if expect {
				let Some(seconds) = parse_u64(word).filter(|seconds| *seconds > 0) else {
					eprint(b"watch: the interval is a whole number of seconds, at least one\n");
					exit();
				};
				interval = seconds;
				expect = false;
				continue;
			}
			// EVERY WORD AFTER THE COMMAND BELONGS TO THE COMMAND, including one that looks like a
			// flag. `watch ls -l` must pass `-l` to `ls`, and a parser that kept claiming options
			// would quietly eat it - which is the classic way a wrapper changes the command it was
			// asked to run.
			if !command.is_empty() {
				if command.try_reserve(1).is_err() {
					eprint(b"watch: out of memory\n");
					exit();
				}
				command.push(word);
				continue;
			}
			match classify(word) {
				Arg::Long(b"interval", Some(value)) => match parse_u64(value).filter(|seconds| *seconds > 0) {
					Some(seconds) => interval = seconds,
					None => {
						eprint(b"watch: the interval is a whole number of seconds, at least one\n");
						exit();
					}
				},
				Arg::Long(b"interval", None) => expect = true,
				Arg::Short(b'n') => expect = true,
				Arg::Value(value) => {
					if command.try_reserve(1).is_err() {
						eprint(b"watch: out of memory\n");
						exit();
					}
					command.push(value);
				}
				_ => {
					eprint(b"watch: usage: watch [-n seconds] <command> [argument...]\n");
					exit();
				}
			}
		}
		if command.is_empty() || expect {
			eprint(b"watch: usage: watch [-n seconds] <command> [argument...]\n");
			exit();
		}
		let Ok(name) = core::str::from_utf8(command[0]) else {
			eprint(b"watch: the command name is not text\n");
			exit();
		};
		let mut args: Vec<u8> = Vec::new();
		for word in command.iter().skip(1) {
			if !args.is_empty() {
				args.push(b' ');
			}
			args.extend_from_slice(word);
		}
		let Ok(args) = String::from_utf8(args) else {
			eprint(b"watch: the arguments are not text\n");
			exit();
		};

		// ARMED BEFORE THE ALTERNATE SCREEN IS ENTERED, so there is no window in which a Ctrl+C
		// kills the process with the terminal switched.
		catch_interrupt();
		print(b"\x1b[?1049h");
		loop {
			let started: u64 = clock_ns();
			let (output, status) = run_once(permsvc, name, &args, &cwd, interval);
			render(name, &args, interval, &status, &output);
			if interrupted() {
				break;
			}
			// MONOTONIC AND FROM THE START OF THE RUN, so the display ticks at the interval rather
			// than at the interval plus however long the command took. A wall clock would also make
			// a clock correction look like a missed refresh.
			let wake: u64 = started.saturating_add(interval.saturating_mul(1_000_000_000));
			let mut stop = false;
			while clock_ns() < wake {
				if interrupted() {
					stop = true;
					break;
				}
				// Short steps, so the interrupt is noticed promptly rather than at the end of the
				// interval - a `watch -n 60` that took a minute to quit would feel broken.
				wait(permsvc, clock() + 5);
			}
			if stop {
				break;
			}
		}
		// EVERY EXIT PATH LEAVES THE TERMINAL AS IT WAS FOUND.
		print(b"\x1b[?1049l");
	}
	exit();
}

// How one run ended, as the line the header shows.
struct Status {
	text: Vec<u8>,
}

// Launch the command once, drain what it prints, and wait for it to finish.
//
// The child's output goes to a CHANNEL rather than to this program's terminal, which is what lets
// the screen be redrawn whole instead of scrolling: nothing the child writes reaches the display
// until this decides where to put it.
unsafe fn run_once(permsvc: u64, name: &str, args: &str, cwd: &str, interval: u64) -> (Vec<u8>, Status) {
	unsafe {
		let Some((read_end, write_end)) = channel() else {
			return (Vec::new(), Status { text: b"could not make an output channel".to_vec() });
		};
		// THE CONCRETE CLIENT, not a hand-rolled transport: the shared provider is what every other
		// tool reaches this service through, and a second way of speaking it is a second thing that
		// can drift from the contract.
		let mut client = PermissionClient::new(permsvc);
		let task: u64 = match client.run(name, args, cwd, &Vec::new(), &write_end) {
			Some(Ok(started)) => started.task,
			// The broker closes the handle it was given on every refusal, so `write_end` is spent
			// either way and only the read end is left here.
			_ => {
				close(read_end);
				return (Vec::new(), Status { text: b"could not start".to_vec() });
			}
		};
		let mut output: Vec<u8> = Vec::new();
		let mut buffer: [u8; CHUNK] = [0u8; CHUNK];
		let deadline: u64 = clock_ns().saturating_add(interval.saturating_mul(RUN_TIMEOUT_INTERVALS).saturating_mul(1_000_000_000));
		let mut timed_out = false;
		loop {
			match try_recv(read_end, &mut buffer) {
				Polled::Message { len, .. } => {
					if output.try_reserve(len).is_ok() {
						output.extend_from_slice(&buffer[..len]);
					}
					// THE TAIL SURVIVES, not the head: what a person watching wants is what the
					// command says now, and a capture that filled up and then ignored everything
					// after it would freeze the display on the first screenful.
					if output.len() > MAX_CAPTURE {
						let drop_to = output.len() - MAX_CAPTURE;
						output.drain(..drop_to);
					}
					continue;
				}
				Polled::Closed => break,
				Polled::Empty => {}
			}
			if interrupted() || clock_ns() >= deadline {
				timed_out = clock_ns() >= deadline;
				break;
			}
			wait(read_end, clock() + 5);
		}
		close(read_end);
		// The child's own answer, which is what `completion_valid` exists to distinguish from a
		// process that never got to give one.
		let stats = process_stats(task);
		let mut text: Vec<u8> = Vec::new();
		if timed_out {
			text.extend_from_slice(b"still running");
			// LEFT RUNNING RATHER THAN KILLED. `watch` was given no authority over the command
			// beyond starting it, and a display tool that could kill what it displays is a
			// different tool - the person who typed it can interrupt it themselves.
		} else {
			match stats {
				Some(stats) if stats.state == PROC_STATE_FAILED => text.extend_from_slice(b"killed or faulted"),
				Some(stats) if stats.completion_valid != 0 => {
					text.extend_from_slice(b"exit ");
					let mut rendered = String::new();
					push_decimal(&mut rendered, stats.completion);
					text.extend_from_slice(rendered.as_bytes());
				}
				Some(_) => text.extend_from_slice(b"running"),
				None => text.extend_from_slice(b"gone"),
			}
		}
		close(task);
		(output, Status { text })
	}
}

// Redraw the screen: a header naming what is being watched and how the last run ended, then the
// output.
//
// CLEARED AND REDRAWN WHOLE, rather than scrolled, which is what the alternate screen is for: a
// display that appended would make every refresh push the previous one up and the thing being
// watched would never sit still.
unsafe fn render(name: &str, args: &str, interval: u64, status: &Status, output: &[u8]) {
	unsafe {
		print(b"\x1b[H\x1b[2J");
		print(b"\x1b[7m every ");
		let mut header = String::new();
		push_decimal(&mut header, interval);
		print(header.as_bytes());
		print(b"s: ");
		print(name.as_bytes());
		if !args.is_empty() {
			print(b" ");
			print(args.as_bytes());
		}
		print(b"  [");
		print(&status.text);
		print(b"]\x1b[0m\n");
		print(output);
		if output.last() != Some(&b'\n') {
			print(b"\n");
		}
	}
}

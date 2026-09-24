// midihold - the MIDI gate's second receiver, on endpoint 1. DEVELOPMENT-ONLY.
//
//   midihold hold N   keep the receiver for N seconds, then stop it
//   midihold dup      send THIS PROGRAM'S RECEIVER down stdout and exit without stopping it

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::{LaunchContext, midi_input};
use rt::*;

// ON THE TERMINAL, WHICH IS STDERR: in a pipeline this program's stdout is the pipe to the next stage,
// and a line printed there is read as data and never seen.
fn fail(line: &[u8]) -> ! {
	eprint(b"midihold: FAIL ");
	eprint(line);
	eprint(b"\n");
	exit();
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let input = recv_tagged(bootstrap, &mut buf, b"MIDIINPUT").unwrap_or(0);
	if input == 0 {
		fail(b"the receiver was not delivered");
	}
	let client = || midi_input::Client::with_deadline(ChannelTransport { chan: input }, clock() + 500);
	if !matches!(client().endpoint(), Some(Ok(endpoint)) if endpoint.id.endpoint == 1 && endpoint.receiving) {
		fail(b"the receiver is not on endpoint 1");
	}
	let mut words = context.arguments.split(' ');
	match words.next().unwrap_or("") {
		"hold" => {
			let seconds: u64 = words.next().and_then(|n| n.parse().ok()).unwrap_or(3);
			sleep_until(clock() + seconds * 100);
			let _ = client().stop();
			eprint(b"midihold: held endpoint 1 and stopped\n");
		}
		"dup" => {
			if !send_caps_blocking(stdout(), b"RECEIVER", &[input]) {
				fail(b"the receiver could not be sent");
			}
			eprint(b"midihold: sent its receiver, and exits\n");
		}
		_ => fail(b"usage: midihold hold N | dup"),
	}
	exit();
}

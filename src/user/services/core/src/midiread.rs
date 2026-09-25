// midiread - the MIDI gate's inventory-only client. DEVELOPMENT-ONLY.
//
// It holds `midi` and nothing else, and proves what that is: the endpoints are listed with their protocol,
// direction and cables, receiving through inventory is denied, and output is unsupported.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{Error, LaunchContext, MidiDirection, MidiProtocol, midi};
use rt::*;
use services::capability_names::*;

fn fail(line: &[u8]) -> ! {
	print(b"midiread: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	if recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode).is_none() {
		exit();
	}
	let inventory = recv_tagged(bootstrap, &mut buf, CAP_MIDI).unwrap_or(0);
	if inventory == 0 {
		fail(b"the inventory grant was not delivered");
	}
	let client = || midi::Client::with_deadline(ChannelTransport { chan: inventory }, clock() + 500);
	let endpoints = match client().endpoints() {
		Some(Ok(endpoints)) => endpoints,
		_ => fail(b"the endpoints could not be listed"),
	};
	let directions: Vec<(MidiDirection, u8)> = endpoints.iter().map(|endpoint| (endpoint.direction, endpoint.cables)).collect();
	if endpoints.iter().any(|endpoint| endpoint.protocol != MidiProtocol::Midi1) || directions != [(MidiDirection::Receive, 2), (MidiDirection::Receive, 1), (MidiDirection::Transmit, 2)] {
		fail(b"the fixture's two receive endpoints and its transmit one were not listed as they are");
	}
	if !matches!(client().open(&endpoints[0].id, &MidiDirection::Receive, &MidiProtocol::Midi1), Some(Err(Error::Denied))) || !matches!(client().open(&endpoints[2].id, &MidiDirection::Transmit, &MidiProtocol::Midi1), Some(Err(Error::Denied))) {
		fail(b"inventory opened a receiver or a sender");
	}
	if !matches!(client().open(&endpoints[0].id, &MidiDirection::Transmit, &MidiProtocol::Midi1), Some(Err(Error::Invalid))) {
		fail(b"a receive endpoint was opened to send");
	}
	print(b"midiread: PASS endpoints listed with protocol, direction and cables; inventory can neither receive nor send\n");
	exit();
}

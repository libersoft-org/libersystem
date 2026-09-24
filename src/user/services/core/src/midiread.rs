// midiread - the MIDI gate's inventory-only client. DEVELOPMENT-ONLY.
//
// It holds `midi` and nothing else, and proves what that is: the endpoints are listed with their protocol,
// direction and cables, receiving through inventory is denied, and output is unsupported.

#![no_std]
#![no_main]

extern crate alloc;

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
	if endpoints.len() != 2 || endpoints.iter().any(|endpoint| endpoint.protocol != MidiProtocol::Midi1 || endpoint.direction != MidiDirection::Receive) || endpoints[0].cables != 2 || endpoints[1].cables != 1 {
		fail(b"the fixture's two receive endpoints were not listed as they are");
	}
	if !matches!(client().open(&endpoints[0].id, &MidiDirection::Receive, &MidiProtocol::Midi1), Some(Err(Error::Denied))) {
		fail(b"inventory opened a receiver");
	}
	if !matches!(client().open(&endpoints[0].id, &MidiDirection::Transmit, &MidiProtocol::Midi1), Some(Err(Error::Unsupported))) {
		fail(b"output was not refused as unsupported");
	}
	print(b"midiread: PASS endpoints listed with protocol, direction and cables; inventory cannot receive; output is unsupported\n");
	exit();
}

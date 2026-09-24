// modemhold - the modem gate's second data client. DEVELOPMENT-ONLY.
//
// It holds a data grant, like `modemcheck`, and exists because an inherited endpoint needs a client other
// than the one observing it: it activates the context, sends the context's identity and THIS PROGRAM'S
// DATA ENDPOINT down stdout, and exits - so the context's owner is gone while a copy of its endpoint lives
// on in the next stage of the pipeline.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::{ContextState, LaunchContext, modem_data};
use rt::*;

// ON THE TERMINAL, WHICH IS STDERR: in a pipeline this program's stdout is the pipe to the next stage,
// and a line printed there is read as data and never seen.
fn fail(line: &[u8]) -> ! {
	eprint(b"modemhold: FAIL ");
	eprint(line);
	eprint(b"\n");
	exit();
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	if recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode).is_none() {
		exit();
	}
	let data = recv_tagged(bootstrap, &mut buf, b"MODEMDATA").unwrap_or(0);
	if data == 0 {
		fail(b"the data grant was not delivered");
	}
	let client = || modem_data::Client::with_deadline(ChannelTransport { chan: data }, clock() + 9000);
	let modem = match client().status() {
		Some(Ok(status)) => status.id,
		_ => fail(b"the modem's status could not be read"),
	};
	let context = match client().activate(&modem) {
		Some(Ok(status)) if status.state == ContextState::Active => status.id,
		_ => fail(b"the context could not be activated"),
	};
	let Some(bytes) = context.encode_vec() else { fail(b"the context did not encode") };
	// THE ENDPOINT ITSELF GOES: the next stage of the pipeline holds it, and this program ends.
	if !send_caps_blocking(stdout(), &bytes, &[data]) {
		fail(b"the endpoint could not be sent");
	}
	eprint(b"modemhold: activated, sent its endpoint and context, and exits\n");
	exit();
}

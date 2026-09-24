// cardhold - the smart-card gate's second client. DEVELOPMENT-ONLY.
//
// It holds a grant on fixture reader A, like `cardcheck`, and exists because a queue, an inherited
// endpoint and a service killed mid-exchange each need a client other than the one observing them.
//
//   cardhold hold N   acquire slot 0, verify on the pinpad, hold it N seconds, release
//   cardhold dup      acquire slot 0, send the transaction and THIS PROGRAM'S ENDPOINT down stdout, exit
//   cardhold slow     acquire slot 0 and start an exchange the fixture was told to hold

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::{LaunchContext, PinResult, smartcard};
use rt::*;

const GET_DISCOVERY: [u8; 9] = [0x00, 0xcb, 0x3f, 0xff, 0x03, 0x5c, 0x01, 0x7e, 0x00];

// ON THE TERMINAL, WHICH IS STDERR: in a pipeline this program's stdout is the pipe to the next stage,
// and a line printed there is read as data and never seen.
fn fail(line: &[u8]) -> ! {
	eprint(b"cardhold: FAIL ");
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
	let card = recv_tagged(bootstrap, &mut buf, b"SMARTCARD").unwrap_or(0);
	if card == 0 {
		fail(b"the smart-card grant was not delivered");
	}
	let client = || smartcard::Client::with_deadline(ChannelTransport { chan: card }, clock() + 3000);
	let transaction = match client().acquire(&0, &10000, &0) {
		Some(Ok(transaction)) => transaction.id,
		_ => fail(b"slot 0 could not be acquired"),
	};
	let mut words = context.arguments.split(' ');
	match words.next().unwrap_or("") {
		"hold" => {
			if !matches!(client().verify_piv_pin(&transaction, &10000), Some(Ok(outcome)) if outcome.result == PinResult::Verified) {
				fail(b"the holder's verification did not succeed");
			}
			let seconds: u64 = words.next().and_then(|n| n.parse().ok()).unwrap_or(3);
			sleep_until(clock() + seconds * 100);
			let _ = client().release(&transaction);
			eprint(b"cardhold: held slot 0 with a verification, and released it\n");
		}
		"dup" => {
			// THE ENDPOINT ITSELF GOES: the next stage of the pipeline holds it, and this program ends.
			if !send_caps_blocking(stdout(), &transaction.to_le_bytes(), &[card]) {
				fail(b"the endpoint could not be sent");
			}
			eprint(b"cardhold: sent its endpoint and transaction, and exits\n");
		}
		"slow" => {
			eprint(b"cardhold: an exchange the fixture holds is in flight\n");
			let _ = client().exchange(&transaction, &GET_DISCOVERY);
		}
		_ => fail(b"usage: cardhold hold N | dup | slow"),
	}
	exit();
}

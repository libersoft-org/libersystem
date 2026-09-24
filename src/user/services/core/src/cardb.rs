// cardb - a client of fixture reader B, which has no pinpad. DEVELOPMENT-ONLY.
//
// Its half of the gate is the one thing a non-pinpad reader says to a request for PIN verification:
// that no trusted input exists - and that nothing else, and no other way to enter a PIN, is offered.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::{PinResult, smartcard};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let _ = recv_launch_bytes(bootstrap);
	let card = recv_tagged(bootstrap, &mut buf, b"SMARTCARD").unwrap_or(0);
	if card == 0 {
		print(b"cardb: FAIL the grant was not delivered\n");
		exit();
	}
	let client = || smartcard::Client::with_deadline(ChannelTransport { chan: card }, clock() + 3000);
	let pinpad = matches!(client().reader(), Some(Ok(reader)) if reader.pinpad);
	let transaction = match client().acquire(&0, &5000, &0) {
		Some(Ok(transaction)) => transaction.id,
		_ => {
			print(b"cardb: FAIL reader B's slot could not be acquired\n");
			exit();
		}
	};
	let unavailable = matches!(client().verify_piv_pin(&transaction, &5000), Some(Ok(outcome)) if outcome.result == PinResult::TrustedInputUnavailable);
	let _ = client().release(&transaction);
	if !pinpad && unavailable {
		print(b"cardb: PASS reader B has no usable pinpad, and verification on it is trusted-input-unavailable\n");
	} else {
		print(b"cardb: FAIL a reader without a pinpad did not refuse verification as it must\n");
	}
	exit();
}

// cardread - an ORDINARY smart-card read client, for the gate's read-only check. DEVELOPMENT-ONLY.
//
// Its grant on fixture reader A carries the read operation and nothing else: it may describe the reader
// and read its slots and events, and it cannot acquire a transaction - so it cannot verify or
// authenticate either, since both need one.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::{Error, smartcard};
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let _ = recv_launch_bytes(bootstrap);
	let card = recv_tagged(bootstrap, &mut buf, b"SMARTCARD").unwrap_or(0);
	if card == 0 {
		print(b"cardread: FAIL the read grant was not delivered\n");
		exit();
	}
	let mut client = smartcard::Client::with_deadline(ChannelTransport { chan: card }, clock() + 3000);
	let described = matches!(client.reader(), Some(Ok(_)));
	let listed = matches!(client.slots(), Some(Ok(slots)) if !slots.is_empty());
	let refused = matches!(client.acquire(&0, &0, &0), Some(Err(Error::Denied)));
	if described && listed && refused {
		print(b"cardread: PASS a read-only grant describes the reader and its slots, and cannot acquire, verify or authenticate\n");
	} else {
		print(b"cardread: FAIL a read-only grant did not behave as one\n");
	}
	exit();
}

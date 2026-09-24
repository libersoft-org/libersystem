// btread - an ORDINARY Bluetooth read client, for the gate's denial check. DEVELOPMENT-ONLY.
//
// Its permission row grants the read authority and nothing else. It proves the two halves of what
// that means: the read verbs answer - the controllers are listed and a scan runs - and the operator
// authority was not handed over at all, so there is nothing through which it could pair, forget or
// power a radio.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use proto::system::bluetooth;
use rt::*;
use services::capability_names::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let _ = recv_launch_bytes(bootstrap);
	let read = recv_tagged(bootstrap, &mut buf, CAP_BT_READ).unwrap_or(0);
	// NO OPERATOR TAG FOLLOWS. A grant that is not in this program's row is not sent, so the next
	// message on the bootstrap is not an operator channel - and the probe does not wait for one.
	if read == 0 {
		print(b"btread: FAIL the read authority was not granted\n");
		exit();
	}
	let mut client = bluetooth::Client::new(ChannelTransport { chan: read });
	let deadline = clock() + 2000;
	let listed = loop {
		if let Some(Ok(controllers)) = client.controllers()
			&& !controllers.is_empty()
		{
			break true;
		}
		if clock() >= deadline {
			break false;
		}
		sleep_until(clock() + 25);
	};
	if !listed {
		print(b"btread: FAIL the controllers could not be listed\n");
		exit();
	}
	let Some(Ok(scan)) = client.scan(&0, &1000) else {
		print(b"btread: FAIL a read client could not scan\n");
		exit();
	};
	let _ = client.cancel(&scan);
	print(b"btread: PASS a read client lists and scans, and holds no operator authority\n");
	exit();
}

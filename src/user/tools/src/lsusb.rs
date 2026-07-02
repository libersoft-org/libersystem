// lsusb - list the USB devices on the bus, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - the xHCI driver's USB bus query client - and forwards it
// the shell's stdout console and the argument string (the sub-form: "" for text or
// "json"). lsusb queries the driver's live inventory of the devices it addressed -
// hot-plugged and detached devices come and go from the list - prints each entry
// (typed to_text/to_json) to the inherited stdout, and exits.

#![no_std]
#![no_main]

extern crate alloc;

use proto::system::usb;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the sub-form ("" for text, "json" for JSON).
		let json: bool = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => &buf[..len] == b"json",
			Received::Closed => exit(),
		};
		// 3. receive the one capability the manifest grants: the USB bus query client.
		let ussvc: u64 = recv_tagged(bootstrap, &mut buf, b"USB").unwrap_or_else(|| exit());
		query_bus(ussvc, json);
	}
	exit();
}

// Query the driver's live inventory through the grant and print each device, as text
// (the default) or as a JSON array.
unsafe fn query_bus(ussvc: u64, json: bool) {
	unsafe {
		let mut client = usb::Client::new(ChannelTransport { chan: ussvc });
		match client.list() {
			Some(Ok(entries)) => {
				if json {
					print(b"[");
					let mut first: bool = true;
					for e in &entries {
						if !first {
							print(b",");
						}
						first = false;
						print(e.to_json().as_bytes());
					}
					print(b"]\n");
				} else {
					for e in &entries {
						print(e.to_text().as_bytes());
						print(b"\n");
					}
				}
			}
			Some(Err(_)) => print(b"lsusb: query error\n"),
			None => print(b"lsusb: service unavailable\n"),
		}
	}
}

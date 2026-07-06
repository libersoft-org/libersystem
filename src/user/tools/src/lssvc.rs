// lssvc - list the system services and their state, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - a ServiceManager status client (a dedicated channel, separate
// from the system graph's) - and forwards it the shell's stdout console and the argument
// string ("json" for JSON, or a name prefix such as "driver." to narrow the list; both
// may be given as "json <prefix>"). lssvc queries the supervisor's typed status view -
// every managed service with its lifecycle state, restart count, watchdog trips and last
// failure, plus the drivers DeviceManager launched (drivers are services too) - prints
// each entry to the inherited stdout, and exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::codec::JsonMode;
use proto::system::supervisor;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - "json" / "json-min" and/or a name-prefix filter.
		let args: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		let (mode, filter): (Option<JsonMode>, &[u8]) = parse_args(&args);
		// 3. receive the one capability the manifest grants: a ServiceManager status client.
		let statsvc: u64 = recv_tagged(bootstrap, &mut buf, b"SERVICES").unwrap_or_else(|| exit());
		query_services(statsvc, mode, filter);
	}
	exit();
}

// Split the argument string into the JSON mode and the optional name-prefix filter:
// "" / "json" / "json-min" / "<prefix>" / "json <prefix>" / "json-min <prefix>".
fn parse_args(args: &[u8]) -> (Option<JsonMode>, &[u8]) {
	if let Some(mode) = JsonMode::parse(args) {
		return (Some(mode), b"");
	}
	if let Some(rest) = args.strip_prefix(b"json-min ") {
		return (Some(JsonMode::Min), rest);
	}
	if let Some(rest) = args.strip_prefix(b"json ") {
		return (Some(JsonMode::Pretty), rest);
	}
	(None, args)
}

// Query the supervisor's status view through the grant and print each entry whose name
// starts with `filter`, as text (the default) or as a JSON array.
unsafe fn query_services(statsvc: u64, mode: Option<JsonMode>, filter: &[u8]) {
	unsafe {
		let mut client = supervisor::Client::new(ChannelTransport { chan: statsvc });
		match client.status() {
			Some(Ok(entries)) => {
				if let Some(mode) = mode {
					let mut out = String::from("[");
					let mut first: bool = true;
					for e in &entries {
						if !e.name.as_bytes().starts_with(filter) {
							continue;
						}
						if !first {
							out.push(',');
						}
						first = false;
						out.push_str(&e.to_json());
					}
					out.push(']');
					print(mode.render(out).as_bytes());
					print(b"\n");
				} else {
					for e in &entries {
						if !e.name.as_bytes().starts_with(filter) {
							continue;
						}
						print(e.to_text().as_bytes());
						print(b"\n");
					}
				}
			}
			Some(Err(_)) => print(b"lssvc: query error\n"),
			None => print(b"lssvc: service unavailable\n"),
		}
	}
}

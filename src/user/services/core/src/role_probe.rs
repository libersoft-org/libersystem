// role_probe - a program whose whole job is to be handed a declared set of bootstrap roles and
// report what `rt::receive_roles` made of them.
//
// THE RECEIVER SIDE IS THE HALF NOTHING WAS TESTING. A bootstrap is a fixed sequence of tagged
// messages, and every service read that sequence by hand: the two ends agreed only because somebody
// kept them agreeing. Three programs once read their tags in an order the sender does not use,
// which made a blocking tagged read consume the message that was actually next and then wait
// forever for one nobody sends - and it surfaced 170 tests away, in an unrelated service.
//
// The probe takes a one-byte case selector as its first bootstrap message, then receives a role
// list chosen to exercise one refusal, and reports the outcome as `tag:reason` (or `ok`). The
// caller can therefore assert that a MISSING role, a role of the WRONG OBJECT TYPE and a role with
// TOO FEW RIGHTS are each refused by name, and - the property that matters most - that a refusal
// leaves no capability behind in this process.

#![no_std]
#![no_main]

extern crate alloc;

use ipc_client::ChannelTransport;
use rt::*;

// The cases the probe knows how to be. One byte, because the selector arrives before anything else
// and a text protocol here would be a parser in a program that exists to test a receiver.
const CASE_ALL_PRESENT: u8 = 0;
const CASE_REQUIRED_MISSING: u8 = 1;
const CASE_WRONG_TYPE: u8 = 2;
const CASE_TOO_FEW_RIGHTS: u8 = 3;
const CASE_OPTIONAL_ABSENT: u8 = 4;
// Call a service through the GENERATED CLIENT over a channel whose far end is gone, and report
// what the client answered. The point is the client, not the channel: the transport has always
// known whether a request left, and every generated method used to throw that away.
const CASE_DEAD_PEER: u8 = 5;

// One serve root and one client, which between them cover both kinds a service is normally handed,
// plus an optional client that a smaller boot leaves empty.
const ROLES: [Role; 3] = [
	Role { tag: b"SERVE", kind: RoleKind::ServeRoot, required: true },
	Role { tag: b"STORAGE", kind: RoleKind::Client, required: true },
	Role { tag: b"MEDIA", kind: RoleKind::Client, required: false },
];

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// THE REPORT CHANNEL ARRIVES FIRST AND SEPARATELY, so the caller can CLOSE the bootstrap
		// channel to make a role never arrive and still hear what happened. Reporting on the
		// channel the roles come in on would make the one case that needs a closed peer the one
		// case that cannot be observed.
		let mut buf = [0u8; 32];
		let (case, report): (u8, u64) = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if len >= 1 && handle != 0 => (buf[0], handle),
			_ => exit(),
		};
		// THE DEAD-PEER CASE TAKES NO ROLES AT ALL. It is handed one channel whose other end the
		// caller has dropped, calls a real service op on it through the generated client, and
		// reports the error code that came back - which is the whole of what this case measures.
		if case == CASE_DEAD_PEER {
			let dead: u64 = match recv_blocking(bootstrap, &mut buf) {
				Received::Message { handle, .. } => handle,
				Received::Closed => exit(),
			};
			let answer = proto::system::volume::Client::new(ChannelTransport { chan: dead }).remove("vol://ram/anything");
			let mut out = alloc::vec::Vec::new();
			match answer {
				// The distinction this case exists for: a typed answer rather than a bare "it
				// did not work", and one that says which side of the line the request is on.
				Some(Err(error)) => {
					out.extend_from_slice(b"err ");
					out.push(b'0' + error as u8);
				}
				Some(Ok(())) => out.extend_from_slice(b"ok"),
				None => out.extend_from_slice(b"none"),
			}
			let _ = send_blocking(report, &out, 0);
			exit();
		}
		// A shorter list for the case that ends early, so the probe asks for a role the caller
		// deliberately never sends rather than waiting on a peer that has already finished.
		let roles: &[Role] = match case {
			CASE_REQUIRED_MISSING => &ROLES[..2],
			_ => &ROLES,
		};
		let mut handles = [0u64; ROLES.len()];
		let outcome = receive_roles(bootstrap, roles, &mut handles);

		// HOW MANY CAPABILITIES THIS PROCESS STILL HOLDS is the assertion the caller cannot make
		// from outside without this. A refusal that left handles behind would be a bootstrap that
		// failed and kept authority anyway.
		let mut report_bytes = alloc::vec::Vec::new();
		match outcome {
			Ok(()) => report_bytes.extend_from_slice(b"ok"),
			Err(error) => {
				report_bytes.extend_from_slice(error.tag());
				report_bytes.push(b':');
				report_bytes.extend_from_slice(error.reason());
			}
		}
		report_bytes.push(b' ');
		let mut held = 0usize;
		for handle in handles.iter() {
			if *handle != 0 {
				held += 1;
			}
		}
		report_bytes.push(b'0' + held as u8);
		let _ = send_blocking(report, &report_bytes, 0);
		let _ = case;
		let _ = CASE_ALL_PRESENT;
		let _ = CASE_WRONG_TYPE;
		let _ = CASE_TOO_FEW_RIGHTS;
		let _ = CASE_OPTIONAL_ABSENT;
		let _ = CASE_DEAD_PEER;
		exit();
	}
}

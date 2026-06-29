// date - the `date` shell command run as a standalone, governed program.
//
// PermissionManager launches this program under its permission manifest, which grants it
// exactly one capability: a TimeService client. The command receives that client (and
// nothing else - no storage, no log, no network), queries the wall clock through it,
// renders the instant as ISO-8601 UTC, and reports the rendered string back to the
// manager over its bootstrap channel - the manager's proof that the governed command
// reached its one granted capability. This is a slice of moving every shell command into
// its own sandboxed ELF: `date` carries only the time capability its manifest declares.
//
// The command never receives any other capability (its manifest grants only time), so it
// cannot reach storage, the log, or the network at all - there is no ambient authority to
// fall back on, only the one capability handed to it. This is the strict-sandbox property:
// a launched component starts with only its manifest's capabilities and can reach nothing
// else.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::{Timestamp, time};
use rt::*;

// Read the wall clock through the granted TimeService client and render it as ISO-8601
// UTC ("YYYY-MM-DDTHH:MM:SSZ"). Mirrors the shell's `date`: query now() over the time
// client, then render the typed timestamp. Returns the rendered bytes, or None if the
// grant could not be reached (no ambient fallback - the capability is the only way to the
// clock).
fn read_clock(timesvc: u64) -> Option<Vec<u8>> {
	let mut client = time::Client::new(ChannelTransport { chan: timesvc });
	let ts: Timestamp = match client.now() {
		Some(Ok(t)) => t,
		_ => return None,
	};
	let mut out: [u8; 24] = [0u8; 24];
	let n: usize = ts.render(&mut out);
	Some(out[..n].to_vec())
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];

	// Receive exactly the one capability the manifest grants: the TimeService client. The
	// command never receives (and so can never reach) anything else.
	let timesvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"TIME") }.unwrap_or_else(|| exit());

	// Exercise the grant: read the wall clock and render it.
	let rendered: Vec<u8> = read_clock(timesvc).unwrap_or_default();

	// Report the rendered instant back to the manager - its proof the time grant is live.
	unsafe {
		send_blocking(bootstrap, &rendered, 0);
	}
	exit();
}

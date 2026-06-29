// request_probe - the governed component that exercises the dynamic permission-request path.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - a LogService client - and nothing else. The probe emits one entry to
// prove that static grant is live, then asks for a capability its manifest does NOT declare,
// a StorageService client, at runtime over its bootstrap channel. PermissionManager applies
// its non-interactive (headless) policy default - which refuses any capability a component
// did not pre-declare - records the refused request in the audit trail as a dynamic decision,
// and replies that the request was denied. The probe reports the outcome back - its proof
// that it could not escalate beyond its manifest at runtime - and exits.
//
// This is the dynamic path for later untrusted apps: a runtime permission request, decided
// by a headless policy and recorded back into the same store as the static grants, with no
// human in the loop. The appliance default is least privilege - an undeclared request is
// denied - so a component can never gain authority its manifest did not declare.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use proto::system::{log, Capability, Entry, Field, Severity};
use rt::*;

// The capability request rides the bootstrap channel as b"REQUEST" + the requested
// capability's ordinal byte; the manager replies tagged with the capability (carrying the
// granted client) if it allowed the request, or a bare b"DENY" (no handle) if its headless
// policy refused it.
const REQUEST_TAG: &[u8] = b"REQUEST";

// Emit one log entry through the granted LogService client - exercising the probe's one
// static grant. Best-effort: the demonstration is that the grant works, not its result.
unsafe fn emit_online(logsvc: u64) {
	let entry: Entry = Entry { timestamp: unsafe { clock() }, severity: Severity::Info, source: String::from("request_probe"), fields: alloc::vec![Field { key: String::from("event"), value: String::from("online") }] };
	let mut client = log::Client::new(ChannelTransport { chan: logsvc });
	let _ = client.emit(&entry);
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];

	// Receive the one capability the manifest grants: a LogService client. The probe never
	// receives anything else statically.
	let logsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"LOG") }.unwrap_or_else(|| exit());
	unsafe {
		emit_online(logsvc);
	}

	// Ask for a capability the manifest does NOT declare - a StorageService client - at
	// runtime, then read the manager's verdict: a tagged STORAGE reply (granted) or a bare
	// DENY (refused by the headless policy).
	let mut request: [u8; 8] = [0u8; 8];
	request[..REQUEST_TAG.len()].copy_from_slice(REQUEST_TAG);
	request[REQUEST_TAG.len()] = Capability::Storage as u8;
	let granted: bool = unsafe {
		send_blocking(bootstrap, &request, 0);
		recv_tagged(bootstrap, &mut buf, b"STORAGE").is_some()
	};

	// Report the outcome - the proof of whether the runtime request escalated our authority.
	let report: &[u8] = if granted { b"storage granted" } else { b"storage denied" };
	unsafe {
		send_blocking(bootstrap, report, 0);
	}
	exit();
}

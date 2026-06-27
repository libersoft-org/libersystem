// sandbox_probe - the M38 sandboxed component.
//
// PermissionManager launches this program under its permission manifest, which grants
// it exactly two capabilities: a StorageService client and a LogService client. The
// probe receives those two clients (and nothing else - no network, no console, no
// devices), then exercises each to prove the grant is live: it emits one entry through
// LogService and reads its one granted file vol://system/hello.txt through
// StorageService. It reports the bytes it read back to the manager over its bootstrap
// channel - the manager's proof that the sandboxed component reached its granted storage
// capability - and exits.
//
// The probe never receives a network client (its manifest does not grant one), so it
// cannot reach the network at all: there is no ambient authority to fall back on, only
// the capabilities handed to it. This is the strict-sandbox property of M38 - a launched
// component starts with only its manifest's capabilities and can reach nothing else.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{log, volume, Entry, Field, OpenOpts, Severity};
use rt::*;

// The one file the probe's storage grant lets it read (proving the grant is live).
const PROBE_FILE: &[u8] = b"vol://system/hello.txt";

// Read the granted file through StorageService into an owned buffer. Mirrors the
// shell's `cat`: open over the volume client, map the returned shared buffer, copy it
// out, then release the mapping and handle. Returns the file bytes, or None on failure.
unsafe fn read_granted_file(storage: u64) -> Option<Vec<u8>> {
	unsafe {
		let opts: OpenOpts = OpenOpts { path: String::from_utf8_lossy(PROBE_FILE).into_owned(), write: false, create: false };
		let mut client = volume::Client::new(ChannelTransport { chan: storage });
		let result = match client.open(&opts) {
			Some(Ok(r)) => r,
			_ => return None,
		};
		if result.file == 0 || result.size == 0 {
			return None;
		}
		let mapped: u64 = map_object(result.file)?;
		let bytes: Vec<u8> = core::slice::from_raw_parts(mapped as *const u8, result.size as usize).to_vec();
		unmap_object(result.file);
		close(result.file);
		Some(bytes)
	}
}

// Emit one log entry through the granted LogService client - exercising the probe's
// second grant. Best-effort: the demonstration is that the grant works, not its result.
unsafe fn emit_online(logsvc: u64) {
	let entry: Entry = Entry { timestamp: unsafe { clock() }, severity: Severity::Info, source: String::from("sandbox_probe"), fields: alloc::vec![Field { key: String::from("event"), value: String::from("online") }] };
	let mut client = log::Client::new(ChannelTransport { chan: logsvc });
	let _ = client.emit(&entry);
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];

	// Receive exactly the two capabilities the manifest grants, in the order the
	// PermissionManager transfers them: the StorageService client, then the LogService
	// client. The probe never receives (and so can never reach) anything else.
	let storage: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"STORAGE") }.unwrap_or_else(|| exit());
	let logsvc: u64 = unsafe { recv_tagged(bootstrap, &mut buf, b"LOG") }.unwrap_or_else(|| exit());

	// Exercise both grants: emit one log entry, then read the one granted file.
	unsafe {
		emit_online(logsvc);
	}
	let contents: Vec<u8> = unsafe { read_granted_file(storage) }.unwrap_or_default();

	// Report the bytes read back to the manager - its proof the storage grant is live.
	unsafe {
		send_blocking(bootstrap, &contents, 0);
	}
	exit();
}

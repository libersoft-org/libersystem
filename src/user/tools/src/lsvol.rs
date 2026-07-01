// lsvol - list the available volumes with a per-volume file count, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - `volumes` - and forwards it the shell's stdout console first, then the
// argument string, then the five volume StorageService clients the capability bundles: the
// `system` (writable LiberFS), `media` (FAT/exFAT), `iso` (ISO9660), `udf` (UDF), and `usb`
// (FAT off the USB stick) volumes.
// lsvol lists each volume's root through its grant, prints the volume set with a per-volume
// file count to the inherited stdout, then exits. A standalone command, not a shell built-in:
// it reaches the volumes only through the one capability the permission store granted it, and
// renders on the same terminal as the shell that launched it. A volume whose disk is absent
// arrives as a closed channel and shows zero files, just as the built-in fallback does.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use proto::system::volume;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - lsvol takes none, but consume the message so the
		//    grants that follow line up.
		match recv_blocking(bootstrap, &mut buf) {
			Received::Message { .. } => {}
			Received::Closed => exit(),
		}
		// 3. receive the five volume clients the `volumes` capability bundles, in grant order;
		//    a volume whose disk is absent arrives as 0 (no handle) and shows zero files.
		let system: u64 = recv_tagged(bootstrap, &mut buf, b"SYSTEM").unwrap_or(0);
		let media: u64 = recv_tagged(bootstrap, &mut buf, b"MEDIA").unwrap_or(0);
		let iso: u64 = recv_tagged(bootstrap, &mut buf, b"ISO").unwrap_or(0);
		let udf: u64 = recv_tagged(bootstrap, &mut buf, b"UDF").unwrap_or(0);
		let usb: u64 = recv_tagged(bootstrap, &mut buf, b"USB").unwrap_or(0);
		list_volumes(system, media, iso, udf, usb);
	}
	exit();
}

// List the volume set with a per-volume file count, read through the five grants: `system`
// (writable LiberFS), `media` (FAT/exFAT), `iso` (ISO9660), `udf` (UDF), and `usb` (FAT off
// the USB stick).
unsafe fn list_volumes(system: u64, media: u64, iso: u64, udf: u64, usb: u64) {
	unsafe {
		let mut out = String::new();
		out.push_str("volumes (5):\n  vol://system (");
		push_count(&mut out, volume_count(system, "vol://system"));
		out.push_str(" files)\n  vol://media (");
		push_count(&mut out, volume_count(media, "vol://media"));
		out.push_str(" files)\n  vol://iso (");
		push_count(&mut out, volume_count(iso, "vol://iso"));
		out.push_str(" files)\n  vol://udf (");
		push_count(&mut out, volume_count(udf, "vol://udf"));
		out.push_str(" files)\n  vol://usb (");
		push_count(&mut out, volume_count(usb, "vol://usb"));
		out.push_str(" files)\n");
		print(out.as_bytes());
	}
}

// Count the files on a volume via the StorageService `list` op; 0 if the volume is absent or
// the service is unavailable.
unsafe fn volume_count(storage: u64, uri: &str) -> usize {
	if storage == 0 {
		return 0;
	}
	let mut client = volume::Client::new(ChannelTransport { chan: storage });
	match client.list(uri) {
		Some(Ok(files)) => files.len(),
		_ => 0,
	}
}

// Append a file count as decimal.
fn push_count(out: &mut String, count: usize) {
	use core::fmt::Write as _;
	let _ = write!(out, "{count}");
}

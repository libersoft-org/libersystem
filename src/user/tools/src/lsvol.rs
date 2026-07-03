// lsvol - list the available volumes with a per-volume file count, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - `volumes` - and forwards it the shell's stdout console first, then the
// argument string ("" for text or "json"), then the five volume StorageService clients the
// capability bundles: the `system` (writable LiberFS), `media` (FAT/exFAT), `iso` (ISO9660),
// `udf` (UDF), and `usb` (FAT off the USB stick) volumes.
// lsvol lists each volume's root through its grant and prints the volume set - each with the
// filesystem its service reports and a file count, the system volume also with its pool
// numbers - to the inherited stdout, then exits. A standalone command, not a shell built-in:
// it reaches the volumes only through the one capability the permission store granted it, and
// renders on the same terminal as the shell that launched it. A volume whose disk is absent
// arrives as a closed channel and shows as absent.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use proto::system::{VolumeStatus, volume};
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
		// 3. receive the five volume clients the `volumes` capability bundles, in grant order;
		//    a volume whose disk is absent arrives as 0 (no handle) and shows as absent.
		let system: u64 = recv_tagged(bootstrap, &mut buf, b"SYSTEM").unwrap_or(0);
		let media: u64 = recv_tagged(bootstrap, &mut buf, b"MEDIA").unwrap_or(0);
		let iso: u64 = recv_tagged(bootstrap, &mut buf, b"ISO").unwrap_or(0);
		let udf: u64 = recv_tagged(bootstrap, &mut buf, b"UDF").unwrap_or(0);
		let usb: u64 = recv_tagged(bootstrap, &mut buf, b"USB").unwrap_or(0);
		list_volumes(system, media, iso, udf, usb, json);
	}
	exit();
}

// List the volume set, read through the five grants: each volume's filesystem (as its
// service reports it in the `status` op) and file count, plus the system volume's pool
// numbers (label, size, free space, compression, mount mode) - the `df` view. `json`
// selects a JSON array over the text lines.
unsafe fn list_volumes(system: u64, media: u64, iso: u64, udf: u64, usb: u64, json: bool) {
	unsafe {
		let rows: [(&str, u64); 5] = [("vol://system", system), ("vol://media", media), ("vol://iso", iso), ("vol://udf", udf), ("vol://usb", usb)];
		let mut out = String::new();
		if json {
			out.push('[');
		} else {
			out.push_str("volumes (5):\n");
		}
		for (i, &(uri, chan)) in rows.iter().enumerate() {
			let status: Option<VolumeStatus> = volume_status(chan);
			let files: usize = volume_count(chan, uri);
			render_row(&mut out, i, uri, chan != 0, status.as_ref(), files, json);
		}
		if json {
			out.push(']');
		}
		out.push('\n');
		print(out.as_bytes());
	}
}

// Append one volume row to `out`, as a text line or a JSON object: the filesystem the
// volume's service reports, the file count, and - when the filesystem tracks a pool
// (the LiberFS system volume) - the used/total numbers, the compression switch and a
// READ-ONLY marker on a degraded mount.
fn render_row(out: &mut String, index: usize, uri: &str, present: bool, status: Option<&VolumeStatus>, files: usize, json: bool) {
	use core::fmt::Write as _;
	if json {
		if index > 0 {
			out.push(',');
		}
		let _ = write!(out, "{{\"volume\":\"{uri}\",\"present\":{present}");
		if let Some(st) = status {
			let _ = write!(out, ",\"filesystem\":\"{}\",\"files\":{files}", st.filesystem);
			if !st.label.is_empty() {
				let _ = write!(out, ",\"label\":\"{}\"", st.label);
			}
			if st.total_bytes > 0 {
				let _ = write!(out, ",\"total_bytes\":{},\"free_bytes\":{},\"compression\":{},\"read_only\":{}", st.total_bytes, st.free_bytes, st.compression, st.read_only);
			}
		}
		out.push('}');
		return;
	}
	let _ = write!(out, "  {uri} (");
	match status {
		Some(st) => {
			let _ = write!(out, "{}, {files} files)", st.filesystem);
			if st.total_bytes > 0 {
				let used: u64 = st.total_bytes - st.free_bytes;
				let _ = write!(out, " - {} / {} MB used, compression {}", used >> 20, st.total_bytes >> 20, if st.compression { "on" } else { "off" });
				if st.read_only {
					out.push_str(", READ-ONLY");
				}
			}
		}
		None => {
			out.push_str(if present { "unavailable)" } else { "absent)" });
		}
	}
	if index < 4 {
		out.push('\n');
	}
}

// The volume's status (its filesystem name and, for LiberFS, the pool numbers), or None
// when the volume is absent or its service unreachable.
fn volume_status(chan: u64) -> Option<VolumeStatus> {
	if chan == 0 {
		return None;
	}
	let mut client = volume::Client::new(ChannelTransport { chan });
	match client.status() {
		Some(Ok(st)) => Some(st),
		_ => None,
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

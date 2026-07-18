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
use ipc_client::ChannelTransport;
use proto::codec::JsonMode;
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
		let mode: Option<JsonMode> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => JsonMode::parse(&buf[..len]),
			Received::Closed => exit(),
		};
		// 3. receive the five volume clients the `volumes` capability bundles, in grant order;
		//    a volume whose disk is absent arrives as 0 (no handle) and shows as absent.
		let system: u64 = recv_tagged(bootstrap, &mut buf, b"SYSTEM").unwrap_or(0);
		let media: u64 = recv_tagged(bootstrap, &mut buf, b"MEDIA").unwrap_or(0);
		let iso: u64 = recv_tagged(bootstrap, &mut buf, b"ISO").unwrap_or(0);
		let udf: u64 = recv_tagged(bootstrap, &mut buf, b"UDF").unwrap_or(0);
		let usb: u64 = recv_tagged(bootstrap, &mut buf, b"USB").unwrap_or(0);
		list_volumes(system, media, iso, udf, usb, mode);
	}
	exit();
}

// List the volume set, read through the five grants: an aligned table of each
// volume's filesystem (as its service reports it in the `status` op), file count,
// and the size / used / free numbers the filesystem declares, with a notes column
// for the read-only and compression flags - the `df` view. `mode` selects a JSON
// array over the table.
unsafe fn list_volumes(system: u64, media: u64, iso: u64, udf: u64, usb: u64, mode: Option<JsonMode>) {
	unsafe {
		let json: bool = mode.is_some();
		let rows: [(&str, u64); 5] = [("vol://system", system), ("vol://media", media), ("vol://iso", iso), ("vol://udf", udf), ("vol://usb", usb)];
		let mut out = String::new();
		if json {
			out.push('[');
		} else {
			out.push_str("\x1b[1mvolume        filesystem  files       size       used       free\x1b[0m\n");
		}
		for (i, &(uri, chan)) in rows.iter().enumerate() {
			let status: Option<VolumeStatus> = volume_status(chan);
			let files: usize = volume_count(chan, uri);
			render_row(&mut out, i, uri, chan != 0, status.as_ref(), files, json);
		}
		if let Some(mode) = mode {
			out.push(']');
			out = mode.render(out);
		}
		out.push('\n');
		print(out.as_bytes());
	}
}

// Append one volume row to `out`, as a table line or a JSON object: the filesystem
// the volume's service reports, the file count, and the size / used / free columns
// (used = total - free; a read-only volume is all in use), plus a notes column with
// the READ-ONLY marker of a degraded or inherently read-only mount and the LiberFS
// compression switch.
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
	push_left(out, uri, 14);
	match status {
		Some(st) => {
			push_left(out, &st.filesystem, 12);
			let mut cell = String::new();
			let _ = write!(cell, "{files}");
			push_right(out, &cell, 5);
			if st.total_bytes > 0 {
				push_right(out, &size_cell(st.total_bytes), 11);
				push_right(out, &size_cell(st.total_bytes - st.free_bytes), 11);
				let free: String = if st.read_only { String::from("-") } else { size_cell(st.free_bytes) };
				push_right(out, &free, 11);
			} else {
				push_right(out, "-", 11);
				push_right(out, "-", 11);
				push_right(out, "-", 11);
			}
			if st.read_only {
				out.push_str("  read-only");
			}
			if st.compression {
				out.push_str("  compression");
			}
		}
		None => {
			push_left(out, if present { "unavailable" } else { "absent" }, 12);
		}
	}
	if index < 4 {
		out.push('\n');
	}
}

// Append `text` padded with spaces on the right to `width` (a left-aligned column,
// two spaces of gutter included in the widths above).
fn push_left(out: &mut String, text: &str, width: usize) {
	out.push_str(text);
	for _ in text.len()..width {
		out.push(' ');
	}
}

// Append `text` padded with spaces on the left to `width` (a right-aligned column).
fn push_right(out: &mut String, text: &str, width: usize) {
	for _ in text.len()..width {
		out.push(' ');
	}
	out.push_str(text);
}

// A byte count scaled to the largest whole unit (kB / MB / GB), one decimal - the
// same rendering lsblk uses for capacities.
fn size_cell(bytes: u64) -> String {
	use core::fmt::Write as _;
	let mut cell = String::new();
	let units: [(&str, u64); 3] = [("GB", 1 << 30), ("MB", 1 << 20), ("kB", 1 << 10)];
	for &(unit, scale) in &units {
		if bytes >= scale {
			let _ = write!(cell, "{}.{} {}", bytes / scale, bytes % scale * 10 / scale, unit);
			return cell;
		}
	}
	let _ = write!(cell, "{bytes} B");
	cell
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
		Some(consumer) => unsafe { drain_stream(consumer, volume::list_read) }.len(),
		None => 0,
	}
}

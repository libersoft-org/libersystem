// lsblk - list the block devices and their mounted volumes, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it
// exactly one capability - `volumes` - and forwards it the shell's stdout console, the
// argument string (the sub-form: "" for text or "json"), then the five volume
// StorageService clients the capability bundles. lsblk asks each volume's service for
// the capacity of the block device backing it (a query the service forwards to the
// disk over the block channel, so it answers even for an unmounted removable volume),
// prints one line per volume - the vol:// name, the backing device, and its size - to
// the inherited stdout, and exits. A volume whose disk is absent shows no size.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use proto::codec::JsonMode;
use proto::system::volume;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the sub-form ("" for text, "json" /
		//    "json-min" for JSON).
		let mode: Option<JsonMode> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => JsonMode::parse(&buf[..len]),
			Received::Closed => exit(),
		};
		// 3. receive the five volume clients the `volumes` capability bundles, in grant
		//    order; a volume whose disk is absent arrives as 0 (no handle).
		let system: u64 = recv_tagged(bootstrap, &mut buf, b"SYSTEM").unwrap_or(0);
		let media: u64 = recv_tagged(bootstrap, &mut buf, b"MEDIA").unwrap_or(0);
		let iso: u64 = recv_tagged(bootstrap, &mut buf, b"ISO").unwrap_or(0);
		let udf: u64 = recv_tagged(bootstrap, &mut buf, b"UDF").unwrap_or(0);
		let usb: u64 = recv_tagged(bootstrap, &mut buf, b"USB").unwrap_or(0);
		list_block_devices(system, media, iso, udf, usb, mode);
	}
	exit();
}

// One row per volume: the vol:// name, the backing block device, and its capacity
// asked through the volume's typed `capacity` query.
unsafe fn list_block_devices(system: u64, media: u64, iso: u64, udf: u64, usb: u64, mode: Option<JsonMode>) {
	unsafe {
		let json: bool = mode.is_some();
		let rows: [(&str, &str, u64); 5] = [
			("vol://system", "virtio-blk", system),
			("vol://media", "virtio-blk", media),
			("vol://iso", "virtio-blk", iso),
			("vol://udf", "virtio-blk", udf),
			("vol://usb", "usb-storage", usb),
		];
		let mut out = String::new();
		if json {
			out.push('[');
		}
		for (i, &(name, device, chan)) in rows.iter().enumerate() {
			let capacity: Option<u64> = volume_capacity(chan);
			render_row(&mut out, i, name, device, capacity, json);
		}
		if let Some(mode) = mode {
			out.push(']');
			out = mode.render(out);
		}
		out.push('\n');
		print(out.as_bytes());
	}
}

// The capacity of the block device behind one volume client, or None when the volume
// (or its disk) is absent.
fn volume_capacity(chan: u64) -> Option<u64> {
	if chan == 0 {
		return None;
	}
	let mut client = volume::Client::new(ChannelTransport { chan });
	match client.capacity() {
		Some(Ok(bytes)) => Some(bytes),
		_ => None,
	}
}

// Append one volume row to `out`, as a text line or a JSON object.
fn render_row(out: &mut String, index: usize, name: &str, device: &str, capacity: Option<u64>, json: bool) {
	if json {
		if index > 0 {
			out.push(',');
		}
		out.push_str("{\"volume\":\"");
		out.push_str(name);
		out.push_str("\",\"device\":\"");
		out.push_str(device);
		out.push('"');
		if let Some(bytes) = capacity {
			out.push_str(",\"bytes\":");
			push_decimal(out, bytes);
		}
		out.push('}');
		return;
	}
	out.push_str(name);
	for _ in name.len()..14 {
		out.push(' ');
	}
	out.push_str(device);
	for _ in device.len()..13 {
		out.push(' ');
	}
	match capacity {
		Some(bytes) => push_size(out, bytes),
		None => out.push('-'),
	}
	if index < 4 {
		out.push('\n');
	}
}

// Append a byte count scaled to the largest whole unit (kB / MB / GB) to `out`.
fn push_size(out: &mut String, bytes: u64) {
	let units: [(&str, u64); 3] = [("GB", 1 << 30), ("MB", 1 << 20), ("kB", 1 << 10)];
	for &(unit, scale) in &units {
		if bytes >= scale {
			push_decimal(out, bytes / scale);
			out.push('.');
			push_decimal(out, bytes % scale * 10 / scale);
			out.push(' ');
			out.push_str(unit);
			return;
		}
	}
	push_decimal(out, bytes);
	out.push_str(" B");
}

// Append a decimal number to `out`.
fn push_decimal(out: &mut String, value: u64) {
	let mut digits: [u8; 20] = [0u8; 20];
	let mut v: u64 = value;
	let mut n: usize = 0;
	loop {
		digits[n] = b'0' + (v % 10) as u8;
		v /= 10;
		n += 1;
		if v == 0 {
			break;
		}
	}
	for i in 0..n {
		out.push(digits[n - 1 - i] as char);
	}
}

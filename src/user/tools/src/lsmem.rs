// lsmem - print the physical memory map, run as its own sandboxed ELF.
//
// PermissionManager launches this program under an empty permission manifest - the
// boot memory map is a free syscall, no capability needed - and forwards it the
// shell's stdout console and the argument string (the sub-form: "" for text or
// "json"). lsmem walks the memory-map regions the kernel retained at boot (base,
// length, kind: usable / reserved / ACPI / ...), prints one line per region to the
// inherited stdout, and exits - the physical layout `free`'s totals come from.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use proto::codec::JsonMode;
use rt::*;
use tools::recv_json_mode;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our
		//    output renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the sub-form ("" for text, "json" /
		//    "json-min" for JSON).
		let mode: Option<JsonMode> = recv_json_mode(bootstrap, &mut buf);
		let json: bool = mode.is_some();
		// 3. walk the retained memory map and render one entry per region.
		let mut out = String::new();
		if json {
			out.push('[');
		}
		let mut index: u64 = 0;
		loop {
			let mut region = MemmapRegion::default();
			if memmap_get(index, &mut region) < 0 {
				break;
			}
			render_region(&mut out, index, &region, json);
			index += 1;
		}
		if let Some(mode) = mode {
			out.push(']');
			out = mode.render(out);
			out.push('\n');
		}
		if index == 0 {
			print(b"lsmem: query error\n");
		} else {
			print(out.as_bytes());
		}
	}
	exit();
}

// The kind names for the MEMMAP_* codes.
fn kind_name(kind: u32) -> &'static str {
	match kind {
		MEMMAP_USABLE => "usable",
		MEMMAP_RESERVED => "reserved",
		MEMMAP_ACPI_RECLAIMABLE => "acpi reclaimable",
		MEMMAP_ACPI_NVS => "acpi nvs",
		MEMMAP_BAD => "bad",
		MEMMAP_BOOTLOADER => "bootloader",
		MEMMAP_KERNEL => "kernel",
		MEMMAP_FRAMEBUFFER => "framebuffer",
		_ => "unknown",
	}
}

// Append one region to `out`, as a text line or a JSON object.
fn render_region(out: &mut String, index: u64, region: &MemmapRegion, json: bool) {
	if json {
		if index > 0 {
			out.push(',');
		}
		out.push_str("{\"base\":");
		push_decimal(out, region.base);
		out.push_str(",\"length\":");
		push_decimal(out, region.length);
		out.push_str(",\"type\":\"");
		out.push_str(kind_name(region.kind));
		out.push_str("\"}");
		return;
	}
	push_hex(out, region.base);
	out.push('-');
	push_hex(out, region.base + region.length);
	out.push(' ');
	push_size(out, region.length);
	out.push(' ');
	out.push_str(kind_name(region.kind));
	out.push('\n');
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

// Append a 16-digit zero-padded hex address to `out`.
fn push_hex(out: &mut String, value: u64) {
	out.push_str("0x");
	for shift in (0..16).rev() {
		let digit: u8 = (value >> (shift * 4) & 0xf) as u8;
		out.push(if digit < 10 { (b'0' + digit) as char } else { (b'a' + digit - 10) as char });
	}
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

// lspci - print the PCI bus inventory, run as its own sandboxed ELF.
//
// PermissionManager launches this program under an empty permission manifest - the
// bus scan is a free syscall, no capability needed - and forwards it the shell's
// stdout console and the argument string (the sub-form: "" for text or "json").
// lspci walks the boot bus scan the kernel retained in full (every present function,
// not just the ones drivers bind), prints one line per function - bus:dev.func,
// vendor:device, class and its name - to the inherited stdout, and exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our
		//    output renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the sub-form ("" for text, "json" for JSON).
		let json: bool = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => &buf[..len] == b"json",
			Received::Closed => exit(),
		};
		// 3. walk the retained bus scan and render one entry per function.
		let mut out = String::new();
		if json {
			out.push('[');
		}
		let mut index: u64 = 0;
		loop {
			let mut info = PciInfo::default();
			if pci_info(index, &mut info) < 0 {
				break;
			}
			render_function(&mut out, index, &info, json);
			index += 1;
		}
		if json {
			out.push_str("]\n");
		}
		if index == 0 {
			print(b"lspci: query error\n");
		} else {
			print(out.as_bytes());
		}
	}
	exit();
}

// The name of a PCI base class code.
fn class_name(class: u8) -> &'static str {
	match class {
		0x00 => "unclassified",
		0x01 => "mass storage controller",
		0x02 => "network controller",
		0x03 => "display controller",
		0x04 => "multimedia controller",
		0x05 => "memory controller",
		0x06 => "bridge",
		0x07 => "communication controller",
		0x08 => "system peripheral",
		0x09 => "input device controller",
		0x0c => "serial bus controller",
		0x0d => "wireless controller",
		0x0e => "intelligent controller",
		0x0f => "satellite controller",
		0x10 => "encryption controller",
		0x11 => "signal processing controller",
		_ => "other",
	}
}

// Append one function to `out`: "bus:dev.func vendor:device class (name)" as a text
// line or a JSON object.
fn render_function(out: &mut String, index: u64, info: &PciInfo, json: bool) {
	if json {
		if index > 0 {
			out.push(',');
		}
		out.push_str("{\"address\":\"");
		push_address(out, info);
		out.push_str("\",\"vendor\":\"");
		push_hex16(out, info.vendor);
		out.push_str("\",\"device\":\"");
		push_hex16(out, info.device);
		out.push_str("\",\"class\":\"");
		push_hex8(out, info.class);
		push_hex8(out, info.subclass);
		out.push_str("\",\"name\":\"");
		out.push_str(class_name(info.class));
		out.push_str("\"}");
		return;
	}
	push_address(out, info);
	out.push(' ');
	push_hex16(out, info.vendor);
	out.push(':');
	push_hex16(out, info.device);
	out.push_str(" class ");
	push_hex8(out, info.class);
	push_hex8(out, info.subclass);
	out.push_str(" (");
	out.push_str(class_name(info.class));
	out.push_str(")\n");
}

// Append the "bus:dev.func" address to `out`.
fn push_address(out: &mut String, info: &PciInfo) {
	push_hex8(out, info.bus);
	out.push(':');
	push_hex8(out, info.dev);
	out.push('.');
	out.push((b'0' + info.func) as char);
}

// Append a two-digit zero-padded hex byte to `out`.
fn push_hex8(out: &mut String, value: u8) {
	for shift in [4u8, 0u8] {
		let digit: u8 = value >> shift & 0xf;
		out.push(if digit < 10 { (b'0' + digit) as char } else { (b'a' + digit - 10) as char });
	}
}

// Append a four-digit zero-padded hex word to `out`.
fn push_hex16(out: &mut String, value: u16) {
	push_hex8(out, (value >> 8) as u8);
	push_hex8(out, value as u8);
}

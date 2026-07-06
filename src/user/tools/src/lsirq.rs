// lsirq - print the device-interrupt vectors, run as its own sandboxed ELF.
//
// PermissionManager launches this program under an empty permission manifest - the
// vector table is a free syscall, no capability needed - and forwards it the shell's
// stdout console and the argument string (the sub-form: "" for text or "json").
// lsirq walks the kernel's device-interrupt windows - the fixed INTx vectors, then
// the per-device MSI-X vectors with the owning device resolved to its type - prints
// each vector in use to the inherited stdout, and exits.

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
		// 3. walk both vector windows and render every vector in use.
		let mut out = String::new();
		if json {
			out.push('[');
		}
		let mut index: u64 = 0;
		let mut printed: u64 = 0;
		loop {
			let mut info = IrqInfo::default();
			if irq_info(index, &mut info) < 0 {
				break;
			}
			index += 1;
			// An unused vector (nothing bound, no owner) is not inventory.
			if info.bound == 0 && info.device == IRQ_NO_DEVICE {
				continue;
			}
			render_vector(&mut out, printed, &info, json);
			printed += 1;
		}
		if let Some(mode) = mode {
			out.push(']');
			out = mode.render(out);
			out.push('\n');
		}
		if index == 0 {
			print(b"lsirq: query error\n");
		} else {
			print(out.as_bytes());
		}
	}
	exit();
}

// The device-type name for an owning device's type code (the ABI's classification).
fn device_type_name(device_type: u32) -> &'static str {
	match device_type {
		VIRTIO_TYPE_NET => "virtio-net",
		VIRTIO_TYPE_BLOCK => "virtio-blk",
		VIRTIO_TYPE_CONSOLE => "virtio-console",
		VIRTIO_TYPE_RNG => "virtio-rng",
		VIRTIO_TYPE_GPU => "virtio-gpu",
		VIRTIO_TYPE_INPUT => "virtio-input",
		VIRTIO_TYPE_SOUND => "virtio-snd",
		DEVICE_TYPE_XHCI => "xhci",
		_ => "unknown",
	}
}

// Append one in-use vector to `out`, as a text line or a JSON object. An MSI-X
// vector's owner is resolved to its device type through the free device-info query.
fn render_vector(out: &mut String, printed: u64, info: &IrqInfo, json: bool) {
	let kind: &str = if info.kind == IRQ_KIND_MSI { "msi" } else { "fixed" };
	if json {
		if printed > 0 {
			out.push(',');
		}
		out.push_str("{\"vector\":");
		push_decimal(out, info.vector as u64);
		out.push_str(",\"type\":\"");
		out.push_str(kind);
		out.push_str("\",\"bound\":");
		out.push_str(if info.bound != 0 { "true" } else { "false" });
		if info.device != IRQ_NO_DEVICE {
			out.push_str(",\"device\":");
			push_decimal(out, info.device as u64);
			out.push_str(",\"device-type\":\"");
			out.push_str(device_type_name(owner_type(info.device)));
			out.push('"');
		}
		out.push('}');
		return;
	}
	out.push_str("vector ");
	push_decimal(out, info.vector as u64);
	out.push_str(": ");
	out.push_str(kind);
	if info.device != IRQ_NO_DEVICE {
		out.push_str(", device ");
		push_decimal(out, info.device as u64);
		out.push_str(" (");
		out.push_str(device_type_name(owner_type(info.device)));
		out.push(')');
	} else if info.bound != 0 {
		out.push_str(", bound");
	}
	out.push('\n');
}

// The device-type code of the discovered device at `index` (0 = unknown).
fn owner_type(index: u32) -> u32 {
	let mut info = DeviceInfo::default();
	if unsafe { device_info(index as u64, &mut info) } { info.device_type } else { 0 }
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

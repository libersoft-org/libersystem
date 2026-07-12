// lscpu - print the CPU inventory, run as its own sandboxed ELF.
//
// PermissionManager launches this program under an empty permission manifest - the
// CPU topology is a free syscall, no capability needed - and forwards it the shell's
// stdout console and the argument string (the sub-form: "" for text or "json").
// lscpu reads the online CPU set (core count and per-core LAPIC ids, retained by the
// kernel at SMP bring-up), prints it to the inherited stdout, and exits.

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
		// 3. read the online CPU set and the CPU model, and render them.
		let mut ids: [u32; 64] = [0u32; 64];
		let count: i64 = cpu_info(&mut ids);
		if count <= 0 {
			print(b"lscpu: query error\n");
			exit();
		}
		let mut model_buf: [u8; 64] = [0u8; 64];
		let model_len: i64 = cpu_name(&mut model_buf);
		let model: &str = if model_len > 0 { core::str::from_utf8(&model_buf[..model_len as usize]).unwrap_or("") } else { "" };
		let n: usize = (count as usize).min(ids.len());
		print(render(&ids[..n], count as u64, model, mode).as_bytes());
	}
	exit();
}

// The architecture this binary was compiled for.
#[cfg(target_arch = "x86_64")]
const ARCH: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
const ARCH: &str = "aarch64";
#[cfg(target_arch = "riscv64")]
const ARCH: &str = "riscv64";

// Render the CPU set as text (the default) or as a JSON object. `model` is the CPU
// brand string (empty when the platform exposes none), rendered as the `name` field.
fn render(ids: &[u32], count: u64, model: &str, mode: Option<JsonMode>) -> String {
	let mut out = String::new();
	if let Some(mode) = mode {
		out.push_str("{\"arch\":\"");
		out.push_str(ARCH);
		out.push('"');
		if !model.is_empty() {
			out.push_str(",\"name\":\"");
			out.push_str(model);
			out.push('"');
		}
		out.push_str(",\"cpus\":");
		push_decimal(&mut out, count);
		out.push_str(",\"lapic\":[");
		for (i, &id) in ids.iter().enumerate() {
			if i > 0 {
				out.push(',');
			}
			push_decimal(&mut out, id as u64);
		}
		out.push_str("]}");
		let mut out = mode.render(out);
		out.push('\n');
		return out;
	}
	out.push_str("arch: ");
	out.push_str(ARCH);
	out.push('\n');
	if !model.is_empty() {
		out.push_str("name: ");
		out.push_str(model);
		out.push('\n');
	}
	out.push_str("cpus: ");
	push_decimal(&mut out, count);
	out.push('\n');
	for (i, &id) in ids.iter().enumerate() {
		out.push_str("cpu");
		push_decimal(&mut out, i as u64);
		out.push_str(": lapic ");
		push_decimal(&mut out, id as u64);
		out.push('\n');
	}
	out
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

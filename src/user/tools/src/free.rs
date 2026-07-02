// free - print the memory totals, run as its own sandboxed ELF.
//
// PermissionManager launches this program under an empty permission manifest - the
// memory totals are a free syscall, no capability needed - and forwards it the
// shell's stdout console and the argument string (the sub-form: "" for bytes or
// "-h" for human-readable units). free reads the physical frame pool's
// total/free counts and the kernel heap's total/free bytes, prints a Mem: and a
// Heap: row to the inherited stdout, and exits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use rt::*;

// Bytes per physical frame.
const FRAME_SIZE: u64 = 4096;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our
		//    output renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - the sub-form ("" for bytes, "-h" for
		//    human-readable units).
		let human: bool = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => &buf[..len] == b"-h",
			Received::Closed => exit(),
		};
		// 3. read the totals and render one row per pool.
		let mut stats = MemoryStats::default();
		if memory_stats(&mut stats) <= 0 {
			print(b"free: query error\n");
			exit();
		}
		let mut out = String::new();
		render_row(&mut out, "Mem:  ", stats.total_frames * FRAME_SIZE, stats.free_frames * FRAME_SIZE, human);
		render_row(&mut out, "Heap: ", stats.heap_total, stats.heap_free, human);
		print(out.as_bytes());
	}
	exit();
}

// Append one "name total X, used Y, free Z" row to `out`.
fn render_row(out: &mut String, name: &str, total: u64, free: u64, human: bool) {
	out.push_str(name);
	out.push_str("total ");
	push_size(out, total, human);
	out.push_str(", used ");
	push_size(out, total - free, human);
	out.push_str(", free ");
	push_size(out, free, human);
	out.push('\n');
}

// Append a byte count to `out`: raw decimal by default, or scaled to the largest
// whole unit (KiB / MiB / GiB, one decimal place) in human-readable form.
fn push_size(out: &mut String, bytes: u64, human: bool) {
	if !human {
		push_decimal(out, bytes);
		return;
	}
	let units: [(&str, u64); 3] = [("GiB", 1 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)];
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

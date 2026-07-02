// uptime - print the time since boot, run as its own sandboxed ELF.
//
// PermissionManager launches this program under an empty permission manifest - the
// monotonic clock is a free syscall, no capability needed - and forwards it the
// shell's stdout console and an (empty) argument string. uptime reads the
// nanosecond monotonic clock, renders it Linux-style ("up 2 days, 4:05:06" /
// "up 0:05:32"), prints it to the inherited stdout, and exits.

#![no_std]
#![no_main]

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our
		//    output renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string (uptime takes none, but the launch protocol
		//    sends one).
		let _ = recv_blocking(bootstrap, &mut buf);
		// 3. render the time since boot from the monotonic clock.
		let seconds: u64 = clock_ns() / 1_000_000_000;
		let mut line: [u8; 48] = [0u8; 48];
		let n: usize = render_uptime(&mut line, seconds);
		print(&line[..n]);
	}
	exit();
}

// Render "up [D day(s), ]H:MM:SS\n" into `out`, returning the byte count.
fn render_uptime(out: &mut [u8], seconds: u64) -> usize {
	let days: u64 = seconds / 86_400;
	let hours: u64 = seconds % 86_400 / 3_600;
	let minutes: u64 = seconds % 3_600 / 60;
	let secs: u64 = seconds % 60;
	let mut n: usize = 0;
	for &b in b"up " {
		out[n] = b;
		n += 1;
	}
	if days > 0 {
		n += push_decimal(&mut out[n..], days);
		let unit: &[u8] = if days == 1 { b" day, " } else { b" days, " };
		for &b in unit {
			out[n] = b;
			n += 1;
		}
	}
	n += push_decimal(&mut out[n..], hours);
	out[n] = b':';
	n += 1;
	n += push_two_digits(&mut out[n..], minutes);
	out[n] = b':';
	n += 1;
	n += push_two_digits(&mut out[n..], secs);
	out[n] = b'\n';
	n += 1;
	n
}

// Render a decimal number into `out`, returning the digit count.
fn push_decimal(out: &mut [u8], value: u64) -> usize {
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
		out[i] = digits[n - 1 - i];
	}
	n
}

// Render a zero-padded two-digit number into `out`, returning 2.
fn push_two_digits(out: &mut [u8], value: u64) -> usize {
	out[0] = b'0' + (value / 10 % 10) as u8;
	out[1] = b'0' + (value % 10) as u8;
	2
}

// beep - play a tone through AudioService, run as its own sandboxed ELF.
//
// PermissionManager launches this program under a permission manifest that grants it exactly
// one capability - an AudioService client - and forwards it the shell's stdout console and
// the argument string ("[hz] [ms]"). beep plays the tone through its grant and prints any
// error to the inherited stdout, then exits. A standalone command, not a shell built-in: it
// reaches the service only through the one capability the permission store granted it, and
// renders on the same terminal as the shell that launched it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::audio;
use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our output
		//    renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string - "[hz] [ms]" (both optional).
		let args: Vec<u8> = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, .. } => buf[..len].to_vec(),
			Received::Closed => exit(),
		};
		// 3. receive the one capability the manifest grants: an AudioService client.
		let audiosvc: u64 = recv_tagged(bootstrap, &mut buf, b"AUDIO").unwrap_or_else(|| exit());
		beep(audiosvc, &args[..]);
	}
	exit();
}

// Play a tone through the grant. Both arguments are optional and default to a 440 Hz tone for
// 200 ms; AudioService clamps them to its supported range. A "no audio device" error is
// reported when the system has no virtio-sound device, so the command degrades cleanly.
unsafe fn beep(audiosvc: u64, args: &[u8]) {
	unsafe {
		let mut freq: u16 = 440;
		let mut millis: u32 = 200;
		let mut parts = args.split(|&b| b == b' ').filter(|s: &&[u8]| !s.is_empty());
		if let Some(f) = parts.next() {
			match parse_usize(f) {
				Some(v) => freq = v.min(u16::MAX as usize) as u16,
				None => {
					print(b"beep: invalid frequency\n");
					return;
				}
			}
		}
		if let Some(m) = parts.next() {
			match parse_usize(m) {
				Some(v) => millis = v.min(u32::MAX as usize) as u32,
				None => {
					print(b"beep: invalid duration\n");
					return;
				}
			}
		}
		let mut client = audio::Client::new(ChannelTransport { chan: audiosvc });
		match client.beep(&freq, &millis) {
			Some(Ok(())) => {}
			Some(Err(_)) => print(b"beep: no audio device\n"),
			None => print(b"beep: service unavailable\n"),
		}
	}
}

// Parse a decimal byte string into a usize, rejecting empty or non-digit input.
fn parse_usize(s: &[u8]) -> Option<usize> {
	if s.is_empty() {
		return None;
	}
	let mut v: usize = 0;
	for &b in s {
		if !b.is_ascii_digit() {
			return None;
		}
		v = v.checked_mul(10)?.checked_add((b - b'0') as usize)?;
	}
	Some(v)
}

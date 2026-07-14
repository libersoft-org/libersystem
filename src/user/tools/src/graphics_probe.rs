// graphics_probe - governed proof of the graphical capability grant path.
//
// PermissionManager launches this tool with exactly three scoped clients: a display
// connection atomically bound to this Process, a raw-key-only InputService connection,
// and a playback-only AudioService connection. Receiving all three proves the launcher
// path; operation-level scope and emergency process kill are covered by service tests.

#![no_std]
#![no_main]

use rt::*;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0; 64];
	unsafe {
		inherit_stdout(bootstrap);
		match recv_blocking(bootstrap, &mut buf) {
			Received::Message { .. } => {}
			Received::Closed => exit(),
		}
		let display: u64 = recv_tagged(bootstrap, &mut buf, b"DISPLAY").unwrap_or_else(|| exit());
		let input: u64 = recv_tagged(bootstrap, &mut buf, b"INPUT_KEYS").unwrap_or_else(|| exit());
		let audio: u64 = recv_tagged(bootstrap, &mut buf, b"AUDIO_STREAM").unwrap_or_else(|| exit());
		if display != 0 && input != 0 && audio != 0 {
			print(b"graphics grants\n");
		}
	}
	exit();
}

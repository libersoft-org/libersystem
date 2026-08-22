// Console input: the kernel's minimal console driver.
//
// Until a virtio-console driver exists, the kernel owns the serial UART. The
// interactive shell runs as an ordinary userspace component, so the kernel feeds
// it keystrokes over a channel the shell registers with SYS_CONSOLE_ATTACH: the
// kernel reads serial bytes and sends them on this channel, and the shell blocks
// receiving them. This keeps the shell a proper userspace component (it blocks in
// `wait` rather than busy-polling a syscall) without yet needing a UART RX
// interrupt or a console driver process.

use alloc::sync::Arc;
use alloc::vec::Vec;

// One and two bytes on the heap, FALLIBLY. This runs on every keystroke and every serial byte -
// an interrupt-driven path ring 3 does not call but the outside world drives - and `alloc::vec![..]`
// there made a short heap a kernel abort. A dropped input byte is what a full queue already costs.
fn try_one(byte: u8) -> Option<Vec<u8>> {
	let mut bytes: Vec<u8> = Vec::new();
	bytes.try_reserve_exact(1).ok()?;
	bytes.push(byte);
	Some(bytes)
}

fn try_two(first: u8, second: u8) -> Option<Vec<u8>> {
	let mut bytes: Vec<u8> = Vec::new();
	bytes.try_reserve_exact(2).ok()?;
	bytes.push(first);
	bytes.push(second);
	Some(bytes)
}

use crate::object::channel::{Channel, Message};
use crate::sync::SpinLock;

pub const SERIAL_INPUT_MARKER: u8 = 0;

// The channel the kernel sends console input bytes on; the shell holds the peer
// endpoint and receives them. None until a shell attaches.
static CONSOLE: SpinLock<Option<Arc<Channel>>> = SpinLock::new(None);

// Register the channel the kernel feeds console input to (set by
// SYS_CONSOLE_ATTACH). Replaces any previous registration.
pub fn attach(channel: Arc<Channel>) {
	*CONSOLE.lock() = Some(channel);
}

// Send one input byte to the attached shell. Returns false if no shell is attached
// or its endpoint has closed (it exited).
// Whether a shell is attached and still listening (its peer endpoint is alive). False once the
// shell exits and drops its end. Asked by the boot tail's shell loop, which the test build has not.
#[cfg(not(test))]
pub fn shell_listening() -> bool {
	match &*CONSOLE.lock() {
		Some(channel) => !channel.is_peer_closed(),
		None => false,
	}
}

pub fn feed(byte: u8) -> bool {
	// ALLOC-OK: the guard holds an `Option<Arc<Channel>>`, so this is a refcount bump and not a
	// copy - taken out of the lock because the send below must not run under it.
	let channel = CONSOLE.lock().clone();
	match channel {
		Some(channel) => match try_one(byte) {
			Some(bytes) => channel.send(Message::new(bytes, Vec::new(), 0)).is_ok(),
			// A short heap: the keystroke is dropped, which is what a full queue already does here.
			None => false,
		},
		None => false,
	}
}

pub fn feed_serial(byte: u8) -> bool {
	// ALLOC-OK: an `Option<Arc<Channel>>` out of the guard - a refcount bump, as in `feed`.
	let channel = CONSOLE.lock().clone();
	match channel {
		Some(channel) => match try_two(SERIAL_INPUT_MARKER, byte) {
			Some(bytes) => channel.send(Message::new(bytes, Vec::new(), 0)).is_ok(),
			None => false,
		},
		None => false,
	}
}

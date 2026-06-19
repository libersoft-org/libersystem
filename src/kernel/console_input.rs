// Console input: the kernel's minimal console driver.
//
// Until a virtio-console driver exists, the kernel owns the serial UART. The
// interactive shell runs as an ordinary userspace component, so the kernel feeds
// it keystrokes over a channel the shell registers with SYS_CONSOLE_ATTACH: the
// kernel reads serial bytes and sends them on this channel, and the shell blocks
// receiving them. This keeps the shell a proper userspace component (it blocks in
// `wait` rather than busy-polling a syscall) without yet needing a UART RX
// interrupt or a console driver process.

#![allow(dead_code)]

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::object::channel::{Channel, Message};
use crate::sync::SpinLock;

// The channel the kernel sends console input bytes on; the shell holds the peer
// endpoint and receives them. None until a shell attaches.
static CONSOLE: SpinLock<Option<Arc<Channel>>> = SpinLock::new(None);

// Register the channel the kernel feeds console input to (set by
// SYS_CONSOLE_ATTACH). Replaces any previous registration.
pub fn attach(channel: Arc<Channel>) {
	*CONSOLE.lock() = Some(channel);
}

// Drop the registration.
pub fn detach() {
	*CONSOLE.lock() = None;
}

// Whether a shell is attached and still listening (its peer endpoint is alive).
// False once the shell exits and drops its end.
pub fn shell_listening() -> bool {
	match &*CONSOLE.lock() {
		Some(channel) => !channel.is_peer_closed(),
		None => false,
	}
}

// Send one input byte to the attached shell. Returns false if no shell is attached
// or its endpoint has closed (it exited).
pub fn feed(byte: u8) -> bool {
	let channel = CONSOLE.lock().clone();
	match channel {
		Some(channel) => channel.send(Message::new(alloc::vec![byte], Vec::new(), 0)).is_ok(),
		None => false,
	}
}

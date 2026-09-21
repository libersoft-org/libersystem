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

/// How many input bytes the kernel holds while the console's queue is full.
///
/// A BURST IS NOT A KEYSTROKE, and this is the difference between the two. One message per byte is
/// the shape of this channel, and a channel has a bounded queue - so a burst longer than that queue
/// had its TAIL SILENTLY DROPPED: a scripted guest typing a sixty-five character command line ran a
/// sixty-four character one, and a person pasting a line got part of it. What that cost, before this
/// existed, was a gate that typed a command with five flags on it and watched the guest run the first
/// three.
///
/// FIVE HUNDRED AND TWELVE, which is a command line or a pasted paragraph and is 1 kB of static
/// storage. The bound is real and is what makes this interrupt-safe: nothing here allocates, and a
/// burst past it is dropped the way it was dropped before rather than growing without limit.
const PENDING_BYTES: usize = 512;

/// The bytes that have arrived and have not been accepted yet, oldest first.
///
/// EACH ENTRY CARRIES WHICH PATH IT CAME FROM. A serial byte becomes a two-byte message with a
/// marker and a framebuffer keystroke becomes a one-byte message, so a ring that held bytes alone
/// would have to guess - and a guess here is a keystroke delivered as the wrong kind of input.
struct Pending {
	entries: [(bool, u8); PENDING_BYTES],
	head: usize,
	len: usize,
}

impl Pending {
	const fn new() -> Pending {
		Pending { entries: [(false, 0); PENDING_BYTES], head: 0, len: 0 }
	}

	/// Add one byte, answering whether there was room. A FULL RING DROPS THE NEWEST and not the
	/// oldest: what a person typed first is what they typed, and dropping the head would deliver a
	/// command line with its beginning missing rather than its end.
	fn push(&mut self, serial: bool, byte: u8) -> bool {
		if self.len == PENDING_BYTES {
			return false;
		}
		self.entries[(self.head + self.len) % PENDING_BYTES] = (serial, byte);
		self.len += 1;
		true
	}

	fn front(&self) -> Option<(bool, u8)> {
		(self.len > 0).then(|| self.entries[self.head])
	}

	fn pop(&mut self) {
		if self.len > 0 {
			self.head = (self.head + 1) % PENDING_BYTES;
			self.len -= 1;
		}
	}

	/// Forget everything waiting, without touching the storage. Used when the console changes: see
	/// `attach`. Cheap on purpose - the entries are left where they are and only the window moves,
	/// because this runs with interrupts masked.
	fn clear(&mut self) {
		self.head = 0;
		self.len = 0;
	}
}

static PENDING: SpinLock<Pending> = SpinLock::new(Pending::new());

// Register the channel the kernel feeds console input to (set by
// SYS_CONSOLE_ATTACH). Replaces any previous registration.
//
// AND WHAT THE PREVIOUS CONSOLE NEVER TOOK IS NOT THIS ONE'S INPUT. A byte is typed AT something:
// the console that was attached when it arrived. Carrying a backlog across an attach delivers it to
// a component that was not running when the key was pressed - after a SystemManager crash, the
// recovery console's first line would begin with whatever the old shell had not read yet.
//
// The two locks are taken in the order the delivery path takes them - the console first - so an
// interrupt arriving mid-swap finds either the old console or the new one, never the new
// registration beside the old console's leftovers.
pub fn attach(channel: Arc<Channel>) {
	let mut console = CONSOLE.lock();
	PENDING.lock().clear();
	*console = Some(channel);
}

// Whether a shell is attached and still listening (its peer endpoint is alive). False once the
// shell exits and drops its end. Asked by `supervise`, which is what decides a boot round is over.
pub fn shell_listening() -> bool {
	listener().is_some()
}

/// The console that could take a byte right now: registered, and with its peer still there.
///
/// A REGISTRATION IS NOT A LISTENER. `attach` replaces the entry and nothing ever removes it, so the
/// last console to attach stays registered long after the process holding the other end has gone -
/// and a channel whose peer is closed refuses every send. Asking both questions at once is what lets
/// the input path tell "nobody is listening" from "the listener is busy", which are the two cases
/// `enqueue` has to answer differently.
fn listener() -> Option<Arc<Channel>> {
	// ALLOC-OK: this clones an `Arc`, which is a refcount bump - taken out of the lock because the
	// send in `drain` must not run under it.
	let console = CONSOLE.lock();
	let channel = console.as_ref()?;
	(!channel.is_peer_closed()).then(|| channel.clone())
}

// Take one input byte for the attached console: a keystroke from the framebuffer path. Returns
// false if no console is attached, if its endpoint has closed (it exited), or if the kernel is
// already holding all the input it will hold.
pub fn feed(byte: u8) -> bool {
	enqueue(false, byte)
}

// The same, for a byte off the serial line: it is delivered with `SERIAL_INPUT_MARKER` in front so
// the console can tell the two paths apart.
pub fn feed_serial(byte: u8) -> bool {
	enqueue(true, byte)
}

/// Take one input byte and deliver as much as the console will accept.
///
/// EVERY BYTE GOES THROUGH THE RING, INCLUDING ONE THAT WOULD FIT. A byte sent directly while others
/// are waiting would arrive BEFORE them, which is a command line with its characters rearranged - and
/// the ordering is the whole of what an input path owes.
fn enqueue(serial: bool, byte: u8) -> bool {
	// NOBODY IS LISTENING, SO THE BYTE IS NOT KEPT FOR WHOEVER LISTENS NEXT.
	//
	// This is what the return value has always claimed - "false if no shell is attached or its
	// endpoint has closed", which is the condition `supervise` reads to end its boot round - and it
	// was not what the code did: every byte went into the ring whatever the console's state, `drain`
	// returned at its first line with nothing registered, and the byte waited there indefinitely for
	// the next attach. What that cost: a kernel test that types one character into a console nobody
	// had attached left it in the ring for two hundred tests, and the first line of the terminal test
	// that attached one next began with a character it had not typed.
	//
	// A byte that arrives with no console is dropped at the door, which is what a machine with no
	// terminal does with a keystroke.
	if listener().is_none() {
		return false;
	}
	// ALLOC-OK: `Pending` is a FIXED-CAPACITY ring - `entries[PENDING_BYTES]` - and its `push`
	// answers `false` when the ring is full rather than growing. Nothing here allocates; what the
	// scanner matched is the word.
	let accepted = PENDING.lock().push(serial, byte);
	drain();
	accepted
}

/// Hand the console everything it will take, oldest first, and stop at the first refusal.
///
/// CALLED FROM THE RECEIVE PATH AND FROM THE IDLE HOOK. The first is what delivers a byte
/// immediately; the second is what empties the ring once the shell has read what it was given, which
/// is the half that makes a burst arrive at all rather than merely arrive in order.
pub fn drain() {
	let Some(channel) = listener() else { return };
	loop {
		let Some((serial, byte)) = PENDING.lock().front() else { return };
		// A short heap drops the byte, which is what a full queue already does here.
		let Some(bytes) = (if serial { try_two(SERIAL_INPUT_MARKER, byte) } else { try_one(byte) }) else {
			PENDING.lock().pop();
			continue;
		};
		if channel.send(Message::new(bytes, Vec::new())).is_err() {
			// THE QUEUE IS FULL OR THE PEER IS GONE, and the byte STAYS at the front either way: a
			// full queue empties when the shell reads, and a peer that has gone takes the whole
			// registration with it at the next attach.
			return;
		}
		PENDING.lock().pop();
	}
}

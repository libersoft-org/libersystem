// driver.virtio-input - the userspace virtio-input keyboard driver.
//
// Unlike the polling drivers (blk/net/console, which busy-wait on a used ring),
// this one is interrupt-driven: DeviceManager hands it, after the usual "DEVICE"
// message, a second "IRQ" message carrying an Interrupt capability for the device's
// PCI line (DeviceManager acquired it, which routed the IOAPIC). The driver sets up
// the event virtqueue (queue 0), posts a pool of device-writable event buffers, and
// then blocks on the Interrupt. Each time the keyboard interrupts it reads the ISR
// register (deasserting the level-triggered line), drains the virtio_input_event
// records the device filled, translates key presses to characters, feeds them to
// the kernel console (driving the interactive shell), re-posts the buffers, and
// re-arms the interrupt.

#![no_std]
#![no_main]

mod common;
mod virtio;

use rt::*;

use crate::virtio::{Queue, Virtio};

// virtio_input_event record: { type: u16, code: u16, value: u32 } little-endian,
// 8 bytes. `type` EV_KEY carries a key event; `value` is 1 (press), 2 (autorepeat)
// or 0 (release).
const EV_KEY: u16 = 1;
const EVENT_SIZE: u64 = 8;

// Linux keycodes for the navigation keys (all above the 64-entry ASCII KEYMAP), which
// the driver turns into ANSI escape sequences rather than glyphs.
const KEY_HOME: u16 = 102;
const KEY_UP: u16 = 103;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_END: u16 = 107;
const KEY_DOWN: u16 = 108;
const KEY_DELETE: u16 = 111;
const KEY_PAGEUP: u16 = 104;
const KEY_PAGEDOWN: u16 = 109;

// Linux keycodes for the modifier keys (tracked across press/release, not emitted).
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_LEFTALT: u16 = 56;
const KEY_CAPSLOCK: u16 = 58;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_RIGHTALT: u16 = 100;

// Linux input keycode -> ASCII for the unshifted main block: the letter keys
// (lowercase), the digit row, and the few control keys a line shell needs. 0 means
// "no character" (modifiers, function keys, unmapped). Indices are KEY_* codes, e.g.
// KEY_A = 30 -> 'a', KEY_ENTER = 28 -> '\n', KEY_BACKSPACE = 14 -> 0x08.
#[rustfmt::skip]
const KEYMAP: [u8; 64] = [
	0,    0,    b'1', b'2',  b'3',  b'4', b'5', b'6', b'7', b'8',
	b'9', b'0', b'-', b'=',  0x08,  0x09, b'q', b'w', b'e', b'r',
	b't', b'y', b'u', b'i',  b'o',  b'p', b'[', b']', b'\n', 0,
	b'a', b's', b'd', b'f',  b'g',  b'h', b'j', b'k', b'l', b';',
	b'\'', b'`', 0,   b'\\', b'z',  b'x', b'c', b'v', b'b', b'n',
	b'm', b',', b'.', b'/',  0,     b'*', 0,    b' ', 0,    0,
	0,    0,    0,    0,
];

// The shifted layout (US): the same indices as KEYMAP, with Shift applied - capitals
// for letters and the shifted symbols for the digit row and punctuation. Caps Lock
// flips only the letters (handled in `layout`).
#[rustfmt::skip]
const KEYMAP_SHIFT: [u8; 64] = [
	0,    0,    b'!', b'@',  b'#',  b'$', b'%', b'^', b'&', b'*',
	b'(', b')', b'_', b'+',  0x08,  0x09, b'Q', b'W', b'E', b'R',
	b'T', b'Y', b'U', b'I',  b'O',  b'P', b'{', b'}', b'\n', 0,
	b'A', b'S', b'D', b'F',  b'G',  b'H', b'J', b'K', b'L', b':',
	b'"', b'~', 0,   b'|',  b'Z',  b'X', b'C', b'V', b'B', b'N',
	b'M', b'<', b'>', b'?',  0,     b'*', 0,    b' ', 0,    0,
	0,    0,    0,    0,
];

// The live modifier state, tracked across key press / release events.
#[derive(Default)]
struct Mods {
	shift: bool,
	ctrl: bool,
	alt: bool,
	caps: bool,
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. bring the device up (recv "DEVICE" + MMIO cap, map, negotiate to FEATURES_OK).
		let device: Virtio = common::bringup(bootstrap);
		// 2. receive our device's Interrupt capability ("IRQ" + handle).
		let irq: u64 = recv_irq(bootstrap);
		// 3. set up the event virtqueue (queue 0) and a pool of device-writable event
		//    buffers (one 8-byte slot per descriptor), post them all, and go live.
		let mut eventq: Queue = match device.setup_queue(0) {
			Some(q) => q,
			None => exit(),
		};
		// this queue is interrupt-driven (the device pushes key events to us).
		eventq.enable_interrupts();
		let (_pool, pool_virt, pool_phys): (u64, u64, u64) = match dma_buffer(4096) {
			Some(t) => t,
			None => exit(),
		};
		let slots: u16 = eventq.size();
		let mut id: u16 = 0;
		while id < slots {
			eventq.post_recv(id, pool_phys + id as u64 * EVENT_SIZE, EVENT_SIZE as u32);
			id += 1;
		}
		eventq.notify();
		device.driver_ok();
		// 4. report in. We do not stand on the bootstrap channel like the polling
		//    drivers - we stand on the interrupt in `event_loop`.
		send_blocking(bootstrap, b"driver.virtio-input: online", 0);
		event_loop(irq, &device, &mut eventq, pool_virt, pool_phys, slots)
	}
}

// Receive the "IRQ" message carrying this device's Interrupt capability, which
// DeviceManager acquired and transferred to us. Exits if it does not arrive.
unsafe fn recv_irq(bootstrap: u64) -> u64 {
	unsafe {
		let mut buf: [u8; 16] = [0u8; 16];
		match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if handle != 0 && len >= 3 && &buf[..3] == b"IRQ" => handle,
			_ => exit(),
		}
	}
}

// Block on the device interrupt forever: each time it fires, deassert the ISR line,
// drain every event buffer the device filled (translating key presses to console
// input), re-post the drained buffers, and re-arm the interrupt.
unsafe fn event_loop(irq: u64, device: &Virtio, eventq: &mut Queue, pool_virt: u64, pool_phys: u64, slots: u16) -> ! {
	unsafe {
		let mut mods: Mods = Mods::default();
		loop {
			// block until the keyboard raises its interrupt.
			wait(irq, 0);
			// read (and so acknowledge) the ISR-status register, deasserting the line.
			device.isr_ack();
			// drain the buffers the device filled, re-posting each as we go.
			while let Some((id, _len)) = eventq.take_used() {
				if id < slots {
					feed_event(pool_virt + id as u64 * EVENT_SIZE, &mut mods);
					eventq.post_recv(id, pool_phys + id as u64 * EVENT_SIZE, EVENT_SIZE as u32);
				}
			}
			eventq.notify();
			// clear the pending flag and unmask the GSI so the next press wakes us.
			interrupt_ack(irq);
		}
	}
}

// Decode the virtio_input_event at `addr`. Modifier keys update `mods` (tracked
// across press and release); an ordinary key press / autorepeat is turned into a
// character through the layout (Shift / Caps / Ctrl / Alt applied) and fed to the
// console, and a navigation key into its ANSI escape sequence.
unsafe fn feed_event(addr: u64, mods: &mut Mods) {
	unsafe {
		let kind: u16 = (addr as *const u16).read_volatile();
		let code: u16 = ((addr + 2) as *const u16).read_volatile();
		let value: u32 = ((addr + 4) as *const u32).read_volatile();
		if kind != EV_KEY {
			return;
		}
		// Modifier keys track press (1) / release (0); Caps Lock toggles on press. They
		// emit no character, so handle them before the press-only gate below.
		match code {
			KEY_LEFTSHIFT | KEY_RIGHTSHIFT => {
				mods.shift = value != 0;
				return;
			}
			KEY_LEFTCTRL | KEY_RIGHTCTRL => {
				mods.ctrl = value != 0;
				return;
			}
			KEY_LEFTALT | KEY_RIGHTALT => {
				mods.alt = value != 0;
				return;
			}
			KEY_CAPSLOCK => {
				if value == 1 {
					mods.caps = !mods.caps;
				}
				return;
			}
			_ => {}
		}
		// 1 = press, 2 = autorepeat (both emit); 0 = release is ignored.
		if value != 1 && value != 2 {
			return;
		}
		// PageUp / PageDown: Shift pages the console's own scrollback (a private control
		// byte the console intercepts); unshifted sends the standard ANSI sequence to the
		// client. Collapsing the chord here means the console needs no input escape parser.
		if code == KEY_PAGEUP || code == KEY_PAGEDOWN {
			if mods.shift {
				console_feed(if code == KEY_PAGEUP { 0x1e } else { 0x1f });
			} else {
				let seq: &[u8] = if code == KEY_PAGEUP { b"\x1b[5~" } else { b"\x1b[6~" };
				for &b in seq {
					console_feed(b);
				}
			}
			return;
		}
		// Navigation keys (arrows / Home / End / Delete) carry no ASCII glyph; emit the
		// ANSI escape sequence a serial terminal sends for them, so the shell's line
		// editor decodes the framebuffer keyboard and a serial terminal identically.
		if let Some(seq) = nav_sequence(code) {
			for &b in seq {
				console_feed(b);
			}
			return;
		}
		if code >= 64 {
			return;
		}
		let ch: u8 = layout(code, mods);
		if ch != 0 {
			// Alt makes the key a "meta" key: prefix the byte with ESC, the convention a
			// serial terminal uses (Alt+x -> ESC x).
			if mods.alt {
				console_feed(0x1b);
			}
			console_feed(ch);
		}
	}
}

// Resolve a main-block keycode to a character given the modifier state: Ctrl maps a
// letter (and `[ \ ]`) to its control code; otherwise Shift (and, for letters, Caps
// Lock) selects the shifted layout. Returns 0 for an unmapped key.
fn layout(code: u16, mods: &Mods) -> u8 {
	let base: u8 = KEYMAP[code as usize];
	if base == 0 {
		return 0;
	}
	if mods.ctrl {
		// Ctrl + letter -> 0x01..0x1a; Ctrl + [ \ ] -> 0x1b..0x1d; other combos ignored.
		let upper: u8 = base.to_ascii_uppercase();
		if upper.is_ascii_uppercase() {
			return upper - b'A' + 1;
		}
		return match base {
			b'[' => 0x1b,
			b'\\' => 0x1c,
			b']' => 0x1d,
			_ => 0,
		};
	}
	// Letters flip with Shift XOR Caps Lock; symbols only with Shift.
	let shifted: bool = if base.is_ascii_lowercase() { mods.shift ^ mods.caps } else { mods.shift };
	if shifted { KEYMAP_SHIFT[code as usize] } else { base }
}

// The ANSI escape sequence a navigation keycode maps to, or None for an ordinary key.
// These are the standard xterm sequences (ESC [ A/B/C/D for the arrows, ESC [ H / F
// for Home / End, ESC [ 3 ~ for Delete) the shell's editor already understands.
fn nav_sequence(code: u16) -> Option<&'static [u8]> {
	match code {
		KEY_UP => Some(b"\x1b[A"),
		KEY_DOWN => Some(b"\x1b[B"),
		KEY_RIGHT => Some(b"\x1b[C"),
		KEY_LEFT => Some(b"\x1b[D"),
		KEY_HOME => Some(b"\x1b[H"),
		KEY_END => Some(b"\x1b[F"),
		KEY_DELETE => Some(b"\x1b[3~"),
		_ => None,
	}
}

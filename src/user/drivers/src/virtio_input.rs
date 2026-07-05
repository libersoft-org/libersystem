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
mod keys;
mod virtio;

use rt::*;

use crate::keys::Mods;
use crate::virtio::{Queue, Virtio};

// virtio_input_event record: { type: u16, code: u16, value: u32 } little-endian,
// 8 bytes. `type` EV_KEY carries a key event; `value` is 1 (press), 2 (autorepeat)
// or 0 (release).
const EV_KEY: u16 = 1;
const EVENT_SIZE: u64 = 8;

// The other virtio_input_event types a pointer reports: EV_SYN (0) ends an event
// group, EV_REL (2) carries a relative axis delta (a mouse), EV_ABS (3) an absolute
// axis value (a tablet). The axis codes (REL_/ABS_ X = 0, Y = 1) and the button
// codes a mouse emits as EV_KEY (left/right/middle).
const EV_SYN: u16 = 0;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const AXIS_X: u16 = 0;
const AXIS_Y: u16 = 1;
const REL_WHEEL: u16 = 8;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;

// virtio-input config access: a select/subsel pair (config offsets 0/1) chooses a
// config block, whose byte length appears at offset 2 and whose data union starts at
// offset 8. Selecting EV_BITS with an event-type subsel reports whether the device
// emits that type (a non-zero size); selecting ABS_INFO with an axis subsel returns
// that axis's range (min/max/... u32 each) in the union. Used to self-identify a
// pointer and read its coordinate range.
const CFG_SELECT: u64 = 0;
const CFG_SUBSEL: u64 = 1;
const CFG_SIZE: u64 = 2;
const CFG_DATA: u64 = 8;
const CFG_EV_BITS: u8 = 0x11;
const CFG_ABS_INFO: u8 = 0x12;
// The normalized coordinate range pointer events are scaled into (0..=NORM_MAX), and
// the fallback clamp range for a relative (mouse) device that reports no ABS range.
const NORM_MAX: u32 = 0xffff;
const REL_RANGE: i32 = 0x7fff;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// 1. bring the device up (recv "DEVICE" + MMIO cap, map, negotiate to FEATURES_OK).
		let mut device: Virtio = common::bringup(bootstrap);
		// 2. self-identify. A virtio-input device is a keyboard or a pointer (mouse /
		//    tablet); the same binary drives both. The device's config tells which: a
		//    pointer reports EV_ABS (a tablet) or EV_REL (a mouse) events.
		let is_pointer: bool = ev_supported(&device, EV_ABS as u8) || ev_supported(&device, EV_REL as u8);
		// 3. receive our device's Interrupt capability ("IRQ" + handle).
		let irq: u64 = recv_irq(bootstrap);
		// route this device's interrupts to MSI-X table entry 0: DeviceManager acquired
		// an MSI-X Interrupt (device_msix_acquire), so the kernel has already programmed
		// the table and enabled MSI-X - we just point the device's config and queue
		// interrupts at that vector before setting the queue up.
		device.set_msix_vector(0);
		// 4. set up the event virtqueue (queue 0) and a pool of device-writable event
		//    buffers (one 8-byte slot per descriptor), post them all, and go live.
		let mut eventq: Queue = match device.setup_queue(0) {
			Some(q) => q,
			None => exit(),
		};
		// this queue is interrupt-driven (the device pushes events to us).
		eventq.enable_interrupts();
		// one event buffer per ring slot, so the pool is sized by the negotiated
		// ring - the device fills any slot it likes.
		let slots: u16 = eventq.size();
		let (_pool, pool_virt, pool_phys): (u64, u64, u64) = match dma_buffer(slots as u64 * EVENT_SIZE) {
			Some(t) => t,
			None => exit(),
		};
		let mut id: u16 = 0;
		while id < slots {
			eventq.post_recv(id, pool_phys + id as u64 * EVENT_SIZE, EVENT_SIZE as u32);
			id += 1;
		}
		eventq.notify();
		device.driver_ok();
		// 5. report in and run the matching event loop. We do not stand on the bootstrap
		//    channel like the polling drivers - we stand on the interrupt in the loop. The
		//    keyboard feeds key bytes to the console; the pointer maps motion/buttons to
		//    text-cell events it sends to InputService over a channel it hands up with its
		//    report (the keyboard's report carries no channel).
		if is_pointer {
			let (producer, consumer): (u64, u64) = match channel() {
				Some(pair) => pair,
				None => exit(),
			};
			let max_x: i32 = axis_max(&device, AXIS_X);
			let max_y: i32 = axis_max(&device, AXIS_Y);
			send_blocking(bootstrap, b"driver.virtio-pointer: online", consumer);
			pointer_loop(irq, &mut eventq, pool_virt, pool_phys, slots, producer, max_x, max_y)
		} else {
			send_blocking(bootstrap, b"driver.virtio-input: online", 0);
			event_loop(irq, &mut eventq, pool_virt, pool_phys, slots)
		}
	}
}

// Whether the device reports events of type `ev`: select its EV_BITS block for that
// type and read the block's byte length - a non-zero length means the device emits
// it. Used to tell a pointer (EV_ABS / EV_REL) from a keyboard (EV_KEY only).
unsafe fn ev_supported(device: &Virtio, ev: u8) -> bool {
	unsafe {
		device.config_write(CFG_SELECT, CFG_EV_BITS);
		device.config_write(CFG_SUBSEL, ev);
		device.config_read(CFG_SIZE) > 0
	}
}

// The maximum value an absolute axis reports, read from its ABS_INFO block (the union
// is min/max/fuzz/flat/res, u32 each; max is the second word). Returns 0 if the axis
// has no ABS_INFO (a relative device), so the caller falls back to a default range.
unsafe fn axis_max(device: &Virtio, axis: u16) -> i32 {
	unsafe {
		device.config_write(CFG_SELECT, CFG_ABS_INFO);
		device.config_write(CFG_SUBSEL, axis as u8);
		if device.config_read(CFG_SIZE) < 8 {
			return 0;
		}
		let mut max: u32 = 0;
		let mut i: u64 = 0;
		while i < 4 {
			max |= (device.config_read(CFG_DATA + 4 + i) as u32) << (8 * i);
			i += 1;
		}
		max as i32
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

// Block on the device interrupt forever: each time it fires, drain every event buffer
// the device filled (translating key presses to console input), re-post the drained
// buffers, and re-arm the interrupt. MSI-X is edge-triggered, so there is no ISR line
// to deassert and no GSI to unmask - the interrupt_ack just clears the pending flag.
unsafe fn event_loop(irq: u64, eventq: &mut Queue, pool_virt: u64, pool_phys: u64, slots: u16) -> ! {
	unsafe {
		let mut mods: Mods = Mods::default();
		loop {
			// block until the keyboard raises its MSI-X interrupt.
			wait(irq, 0);
			// drain the buffers the device filled, re-posting each as we go.
			while let Some((id, _len)) = eventq.take_used() {
				if id < slots {
					feed_event(pool_virt + id as u64 * EVENT_SIZE, &mut mods);
					eventq.post_recv(id, pool_phys + id as u64 * EVENT_SIZE, EVENT_SIZE as u32);
				}
			}
			eventq.notify();
			// clear the pending flag so the next press wakes us (edge-triggered: no source
			// to unmask).
			interrupt_ack(irq);
		}
	}
}

// The accumulated pointer state across an event group: the absolute position (device
// units, clamped to the axis range) and the button bitmask (bit 0 left, 1 right, 2
// middle). Compared between frames so an unchanged frame sends nothing.
#[derive(Default, Clone, Copy, PartialEq)]
struct Pointer {
	x: i32,
	y: i32,
	buttons: u8,
}

// Block on the pointer's interrupt forever: each time it fires, drain the event
// buffers the device filled, fold them into the current pointer state, and - once a
// frame completes (EV_SYN) and the state actually changed - send the normalized
// position and buttons to InputService (which maps them to the text-cell grid). The
// send coalesces motion within one interrupt (the latest position wins). Retires if
// InputService closes its end.
unsafe fn pointer_loop(irq: u64, eventq: &mut Queue, pool_virt: u64, pool_phys: u64, slots: u16, sink: u64, max_x: i32, max_y: i32) -> ! {
	unsafe {
		let bound_x: i32 = if max_x > 0 { max_x } else { REL_RANGE };
		let bound_y: i32 = if max_y > 0 { max_y } else { REL_RANGE };
		let mut state: Pointer = Pointer::default();
		let mut sent: Pointer = Pointer { x: -1, y: -1, buttons: 0 };
		loop {
			wait(irq, 0);
			let mut synced: bool = false;
			let mut wheel: i32 = 0;
			while let Some((id, _len)) = eventq.take_used() {
				if id < slots {
					if pointer_event(pool_virt + id as u64 * EVENT_SIZE, &mut state, &mut wheel, bound_x, bound_y) {
						synced = true;
					}
					eventq.post_recv(id, pool_phys + id as u64 * EVENT_SIZE, EVENT_SIZE as u32);
				}
			}
			eventq.notify();
			interrupt_ack(irq);
			// Send when a frame completed and either the position/buttons changed or the
			// wheel ticked (the wheel is a momentary delta, not part of the held state).
			if synced && (state != sent || wheel != 0) {
				let nx: u16 = normalize(state.x, bound_x);
				let ny: u16 = normalize(state.y, bound_y);
				let mut msg: [u8; 6] = [0u8; 6];
				msg[0..2].copy_from_slice(&nx.to_le_bytes());
				msg[2..4].copy_from_slice(&ny.to_le_bytes());
				msg[4] = state.buttons;
				msg[5] = wheel.clamp(-127, 127) as i8 as u8;
				if !send_blocking(sink, &msg, 0) {
					// InputService dropped its end: there is no consumer, so retire.
					exit();
				}
				sent = state;
			}
		}
	}
}

// Fold one virtio_input_event into the pointer state: an EV_ABS sets an axis, an
// EV_REL nudges it (clamped to the axis range), an EV_KEY toggles a button bit, and an
// EV_REL wheel tick accumulates into `wheel` (a momentary delta, reset after each send).
// Returns true on EV_SYN - the end of an event group, the point at which the
// accumulated state is a complete frame ready to send.
unsafe fn pointer_event(addr: u64, state: &mut Pointer, wheel: &mut i32, max_x: i32, max_y: i32) -> bool {
	unsafe {
		let kind: u16 = (addr as *const u16).read_volatile();
		let code: u16 = ((addr + 2) as *const u16).read_volatile();
		let value: i32 = ((addr + 4) as *const u32).read_volatile() as i32;
		match kind {
			EV_SYN => return true,
			EV_ABS => {
				if code == AXIS_X {
					state.x = value.clamp(0, max_x);
				} else if code == AXIS_Y {
					state.y = value.clamp(0, max_y);
				}
			}
			EV_REL => {
				if code == AXIS_X {
					state.x = (state.x + value).clamp(0, max_x);
				} else if code == AXIS_Y {
					state.y = (state.y + value).clamp(0, max_y);
				} else if code == REL_WHEEL {
					*wheel += value;
				}
			}
			EV_KEY => {
				let bit: u8 = match code {
					BTN_LEFT => 1,
					BTN_RIGHT => 2,
					BTN_MIDDLE => 4,
					_ => 0,
				};
				if bit != 0 {
					if value != 0 {
						state.buttons |= bit;
					} else {
						state.buttons &= !bit;
					}
				}
			}
			_ => {}
		}
		false
	}
}

// Scale an axis value in [0, max] into the normalized 0..=NORM_MAX range InputService
// maps to the text-cell grid. A zero or negative max (no range) yields 0.
fn normalize(v: i32, max: i32) -> u16 {
	if max <= 0 {
		return 0;
	}
	let v: i32 = v.clamp(0, max);
	((v as u64 * NORM_MAX as u64) / max as u64) as u16
}

// Decode the virtio_input_event at `addr` and feed a key event into the shared
// keyboard logic: modifier tracking, layout, navigation escapes and the console
// injection all live in `keys::feed_key`.
unsafe fn feed_event(addr: u64, mods: &mut Mods) {
	unsafe {
		let kind: u16 = (addr as *const u16).read_volatile();
		let code: u16 = ((addr + 2) as *const u16).read_volatile();
		let value: u32 = ((addr + 4) as *const u32).read_volatile();
		if kind != EV_KEY {
			return;
		}
		keys::feed_key(code, value, mods);
	}
}

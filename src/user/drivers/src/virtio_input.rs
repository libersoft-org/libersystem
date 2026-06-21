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

// Linux input keycode -> ASCII for the unshifted main block: the letter keys
// (lowercase), the digit row, and the few control keys a line shell needs. 0 means
// "no character" (modifiers, function keys, unmapped). Indices are KEY_* codes, e.g.
// KEY_A = 30 -> 'a', KEY_ENTER = 28 -> '\n', KEY_BACKSPACE = 14 -> 0x08. Shift and
// the other modifiers are not yet tracked (a later refinement).
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
		loop {
			// block until the keyboard raises its interrupt.
			wait(irq, 0);
			// read (and so acknowledge) the ISR-status register, deasserting the line.
			device.isr_ack();
			// drain the buffers the device filled, re-posting each as we go.
			while let Some((id, _len)) = eventq.take_used() {
				if id < slots {
					feed_event(pool_virt + id as u64 * EVENT_SIZE);
					eventq.post_recv(id, pool_phys + id as u64 * EVENT_SIZE, EVENT_SIZE as u32);
				}
			}
			eventq.notify();
			// clear the pending flag and unmask the GSI so the next press wakes us.
			interrupt_ack(irq);
		}
	}
}

// Decode the virtio_input_event at `addr` and, if it is a key press (or autorepeat)
// of a key we map, feed the resulting character to the kernel console.
unsafe fn feed_event(addr: u64) {
	unsafe {
		let kind: u16 = (addr as *const u16).read_volatile();
		let code: u16 = ((addr + 2) as *const u16).read_volatile();
		let value: u32 = ((addr + 4) as *const u32).read_volatile();
		if kind != EV_KEY {
			return;
		}
		// 1 = press, 2 = autorepeat (both emit); 0 = release is ignored.
		if value != 1 && value != 2 {
			return;
		}
		if code >= 64 {
			return;
		}
		let ch: u8 = KEYMAP[code as usize];
		if ch != 0 {
			console_feed(ch);
		}
	}
}

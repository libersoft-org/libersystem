// driver.virtio-console - the userspace virtio serial/console driver.
//
// IT NEGOTIATES MULTIPORT AND FALLS BACK WITHOUT IT. A device that offers
// `VIRTIO_CONSOLE_F_MULTIPORT` has a CONTROL QUEUE - queues 2 and 3 - and does not open port 0 until
// the driver has said `DEVICE_READY` and answered the port's `PORT_ADD` with `PORT_READY`; a device
// that does not offer it is a single console whose port is always open. Both are ordinary here, and
// which one this is is in the report.
//
// WHY THE FEATURE IS WORTH NEGOTIATING AT ALL, on a device this system already drives: without it
// there is no control queue, so a GENERIC port cannot be opened - and the harness says so where it
// attaches the development channel as a `virtconsole` rather than a `virtserialport`, at the cost of
// a UEFI firmware preamble on the channel. The control half is what a named provisioning or
// diagnostic port needs, and it is what this driver now has.
//
// THE DECISIONS ARE NOT IN THIS FILE. Which queues a port owns, what a control message means and
// what is refused are in `drivers::console`, with the hostile cases as host fixtures: every input
// here is bytes the device chose.
//
// AND EVERY OPEN GENERIC PORT IS A PUBLISHED BYTE STREAM. The control half alone opens ports nothing
// can reach; what makes one usable is the same `console-stream` the development channel serves, and
// it is literally the same code - `drivers::serial_port` - rather than a second implementation of a
// byte-stream contract whose divergence would be invisible. One publication per open port, NAMED
// with the name the host gave the port, because a device with two ports publishes two providers of
// one kind and every other field the catalogue carries is identical for both.
//
// THE CONSOLE PORT IS NOT ONE OF THEM. Port 0 of a virtio-console device is the guest's console: its
// transmit queue carries the banner below and, on the machines this system boots, the boot log. A
// second writer on it would interleave with that, and a consumer reading it would be reading the
// log. It stays what it is; the ports this publishes are the GENERIC ones the control queue opened.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use rt::*;

use crate::virtio::Queue;
use drivers::console::{self, Action, Control, Ports, event};
use drivers::serial_port::{self, Stream};
use drivers::{common, virtio};

// The line the driver writes over the console transmit queue.
const BANNER: &[u8] = b"virtio-console driver online: console output over the virtqueue\n";

// One control buffer: a header and a name. The name cap is the module's, and the header is eight
// bytes, so a slot holds any message the specification defines.
const CONTROL_SLOT: u64 = 64;
// How many receive slots the control queue is given. A control message is a rare event - a port
// arriving, being named, opening - so this is a depth rather than a throughput.
const CONTROL_SLOTS: u16 = 8;
// THE HANDSHAKE IS BOUNDED. A device that never answers must leave this driver online with the
// console it already has rather than spinning: the ports it did not announce are ports nobody can
// use, which is a smaller failure than a driver that never reports at all.
const CONTROL_ROUNDS: u32 = 4096;

// THE MOST PORTS THIS DRIVER PUBLISHES, which is the bring-up protocol's bound rather than a choice
// made here: `MAX_INITIAL_OFFERS` publications travel with one handshake, and a fifth would be
// refused with its channel closed. A device that opens more generic ports than that gets the first
// four served, and the report says how many of the open ones that was.
const PUBLISHED_PORTS: usize = driver_protocol::MAX_INITIAL_OFFERS;

// ONE PUBLISHED PORT: the byte stream serving it, the port it is, the name it went into the
// catalogue under and the endpoint the first consumer arrives on.
//
// THE NAME IS COPIED OUT OF `Ports` rather than borrowed from it. `Ports` goes on being written to
// while the handshake runs - a `PORT_NAME` for a later port, a `PORT_OPEN` for this one closing -
// and a publication's name is what it was published as, which cannot change afterwards without the
// catalogue and the driver disagreeing about what a consumer selected.
struct Published<'a> {
	stream: Stream<'a>,
	name: [u8; console::MAX_NAME],
	name_len: usize,
	// The two ends of this publication's first connection: `near` is what this driver serves, `far`
	// is what travels to the manager with the online report and reaches the first consumer that asks
	// for it. Later consumers arrive as a `CONNECT` naming this publication's token.
	near: u64,
	far: u64,
}

impl Published<'_> {
	fn name(&self) -> &[u8] {
		&self.name[..self.name_len]
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, resources) = common::handshake(bootstrap);
		let mut device: virtio::Virtio = common::bringup_bound(bootstrap, &bind, &resources, console::FEATURE_MULTIPORT);
		let multiport: bool = device.features_word0() & console::FEATURE_MULTIPORT != 0;
		// THE INTERRUPT IS WHAT MAKES A PORT SERVABLE, and a driver without one publishes nothing.
		//
		// A byte stream has to block on arriving bytes: polling one would spin for the guest's whole
		// life, which under a cooperative scheduler starves every thread that still has boot work.
		// The manager grants this driver an MSI-X vector; a binding that did not would leave the
		// console working exactly as it did before ports were served at all, which is the honest
		// degradation rather than a wait on a handle that is not one.
		let irq: u64 = resources.irq;
		if irq != 0 {
			device.set_msix_vector(0);
		}
		// single-port virtio-console: receiveq = 0, transmitq = 1. The control pair follows them and
		// exists only under MULTIPORT.
		let _rx = device.setup_queue(0);
		let tx = device.setup_queue(1);
		let mut ports = Ports::new(if multiport { max_ports(&device) } else { 1 });
		let control = if multiport { Some((device.setup_queue(console::CONTROL_RECEIVE_QUEUE), device.setup_queue(console::CONTROL_TRANSMIT_QUEUE))) } else { None };
		device.driver_ok();

		// THE HANDSHAKE, IN THE ORDER THE DEVICE EXPECTS IT: the driver says it is ready, the device
		// announces its ports, and each announced port is answered before the device will open it.
		// Every generic port that opens becomes a byte stream here, because the queues a port owns
		// are what the handshake has just decided: setting them all up in advance would be
		// programming a device about ports it does not have.
		let mut published: Vec<Published> = Vec::new();
		let discovered: u32 = match control {
			Some((Some(mut control_rx), Some(control_tx))) => run_handshake(&device, irq, &mut control_rx, &control_tx, &mut ports, &mut published),
			_ => 0,
		};

		// KEPT, NOT DROPPED: the queue's capability is what `finish_stop` hands to `device_quiesced`
		// when the manager asks this driver to stop, so the kernel may reclaim the frames and masked
		// vectors this binding was holding.
		let ok: bool = match &tx {
			Some(q) => write_console(q, BANNER),
			None => false,
		};
		let queue_capability: u64 = device.capability;
		let mut line = [0u8; 64];
		let mut detail = [0u8; 40];
		let n = describe_ports(&mut detail, multiport, discovered, ports.open_count(), published.len(), ok);
		let n = common::describe(&mut line, b"virtio-console", &device, &detail[..n]);
		// ONE OFFER PER PUBLISHED PORT, EACH UNDER THE PORT'S OWN NAME. The token is the position in
		// this list, which `online_named` assigns and which is therefore the index back into
		// `published` - that is what lets a `CONNECT` or a departure name the port it is about.
		let offers: Vec<(u16, u64, &[u8])> = published.iter().map(|entry| (driver_protocol::provider::CONSOLE_BYTES, entry.far, entry.name())).collect();
		let online: bool = common::online_named(bootstrap, &bind, &line[..n], &offers);
		drop(offers);
		if !online {
			exit();
		}
		pump(&bind, irq, bootstrap, &mut published, queue_capability)
	}
}

// Move bytes between every published port and the consumers the catalogue gave them to.
//
// ONE LOOP FOR ALL OF THEM AND NOT A THREAD EACH. They share one device, one interrupt and one
// transmit-completion path, so threads would contend over exactly what they are meant to separate -
// and a port with no consumer costs this loop one `take_used` that answers None.
fn pump(bind: &common::Bind, irq: u64, bootstrap: u64, published: &mut [Published], capability: u64) -> ! {
	// THE TOKEN IS THE POSITION IN THE OFFER LIST, which `online_named` assigned and which is
	// therefore the index back into `published` - that is what lets a `CONNECT`, a request or a
	// departure name the port it is about.
	let mut offers: [(u16, u64); PUBLISHED_PORTS] = [(0, 0); PUBLISHED_PORTS];
	for (token, slot) in offers.iter_mut().enumerate().take(published.len()) {
		*slot = (token as u16, published[token].near);
	}
	let mut serving: common::Serving = common::Serving::from_offers(&offers[..published.len()]);
	let mut buffers: serial_port::Buffers = serial_port::Buffers::default();
	let devices: [u64; 1] = [irq];
	let waits: &[u64] = if irq != 0 { &devices } else { &[] };
	loop {
		// Anything any port has already been handed goes out before this thread parks, so a
		// completion that arrived with an interrupt another port's wait consumed is not left
		// waiting for the next one. They share a single MSI-X vector, which is exactly why.
		let mut worked: bool = false;
		for entry in published.iter_mut() {
			if entry.stream.drain(bind, bootstrap, &mut buffers) {
				worked = true;
			}
		}
		if worked {
			continue;
		}
		match common::wait_providers_or_answer(bootstrap, bind, &mut serving, waits) {
			None => ended(bootstrap, bind, capability),
			// A REPLACEMENT CONSUMER on one port, and that port's session before it is over. The
			// others are untouched: a restarted consumer of the diagnostic port has nothing to do
			// with whoever is reading another one.
			Some(common::ProviderReady::Connected(index)) => {
				if let Some(entry) = published.get_mut(serving.token_at(index) as usize) {
					entry.stream.reset();
				}
			}
			Some(common::ProviderReady::Consumer(index)) => {
				let token: u16 = serving.token_at(index);
				let Some(entry) = published.get_mut(token as usize) else {
					// A token no publication of this driver's owns. It cannot be served and
					// cannot be reported as a departure of anything, so the endpoint goes.
					serving.close_at(index);
					continue;
				};
				if !entry.stream.serve(&mut serving, index, bind, bootstrap, &mut buffers) {
					let token: u16 = serving.close_at(index);
					entry.stream.reset();
					// THE MANAGER'S COUNT IS WHAT ADMITS THE REPLACEMENT. Each of these
					// publications admits one consumer, so an unreported departure would refuse
					// the next `open` for the life of the binding: the port would be alive with
					// nobody able to reach it.
					if !common::disconnected(bootstrap, bind, token) {
						ended(bootstrap, bind, capability);
					}
				}
			}
			Some(common::ProviderReady::Device(_)) => {
				// Read the ISR to deassert the device's level-triggered INTx line before acking
				// (a harmless zero read on MSI-X, which is edge-triggered). The vector is the
				// device's rather than a port's, so every port reaps.
				if let Some(entry) = published.first() {
					let _ = entry.stream.device().read_isr();
				}
				interrupt_ack(irq);
				for entry in published.iter_mut() {
					entry.stream.reclaim();
				}
			}
		}
	}
}

// The driver is finished. A stop that was ASKED FOR is certified after the device is quiet, which is
// what lets the kernel reclaim the frames and masked vectors this binding held; a channel that
// simply closed has nothing to certify.
fn ended(bootstrap: u64, bind: &common::Bind, capability: u64) -> ! {
	if common::stop_requested() {
		common::finish_stop(bootstrap, bind, capability, common::quiesce_virtio());
	}
	exit()
}

// `max_nr_ports` from the device configuration: two 16-bit fields precede it.
//
// WHAT THE DEVICE SAYS AND NOT WHAT THIS DRIVER TRACKS. `Ports::new` clamps it, because the number is
// one the device writes into its own configuration and sizing anything from it would let the device
// choose how much this driver allocates.
unsafe fn max_ports(device: &virtio::Virtio) -> u32 {
	let mut value: u32 = 0;
	for index in 0..4u64 {
		value |= (device.config_read(4 + index) as u32) << (index * 8);
	}
	value
}

// Say `DEVICE_READY`, then answer what the device announces until it goes quiet. Returns how many
// ports were added.
unsafe fn run_handshake<'a>(device: &'a virtio::Virtio, irq: u64, control_rx: &mut Queue, control_tx: &Queue, ports: &mut Ports, published: &mut Vec<Published<'a>>) -> u32 {
	unsafe {
		let Some((pool, virt, _)) = dma_buffer_for(device.capability, CONTROL_SLOTS as u64 * CONTROL_SLOT) else { return 0 };
		let mut slot: u16 = 0;
		while slot < CONTROL_SLOTS {
			let phys = dma_buffer_phys_at(pool, slot as u64 * CONTROL_SLOT);
			control_rx.post_recv(slot, phys, CONTROL_SLOT as u32);
			slot += 1;
		}
		control_rx.notify();
		send_control(device, control_tx, Control { id: 0, event: event::DEVICE_READY, value: 1 });

		let mut added: u32 = 0;
		let mut quiet: u32 = 0;
		for _ in 0..CONTROL_ROUNDS {
			let Some((id, len)) = control_rx.take_used() else {
				quiet += 1;
				// A DEVICE WITH NOTHING MORE TO SAY IS THE ORDINARY END OF THIS. What is NOT ordinary
				// is waiting for one for ever, which is why the round count above is a bound.
				if quiet > 64 {
					break;
				}
				continue;
			};
			quiet = 0;
			let base = virt + id as u64 * CONTROL_SLOT;
			let length = (len as u64).min(CONTROL_SLOT) as usize;
			let mut message = [0u8; CONTROL_SLOT as usize];
			for offset in 0..length {
				message[offset] = ((base + offset as u64) as *const u8).read_volatile();
			}
			match ports.apply(&message[..length]) {
				// A PORT IS ANSWERED BEFORE THE DEVICE WILL OPEN IT, which is the whole reason the
				// control queue exists: an unanswered `PORT_ADD` is a port that never opens.
				Action::Add(port) => {
					added += 1;
					send_control(device, control_tx, Control { id: port, event: event::PORT_READY, value: 1 });
				}
				// AND AN OPEN PORT IS ANSWERED TOO, because the host end has to know the guest end is
				// listening before it will deliver anything to it.
				Action::Open(port) => {
					send_control(device, control_tx, Control { id: port, event: event::PORT_OPEN, value: 1 });
					// AND A GENERIC PORT BECOMES A PUBLISHED BYTE STREAM, with one line written
					// to it so that the host end of THAT port - and no other - carries a byte this
					// driver put there. That stamp is the smallest observable effect the feature
					// has: a single-port driver cannot reach this port at all, so a line on its
					// own chardev is proof the control queue worked, whether or not a consumer
					// ever attaches. The console port is left alone: its first bytes are the
					// banner below, and a second writer on it would interleave with the console.
					if !ports.get(port).map(|state| state.console).unwrap_or(false) {
						publish_port(device, irq, port, ports, published);
					}
				}
				Action::Close(port) => send_control(device, control_tx, Control { id: port, event: event::PORT_OPEN, value: 0 }),
				_ => {}
			}
			let phys = dma_buffer_phys_at(pool, id as u64 * CONTROL_SLOT);
			control_rx.post_recv(id, phys, CONTROL_SLOT as u32);
			control_rx.notify();
		}
		added
	}
}

// THE LINE A GENERIC PORT GETS, which names the port it went to so a capture cannot be mistaken for
// the console's.
const PORT_STAMP: &[u8] = b"virtio-console: generic port open, this is port ";

// Serve an open generic port as a byte stream, and write one line to it.
//
// THE QUEUES ARE SET UP HERE AND NOT AT BRING-UP, because which queues exist is what the control
// handshake has just decided: a port that was never added has no queues to set up, and a driver that
// set up all of them anyway would be programming a device about ports it does not have.
//
// THE STAMP GOES OUT ON THE SAME ASYNCHRONOUS PATH every later write uses, so the pump reaps its
// completion like any other. A synchronous `submit` on this queue would busy-poll the used ring
// without touching the indices the reaper accounts against, and the first real write would then be
// looking at a completion it was never told to expect.
unsafe fn publish_port<'a>(device: &'a virtio::Virtio, irq: u64, port: u32, ports: &Ports, published: &mut Vec<Published<'a>>) {
	unsafe {
		// A PORT WITH NO INTERRUPT IS NOT SERVED - see `__user_main` - and neither is one past the
		// handshake's publication bound. Either way the port stays open at the device, which is
		// honestly what it then is: a port nothing in this image reads, rather than a driver that
		// failed to start.
		if irq == 0 || published.len() >= PUBLISHED_PORTS {
			return;
		}
		let Some((receive, transmit)) = Ports::queue_pair(port) else { return };
		let Some(mut stream) = Stream::open(device, irq, receive, transmit) else { return };
		let mut line = [0u8; 64];
		let mut written = 0usize;
		push_bytes(&mut line, &mut written, PORT_STAMP);
		push_number(&mut line, &mut written, port as u64);
		push_bytes(&mut line, &mut written, b"\n");
		stream.stamp(&line[..written]);
		let Some((near, far)) = channel() else { return };
		// THE NAME AS IT STANDS AT THE MOMENT OF PUBLICATION, copied rather than borrowed. The
		// specification sends `PORT_NAME` before `PORT_OPEN`, so an open port the host named has its
		// name here; one the host did not name is published unnamed, and a consumer selecting by
		// name simply does not match it. `Ports` goes on being written to afterwards, and what a
		// publication was published AS cannot change without the catalogue and this driver
		// disagreeing about what a consumer selected.
		let mut name = [0u8; console::MAX_NAME];
		let given: &[u8] = ports.get(port).map(|state| state.name()).unwrap_or(&[]);
		name[..given.len()].copy_from_slice(given);
		published.push(Published { stream, name, name_len: given.len(), near, far });
	}
}

// One control message out, which is eight bytes and no payload.
unsafe fn send_control(device: &virtio::Virtio, control_tx: &Queue, message: Control) {
	unsafe {
		let Some((_handle, virt, phys)) = dma_buffer_for(device.capability, CONTROL_SLOT) else { return };
		let mut bytes = [0u8; Control::LEN];
		if message.encode(&mut bytes).is_none() {
			return;
		}
		for (offset, byte) in bytes.iter().enumerate() {
			((virt + offset as u64) as *mut u8).write_volatile(*byte);
		}
		let _ = control_tx.submit(&[(phys, Control::LEN as u32, false)]);
	}
}

// What the report says about the ports, which is the difference between a device with a control
// queue and one without.
fn describe_ports(out: &mut [u8], multiport: bool, discovered: u32, open: usize, served: usize, tx_ok: bool) -> usize {
	let mut written = 0usize;
	// SHORT, BECAUSE THE LINE IT GOES INTO IS SIXTY-FOUR BYTES and the address and the driver name
	// are most of it: `multiport 1/1` is ports announced over ports open, which is the pair a reader
	// needs - a device that announced three and opened one is a different picture from one that
	// opened all three.
	if multiport {
		push_bytes(out, &mut written, b"multiport ");
		push_number(out, &mut written, discovered as u64);
		push_bytes(out, &mut written, b"/");
		push_number(out, &mut written, open as u64);
		push_bytes(out, &mut written, b", ");
		// AND HOW MANY OF THE OPEN ONES ARE SERVED, which is not the same number: the console port
		// is never published, and a device that opened more generic ports than one handshake may
		// carry publications for has the rest open and unread. A reader comparing the two sees that
		// rather than having to infer it.
		push_number(out, &mut written, served as u64);
		push_bytes(out, &mut written, b" served, ");
	} else {
		push_bytes(out, &mut written, b"one port, ");
	}
	push_bytes(out, &mut written, if tx_ok { b"tx ok" } else { b"tx failed" });
	written
}

fn push_bytes(out: &mut [u8], written: &mut usize, bytes: &[u8]) {
	for byte in bytes {
		if let Some(slot) = out.get_mut(*written) {
			*slot = *byte;
			*written += 1;
		}
	}
}

fn push_number(out: &mut [u8], written: &mut usize, value: u64) {
	if value == 0 {
		if let Some(slot) = out.get_mut(*written) {
			*slot = b'0';
			*written += 1;
		}
		return;
	}
	let mut digits = [0u8; 20];
	let mut count = 0usize;
	let mut value = value;
	while value > 0 {
		digits[count] = b'0' + (value % 10) as u8;
		value /= 10;
		count += 1;
	}
	while count > 0 {
		count -= 1;
		if let Some(slot) = out.get_mut(*written) {
			*slot = digits[count];
			*written += 1;
		}
	}
}

// Write `bytes` to the console over the transmit queue (virtio-console transmit
// buffers are raw bytes, no header).
unsafe fn write_console(tx: &Queue, bytes: &[u8]) -> bool {
	unsafe {
		let (_handle, virt, phys): (u64, u64, u64) = match dma_buffer_for(tx.capability, 4096) {
			Some(t) => t,
			None => return false,
		};
		let n: usize = if bytes.len() < 4096 { bytes.len() } else { 4096 };
		for (i, &b) in bytes[..n].iter().enumerate() {
			((virt + i as u64) as *mut u8).write_volatile(b);
		}
		tx.submit(&[(phys, n as u32, false)]).is_some()
	}
}

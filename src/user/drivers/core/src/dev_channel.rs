// The development channel port. Its report names the DEVICE - `virtio-console`, with the PCI address
// that tells it from the other one - because being the development channel is what this driver does
// and not what the device is. See the `describe` call below.
//
// The host attaches a SECOND single-port virtio-serial device, pinned to a fixed PCI
// address, and DeviceManager binds this program to it instead of the console driver. That
// is the whole point of a second device rather than a second port: no MULTIPORT
// negotiation, no control-queue port discovery, and the console keeps its own device, so
// either channel can fail without taking the other down.
//
// This program is a transport and nothing else. It moves raw bytes between the port and the
// consumers that reach it THROUGH THE PROVIDER CATALOGUE; the framing, the session and the artifact
// registry all live above it. A driver holds a device capability and an MMIO mapping, and that is
// not where megabytes of unverified bytes a host streamed in belong. Nothing here knows what a frame
// is, which is why the multiport console driver now carries the same protocol on a named port
// without anything above it changing: the port itself is `drivers::serial_port`, shared by both.
//
// AND THE WIRE ABOVE IT IS A CONTRACT NOW, not three conventions (2026-09-13). This driver used to
// send whatever a receive buffer held as an untyped message, read frames back the same way, say "the
// port would not take that" with an EMPTY message, and take a replacement consumer as a handle under
// a `BYTES` tag on its own bootstrap. None of that carried a version, so the two ends could only
// agree by both being edited at once - and a byte stream is the one wire whose disagreement is
// invisible: mismatched bytes are refused nowhere, they arrive somewhere else in the reader. What it
// serves now is `liber:device@1`'s `console-stream`, whose `attach` settles a version before a byte
// moves, whose `write` ANSWERS - `again` is a host that stopped reading - and whose `receive` hands
// back a stream endpoint. The replacement consumer arrives as an ordinary `CONNECT` from the
// manager, which is what every other provider in this tree already does.
//
// Both queues are interrupt-driven. The receive side because a control channel is idle
// almost all the time, and a driver that polled it would spend the guest's whole life
// spinning - under a cooperative scheduler that is not a waste of cycles but a correctness
// problem, because a runnable spinner starves the threads that still have boot work to do.
// The transmit side because its completions are the only proof the buffer came back.

#![no_std]
#![no_main]

extern crate alloc;

use rt::*;

use drivers::serial_port::{self, Stream};
use drivers::{common, virtio};

// THE TOKEN THIS DRIVER'S ONE PUBLICATION IS OFFERED UNDER. `online_named` numbers offers by their
// position in the list it is given, and this driver offers exactly one - so the token is zero, and
// naming it is what keeps `disconnected` from reporting a consumer of some other publication.
const CONSOLE_TOKEN: u16 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// Bring the device up and take its MSI-X Interrupt, then route this device's
		// interrupts to table entry 0 (DeviceManager acquired it and the kernel programmed
		// the table), before the queues are set up so each queue is told the vector.
		let (bind, resources) = common::handshake(bootstrap);
		let mut device: virtio::Virtio = common::bringup_bound(bootstrap, &bind, &resources, 0);
		let irq: u64 = resources.irq;
		device.set_msix_vector(0);
		// Single port, exactly like the console device: receiveq = 0, transmitq = 1.
		let Some(mut stream) = Stream::open(&device, irq, 0, 1) else { exit() };
		device.driver_ok();
		// Create the first connection and hand its far end up with the online report, the way every
		// driver with a consumer above it does. The manager publishes it in the catalogue and gives
		// it to the first consumer that asks for this kind; later consumers arrive as `CONNECT`.
		let (bytes, bytes_far): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => exit(),
		};
		// ONE NAME FOR ONE DEVICE, AND THE ADDRESS IS WHAT TELLS TWO OF THEM APART.
		//
		// The line said `driver.dev-channel: online (transport)` - a ROLE where the device type
		// belongs, and no address at all - so the two virtio-console functions of one machine were
		// told apart by nothing, and the same PCI function was called `virtio-console` in the device
		// and DMA inventories and `dev-channel` in the driver report. A reader joining the two by
		// name could not. The address was added first and the name was left, which fixed half of it.
		//
		// The registry binds this driver to a SECOND virtio-console function; being the development
		// channel is what this driver DOES, not what the device IS, and the address is the
		// distinguisher the report already carries.
		let mut line = [0u8; 64];
		let n = common::describe(&mut line, b"virtio-console", &device, b"transport");
		// THE PUBLICATION IS NAMED, and the name is what a consumer selects on rather than the kind.
		//
		// A development image now holds TWO publishers of `console-bytes`: this one, and a port of
		// the multiport console driver. Both speak the same contract at the same version, so the
		// handshake cannot separate them - it was never able to, and until there was a second
		// publisher nothing depended on it being able to. The name can, and the agent above this
		// driver asks for this one by it.
		common::online_named(bootstrap, &bind, &line[..n], &[(driver_protocol::provider::CONSOLE_BYTES, bytes_far, driver_protocol::provider::DEV_CHANNEL_NAME)]);
		pump(&bind, irq, bootstrap, bytes, &mut stream)
	}
}

// The transmit side of the port, and the reason it is not a simple synchronous write. The
// host end can stop reading at any moment - a killed tool, a full socket buffer, a terminal
// that went away - and QEMU responds by ceasing to consume the transmit queue rather than
// discarding what it cannot deliver. The device therefore keeps ownership of the buffer it
// was handed. A polled write that gave up on such a buffer would leave the device holding a
// descriptor that the next write overwrites, which corrupts the ring permanently and takes
// the channel down for the rest of the guest's life. So completions are reaped explicitly
// and the buffer is never refilled until the device gives it back. Waiting for that is
// bounded; the port recovers by itself once the host reads again, precisely because the
// descriptor was never reused behind the device's back.

fn pump(bind: &common::Bind, irq: u64, bootstrap: u64, bytes: u64, stream: &mut Stream) -> ! {
	let mut serving: common::Serving = common::Serving::from_offers(&[(CONSOLE_TOKEN, bytes)]);
	let mut buffers: serial_port::Buffers = serial_port::Buffers::default();
	loop {
		// Anything the device has already handed back goes out before this thread parks, so a
		// completion that arrived with an interrupt somebody else consumed is not left waiting
		// for the next one.
		if stream.drain(bind, bootstrap, &mut buffers) {
			continue;
		}
		match common::wait_providers_or_answer(bootstrap, bind, &mut serving, &[irq]) {
			None => ended(bootstrap, bind, stream.capability()),
			// A REPLACEMENT CONSUMER, and the session before it is over. This is the whole of
			// what used to be the `BYTES` tag and the `adopt` loop underneath it: the manager
			// mints the pair, the driver is told, and nothing here has to know that the process
			// above it was restarted.
			Some(common::ProviderReady::Connected(_)) => stream.reset(),
			Some(common::ProviderReady::Consumer(index)) => {
				if !stream.serve(&mut serving, index, bind, bootstrap, &mut buffers) {
					let token: u16 = serving.close_at(index);
					stream.reset();
					// THE MANAGER'S COUNT IS WHAT ADMITS THE REPLACEMENT. This provider admits
					// one consumer, so an unreported departure would refuse the next `open`
					// for the life of the binding - the port would be alive with nobody able
					// to reach it, which is exactly the failure the old private hand-off had
					// no way to express either.
					if !common::disconnected(bootstrap, bind, token) {
						ended(bootstrap, bind, stream.capability());
					}
				}
			}
			Some(common::ProviderReady::Device(_)) => {
				// Read the ISR to deassert the device's level-triggered INTx line before
				// acking (a harmless zero read on MSI-X, which is edge-triggered).
				let _ = stream.device().read_isr();
				interrupt_ack(irq);
				stream.reclaim();
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

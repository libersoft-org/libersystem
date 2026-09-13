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
// is, so a later multiport driver can carry the same protocol on a named port without anything above
// it changing.
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

use alloc::vec::Vec;
use rt::*;

use crate::virtio::{Queue, Virtio};
use drivers::{common, virtio};
use proto::system::{ConsoleAttachment, ConsoleChunk, Error, console_stream};
use wire::Handles;

// The receive pool: enough slots that a burst of host writes is absorbed without the device
// running out of buffers between two interrupts, each large enough that an ordinary control
// frame arrives in one.
const RX_SLOTS: u16 = 8;
const RX_SLOT: u64 = 4096;

// The largest transmit buffer this driver needs. The protocol above defines a larger frame than one
// `write` may carry, and that is deliberate: a byte stream has no frame boundaries, so a consumer
// with more to say than the contract's bound simply says it in two writes.
const MAX_FRAME: usize = 65536;

// THE CONTRACT'S OWN BOUND, and it is not a choice. A `list<u8>` is length-prefixed with a `u16` on
// this wire, so 65535 is the largest one that can be expressed at all - the schema refuses a
// `@bound` above it rather than letting a driver promise a frame the encoder could not write.
const MAX_WRITE: usize = 65535;

// The version of `console-stream` this driver speaks. A consumer that asks for another one is
// refused: guessing at framing is exactly what the handshake exists to stop.
const CONTRACT_VERSION: u32 = 1;

// How long a write may wait for the transmit buffer to come back, in scheduler ticks
// (100 Hz). The host end can stop reading at any moment, and QEMU then stops consuming the
// transmit queue rather than discarding what it cannot deliver, so the buffer stays with the
// device.
//
// This matches the session's own idle deadline rather than being an independent, shorter
// guess. A shorter one drops replies from a host that is merely slow to read - a host
// pipelining up to the advertised outstanding bound can easily have more reply bytes in
// flight than a socket buffer holds, and losing its answers for reading a moment late is not
// a bound, it is data loss. A host that has genuinely gone is caught by the same window the
// session uses to decide the same thing, which is the only question this deadline is really
// asking.
const TX_DRAIN_TICKS: u64 = 3000;

// THE TOKEN THIS DRIVER'S ONE PUBLICATION IS OFFERED UNDER. `online` numbers offers by their
// position in the list it is given, and this driver offers exactly one - so the token is zero, and
// naming it is what keeps `disconnected` from reporting a consumer of some other publication.
const CONSOLE_TOKEN: u16 = 0;

// How many chunks a consumer may fall behind by before the port stops taking new ones for it. The
// receive pool is the real bound on what can be in flight at once; twice it leaves room for the
// consumer to be between two reads without the ring stalling.
const STREAM_DEPTH: u64 = RX_SLOTS as u64 * 2;

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// Bring the device up and take its MSI-X Interrupt, then route this device's
		// interrupts to table entry 0 (DeviceManager acquired it and the kernel programmed
		// the table), before the queues are set up so each queue is told the vector.
		let (bind, resources) = common::handshake(bootstrap);
		let mut device: Virtio = common::bringup_bound(bootstrap, &bind, &resources, 0);
		let irq: u64 = resources.irq;
		device.set_msix_vector(0);
		// Single port, exactly like the console device: receiveq = 0, transmitq = 1.
		let mut rx: Queue = match device.setup_queue(0) {
			Some(q) => q,
			None => exit(),
		};
		let mut tx: Queue = match device.setup_queue(1) {
			Some(q) => q,
			None => exit(),
		};
		rx.enable_interrupts();
		tx.enable_interrupts();
		let (rxpool, rx_virt, _): (u64, u64, u64) = match dma_buffer_for(device.capability, RX_SLOTS as u64 * RX_SLOT) {
			Some(t) => t,
			None => exit(),
		};
		let (_txbuf, tx_virt, tx_phys): (u64, u64, u64) = match dma_buffer_for(device.capability, MAX_FRAME as u64) {
			Some(t) => t,
			None => exit(),
		};
		// Post the receive pool and go live, each slot at its own physical address with the
		// contiguous virtual mapping read back as `rx_virt + id * RX_SLOT`.
		let mut rx_phys: [u64; RX_SLOTS as usize] = [0u64; RX_SLOTS as usize];
		let mut id: u16 = 0;
		while id < RX_SLOTS {
			rx_phys[id as usize] = dma_buffer_phys_at(rxpool, id as u64 * RX_SLOT);
			rx.post_recv(id, rx_phys[id as usize], RX_SLOT as u32);
			id += 1;
		}
		rx.notify();
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
		common::online(bootstrap, &bind, &line[..n], &[(driver_protocol::provider::CONSOLE_BYTES, bytes_far)]);
		let mut port: Port = Port { device: &device, irq, tx: &mut tx, virt: tx_virt, phys: tx_phys, busy: false };
		pump(&bind, irq, bootstrap, bytes, &mut rx, &mut port, rx_virt, &rx_phys)
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
struct Port<'a> {
	device: &'a Virtio,
	irq: u64,
	tx: &'a mut Queue,
	virt: u64,
	phys: u64,
	// The device owns the transmit buffer and has not returned it yet.
	busy: bool,
}

impl Port<'_> {
	// Reap every transmit completion the device has posted, which is what releases the
	// buffer for the next write.
	fn reclaim(&mut self) {
		while self.tx.take_used().is_some() {
			self.busy = false;
		}
	}

	// Write bytes to the port, waiting - bounded - for the previous write to be taken first.
	//
	// THE REFUSAL IS TYPED NOW. This answered `false`, and the loop above it turned that into an
	// EMPTY message to the consumer: a convention that only worked because every frame the
	// consumer sent was at least a header long. `again` says the same thing in the contract's own
	// words - nothing was written and these bytes may be offered again - and it is the answer to
	// the very call that made them, rather than a message arriving out of order with the reply.
	unsafe fn write(&mut self, payload: &[u8], bind: &common::Bind, bootstrap: u64) -> Result<u32, Error> {
		unsafe {
			if payload.is_empty() || payload.len() > MAX_WRITE {
				return Err(Error::Invalid);
			}
			let limit: u64 = clock() + TX_DRAIN_TICKS;
			loop {
				// A host that is slow to consume bytes has not stopped this control path.
				if !common::answer_ping(bootstrap, bind) {
					return Err(Error::Closed);
				}
				self.reclaim();
				if !self.busy {
					break;
				}
				if clock() >= limit {
					return Err(Error::Again);
				}
				if wait_any(&[self.irq, bootstrap], limit) == 0 {
					let _ = self.device.read_isr();
					interrupt_ack(self.irq);
				}
			}
			core::ptr::copy_nonoverlapping(payload.as_ptr(), self.virt as *mut u8, payload.len());
			self.busy = self.tx.submit_async(&[(self.phys, payload.len() as u32, false)]);
			if self.busy { Ok(payload.len() as u32) } else { Err(Error::Io) }
		}
	}
}

// WHAT ONE ATTACHED CONSUMER IS, and all of it: whether it settled a version, the stream endpoint it
// asked for, and how many chunks have gone down that stream.
//
// A session and nothing more. It is reset rather than carried when a consumer is replaced, because
// the bytes a dead consumer's session held belong to that session: handing a fragment of one to its
// replacement would be read as the beginning of a frame.
#[derive(Default)]
struct Attachment {
	attached: bool,
	// The producer end of the receive stream, kept here for the life of the connection. Closing it
	// is what tells the consumer the stream ended.
	stream: u64,
	// The next chunk's sequence number on that stream, which the frame carries so a consumer can
	// see a gap rather than read past one.
	seq: u32,
	// The endpoint `receive` owes its caller, held between the service call that minted it and the
	// reply that carries it away.
	granted: u64,
}

impl Attachment {
	// This consumer is over. The stream goes with it - a consumer that is gone cannot be the one
	// still being written to - and anything a half-finished `receive` minted is closed rather than
	// leaked into the next session.
	fn reset(&mut self) {
		if self.stream != 0 {
			close(self.stream);
		}
		if self.granted != 0 {
			close(self.granted);
		}
		*self = Attachment::default();
	}
}

// The service, which is the port seen through the contract. It borrows rather than owns: the port is
// the driver's and outlives every consumer that ever attaches to it.
struct Console<'a, 'b> {
	port: &'a mut Port<'b>,
	bind: &'a common::Bind,
	bootstrap: u64,
	state: &'a mut Attachment,
}

impl console_stream::Service for Console<'_, '_> {
	fn attach(&mut self, version: u32) -> Result<ConsoleAttachment, Error> {
		if version != CONTRACT_VERSION {
			return Err(Error::Unsupported);
		}
		self.state.attached = true;
		Ok(ConsoleAttachment { version: CONTRACT_VERSION, max_frame: MAX_WRITE as u32 })
	}

	fn write(&mut self, bytes: Vec<u8>) -> Result<u32, Error> {
		// NOT ATTACHED IS NOT A SMALL WRITE. A consumer that has not settled a version does not
		// know how this driver frames what it sends back, so letting its bytes reach the host would
		// put a stream on the wire that neither end can parse.
		if !self.state.attached {
			return Err(Error::Invalid);
		}
		// SAFETY: the port's transmit buffer and its queue are this driver's for the life of the
		// process; `write` is unsafe because it copies into that mapping and submits a descriptor.
		unsafe { self.port.write(&bytes, self.bind, self.bootstrap) }
	}

	fn receive(&mut self) -> Result<Vec<ConsoleChunk>, Error> {
		if !self.state.attached {
			return Err(Error::Invalid);
		}
		// ONE SUBSCRIPTION PER CONNECTION. A second endpoint would be a second reader of one port,
		// and the bytes would be split between them by whichever happened to be drained first.
		if self.state.stream != 0 {
			return Err(Error::Again);
		}
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else {
			return Err(Error::Exhausted);
		};
		self.state.stream = producer;
		self.state.granted = consumer;
		// The snapshot is EMPTY and always will be: a byte stream has no state to describe, only
		// bytes that have not arrived yet.
		Ok(Vec::new())
	}
}

// Move bytes between the port and whichever consumer the catalogue gave this provider to.
//
// The receive ring is drained before every wait rather than only after an interrupt. Both queues
// share the device's single MSI-X vector, so the wait inside a transmit drain can consume the very
// interrupt that announced newly arrived bytes; draining first means the loop never blocks while
// work is already queued, whichever wait observed it.
fn pump(bind: &common::Bind, irq: u64, bootstrap: u64, bytes: u64, rx: &mut Queue, port: &mut Port, rx_virt: u64, rx_phys: &[u64]) -> ! {
	let mut serving: common::Serving = common::Serving::from_offers(&[(CONSOLE_TOKEN, bytes)]);
	let mut state: Attachment = Attachment::default();
	let mut request: Vec<u8> = alloc::vec![0u8; MAX_WRITE + 64];
	let mut reply: Vec<u8> = alloc::vec![0u8; 128];
	let mut frame: Vec<u8> = alloc::vec![0u8; RX_SLOT as usize + 32];
	loop {
		// Anything the device has already handed back goes out before this thread parks, so a
		// completion that arrived with an interrupt somebody else consumed is not left waiting
		// for the next one.
		if drain_receive(bind, bootstrap, rx, rx_virt, rx_phys, &mut state, &mut frame) {
			continue;
		}
		match common::wait_providers_or_answer(bootstrap, bind, &mut serving, &[irq]) {
			None => ended(bootstrap, bind, rx.capability),
			// A REPLACEMENT CONSUMER, and the session before it is over. This is the whole of
			// what used to be the `BYTES` tag and the `adopt` loop underneath it: the manager
			// mints the pair, the driver is told, and nothing here has to know that the process
			// above it was restarted.
			Some(common::ProviderReady::Connected(_)) => state.reset(),
			Some(common::ProviderReady::Consumer(index)) => {
				if !serve_once(&mut serving, index, &mut state, port, bind, bootstrap, &mut request, &mut reply) {
					let token: u16 = serving.close_at(index);
					state.reset();
					// THE MANAGER'S COUNT IS WHAT ADMITS THE REPLACEMENT. This provider admits
					// one consumer, so an unreported departure would refuse the next `open`
					// for the life of the binding - the port would be alive with nobody able
					// to reach it, which is exactly the failure the old private hand-off had
					// no way to express either.
					if !common::disconnected(bootstrap, bind, token) {
						ended(bootstrap, bind, rx.capability);
					}
				}
			}
			Some(common::ProviderReady::Device(_)) => {
				// Read the ISR to deassert the device's level-triggered INTx line before
				// acking (a harmless zero read on MSI-X, which is edge-triggered).
				let _ = port.device.read_isr();
				interrupt_ack(irq);
				port.reclaim();
			}
		}
	}
}

// Serve one request from one consumer. False means that consumer is gone.
fn serve_once(serving: &mut common::Serving, index: usize, state: &mut Attachment, port: &mut Port, bind: &common::Bind, bootstrap: u64, request: &mut [u8], reply: &mut [u8]) -> bool {
	let end: u64 = serving.at(index);
	let (len, handle): (usize, u64) = match recv_blocking(end, request) {
		Received::Message { len, handle } => (len, handle),
		Received::Closed => return false,
	};
	// A request carrying a capability is a request this contract does not define. Closed rather
	// than dropped, so whatever was sent does not stay charged to the sender for ever.
	if handle != 0 {
		close(handle);
	}
	// THE OPCODE IS READ BEFORE THE DISPATCHER SEES IT, because `receive` answers with an ENDPOINT
	// and the generated dispatcher writes bytes. Reading two bytes here is not a second parser: the
	// dispatcher re-reads them and refuses anything it does not know.
	if len >= 2 && u16::from_le_bytes([request[0], request[1]]) == console_stream::OP_RECEIVE {
		return serve_receive(end, state, port, bind, bootstrap, &request[..len], reply);
	}
	let mut request_handles: Handles = Handles::new();
	let mut reply_handles: Handles = Handles::new();
	let mut console: Console = Console { port, bind, bootstrap, state };
	let Some(written) = console_stream::dispatch(&mut console, &request[..len], &mut request_handles, reply, &mut reply_handles) else {
		// A request this contract does not define, or one whose reply would not fit. Neither is
		// answerable, and answering the NEXT request on this connection with this one's reply is
		// the failure a correlation id exists to make visible - so the connection ends here.
		return false;
	};
	send_blocking(end, &reply[..written], reply_handles.first())
}

// `receive`, whose reply carries the stream endpoint. The service mints the pair - it is the one
// that knows whether this consumer may have one - and this hands it over.
fn serve_receive(end: u64, state: &mut Attachment, port: &mut Port, bind: &common::Bind, bootstrap: u64, request: &[u8], reply: &mut [u8]) -> bool {
	let mut request_handles: Handles = Handles::new();
	let granted: u64;
	let outcome: Option<(u32, Result<Vec<ConsoleChunk>, Error>)> = {
		let mut console: Console = Console { port, bind, bootstrap, state };
		console_stream::receive_open(&mut console, request, &mut request_handles)
	};
	let Some((corr, result)) = outcome else { return false };
	match result {
		Ok(_) => {
			granted = core::mem::take(&mut state.granted);
			let Some(written) = console_stream::receive_reply_ok(corr, reply) else {
				close(granted);
				return false;
			};
			if !send_blocking(end, &reply[..written], granted) {
				// The transfer did not happen, so the endpoint is still this driver's - and a
				// producer nobody will ever read is a stream that has to end rather than linger.
				close(granted);
				state.reset();
				return false;
			}
			true
		}
		Err(error) => {
			let Some(written) = console_stream::receive_reply_err(corr, &error, reply) else { return false };
			send_blocking(end, &reply[..written], 0)
		}
	}
}

// Hand the device's completed receive buffers to the attached consumer and put them back on the
// ring. True when anything was taken, which is what tells the caller to look again before parking.
//
// A consumer that has not asked for a stream, or none at all, is not an error: the bytes are
// discarded and the buffer is recycled. Both halves matter. Discarding is right because the bytes
// belong to a session nobody is holding; recycling is right because eight buffers is all the device
// has, and stopping here would leave it with nowhere to put what a host is still writing.
fn drain_receive(bind: &common::Bind, bootstrap: u64, rx: &mut Queue, rx_virt: u64, rx_phys: &[u64], state: &mut Attachment, frame: &mut [u8]) -> bool {
	let mut worked: bool = false;
	for _ in 0..RX_SLOTS {
		let Some((id, len)) = rx.take_used() else { break };
		if id >= RX_SLOTS {
			continue;
		}
		if len > 0 && state.stream != 0 {
			let n: usize = if len as u64 > RX_SLOT { RX_SLOT as usize } else { len as usize };
			// SAFETY: the slot is this driver's DMA mapping, `n` is bounded by the slot size, and
			// the device has given the buffer back - which is what `take_used` means.
			let chunk: &[u8] = unsafe { core::slice::from_raw_parts((rx_virt + id as u64 * RX_SLOT) as *const u8, n) };
			send_chunk(bind, bootstrap, state, chunk, frame);
		}
		rx.post_recv(id, rx_phys[id as usize], RX_SLOT as u32);
		worked = true;
	}
	if worked {
		rx.notify();
	}
	worked
}

// One chunk down the stream, with backpressure retained rather than dropped.
//
// A consumer that is merely slow has not gone, so a stalled send is waited on - and the manager's
// channel is answered while waiting, because a driver that stops answering pings while a consumer
// is behind is a driver a watchdog kills for someone else's slowness. A stream whose consumer has
// closed ends the attachment: there is nobody left to read what the port produces.
fn send_chunk(bind: &common::Bind, bootstrap: u64, state: &mut Attachment, chunk: &[u8], frame: &mut [u8]) {
	let mut handles: Handles = Handles::new();
	let item: ConsoleChunk = ConsoleChunk { bytes: chunk.to_vec() };
	let Some(written) = console_stream::receive_frame(state.seq, &item, frame, &mut handles) else { return };
	state.seq = state.seq.wrapping_add(1);
	loop {
		if !common::answer_ping(bootstrap, bind) {
			return;
		}
		match try_send_outcome(state.stream, &frame[..written], 0) {
			SendOutcome::Delivered => return,
			SendOutcome::Failed => {
				close(state.stream);
				state.stream = 0;
				return;
			}
			SendOutcome::Stalled => {
				// SYS_WAIT_ANY observes readability, not writability, so there is nothing to wait
				// ON here: the wait is on the manager's channel with a one-tick deadline, which
				// keeps the control path live and retries capacity on the next clock tick.
				let ready: i64 = wait_any_periodic(&[bootstrap], clock().saturating_add(1));
				if ready < 0 && ready != ERR_TIMED_OUT {
					return;
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
